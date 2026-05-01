//! Linear watcher — polls Linear's GraphQL API for review queue issues.
//!
//! Polls Linear for issues matching configured review queue queries, converting
//! each into a SignalUpdate. Supports query predicates like `assignee:me`,
//! `state:active`, `team:ENG`, etc. that are translated to Linear GraphQL
//! filter variables.
//!
//! Read-only: no Linear mutation API calls are ever made.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use async_trait::async_trait;
use color_eyre::Result;
use tracing::{info, warn};

/// HTTP request timeout for Linear API calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

use super::Watcher;
use crate::buzz::{
    config::{LinearReviewQueueEntry, LinearWatcherConfig},
    signal::{Severity, SignalUpdate, store::SignalStore},
};

const SOURCE: &str = "linear_review_queue";

/// A single parsed predicate from a query string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPredicate {
    /// `assignee:me` — issues assigned to the authenticated user.
    AssigneeMe,
    /// `subscriber:me` — issues the user is subscribed to.
    SubscriberMe,
    /// `mention:me` — issues where the user is mentioned (comments).
    MentionMe,
    /// `notifications:unread` — unread inbox notifications.
    NotificationsUnread,
    /// `team:<key>` — filter by team key (e.g. `team:ENG`).
    Team(String),
    /// `state:<name>` — filter by workflow state name (e.g. `state:InProgress`).
    State(String),
    /// `priority:<0-4>` — filter by priority level.
    Priority(u8),
}

/// Parse a query string into a list of predicates.
///
/// Multiple predicates in one query are AND-ed together.
/// Unknown predicates are logged and skipped.
pub fn parse_query(query: &str) -> Vec<QueryPredicate> {
    let mut predicates = Vec::new();

    for token in query.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            match key {
                "assignee" if value == "me" => predicates.push(QueryPredicate::AssigneeMe),
                "subscriber" if value == "me" => predicates.push(QueryPredicate::SubscriberMe),
                "mention" if value == "me" => predicates.push(QueryPredicate::MentionMe),
                "notifications" if value == "unread" => {
                    predicates.push(QueryPredicate::NotificationsUnread)
                }
                "team" => predicates.push(QueryPredicate::Team(value.to_string())),
                "state" => predicates.push(QueryPredicate::State(value.to_string())),
                "priority" => {
                    if let Ok(p) = value.parse::<u8>() {
                        if p <= 4 {
                            predicates.push(QueryPredicate::Priority(p));
                        } else {
                            warn!(predicate = token, "invalid priority value (must be 0-4)");
                        }
                    } else {
                        warn!(predicate = token, "invalid priority value");
                    }
                }
                _ => {
                    warn!(predicate = token, "unknown query predicate, skipping");
                }
            }
        } else {
            warn!(token = token, "malformed query token (expected key:value)");
        }
    }

    predicates
}

/// Build a Linear GraphQL `IssueFilter` JSON object from parsed predicates.
///
/// Returns `None` if the predicates include `notifications:unread` (which uses
/// a different query) or if there are no issue-level predicates.
pub fn build_issue_filter(predicates: &[QueryPredicate]) -> Option<serde_json::Value> {
    if predicates
        .iter()
        .any(|p| matches!(p, QueryPredicate::NotificationsUnread))
    {
        return None;
    }

    let mut filter = serde_json::Map::new();

    for predicate in predicates {
        match predicate {
            QueryPredicate::AssigneeMe => {
                filter.insert(
                    "assignee".to_string(),
                    serde_json::json!({ "isMe": { "eq": true } }),
                );
            }
            QueryPredicate::SubscriberMe => {
                filter.insert(
                    "subscribers".to_string(),
                    serde_json::json!({ "isMe": { "eq": true } }),
                );
            }
            QueryPredicate::MentionMe => {
                // Linear doesn't have a direct "mention:me" filter in the GraphQL API.
                // We approximate by using subscriber (mentioned users are auto-subscribed).
                filter.insert(
                    "subscribers".to_string(),
                    serde_json::json!({ "isMe": { "eq": true } }),
                );
            }
            QueryPredicate::Team(key) => {
                filter.insert(
                    "team".to_string(),
                    serde_json::json!({ "key": { "eq": key } }),
                );
            }
            QueryPredicate::State(name) => {
                filter.insert(
                    "state".to_string(),
                    serde_json::json!({ "name": { "eqCaseInsensitive": name } }),
                );
            }
            QueryPredicate::Priority(p) => {
                filter.insert("priority".to_string(), serde_json::json!({ "eq": p }));
            }
            QueryPredicate::NotificationsUnread => {
                // Handled separately — should not reach here.
            }
        }
    }

    if filter.is_empty() {
        return None;
    }

    Some(serde_json::Value::Object(filter))
}

/// Returns `true` if the predicates include `notifications:unread`.
fn is_notification_query(predicates: &[QueryPredicate]) -> bool {
    predicates
        .iter()
        .any(|p| matches!(p, QueryPredicate::NotificationsUnread))
}

/// Map Linear priority (0-4) to apiari Severity.
///
/// - 1 (urgent) -> Critical
/// - 2 (high), 3 (medium) -> Warning
/// - 0 (none), 4 (low) -> Info
pub fn priority_to_severity(priority: u8) -> Severity {
    match priority {
        1 => Severity::Critical,
        2 | 3 => Severity::Warning,
        _ => Severity::Info,
    }
}

/// An issue as returned from the Linear GraphQL API (minimally parsed).
#[derive(Debug, Clone)]
struct LinearIssue {
    #[allow(dead_code)]
    id: String,
    identifier: String,
    title: String,
    url: String,
    priority: u8,
    updated_at: String,
    state_name: String,
    team_key: String,
}

/// Watches Linear for review queue issues via the GraphQL API.
pub struct LinearWatcher {
    config: LinearWatcherConfig,
    client: reqwest::Client,
    watcher_name: String,
    /// Cursor key for persisting the seen map across restarts.
    cursor_key: String,
    /// Map of issue identifier → updatedAt for cross-poll change detection.
    /// Persisted to the cursor store as JSON after each poll.
    seen: HashMap<String, String>,
    /// All fetched issue external_ids from the last poll (for reconciliation).
    fetched_ids: Option<Vec<String>>,
}

impl LinearWatcher {
    pub fn new(config: LinearWatcherConfig) -> Self {
        let watcher_name = format!("{}_linear", config.name);
        let cursor_key = format!("linear_{}_seen", config.name);
        Self {
            config,
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|e| {
                    warn!(
                        "failed to build reqwest client with timeout: {e}, falling back to default"
                    );
                    reqwest::Client::new()
                }),
            watcher_name,
            cursor_key,
            seen: HashMap::new(),
            fetched_ids: None,
        }
    }

    /// Get the cursor key (for loading initial seen map from daemon).
    pub fn cursor_key(&self) -> &str {
        &self.cursor_key
    }

    /// Set the initial seen map from a previously persisted cursor.
    pub fn set_initial_seen(&mut self, seen: HashMap<String, String>) {
        self.seen = seen;
    }

    /// Execute a GraphQL query against the Linear API.
    async fn graphql(
        &self,
        query: &str,
        variables: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut body = serde_json::json!({ "query": query });
        if let Some(vars) = variables {
            body["variables"] = vars;
        }

        let response = match self
            .client
            .post("https://api.linear.app/graphql")
            .header("Authorization", &self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                warn!(error = %e, "failed to send Linear GraphQL request");
                return None;
            }
        };

        if !response.status().is_success() {
            warn!(
                status = %response.status(),
                "Linear GraphQL API returned error status"
            );
            return None;
        }

        match response.json::<serde_json::Value>().await {
            Ok(json) => {
                if let Some(errors) = json.get("errors") {
                    warn!(errors = %errors, "Linear GraphQL returned errors");
                    return None;
                }
                json.get("data").cloned()
            }
            Err(e) => {
                warn!(error = %e, "failed to parse Linear GraphQL response");
                None
            }
        }
    }

    /// Fetch issues matching a filter.
    async fn fetch_issues(&self, filter: &serde_json::Value) -> Vec<LinearIssue> {
        let query = r#"
            query Issues($filter: IssueFilter) {
                issues(filter: $filter, first: 50) {
                    nodes {
                        id
                        identifier
                        title
                        url
                        priority
                        updatedAt
                        state { name }
                        team { key name }
                        assignee { name }
                    }
                }
            }
        "#;

        let variables = serde_json::json!({ "filter": filter });
        let data = match self.graphql(query, Some(variables)).await {
            Some(d) => d,
            None => return Vec::new(),
        };

        self.parse_issues(&data, "issues")
    }

    /// Fetch unread notifications and extract issues from them.
    async fn fetch_notification_issues(&self) -> Vec<LinearIssue> {
        let query = r#"
            query {
                notifications(filter: { readAt: { null: true } }, first: 50) {
                    nodes {
                        id
                        type
                        createdAt
                        issue {
                            id
                            identifier
                            title
                            url
                            priority
                            updatedAt
                            state { name }
                            team { key name }
                        }
                    }
                }
            }
        "#;

        let data = match self.graphql(query, None).await {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut issues = Vec::new();
        if let Some(nodes) = data
            .get("notifications")
            .and_then(|n| n.get("nodes"))
            .and_then(|n| n.as_array())
        {
            for node in nodes {
                if let Some(issue) = node.get("issue").and_then(|i| self.parse_issue(i)) {
                    issues.push(issue);
                }
            }
        }

        issues
    }

    /// Parse a list of issue nodes from a GraphQL response.
    fn parse_issues(&self, data: &serde_json::Value, root_key: &str) -> Vec<LinearIssue> {
        let mut issues = Vec::new();
        if let Some(nodes) = data
            .get(root_key)
            .and_then(|n| n.get("nodes"))
            .and_then(|n| n.as_array())
        {
            for node in nodes {
                if let Some(issue) = self.parse_issue(node) {
                    issues.push(issue);
                }
            }
        }
        issues
    }

    /// Parse a single issue JSON node.
    fn parse_issue(&self, node: &serde_json::Value) -> Option<LinearIssue> {
        let id = node.get("id")?.as_str()?.to_string();
        let identifier = node.get("identifier")?.as_str()?.to_string();
        let title = node.get("title")?.as_str()?.to_string();
        let url = node.get("url")?.as_str()?.to_string();
        let priority = node.get("priority").and_then(|p| p.as_u64()).unwrap_or(0) as u8;
        let updated_at = node.get("updatedAt")?.as_str()?.to_string();
        let state_name = node
            .get("state")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("Unknown")
            .to_string();
        let team_key = node
            .get("team")
            .and_then(|t| t.get("key"))
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .to_string();

        Some(LinearIssue {
            id,
            identifier,
            title,
            url,
            priority,
            updated_at,
            state_name,
            team_key,
        })
    }

    /// Poll a single review queue entry and return issues.
    async fn poll_query(&self, entry: &LinearReviewQueueEntry) -> Vec<(LinearIssue, String)> {
        let predicates = parse_query(&entry.query);

        let issues = if is_notification_query(&predicates) {
            self.fetch_notification_issues().await
        } else if let Some(filter) = build_issue_filter(&predicates) {
            self.fetch_issues(&filter).await
        } else {
            warn!(
                query = entry.query.as_str(),
                name = entry.name.as_str(),
                "no actionable predicates in query"
            );
            Vec::new()
        };

        issues
            .into_iter()
            .map(|issue| {
                let query_name = entry.name.clone();
                (issue, query_name)
            })
            .collect()
    }

    /// Convert a Linear issue into a SignalUpdate.
    fn issue_to_signal(&self, issue: &LinearIssue, query_name: &str) -> SignalUpdate {
        let severity = priority_to_severity(issue.priority);
        let external_id = format!("linear-review-{}", issue.identifier);
        let title = format!("[{}] {}: {}", issue.team_key, issue.identifier, issue.title);

        let metadata = serde_json::json!({
            "query_name": query_name,
            "priority": issue.priority,
            "team": issue.team_key,
            "state": issue.state_name,
        });

        SignalUpdate::new(SOURCE, &external_id, title, severity)
            .with_body(format!(
                "{} — {} ({})",
                query_name, issue.state_name, issue.team_key
            ))
            .with_url(&issue.url)
            .with_metadata(metadata.to_string())
    }
}

#[async_trait]
impl Watcher for LinearWatcher {
    fn name(&self) -> &str {
        &self.watcher_name
    }

    fn signal_source(&self) -> &str {
        SOURCE
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        if self.config.api_key.is_empty() {
            warn!("linear.api_key is empty, skipping poll");
            return Ok(Vec::new());
        }

        let mut all_signals = Vec::new();
        let mut current_seen: HashMap<String, String> = HashMap::new();
        let mut fetched_ids = Vec::new();
        // Dedup across queries within a single poll — first query wins.
        let mut poll_identifiers: HashSet<String> = HashSet::new();

        for entry in &self.config.review_queue.clone() {
            let results = self.poll_query(entry).await;
            for (issue, query_name) in results {
                // Dedup across queries: if same issue appears in multiple queries,
                // highest priority (lowest index) wins.
                if !poll_identifiers.insert(issue.identifier.clone()) {
                    continue;
                }

                let external_id = format!("linear-review-{}", issue.identifier);
                fetched_ids.push(external_id);
                current_seen.insert(issue.identifier.clone(), issue.updated_at.clone());

                // Only emit if the issue is new or has been updated since last seen.
                let should_emit = match self.seen.get(&issue.identifier) {
                    None => true,
                    Some(prev_updated_at) => *prev_updated_at != issue.updated_at,
                };

                if should_emit {
                    all_signals.push(self.issue_to_signal(&issue, &query_name));
                }
            }
        }

        // Update seen map to reflect current poll (drops issues no longer returned).
        self.seen = current_seen;
        self.fetched_ids = Some(fetched_ids);

        if !all_signals.is_empty() {
            info!("linear: {} signal(s)", all_signals.len());
        }

        Ok(all_signals)
    }

    fn reconcile(&self, source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        // Persist seen map to cursor store.
        if let Ok(json) = serde_json::to_string(&self.seen)
            && let Err(e) = store.set_cursor(&self.cursor_key, &json)
        {
            warn!("linear: failed to persist seen map: {e}");
        }

        // Use fetched issue IDs (not emitted signal IDs) for reconcile,
        // because change detection means not all fetched issues emit signals.
        let Some(ref ids) = self.fetched_ids else {
            // Return 1 to prevent auto-reconcile fallback even when poll
            // hasn't run yet (e.g. empty API key early return).
            return Ok(1);
        };
        let resolved = store.resolve_missing_signals(source, ids)?;
        if resolved > 0 {
            info!("linear: reconciled {resolved} resolved signal(s)");
        }
        // Return at least 1 to signal custom reconciliation was handled,
        // preventing framework auto-reconcile with partial poll_ids.
        Ok(resolved.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Query parsing tests --

    #[test]
    fn parse_assignee_me() {
        let predicates = parse_query("assignee:me");
        assert_eq!(predicates, vec![QueryPredicate::AssigneeMe]);
    }

    #[test]
    fn parse_subscriber_me() {
        let predicates = parse_query("subscriber:me");
        assert_eq!(predicates, vec![QueryPredicate::SubscriberMe]);
    }

    #[test]
    fn parse_mention_me() {
        let predicates = parse_query("mention:me");
        assert_eq!(predicates, vec![QueryPredicate::MentionMe]);
    }

    #[test]
    fn parse_notifications_unread() {
        let predicates = parse_query("notifications:unread");
        assert_eq!(predicates, vec![QueryPredicate::NotificationsUnread]);
    }

    #[test]
    fn parse_team_key() {
        let predicates = parse_query("team:ENG");
        assert_eq!(predicates, vec![QueryPredicate::Team("ENG".to_string())]);
    }

    #[test]
    fn parse_state_name() {
        let predicates = parse_query("state:InProgress");
        assert_eq!(
            predicates,
            vec![QueryPredicate::State("InProgress".to_string())]
        );
    }

    #[test]
    fn parse_priority() {
        let predicates = parse_query("priority:2");
        assert_eq!(predicates, vec![QueryPredicate::Priority(2)]);
    }

    #[test]
    fn parse_priority_out_of_range() {
        let predicates = parse_query("priority:5");
        assert!(predicates.is_empty());
    }

    #[test]
    fn parse_multiple_predicates() {
        let predicates = parse_query("assignee:me state:active team:ENG");
        assert_eq!(
            predicates,
            vec![
                QueryPredicate::AssigneeMe,
                QueryPredicate::State("active".to_string()),
                QueryPredicate::Team("ENG".to_string()),
            ]
        );
    }

    #[test]
    fn parse_unknown_predicate_skipped() {
        let predicates = parse_query("assignee:me unknown:value");
        assert_eq!(predicates, vec![QueryPredicate::AssigneeMe]);
    }

    #[test]
    fn parse_empty_query() {
        let predicates = parse_query("");
        assert!(predicates.is_empty());
    }

    // -- GraphQL filter building tests --

    #[test]
    fn build_filter_assignee_me() {
        let predicates = vec![QueryPredicate::AssigneeMe];
        let filter = build_issue_filter(&predicates).unwrap();
        assert_eq!(
            filter,
            serde_json::json!({
                "assignee": { "isMe": { "eq": true } }
            })
        );
    }

    #[test]
    fn build_filter_team_and_state() {
        let predicates = vec![
            QueryPredicate::Team("ENG".to_string()),
            QueryPredicate::State("Todo".to_string()),
        ];
        let filter = build_issue_filter(&predicates).unwrap();
        assert_eq!(
            filter.get("team"),
            Some(&serde_json::json!({ "key": { "eq": "ENG" } }))
        );
        assert_eq!(
            filter.get("state"),
            Some(&serde_json::json!({ "name": { "eqCaseInsensitive": "Todo" } }))
        );
    }

    #[test]
    fn build_filter_priority() {
        let predicates = vec![QueryPredicate::Priority(1)];
        let filter = build_issue_filter(&predicates).unwrap();
        assert_eq!(
            filter.get("priority"),
            Some(&serde_json::json!({ "eq": 1 }))
        );
    }

    #[test]
    fn build_filter_notifications_returns_none() {
        let predicates = vec![QueryPredicate::NotificationsUnread];
        assert!(build_issue_filter(&predicates).is_none());
    }

    #[test]
    fn build_filter_empty_returns_none() {
        let predicates: Vec<QueryPredicate> = vec![];
        assert!(build_issue_filter(&predicates).is_none());
    }

    // -- Priority -> Severity mapping tests --

    #[test]
    fn priority_urgent_is_critical() {
        assert_eq!(priority_to_severity(1), Severity::Critical);
    }

    #[test]
    fn priority_high_is_warning() {
        assert_eq!(priority_to_severity(2), Severity::Warning);
    }

    #[test]
    fn priority_medium_is_warning() {
        assert_eq!(priority_to_severity(3), Severity::Warning);
    }

    #[test]
    fn priority_low_is_info() {
        assert_eq!(priority_to_severity(4), Severity::Info);
    }

    #[test]
    fn priority_none_is_info() {
        assert_eq!(priority_to_severity(0), Severity::Info);
    }

    // -- Deduplication logic test --

    #[test]
    fn dedup_across_queries_first_wins() {
        let mut seen_identifiers: HashSet<String> = HashSet::new();
        let results = vec![
            ("ENG-123", "Assigned to me"),
            ("ENG-456", "Assigned to me"),
            ("ENG-123", "Team issues"), // duplicate — should be skipped
            ("ENG-789", "Team issues"),
        ];

        let mut kept: Vec<(&str, &str)> = Vec::new();
        for (id, query_name) in &results {
            if seen_identifiers.insert(id.to_string()) {
                kept.push((id, query_name));
            }
        }

        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0], ("ENG-123", "Assigned to me"));
        assert_eq!(kept[1], ("ENG-456", "Assigned to me"));
        assert_eq!(kept[2], ("ENG-789", "Team issues"));
    }

    // -- Issue parsing test --

    #[test]
    fn parse_issue_from_json() {
        let watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        let json = serde_json::json!({
            "id": "abc-123",
            "identifier": "ENG-42",
            "title": "Fix the bug",
            "url": "https://linear.app/team/issue/ENG-42",
            "priority": 2,
            "updatedAt": "2025-01-01T00:00:00Z",
            "state": { "name": "In Progress" },
            "team": { "key": "ENG", "name": "Engineering" },
            "assignee": { "name": "Alice" }
        });

        let issue = watcher.parse_issue(&json).unwrap();
        assert_eq!(issue.identifier, "ENG-42");
        assert_eq!(issue.title, "Fix the bug");
        assert_eq!(issue.priority, 2);
        assert_eq!(issue.updated_at, "2025-01-01T00:00:00Z");
        assert_eq!(issue.state_name, "In Progress");
        assert_eq!(issue.team_key, "ENG");
    }

    #[test]
    fn issue_to_signal_mapping() {
        let watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        let issue = LinearIssue {
            id: "abc-123".to_string(),
            identifier: "ENG-42".to_string(),
            title: "Fix the bug".to_string(),
            url: "https://linear.app/team/issue/ENG-42".to_string(),
            priority: 1, // urgent
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            state_name: "In Progress".to_string(),
            team_key: "ENG".to_string(),
        };

        let signal = watcher.issue_to_signal(&issue, "Assigned to me");

        assert_eq!(signal.source, "linear_review_queue");
        assert_eq!(signal.severity, Severity::Critical);
        assert_eq!(signal.title, "[ENG] ENG-42: Fix the bug");
        assert_eq!(signal.external_id, "linear-review-ENG-42");
        assert_eq!(
            signal.url.as_deref(),
            Some("https://linear.app/team/issue/ENG-42")
        );
    }

    // -- Config parsing tests --

    #[test]
    fn test_config_parsing() {
        let toml_str = r#"
[[linear]]
name = "work"
api_key = "lin_api_test"
poll_interval_secs = 120

[[linear.review_queue]]
name = "Assigned to me"
query = "assignee:me"

[[linear.review_queue]]
name = "Team issues"
query = "team:ENG state:active"
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.linear.len(), 1);
        assert_eq!(config.linear[0].name, "work");
        assert_eq!(config.linear[0].api_key, "lin_api_test");
        assert_eq!(config.linear[0].poll_interval_secs, 120);
        assert_eq!(config.linear[0].review_queue.len(), 2);
        assert_eq!(config.linear[0].review_queue[0].name, "Assigned to me");
        assert_eq!(config.linear[0].review_queue[0].query, "assignee:me");
    }

    #[test]
    fn test_config_parsing_buzz_config() {
        let toml_str = r#"
[[watchers.linear]]
name = "work"
api_key = "lin_api_test"

[[watchers.linear.review_queue]]
name = "My issues"
query = "assignee:me"
"#;
        let config: crate::buzz::config::BuzzConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.watchers.linear.len(), 1);
        assert_eq!(config.watchers.linear[0].name, "work");
        assert_eq!(config.watchers.linear[0].api_key, "lin_api_test");
        assert_eq!(config.watchers.linear[0].poll_interval_secs, 60); // default
        assert_eq!(config.watchers.linear[0].review_queue.len(), 1);
    }

    #[test]
    fn test_config_parsing_no_linear_section() {
        let toml_str = r#"
[github]
enabled = true
interval_secs = 60
repos = ["org/repo"]
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert!(config.linear.is_empty());
    }

    // -- Change detection tests --

    #[test]
    fn change_detection_skips_unchanged_issues() {
        let mut watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        // Pre-load seen map (simulating restored cursor state).
        let mut seen = HashMap::new();
        seen.insert("ENG-42".to_string(), "2025-01-01T00:00:00Z".to_string());
        watcher.set_initial_seen(seen);

        let issue = LinearIssue {
            id: "abc-123".to_string(),
            identifier: "ENG-42".to_string(),
            title: "Fix the bug".to_string(),
            url: "https://linear.app/team/issue/ENG-42".to_string(),
            priority: 1,
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            state_name: "In Progress".to_string(),
            team_key: "ENG".to_string(),
        };

        // Same updatedAt → should NOT emit.
        let should_emit = match watcher.seen.get(&issue.identifier) {
            None => true,
            Some(prev) => *prev != issue.updated_at,
        };
        assert!(!should_emit, "unchanged issue should not be re-emitted");
    }

    #[test]
    fn change_detection_emits_updated_issues() {
        let mut watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        let mut seen = HashMap::new();
        seen.insert("ENG-42".to_string(), "2025-01-01T00:00:00Z".to_string());
        watcher.set_initial_seen(seen);

        let issue = LinearIssue {
            id: "abc-123".to_string(),
            identifier: "ENG-42".to_string(),
            title: "Fix the bug".to_string(),
            url: "https://linear.app/team/issue/ENG-42".to_string(),
            priority: 1,
            updated_at: "2025-01-02T12:00:00Z".to_string(), // newer
            state_name: "In Progress".to_string(),
            team_key: "ENG".to_string(),
        };

        // Different updatedAt → should emit.
        let should_emit = match watcher.seen.get(&issue.identifier) {
            None => true,
            Some(prev) => *prev != issue.updated_at,
        };
        assert!(should_emit, "updated issue should be re-emitted");
    }

    #[test]
    fn change_detection_emits_new_issues() {
        let watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        // Not in seen → should emit.
        let should_emit = match watcher.seen.get("ENG-99") {
            None => true,
            Some(prev) => *prev != "2025-01-01T00:00:00Z",
        };
        assert!(should_emit, "new issue should be emitted");
    }

    #[test]
    fn reconcile_uses_fetched_ids() {
        let mut watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        // Simulate poll that fetched 2 issues but only emitted 1 signal.
        watcher.fetched_ids = Some(vec![
            "linear-review-ENG-1".to_string(),
            "linear-review-ENG-2".to_string(),
        ]);

        // fetched_ids should contain both IDs (not just the emitted one).
        let ids = watcher.fetched_ids.as_ref().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"linear-review-ENG-1".to_string()));
        assert!(ids.contains(&"linear-review-ENG-2".to_string()));
    }

    #[test]
    fn reconcile_returns_nonzero_without_fetched_ids() {
        let watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        // Before any poll, fetched_ids is None.
        assert!(watcher.fetched_ids.is_none());

        // reconcile should still return non-zero to prevent auto-reconcile fallback.
        let store = SignalStore::open_memory("test_ws").unwrap();
        let result = watcher.reconcile(SOURCE, &[], &store).unwrap();
        assert!(
            result > 0,
            "reconcile should return non-zero even without fetched_ids"
        );
    }

    #[test]
    fn cursor_key_format() {
        let watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "work".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });
        assert_eq!(watcher.cursor_key(), "linear_work_seen");
    }

    #[test]
    fn parse_issue_requires_updated_at() {
        let watcher = LinearWatcher::new(LinearWatcherConfig {
            name: "test".to_string(),
            api_key: "test".to_string(),
            poll_interval_secs: 60,
            review_queue: vec![],
        });

        // Missing updatedAt should return None.
        let json = serde_json::json!({
            "id": "abc-123",
            "identifier": "ENG-42",
            "title": "Fix the bug",
            "url": "https://linear.app/team/issue/ENG-42",
            "priority": 2,
            "state": { "name": "In Progress" },
            "team": { "key": "ENG" }
        });
        assert!(
            watcher.parse_issue(&json).is_none(),
            "missing updatedAt should fail parse"
        );
    }
}

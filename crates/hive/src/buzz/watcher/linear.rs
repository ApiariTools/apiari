//! Linear watcher — polls Linear's GraphQL API for review queue issues.
//!
//! Polls Linear for issues matching configured review queue queries, converting
//! each into a Signal. Supports query predicates like `assignee:me`, `state:active`,
//! `team:ENG`, etc. that are translated to Linear GraphQL filter variables.
//!
//! Read-only: no Linear mutation API calls are ever made.

use std::any::Any;
use std::collections::HashSet;

use crate::buzz::config::{LinearConfig, LinearReviewQueueEntry};
use crate::signal::{Severity, Signal};
use async_trait::async_trait;
use color_eyre::Result;

use super::Watcher;

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
                            tracing::warn!(
                                predicate = token,
                                "invalid priority value (must be 0-4)"
                            );
                        }
                    } else {
                        tracing::warn!(predicate = token, "invalid priority value");
                    }
                }
                _ => {
                    tracing::warn!(predicate = token, "unknown query predicate, skipping");
                }
            }
        } else {
            tracing::warn!(token = token, "malformed query token (expected key:value)");
        }
    }

    predicates
}

/// Build a Linear GraphQL `IssueFilter` JSON object from parsed predicates.
///
/// Returns `None` if the predicates include `notifications:unread` (which uses
/// a different query) or if there are no issue-level predicates.
pub fn build_issue_filter(predicates: &[QueryPredicate]) -> Option<serde_json::Value> {
    // If any predicate is NotificationsUnread, this query uses the notifications endpoint.
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
/// - 1 (urgent) → Critical
/// - 2 (high) → Error (mapped to Warning since apiari has no Error level)
/// - 3 (medium) → Warning
/// - 0 (none), 4 (low) → Info
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
    state_name: String,
    team_key: String,
}

/// Watches Linear for review queue issues via the GraphQL API.
pub struct LinearWatcher {
    config: LinearConfig,
    client: reqwest::Client,
    /// Dedup keys of signals emitted in previous polls, keyed by the set of
    /// currently-active dedup keys. Updated each poll to exactly the current
    /// active set.
    seen: HashSet<String>,
}

impl LinearWatcher {
    pub fn new(config: LinearConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            seen: HashSet::new(),
        }
    }

    /// Return the current seen set (for persistence).
    pub fn seen(&self) -> &HashSet<String> {
        &self.seen
    }

    /// Restore seen set from persisted state.
    pub fn restore_seen(&mut self, seen: HashSet<String>) {
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
                tracing::error!(error = %e, "failed to send Linear GraphQL request");
                return None;
            }
        };

        if !response.status().is_success() {
            tracing::error!(
                status = %response.status(),
                "Linear GraphQL API returned error status"
            );
            return None;
        }

        match response.json::<serde_json::Value>().await {
            Ok(json) => {
                if let Some(errors) = json.get("errors") {
                    tracing::error!(errors = %errors, "Linear GraphQL returned errors");
                    return None;
                }
                json.get("data").cloned()
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to parse Linear GraphQL response");
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
            state_name,
            team_key,
        })
    }

    /// Poll a single review queue entry and return deduplicated issues.
    async fn poll_query(&self, entry: &LinearReviewQueueEntry) -> Vec<(LinearIssue, String)> {
        let predicates = parse_query(&entry.query);

        let issues = if is_notification_query(&predicates) {
            self.fetch_notification_issues().await
        } else if let Some(filter) = build_issue_filter(&predicates) {
            self.fetch_issues(&filter).await
        } else {
            tracing::warn!(
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

    /// Convert a Linear issue into a Signal.
    fn issue_to_signal(&self, issue: &LinearIssue, query_name: &str) -> Signal {
        let severity = priority_to_severity(issue.priority);

        let metadata = serde_json::json!({
            "query_name": query_name,
            "priority": issue.priority,
            "team": issue.team_key,
            "state": issue.state_name,
        });

        Signal::new(
            "linear_review_queue",
            severity,
            format!("[{}] {}: {}", issue.team_key, issue.identifier, issue.title),
            metadata.to_string(),
        )
        .with_url(&issue.url)
        .with_dedup_key(format!("linear-review-{}", issue.identifier))
        .with_tags(["linear", "review", &issue.team_key])
    }
}

#[async_trait]
impl Watcher for LinearWatcher {
    fn name(&self) -> &str {
        "linear"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn poll(&mut self) -> Result<Vec<Signal>> {
        if self.config.api_key.is_empty() {
            tracing::warn!("linear.api_key is empty, skipping poll");
            return Ok(Vec::new());
        }

        let mut all_signals = Vec::new();
        let mut current_keys: HashSet<String> = HashSet::new();
        // Track which issue identifiers have been seen — first query wins (highest priority).
        let mut seen_identifiers: HashSet<String> = HashSet::new();

        for entry in &self.config.review_queue.clone() {
            let results = self.poll_query(entry).await;
            for (issue, query_name) in results {
                // Dedup across queries: if same issue appears in multiple queries,
                // highest priority (lowest index) wins.
                if !seen_identifiers.insert(issue.identifier.clone()) {
                    continue;
                }

                let signal = self.issue_to_signal(&issue, &query_name);
                if let Some(ref key) = signal.dedup_key {
                    current_keys.insert(key.clone());
                    if !self.seen.contains(key) {
                        all_signals.push(signal);
                    }
                } else {
                    all_signals.push(signal);
                }
            }
        }

        // Prune seen to only currently-active signals.
        self.seen = current_keys;

        Ok(all_signals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Query parsing tests ──

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

    // ── GraphQL filter building tests ──

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

    // ── Priority → Severity mapping tests ──

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

    // ── Deduplication logic test ──

    #[test]
    fn dedup_across_queries_first_wins() {
        // Simulate the dedup logic: if an issue identifier appears in multiple
        // query results, the first query (highest priority) wins.
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

    // ── Issue parsing test ──

    #[test]
    fn parse_issue_from_json() {
        let watcher = LinearWatcher::new(LinearConfig {
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
        assert_eq!(issue.state_name, "In Progress");
        assert_eq!(issue.team_key, "ENG");
    }

    #[test]
    fn issue_to_signal_mapping() {
        let watcher = LinearWatcher::new(LinearConfig {
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
            state_name: "In Progress".to_string(),
            team_key: "ENG".to_string(),
        };

        let signal = watcher.issue_to_signal(&issue, "Assigned to me");

        assert_eq!(signal.source, "linear_review_queue");
        assert_eq!(signal.severity, Severity::Critical);
        assert_eq!(signal.title, "[ENG] ENG-42: Fix the bug");
        assert_eq!(signal.dedup_key.as_deref(), Some("linear-review-ENG-42"));
        assert_eq!(
            signal.url.as_deref(),
            Some("https://linear.app/team/issue/ENG-42")
        );
        assert!(signal.tags.contains(&"linear".to_string()));
        assert!(signal.tags.contains(&"review".to_string()));
    }
}

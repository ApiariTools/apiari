//! Notion watcher — polls the Notion API for mentions and assignments.
//!
//! Emits signals into `{name}_review_queue` so they auto-appear in the Reviews
//! panel. Two signal types:
//! 1. Comments mentioning the configured user
//! 2. Database pages assigned to the configured user

use async_trait::async_trait;
use color_eyre::Result;
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::config::NotionWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";

/// Common assignee-like property names to try when querying databases.
const ASSIGNEE_PROPERTY_NAMES: &[&str] = &["Assignee", "Assigned to", "Owner", "Person"];

/// Watches Notion for comment mentions and database assignments.
pub struct NotionWatcher {
    config: NotionWatcherConfig,
    client: reqwest::Client,
    watcher_name: String,
    source: String,
    search_cursor_key: String,
    last_poll_key: String,
    last_poll_time: Option<String>,
}

impl NotionWatcher {
    pub fn new(config: NotionWatcherConfig) -> Self {
        let watcher_name = format!("{}_notion", config.name);
        let source = format!("{}_review_queue", config.name);
        let search_cursor_key = format!("notion_{}_search_cursor", config.name);
        let last_poll_key = format!("notion_{}_last_poll", config.name);
        Self {
            config,
            client: reqwest::Client::new(),
            watcher_name,
            source,
            search_cursor_key,
            last_poll_key,
            last_poll_time: None,
        }
    }

    /// Build the signal source name for a Notion watcher.
    pub fn source_name(name: &str) -> String {
        format!("{name}_review_queue")
    }

    /// Build the search cursor key for a Notion watcher.
    pub fn search_cursor_key_for(name: &str) -> String {
        format!("notion_{name}_search_cursor")
    }

    /// Build the last-poll cursor key for a Notion watcher.
    pub fn last_poll_key_for(name: &str) -> String {
        format!("notion_{name}_last_poll")
    }

    /// Set the initial last-poll time from a previously persisted cursor.
    pub fn set_initial_last_poll(&mut self, time: String) {
        self.last_poll_time = Some(time);
    }

    /// Get the last-poll cursor key (for loading initial cursor from daemon).
    pub fn last_poll_key(&self) -> &str {
        &self.last_poll_key
    }

    /// Make an authenticated request to the Notion API.
    async fn notion_request(
        &self,
        method: reqwest::Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, NotionApiError> {
        Ok(self
            .client
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Notion-Version", NOTION_VERSION)
            .header("Content-Type", "application/json"))
    }

    /// Search for recently-edited pages.
    async fn search_pages(&self) -> std::result::Result<Vec<NotionPage>, NotionApiError> {
        let url = format!("{NOTION_API_BASE}/search");
        let mut body = serde_json::json!({
            "filter": {"value": "page", "property": "object"},
            "sort": {"direction": "descending", "timestamp": "last_edited_time"}
        });

        // If we have a last poll time, use it to limit results
        if let Some(ref last_poll) = self.last_poll_time {
            body["filter"]["last_edited_time"] = serde_json::json!({"after": last_poll});
        }

        let req = self.notion_request(reqwest::Method::POST, &url).await?;
        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(NotionApiError::Network)?;

        check_response_status(&resp)?;

        let json: serde_json::Value = resp.json().await.map_err(NotionApiError::Network)?;
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(results.into_iter().filter_map(parse_page).collect())
    }

    /// Fetch comments for a page/block.
    async fn fetch_comments(
        &self,
        block_id: &str,
    ) -> std::result::Result<Vec<NotionComment>, NotionApiError> {
        let url = format!("{NOTION_API_BASE}/comments?block_id={block_id}");
        let req = self.notion_request(reqwest::Method::GET, &url).await?;
        let resp = req.send().await.map_err(NotionApiError::Network)?;

        check_response_status(&resp)?;

        let json: serde_json::Value = resp.json().await.map_err(NotionApiError::Network)?;
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(results.into_iter().filter_map(parse_comment).collect())
    }

    /// Query a database for pages assigned to the configured user.
    async fn query_database_assignments(
        &self,
        database_id: &str,
    ) -> std::result::Result<Vec<NotionPage>, NotionApiError> {
        let url = format!("{NOTION_API_BASE}/databases/{database_id}/query");

        // Try common property names for assignee
        for prop_name in ASSIGNEE_PROPERTY_NAMES {
            let body = serde_json::json!({
                "filter": {
                    "property": prop_name,
                    "people": {"contains": self.config.user_id}
                }
            });

            let req = self.notion_request(reqwest::Method::POST, &url).await?;
            let resp = req
                .json(&body)
                .send()
                .await
                .map_err(NotionApiError::Network)?;

            // If we get a validation error (400), try next property name
            if resp.status() == reqwest::StatusCode::BAD_REQUEST {
                continue;
            }

            check_response_status(&resp)?;

            let json: serde_json::Value = resp.json().await.map_err(NotionApiError::Network)?;
            let results = json
                .get("results")
                .and_then(|r| r.as_array())
                .cloned()
                .unwrap_or_default();

            return Ok(results.into_iter().filter_map(parse_page).collect());
        }

        // None of the property names worked — skip this database
        Ok(Vec::new())
    }

    /// Poll for comment mentions and build signals.
    async fn poll_mentions(&self) -> Vec<SignalUpdate> {
        let pages = match self.search_pages().await {
            Ok(p) => p,
            Err(e) => {
                log_api_error(&self.watcher_name, "search", e);
                return Vec::new();
            }
        };

        let mut signals = Vec::new();
        for page in &pages {
            let comments = match self.fetch_comments(&page.id).await {
                Ok(c) => c,
                Err(e) => {
                    log_api_error(&self.watcher_name, "comments", e);
                    continue;
                }
            };

            for comment in &comments {
                // Skip if:
                // - We wrote this comment ourselves
                // - It doesn't mention us and isn't new since last poll
                let mentions_user = comment_mentions_user(comment, &self.config.user_id);
                let is_other_user_comment = comment.created_by_id != self.config.user_id;
                let is_new = self
                    .last_poll_time
                    .as_ref()
                    .is_none_or(|lp| comment.created_time.as_str() > lp.as_str());

                if !mentions_user && !(is_other_user_comment && is_new) {
                    continue;
                }

                signals.push(build_comment_signal(&self.source, &page.title, comment));
            }
        }

        signals
    }

    /// Poll for database assignments and build signals.
    async fn poll_assignments(&self) -> Vec<SignalUpdate> {
        let db_ids = match &self.config.poll_database_ids {
            Some(ids) if !ids.is_empty() => ids.clone(),
            _ => return Vec::new(),
        };

        let mut signals = Vec::new();
        for db_id in &db_ids {
            let pages = match self.query_database_assignments(db_id).await {
                Ok(p) => p,
                Err(e) => {
                    log_api_error(&self.watcher_name, "database query", e);
                    continue;
                }
            };

            for page in &pages {
                signals.push(build_assignment_signal(&self.source, page, db_id));
            }
        }

        signals
    }
}

#[async_trait]
impl Watcher for NotionWatcher {
    fn name(&self) -> &str {
        &self.watcher_name
    }

    fn signal_source(&self) -> &str {
        &self.source
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        let mut all_signals = Vec::new();

        // 1. Comment mentions
        let mention_signals = self.poll_mentions().await;
        all_signals.extend(mention_signals);

        // 2. Database assignments
        let assignment_signals = self.poll_assignments().await;
        all_signals.extend(assignment_signals);

        if !all_signals.is_empty() {
            info!(
                "[{}] {} signal(s) from Notion",
                self.watcher_name,
                all_signals.len()
            );
        }

        // Update last poll time
        let now = chrono::Utc::now().to_rfc3339();
        self.last_poll_time = Some(now);

        Ok(all_signals)
    }

    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        // Persist last poll cursor
        if let Some(ref lp) = self.last_poll_time
            && let Err(e) = store.set_cursor(&self.last_poll_key, lp)
        {
            warn!(
                "[{}] failed to persist last poll cursor: {e}",
                self.watcher_name
            );
        }
        // Return 0 — let the framework auto-reconcile assignment signals.
        // Comment signals are additive (like email), but assignment signals
        // should be resolved when the user is unassigned (auto-reconcile
        // handles this since we emit all current assignments each poll).
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// A parsed Notion page.
#[derive(Debug, Clone)]
pub struct NotionPage {
    pub id: String,
    pub title: String,
    pub url: String,
    pub status: Option<String>,
}

/// A parsed Notion comment.
#[derive(Debug, Clone)]
pub struct NotionComment {
    pub id: String,
    pub created_by_id: String,
    pub created_by_name: Option<String>,
    pub created_time: String,
    pub plain_text: String,
    pub rich_text: Vec<serde_json::Value>,
}

/// Notion API error categories.
#[derive(Debug)]
pub enum NotionApiError {
    Unauthorized,
    RateLimited,
    Network(reqwest::Error),
    Other(u16, String),
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse a page object from Notion API response.
fn parse_page(val: serde_json::Value) -> Option<NotionPage> {
    let id = val.get("id")?.as_str()?.to_string();
    let url = val
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();

    // Title can be in properties.title, properties.Name, or just "title"
    let title = extract_page_title(&val).unwrap_or_else(|| "Untitled".to_string());

    // Try to extract status from properties
    let status = val
        .get("properties")
        .and_then(|p| p.get("Status"))
        .and_then(|s| s.get("status"))
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    Some(NotionPage {
        id,
        title,
        url,
        status,
    })
}

/// Extract a page title from various Notion property formats.
pub fn extract_page_title(page: &serde_json::Value) -> Option<String> {
    let props = page.get("properties")?;

    // Try "Name" and "title" property names (most common)
    for key in &["Name", "title", "Title"] {
        if let Some(prop) = props.get(*key) {
            if let Some(title_arr) = prop.get("title").and_then(|t| t.as_array()) {
                let text: String = title_arr
                    .iter()
                    .filter_map(|t| t.get("plain_text").and_then(|p| p.as_str()))
                    .collect();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }

    None
}

/// Parse a comment from Notion API response.
fn parse_comment(val: serde_json::Value) -> Option<NotionComment> {
    let id = val.get("id")?.as_str()?.to_string();
    let created_by_id = val
        .get("created_by")
        .and_then(|c| c.get("id"))
        .and_then(|i| i.as_str())?
        .to_string();
    let created_by_name = val
        .get("created_by")
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    let created_time = val.get("created_time")?.as_str()?.to_string();

    let rich_text = val
        .get("rich_text")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();

    let plain_text: String = rich_text
        .iter()
        .filter_map(|t| t.get("plain_text").and_then(|p| p.as_str()))
        .collect();

    Some(NotionComment {
        id,
        created_by_id,
        created_by_name,
        created_time,
        plain_text,
        rich_text,
    })
}

/// Check if a comment mentions a specific user ID.
pub fn comment_mentions_user(comment: &NotionComment, user_id: &str) -> bool {
    comment.rich_text.iter().any(|element| {
        element.get("type").and_then(|t| t.as_str()) == Some("mention")
            && element
                .get("mention")
                .and_then(|m| m.get("user"))
                .and_then(|u| u.get("id"))
                .and_then(|i| i.as_str())
                == Some(user_id)
    })
}

/// Check the HTTP response for Notion-specific error codes.
fn check_response_status(resp: &reqwest::Response) -> std::result::Result<(), NotionApiError> {
    match resp.status().as_u16() {
        200..=299 => Ok(()),
        401 => Err(NotionApiError::Unauthorized),
        429 => Err(NotionApiError::RateLimited),
        code => Err(NotionApiError::Other(code, format!("HTTP {code}"))),
    }
}

/// Log a Notion API error with appropriate severity.
fn log_api_error(watcher_name: &str, operation: &str, error: NotionApiError) {
    match error {
        NotionApiError::Unauthorized => {
            warn!("[{watcher_name}] Notion token invalid or expired (during {operation})");
        }
        NotionApiError::RateLimited => {
            warn!(
                "[{watcher_name}] Notion rate limited, skipping this poll cycle (during {operation})"
            );
        }
        NotionApiError::Network(e) => {
            warn!("[{watcher_name}] Notion network error during {operation}: {e}");
        }
        NotionApiError::Other(code, msg) => {
            warn!("[{watcher_name}] Notion API error during {operation}: {msg} (HTTP {code})");
        }
    }
}

// ---------------------------------------------------------------------------
// Signal builders
// ---------------------------------------------------------------------------

/// Build a signal from a comment mention.
pub fn build_comment_signal(
    source: &str,
    page_title: &str,
    comment: &NotionComment,
) -> SignalUpdate {
    let commenter = comment.created_by_name.as_deref().unwrap_or("Unknown user");
    let external_id = format!("comment-{}", comment.id);
    let title = format!("{page_title} — {commenter}");

    SignalUpdate::new(source, external_id, title, Severity::Info).with_body(&comment.plain_text)
}

/// Build a signal from a database assignment.
pub fn build_assignment_signal(source: &str, page: &NotionPage, database_id: &str) -> SignalUpdate {
    let external_id = format!("page-{}", page.id);
    let body = if let Some(ref status) = page.status {
        format!("Database: {database_id} | Status: {status}")
    } else {
        format!("Database: {database_id}")
    };

    let mut signal =
        SignalUpdate::new(source, external_id, &page.title, Severity::Info).with_body(body);
    if !page.url.is_empty() {
        signal = signal.with_url(&page.url);
    }
    signal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::config::NotionWatcherConfig;

    fn test_config() -> NotionWatcherConfig {
        NotionWatcherConfig {
            name: "notion".to_string(),
            token: "secret_test_token".to_string(),
            user_id: "user-abc-123".to_string(),
            poll_database_ids: Some(vec!["db-id-1".to_string()]),
            interval_secs: 120,
        }
    }

    // --- Config and naming ---

    #[test]
    fn test_source_name_format() {
        assert_eq!(NotionWatcher::source_name("notion"), "notion_review_queue");
    }

    #[test]
    fn test_source_name_custom() {
        assert_eq!(
            NotionWatcher::source_name("work_notion"),
            "work_notion_review_queue"
        );
    }

    #[test]
    fn test_watcher_name_and_source() {
        let watcher = NotionWatcher::new(test_config());
        assert_eq!(watcher.name(), "notion_notion");
        assert_eq!(watcher.signal_source(), "notion_review_queue");
    }

    #[test]
    fn test_config_parsing() {
        let toml_str = r#"
[[notion]]
name = "notion"
token = "secret_xxxx"
user_id = "abc123"
poll_database_ids = ["db-1", "db-2"]
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.notion.len(), 1);
        assert_eq!(config.notion[0].name, "notion");
        assert_eq!(config.notion[0].token, "secret_xxxx");
        assert_eq!(config.notion[0].user_id, "abc123");
        assert_eq!(
            config.notion[0].poll_database_ids,
            Some(vec!["db-1".to_string(), "db-2".to_string()])
        );
    }

    #[test]
    fn test_config_parsing_no_databases() {
        let toml_str = r#"
[[notion]]
name = "notion"
token = "secret_xxxx"
user_id = "abc123"
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.notion.len(), 1);
        assert!(config.notion[0].poll_database_ids.is_none());
    }

    #[test]
    fn test_config_parsing_no_notion_section() {
        let toml_str = r#"
[github]
enabled = true
interval_secs = 60
repos = ["org/repo"]
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert!(config.notion.is_empty());
    }

    // --- Cursor key naming ---

    #[test]
    fn test_cursor_key_naming() {
        assert_eq!(
            NotionWatcher::search_cursor_key_for("work"),
            "notion_work_search_cursor"
        );
        assert_eq!(
            NotionWatcher::last_poll_key_for("work"),
            "notion_work_last_poll"
        );
    }

    // --- Comment signal construction ---

    #[test]
    fn test_build_comment_signal() {
        let comment = NotionComment {
            id: "comment-001".to_string(),
            created_by_id: "other-user".to_string(),
            created_by_name: Some("Alice".to_string()),
            created_time: "2026-03-17T10:00:00Z".to_string(),
            plain_text: "Hey, can you review this?".to_string(),
            rich_text: vec![],
        };

        let signal = build_comment_signal("notion_review_queue", "Project Plan", &comment);
        assert_eq!(signal.source, "notion_review_queue");
        assert_eq!(signal.external_id, "comment-comment-001");
        assert_eq!(signal.title, "Project Plan — Alice");
        assert_eq!(signal.severity, Severity::Info);
        assert_eq!(signal.body.as_deref(), Some("Hey, can you review this?"));
    }

    #[test]
    fn test_build_comment_signal_unknown_user() {
        let comment = NotionComment {
            id: "c-002".to_string(),
            created_by_id: "other".to_string(),
            created_by_name: None,
            created_time: "2026-03-17T10:00:00Z".to_string(),
            plain_text: "Check this".to_string(),
            rich_text: vec![],
        };

        let signal = build_comment_signal("src", "Page", &comment);
        assert_eq!(signal.title, "Page — Unknown user");
    }

    // --- Assignment signal construction ---

    #[test]
    fn test_build_assignment_signal() {
        let page = NotionPage {
            id: "page-xyz".to_string(),
            title: "Fix login bug".to_string(),
            url: "https://notion.so/page-xyz".to_string(),
            status: Some("In Progress".to_string()),
        };

        let signal = build_assignment_signal("notion_review_queue", &page, "db-id-1");
        assert_eq!(signal.source, "notion_review_queue");
        assert_eq!(signal.external_id, "page-page-xyz");
        assert_eq!(signal.title, "Fix login bug");
        assert_eq!(signal.severity, Severity::Info);

        let body = signal.body.unwrap();
        assert!(body.contains("db-id-1"));
        assert!(body.contains("In Progress"));
        assert_eq!(signal.url.as_deref(), Some("https://notion.so/page-xyz"));
    }

    #[test]
    fn test_build_assignment_signal_no_status() {
        let page = NotionPage {
            id: "page-abc".to_string(),
            title: "Task".to_string(),
            url: "".to_string(),
            status: None,
        };

        let signal = build_assignment_signal("src", &page, "db-1");
        let body = signal.body.unwrap();
        assert!(body.contains("db-1"));
        assert!(!body.contains("Status"));
        assert!(signal.url.is_none());
    }

    // --- Mention detection ---

    #[test]
    fn test_comment_mentions_user_positive() {
        let comment = NotionComment {
            id: "c1".to_string(),
            created_by_id: "other".to_string(),
            created_by_name: None,
            created_time: "2026-03-17T10:00:00Z".to_string(),
            plain_text: "@User mentioned".to_string(),
            rich_text: vec![serde_json::json!({
                "type": "mention",
                "mention": {
                    "type": "user",
                    "user": {"id": "target-user-id"}
                },
                "plain_text": "@User"
            })],
        };

        assert!(comment_mentions_user(&comment, "target-user-id"));
    }

    #[test]
    fn test_comment_mentions_user_negative() {
        let comment = NotionComment {
            id: "c2".to_string(),
            created_by_id: "other".to_string(),
            created_by_name: None,
            created_time: "2026-03-17T10:00:00Z".to_string(),
            plain_text: "No mentions here".to_string(),
            rich_text: vec![serde_json::json!({
                "type": "text",
                "plain_text": "No mentions here"
            })],
        };

        assert!(!comment_mentions_user(&comment, "target-user-id"));
    }

    #[test]
    fn test_comment_mentions_user_wrong_user() {
        let comment = NotionComment {
            id: "c3".to_string(),
            created_by_id: "other".to_string(),
            created_by_name: None,
            created_time: "2026-03-17T10:00:00Z".to_string(),
            plain_text: "@Other mentioned".to_string(),
            rich_text: vec![serde_json::json!({
                "type": "mention",
                "mention": {
                    "type": "user",
                    "user": {"id": "different-user-id"}
                },
                "plain_text": "@Other"
            })],
        };

        assert!(!comment_mentions_user(&comment, "target-user-id"));
    }

    // --- Page title extraction ---

    #[test]
    fn test_extract_page_title() {
        let page = serde_json::json!({
            "properties": {
                "Name": {
                    "title": [{"plain_text": "My Page"}]
                }
            }
        });
        assert_eq!(extract_page_title(&page), Some("My Page".to_string()));
    }

    // --- Parse helpers ---

    #[test]
    fn test_parse_page() {
        let val = serde_json::json!({
            "id": "page-123",
            "url": "https://notion.so/page-123",
            "properties": {
                "Name": {
                    "title": [{"plain_text": "Test Page"}]
                },
                "Status": {
                    "status": {"name": "Done"}
                }
            }
        });

        let page = parse_page(val).unwrap();
        assert_eq!(page.id, "page-123");
        assert_eq!(page.title, "Test Page");
        assert_eq!(page.url, "https://notion.so/page-123");
        assert_eq!(page.status, Some("Done".to_string()));
    }

    #[test]
    fn test_parse_comment() {
        let val = serde_json::json!({
            "id": "comment-456",
            "created_by": {"id": "user-789", "name": "Bob"},
            "created_time": "2026-03-17T12:00:00Z",
            "rich_text": [
                {"type": "text", "plain_text": "Hello "},
                {"type": "mention", "plain_text": "@Alice",
                 "mention": {"type": "user", "user": {"id": "user-abc"}}}
            ]
        });

        let comment = parse_comment(val).unwrap();
        assert_eq!(comment.id, "comment-456");
        assert_eq!(comment.created_by_id, "user-789");
        assert_eq!(comment.created_by_name, Some("Bob".to_string()));
        assert_eq!(comment.plain_text, "Hello @Alice");
        assert_eq!(comment.rich_text.len(), 2);
    }
}

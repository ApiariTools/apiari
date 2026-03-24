//! Sentry watcher — polls the Sentry API for unresolved issues.
//!
//! Uses cross-poll dedup via seen_issues map to avoid re-emitting
//! stale issues that reorder in Sentry's date-sorted results.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// HTTP request timeout for Sentry API calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

use super::Watcher;
use crate::buzz::config::SentryWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

/// Per-issue metadata tracked across polls for dedup decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueMeta {
    pub last_triaged_at: DateTime<Utc>,
    pub event_count: u64,
    pub severity: String,
}

/// Watches Sentry for unresolved issues via the REST API.
pub struct SentryWatcher {
    config: SentryWatcherConfig,
    client: reqwest::Client,
    seen_issues: HashMap<String, IssueMeta>,
    /// All fetched issue IDs (not just emitted), for reconciliation.
    /// Sentry needs custom reconcile because it tracks more IDs than it emits.
    fetched_ids: Option<Vec<String>>,
}

impl SentryWatcher {
    pub fn new(config: SentryWatcherConfig) -> Self {
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
            seen_issues: HashMap::new(),
            fetched_ids: None,
        }
    }

    fn map_severity(level: &str) -> Severity {
        match level {
            "fatal" | "error" => Severity::Critical,
            "warning" => Severity::Warning,
            _ => Severity::Info,
        }
    }

    fn extract_issue_fields(issue: &serde_json::Value) -> Option<(&str, &str, u64)> {
        let id = issue.get("id")?.as_str()?;
        let level = issue
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let count = issue
            .get("count")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Some((id, level, count))
    }

    async fn fetch_issues(&self) -> Result<Vec<serde_json::Value>> {
        let url = format!(
            "https://sentry.io/api/0/projects/{org}/{project}/issues/",
            org = self.config.org,
            project = self.config.project,
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .query(&[
                ("query", "is:unresolved"),
                ("sort", "date"),
                ("per_page", "100"),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            warn!(
                "Sentry API returned {status} for {org}/{project}: {body}",
                org = self.config.org,
                project = self.config.project,
            );
            return Ok(Vec::new());
        }

        let issues: Vec<serde_json::Value> = response.json().await?;
        Ok(issues)
    }

    fn issue_to_signal(&self, issue: &serde_json::Value) -> Option<SignalUpdate> {
        let id = issue.get("id")?.as_str()?;
        let title = issue.get("title")?.as_str()?;
        let culprit = issue.get("culprit").and_then(|v| v.as_str()).unwrap_or("");
        let level = issue
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let permalink = issue.get("permalink").and_then(|v| v.as_str());
        let count = issue.get("count").and_then(|v| v.as_str()).unwrap_or("0");

        let severity = Self::map_severity(level);
        let body = format!("{culprit}\nLevel: {level} | Events: {count}");

        let mut signal = SignalUpdate::new(
            "sentry",
            format!("sentry-{id}"),
            format!(
                "[{org}/{project}] {title}",
                org = self.config.org,
                project = self.config.project,
            ),
            severity,
        )
        .with_body(body);

        if let Some(url) = permalink {
            signal = signal.with_url(url);
        }

        Some(signal)
    }

    fn record_seen(&mut self, id: &str, level: &str, event_count: u64) {
        self.seen_issues.insert(
            id.to_string(),
            IssueMeta {
                last_triaged_at: Utc::now(),
                event_count,
                severity: level.to_string(),
            },
        );
    }
}

#[async_trait]
impl Watcher for SentryWatcher {
    fn name(&self) -> &str {
        "sentry"
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        let issues = match self.fetch_issues().await {
            Ok(issues) => issues,
            Err(e) => {
                warn!(
                    "failed to fetch sentry issues for {org}/{project}: {e}",
                    org = self.config.org,
                    project = self.config.project,
                );
                return Ok(Vec::new());
            }
        };

        if issues.is_empty() {
            // All issues resolved — empty list so reconcile resolves remaining DB signals.
            self.fetched_ids = Some(Vec::new());
            return Ok(Vec::new());
        }

        // Use a high spike ratio for cross-poll dedup
        let spike_ratio = 2.0_f64;

        let mut signals = Vec::new();
        let mut current_ids = Vec::new();

        for issue in &issues {
            let Some((id, level, event_count)) = Self::extract_issue_fields(issue) else {
                continue;
            };

            current_ids.push(format!("sentry-{id}"));

            let should_emit = match self.seen_issues.get(id) {
                None => true,
                Some(meta) => {
                    meta.event_count > 0
                        && event_count as f64 >= spike_ratio * meta.event_count as f64
                }
            };

            if should_emit && let Some(signal) = self.issue_to_signal(issue) {
                signals.push(signal);
            }

            self.record_seen(id, level, event_count);
        }

        if !signals.is_empty() {
            info!("sentry: {} new signal(s)", signals.len());
        }

        self.fetched_ids = Some(current_ids);

        Ok(signals)
    }

    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        // Sentry uses fetched issue IDs (not emitted signal IDs) for reconcile,
        // because cross-poll dedup means not all fetched issues emit signals.
        let Some(ref ids) = self.fetched_ids else {
            return Ok(0);
        };
        let resolved = store.resolve_missing_signals("sentry", ids)?;
        if resolved > 0 {
            info!("sentry: reconciled {resolved} resolved signal(s)");
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SentryWatcherConfig {
        SentryWatcherConfig {
            enabled: true,
            interval_secs: 60,
            org: "test-org".to_string(),
            project: "test-project".to_string(),
            token: "test-token".to_string(),
        }
    }

    #[test]
    fn test_map_severity() {
        assert_eq!(SentryWatcher::map_severity("fatal"), Severity::Critical);
        assert_eq!(SentryWatcher::map_severity("error"), Severity::Critical);
        assert_eq!(SentryWatcher::map_severity("warning"), Severity::Warning);
        assert_eq!(SentryWatcher::map_severity("info"), Severity::Info);
        assert_eq!(SentryWatcher::map_severity("debug"), Severity::Info);
    }

    #[test]
    fn test_extract_issue_fields() {
        let issue = serde_json::json!({
            "id": "12345",
            "level": "error",
            "count": "42",
        });
        let (id, level, count) = SentryWatcher::extract_issue_fields(&issue).unwrap();
        assert_eq!(id, "12345");
        assert_eq!(level, "error");
        assert_eq!(count, 42);
    }

    #[test]
    fn test_extract_issue_fields_defaults() {
        let issue = serde_json::json!({
            "id": "99",
        });
        let (id, level, count) = SentryWatcher::extract_issue_fields(&issue).unwrap();
        assert_eq!(id, "99");
        assert_eq!(level, "error"); // default
        assert_eq!(count, 0); // default
    }

    #[test]
    fn test_issue_to_signal() {
        let watcher = SentryWatcher::new(test_config());
        let issue = serde_json::json!({
            "id": "42",
            "title": "NullPointerException",
            "culprit": "com.example.Main",
            "level": "error",
            "count": "100",
            "permalink": "https://sentry.io/issue/42",
        });

        let signal = watcher.issue_to_signal(&issue).unwrap();
        assert_eq!(signal.source, "sentry");
        assert_eq!(signal.external_id, "sentry-42");
        assert!(signal.title.contains("NullPointerException"));
        assert!(signal.title.contains("test-org/test-project"));
        assert_eq!(signal.severity, Severity::Critical);
        assert_eq!(signal.url.as_deref(), Some("https://sentry.io/issue/42"));
    }

    #[test]
    fn test_record_seen_dedup() {
        let mut watcher = SentryWatcher::new(test_config());
        watcher.record_seen("42", "error", 10);

        assert!(watcher.seen_issues.contains_key("42"));
        let meta = &watcher.seen_issues["42"];
        assert_eq!(meta.event_count, 10);
        assert_eq!(meta.severity, "error");
    }

    #[test]
    fn test_issue_to_signal_missing_fields() {
        let watcher = SentryWatcher::new(test_config());
        let issue = serde_json::json!({}); // no id or title
        assert!(watcher.issue_to_signal(&issue).is_none());
    }
}

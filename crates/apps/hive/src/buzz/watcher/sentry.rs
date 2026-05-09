//! Sentry watcher — polls the Sentry API for unresolved issues.
//!
//! Uses the Sentry Web API to fetch unresolved issues for a given
//! organization/project, converting each into a Signal.
//!
//! The watcher tracks a `last_seen_id` cursor so that subsequent polls only
//! return issues newer than the most recent one already processed.
//!
//! ## Sweep mode
//!
//! When a `SweepConfig` is present, the watcher also supports a periodic
//! "sweep" that fetches all unresolved issues and re-evaluates them against
//! a `seen_issues` set. Issues are re-surfaced when they are new, spiking
//! in events, stale (unresolved for too long), or changed in severity.

use std::any::Any;
use std::collections::HashMap;

use crate::buzz::config::{SentryConfig, SweepConfig};
use crate::signal::{Severity, Signal};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use color_eyre::Result;
use serde::{Deserialize, Serialize};

use super::Watcher;

/// Per-issue metadata tracked across sweeps for re-triage decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueMeta {
    /// When this issue was last triaged (either by fast poll or sweep).
    pub last_triaged_at: DateTime<Utc>,
    /// Event count at last triage.
    pub event_count: u64,
    /// Sentry severity level at last triage (e.g. "error", "warning").
    pub severity: String,
}

/// Watches Sentry for new unresolved issues via the Sentry REST API.
pub struct SentryWatcher {
    config: SentryConfig,
    /// HTTP client, reused across polls for connection pooling.
    client: reqwest::Client,
    /// Cursor for incremental polling — the ID of the last seen issue.
    /// Issues are sorted by date descending, so we track the newest ID
    /// and skip everything at or before it on subsequent polls.
    last_seen_id: Option<String>,
    /// Per-issue metadata for sweep re-triage (keyed by Sentry issue ID).
    seen_issues: HashMap<String, IssueMeta>,
    /// Sweep configuration (optional — None means sweep is disabled).
    sweep_config: Option<SweepConfig>,
}

impl SentryWatcher {
    pub fn new(config: SentryConfig) -> Self {
        let sweep_config = config.sweep.clone();
        Self {
            config,
            client: reqwest::Client::new(),
            last_seen_id: None,
            seen_issues: HashMap::new(),
            sweep_config,
        }
    }

    /// Map a Sentry issue level string to our severity enum.
    fn map_severity(level: &str) -> Severity {
        match level {
            "fatal" | "error" => Severity::Critical,
            "warning" => Severity::Warning,
            _ => Severity::Info,
        }
    }

    /// Extract the issue ID, level, and event count from a Sentry issue JSON object.
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

    /// Fetch unresolved issues from the Sentry API (cursor-based, for fast poll).
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
                ("per_page", "25"),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            tracing::error!(
                status = %status,
                org = self.config.org.as_str(),
                project = self.config.project.as_str(),
                body = body.as_str(),
                "API returned error"
            );
            return Ok(Vec::new());
        }

        let issues: Vec<serde_json::Value> = response.json().await?;
        Ok(issues)
    }

    /// Fetch all unresolved issues for sweep (no cursor filter, configurable page size).
    async fn fetch_all_issues(&self, max_issues: u32) -> Result<Vec<serde_json::Value>> {
        let url = format!(
            "https://sentry.io/api/0/projects/{org}/{project}/issues/",
            org = self.config.org,
            project = self.config.project,
        );

        let per_page = max_issues.min(100).to_string();

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .query(&[
                ("query", "is:unresolved"),
                ("sort", "date"),
                ("per_page", per_page.as_str()),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            tracing::error!(
                status = %status,
                org = self.config.org.as_str(),
                project = self.config.project.as_str(),
                body = body.as_str(),
                "sweep API returned error"
            );
            return Ok(Vec::new());
        }

        let issues: Vec<serde_json::Value> = response.json().await?;
        Ok(issues)
    }

    /// Convert a Sentry issue JSON object to a Signal.
    fn issue_to_signal(&self, issue: &serde_json::Value, extra_tags: &[&str]) -> Option<Signal> {
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

        let mut tags: Vec<&str> = vec!["sentry", level];
        tags.extend(extra_tags);

        let mut signal = Signal::new(
            "sentry",
            severity,
            format!(
                "[{org}/{project}] {title}",
                org = self.config.org,
                project = self.config.project,
            ),
            body,
        )
        .with_dedup_key(format!("sentry-{id}"))
        .with_tags(tags);

        if let Some(url) = permalink {
            signal = signal.with_url(url);
        }

        Some(signal)
    }

    /// Record an issue in the seen set (used by both poll and sweep).
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

    /// Get the seen issues map (for persistence).
    pub fn seen_issues(&self) -> &HashMap<String, IssueMeta> {
        &self.seen_issues
    }

    /// Restore seen issues from persisted state.
    pub fn restore_seen_issues(&mut self, issues: HashMap<String, IssueMeta>) {
        self.seen_issues = issues;
    }

    /// Clear all seen issues so the next sweep treats everything as new.
    pub fn clear_seen_issues(&mut self) {
        self.seen_issues.clear();
    }

    /// Return the sweep cron schedule, if configured.
    pub fn sweep_schedule(&self) -> Option<&str> {
        self.sweep_config.as_ref().map(|s| s.schedule.as_str())
    }
}

#[async_trait]
impl Watcher for SentryWatcher {
    fn name(&self) -> &str {
        "sentry"
    }

    fn cursor(&self) -> Option<String> {
        self.last_seen_id.clone()
    }

    fn set_cursor(&mut self, cursor: String) {
        self.last_seen_id = Some(cursor);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn has_sweep(&self) -> bool {
        self.sweep_config.is_some()
    }

    async fn poll(&mut self) -> Result<Vec<Signal>> {
        let issues = match self.fetch_issues().await {
            Ok(issues) => issues,
            Err(e) => {
                tracing::error!(
                    org = self.config.org.as_str(),
                    project = self.config.project.as_str(),
                    error = %e,
                    "failed to fetch issues"
                );
                return Ok(Vec::new());
            }
        };

        if issues.is_empty() {
            return Ok(Vec::new());
        }

        let spike_ratio = self
            .sweep_config
            .as_ref()
            .map(|s| s.event_spike_ratio)
            .unwrap_or(f64::MAX);

        let mut signals = Vec::new();
        let mut new_last_seen_id: Option<String> = None;

        for issue in &issues {
            let Some((id, level, event_count)) = Self::extract_issue_fields(issue) else {
                continue;
            };

            // Track the newest ID (first in the list since sorted by date desc).
            if new_last_seen_id.is_none() {
                new_last_seen_id = Some(id.to_string());
            }

            // Use seen_issues for cross-poll dedup instead of a cursor.
            //
            // The old cursor approach (break on id == last_seen_id) breaks when
            // recurring issues accumulate new events and leapfrog each other in
            // Sentry's sort=date ordering — the same issue keeps appearing above
            // the cursor and gets re-emitted every poll.
            //
            // Instead: emit only if (a) we've never seen this issue, or (b) its
            // event count has spiked significantly since last triage.
            let should_emit = match self.seen_issues.get(id) {
                None => true,
                Some(meta) => {
                    meta.event_count > 0
                        && event_count as f64 >= spike_ratio * meta.event_count as f64
                }
            };

            if should_emit && let Some(signal) = self.issue_to_signal(issue, &[]) {
                signals.push(signal);
            }

            // Always record so sweep and subsequent polls have fresh metadata.
            self.record_seen(id, level, event_count);
        }

        // Advance cursor to the newest issue we saw (kept for state tracking).
        if let Some(new_id) = new_last_seen_id {
            self.last_seen_id = Some(new_id);
        }

        Ok(signals)
    }

    async fn sweep(&mut self) -> Result<Vec<Signal>> {
        let sweep_config = match &self.sweep_config {
            Some(c) => c.clone(),
            None => return Ok(Vec::new()),
        };

        let issues = match self.fetch_all_issues(sweep_config.max_issues).await {
            Ok(issues) => issues,
            Err(e) => {
                tracing::error!(
                    org = self.config.org.as_str(),
                    project = self.config.project.as_str(),
                    error = %e,
                    "sweep failed"
                );
                return Ok(Vec::new());
            }
        };

        if issues.is_empty() {
            return Ok(Vec::new());
        }

        let now = Utc::now();
        let stale_threshold = chrono::Duration::days(sweep_config.stale_days as i64);
        let mut signals = Vec::new();

        for issue in &issues {
            let Some((id, level, event_count)) = Self::extract_issue_fields(issue) else {
                continue;
            };

            let trigger = match self.seen_issues.get(id) {
                None => {
                    // New issue — not in seen set.
                    "new"
                }
                Some(meta) => {
                    // Check event spike.
                    if meta.event_count > 0
                        && event_count as f64
                            >= sweep_config.event_spike_ratio * meta.event_count as f64
                    {
                        "spiking"
                    } else if now - meta.last_triaged_at >= stale_threshold {
                        // Stale — unresolved for too long since last triage.
                        "stale"
                    } else if meta.severity != level {
                        // Severity changed.
                        "severity-changed"
                    } else {
                        // Nothing changed — skip.
                        continue;
                    }
                }
            };

            if let Some(signal) = self.issue_to_signal(issue, &["sweep", trigger]) {
                signals.push(signal);
            }

            // Update seen set.
            self.record_seen(id, level, event_count);
        }

        tracing::info!(
            retriaged = signals.len(),
            total = issues.len(),
            "sweep found issue(s) to re-triage"
        );

        Ok(signals)
    }
}

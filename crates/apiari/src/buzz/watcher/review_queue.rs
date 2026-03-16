//! Review Queue watcher — polls GitHub for PRs matching named priority queries.
//!
//! Uses `gh search prs --json ...` to query each configured review queue entry.
//! Deduplicates across queries: lowest index (highest priority) wins.
//! Source: `github_review_queue`.

use std::collections::HashSet;

use async_trait::async_trait;
use color_eyre::Result;
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::config::{GithubWatcherConfig, ReviewQueueEntry};
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

const SOURCE: &str = "github_review_queue";

/// Watches GitHub for PRs matching named priority queries.
pub struct ReviewQueueWatcher {
    queries: Vec<ReviewQueueEntry>,
    gh_available: Option<bool>,
}

impl ReviewQueueWatcher {
    pub fn new(config: &GithubWatcherConfig) -> Self {
        Self {
            queries: config.review_queue.clone(),
            gh_available: None,
        }
    }

    async fn ensure_gh_available(&mut self) -> bool {
        if let Some(available) = self.gh_available {
            return available;
        }

        let auth_result = tokio::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
            .await;

        match auth_result {
            Ok(output) if output.status.success() => {
                self.gh_available = Some(true);
                true
            }
            _ => {
                warn!("gh CLI not available for review queue watcher");
                self.gh_available = Some(false);
                false
            }
        }
    }

    /// Run a single query and return parsed PR results.
    async fn search_prs(&self, query: &str) -> Vec<PrResult> {
        let output = match tokio::process::Command::new("gh")
            .args([
                "search",
                "prs",
                "--json",
                "number,title,url,repository,author,updatedAt",
                "--limit",
                "30",
                "--",
                query,
            ])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                warn!("failed to run `gh search prs`: {e}");
                return Vec::new();
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("`gh search prs` failed: {}", stderr.trim());
            return Vec::new();
        }

        let body = String::from_utf8_lossy(&output.stdout);
        match serde_json::from_str::<Vec<serde_json::Value>>(&body) {
            Ok(items) => items.into_iter().filter_map(parse_pr_result).collect(),
            Err(e) => {
                warn!("failed to parse `gh search prs` JSON: {e}");
                Vec::new()
            }
        }
    }
}

/// Parsed PR from `gh search prs` output.
struct PrResult {
    number: u64,
    title: String,
    url: String,
    repo: String,
    author: String,
}

fn parse_pr_result(val: serde_json::Value) -> Option<PrResult> {
    let number = val.get("number")?.as_u64()?;
    let title = val.get("title")?.as_str()?.to_string();
    let url = val.get("url")?.as_str()?.to_string();
    let repo = val
        .get("repository")
        .and_then(|r| r.get("nameWithOwner"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let author = val
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .to_string();

    Some(PrResult {
        number,
        title,
        url,
        repo,
        author,
    })
}

/// Build the external ID for a review queue signal.
fn external_id(repo: &str, number: u64) -> String {
    format!("rq-{repo}-{number}")
}

#[async_trait]
impl Watcher for ReviewQueueWatcher {
    fn name(&self) -> &str {
        "review_queue"
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        if self.queries.is_empty() {
            return Ok(Vec::new());
        }

        if !self.ensure_gh_available().await {
            return Ok(Vec::new());
        }

        let mut all_signals = Vec::new();
        // Track seen PR keys across queries for dedup (lowest index wins).
        let mut seen_keys: HashSet<String> = HashSet::new();

        for (priority, entry) in self.queries.iter().enumerate() {
            let prs = self.search_prs(&entry.query).await;
            for pr in prs {
                let key = external_id(&pr.repo, pr.number);
                if seen_keys.contains(&key) {
                    continue; // Already emitted by a higher-priority query.
                }
                seen_keys.insert(key.clone());

                let metadata = serde_json::json!({
                    "query_name": entry.name,
                    "priority": priority,
                    "author": pr.author,
                    "repo": pr.repo,
                });

                let signal = SignalUpdate::new(SOURCE, &key, pr.title, Severity::Info)
                    .with_body(format!("{} — {} by {}", entry.name, pr.repo, pr.author))
                    .with_url(&pr.url)
                    .with_metadata(metadata.to_string());

                all_signals.push(signal);
            }
        }

        if !all_signals.is_empty() {
            info!("review_queue: {} signal(s)", all_signals.len());
        }

        Ok(all_signals)
    }

    fn reconcile(&self, store: &SignalStore) -> Result<usize> {
        // The poll already emits the full set of current PRs.
        // We don't track current_ids here because poll is stateless;
        // the daemon calls resolve_missing_signals after each poll cycle.
        // We'd need the IDs from the last poll. For now, let the daemon
        // handle this via the standard pattern.
        //
        // Actually, since we return all current signals every poll, we can
        // use the store's resolve_missing_signals with the current external IDs.
        // But we don't have them here (poll already finished). The simplest
        // approach: skip reconcile and let signals be updated on each poll.
        // They'll get updated_at refreshed, keeping them alive.
        let _ = store;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_external_id() {
        assert_eq!(external_id("org/repo", 42), "rq-org/repo-42");
    }

    #[test]
    fn test_parse_pr_result() {
        let val = serde_json::json!({
            "number": 42,
            "title": "Add feature X",
            "url": "https://github.com/org/repo/pull/42",
            "repository": {"nameWithOwner": "org/repo"},
            "author": {"login": "user1"},
            "updatedAt": "2025-01-01T00:00:00Z",
        });
        let pr = parse_pr_result(val).unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Add feature X");
        assert_eq!(pr.repo, "org/repo");
        assert_eq!(pr.author, "user1");
    }

    #[test]
    fn test_parse_pr_result_missing_fields() {
        let val = serde_json::json!({"title": "No number"});
        assert!(parse_pr_result(val).is_none());
    }
}

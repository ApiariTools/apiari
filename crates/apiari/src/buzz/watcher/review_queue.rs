//! Review Queue watcher — polls GitHub for PRs matching named priority queries.
//!
//! Uses the GitHub REST Search API (`/search/issues`) to query each configured
//! review queue entry. Deduplicates across queries: lowest index (highest
//! priority) wins. Source: `github_review_queue`.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use color_eyre::Result;
use tracing::{info, warn};

/// HTTP request timeout for GitHub API calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

use super::Watcher;
use crate::buzz::config::{GithubWatcherConfig, ReviewQueueEntry};
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

const SOURCE: &str = "github_review_queue";

/// Try `GITHUB_TOKEN` env → `GH_TOKEN` env → `gh auth token` CLI fallback.
fn resolve_github_token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN")
        && !t.is_empty()
    {
        return Some(t);
    }
    if let Ok(t) = std::env::var("GH_TOKEN")
        && !t.is_empty()
    {
        return Some(t);
    }
    // Fallback: ask gh CLI
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

/// Watches GitHub for PRs matching named priority queries.
pub struct ReviewQueueWatcher {
    queries: Vec<ReviewQueueEntry>,
    client: reqwest::Client,
    /// Lazy-init: `None` = untried, `Some(None)` = failed, `Some(Some(t))` = resolved.
    token: Option<Option<String>>,
}

impl ReviewQueueWatcher {
    pub fn new(config: &GithubWatcherConfig) -> Self {
        Self {
            queries: config.review_queue.clone(),
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|e| {
                    warn!(
                        "failed to build reqwest client with timeout: {e}, falling back to default"
                    );
                    reqwest::Client::new()
                }),
            token: None,
        }
    }

    /// Resolve and cache the GitHub token. Returns `Some(token)` on success.
    fn ensure_token(&mut self) -> Option<&str> {
        if self.token.is_none() {
            let resolved = resolve_github_token();
            if resolved.is_none() {
                warn!("no GitHub token available for review queue watcher");
            }
            self.token = Some(resolved);
        }
        self.token.as_ref().unwrap().as_deref()
    }

    /// Run a single query via the GitHub REST Search API and return parsed PR results.
    async fn search_prs(&self, query: &str, token: &str) -> Vec<PrResult> {
        let resp = match self
            .client
            .get("https://api.github.com/search/issues")
            .query(&[("q", query), ("per_page", "30")])
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "apiari-buzz")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("GitHub search request failed: {e}");
                return Vec::new();
            }
        };

        let status = resp.status();
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            warn!("GitHub search rate limited (HTTP {status})");
            return Vec::new();
        }
        if !status.is_success() {
            warn!("GitHub search returned HTTP {status}");
            return Vec::new();
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to parse GitHub search response: {e}");
                return Vec::new();
            }
        };

        let Some(items) = body.get("items").and_then(|v| v.as_array()) else {
            warn!("GitHub search response missing `items` array");
            return Vec::new();
        };

        items.iter().filter_map(parse_pr_result).collect()
    }
}

/// Parsed PR from GitHub search result.
struct PrResult {
    number: u64,
    title: String,
    url: String,
    repo: String,
    author: String,
}

fn parse_pr_result(val: &serde_json::Value) -> Option<PrResult> {
    let number = val.get("number")?.as_u64()?;
    let title = val.get("title")?.as_str()?.to_string();
    let url = val.get("html_url")?.as_str()?.to_string();
    // repository_url is like "https://api.github.com/repos/org/repo"
    let repo = val
        .get("repository_url")
        .and_then(|r| r.as_str())
        .and_then(|u| u.strip_prefix("https://api.github.com/repos/"))
        .unwrap_or("")
        .to_string();
    let author = val
        .get("user")
        .and_then(|u| u.get("login"))
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

    fn signal_source(&self) -> &str {
        SOURCE
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        if self.queries.is_empty() {
            return Ok(Vec::new());
        }

        let token = match self.ensure_token() {
            Some(t) => t.to_string(),
            None => return Ok(Vec::new()),
        };

        let mut all_signals = Vec::new();
        // Track seen PR keys across queries for dedup (lowest index wins).
        let mut seen_keys: HashSet<String> = HashSet::new();

        for (priority, entry) in self.queries.iter().enumerate() {
            let prs = self.search_prs(&entry.query, &token).await;
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
                    "pr_number": pr.number,
                });

                // First query (priority 0) = Warning, others = Info
                let severity = if priority == 0 {
                    Severity::Warning
                } else {
                    Severity::Info
                };

                let signal = SignalUpdate::new(SOURCE, &key, pr.title, severity)
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
            "html_url": "https://github.com/org/repo/pull/42",
            "repository_url": "https://api.github.com/repos/org/repo",
            "user": {"login": "user1"},
            "updated_at": "2025-01-01T00:00:00Z",
        });
        let pr = parse_pr_result(&val).unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Add feature X");
        assert_eq!(pr.url, "https://github.com/org/repo/pull/42");
        assert_eq!(pr.repo, "org/repo");
        assert_eq!(pr.author, "user1");
    }

    #[test]
    fn test_parse_pr_result_missing_fields() {
        let val = serde_json::json!({"title": "No number"});
        assert!(parse_pr_result(&val).is_none());
    }
}

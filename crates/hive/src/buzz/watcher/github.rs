//! GitHub watcher — polls GitHub for events using the `gh` CLI.
//!
//! Shells out to `gh api` to query:
//!   - Open issues assigned to the authenticated user
//!   - PR reviews that need attention (review requested for the authenticated user)
//!   - Issues with specific labels (configurable via `watch_labels`)
//!   - Failed CI checks on the default branch
//!
//! Handles errors gracefully: if `gh` is not installed, not authenticated, or
//! the API returns errors, individual failures are logged and skipped without
//! crashing the entire poll cycle.

use std::any::Any;
use std::collections::HashSet;

use crate::signal::{Severity, Signal};
use async_trait::async_trait;
use color_eyre::Result;

use super::Watcher;
use crate::buzz::config::GithubConfig;

/// Watches GitHub repositories for new events (issues, PRs, CI failures)
/// by shelling out to the `gh api` CLI.
pub struct GithubWatcher {
    config: GithubConfig,
    /// Whether we have verified that `gh` is installed and authenticated.
    gh_available: Option<bool>,
    /// Cached GitHub username (resolved from `gh api user`).
    username: Option<String>,
    /// Dedup keys of signals emitted in previous polls, keyed by the set of
    /// currently-active dedup keys. Updated each poll to exactly the current
    /// active set so resolved conditions (CI passes, issue closed) are pruned.
    seen: HashSet<String>,
}

impl GithubWatcher {
    pub fn new(config: GithubConfig) -> Self {
        Self {
            config,
            gh_available: None,
            username: None,
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

    /// Check that the `gh` CLI is installed and authenticated.
    /// Also resolves and caches the GitHub username.
    /// Caches the result so we only check once per session.
    async fn ensure_gh_available(&mut self) -> bool {
        if let Some(available) = self.gh_available {
            return available;
        }

        // Check if `gh` is on PATH.
        let which_result = tokio::process::Command::new("which")
            .arg("gh")
            .output()
            .await;

        match which_result {
            Ok(output) if output.status.success() => {}
            _ => {
                tracing::warn!("`gh` CLI is not installed or not on PATH");
                self.gh_available = Some(false);
                return false;
            }
        }

        // Check if `gh` is authenticated.
        let auth_result = tokio::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
            .await;

        match auth_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(error = stderr.trim(), "`gh` is not authenticated");
                self.gh_available = Some(false);
                return false;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to check `gh auth status`");
                self.gh_available = Some(false);
                return false;
            }
        }

        // Resolve GitHub username for API queries.
        let user_result = tokio::process::Command::new("gh")
            .args(["api", "user", "--jq", ".login"])
            .output()
            .await;

        match user_result {
            Ok(output) if output.status.success() => {
                self.username = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
            }
            _ => {
                tracing::warn!("could not resolve username, some queries may be skipped");
            }
        }

        self.gh_available = Some(true);
        true
    }

    /// Run a `gh api` command and return the parsed JSON, or None on failure.
    async fn gh_api(&self, endpoint: &str) -> Option<serde_json::Value> {
        let output = match tokio::process::Command::new("gh")
            .args(["api", endpoint])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                tracing::error!(endpoint = endpoint, error = %e, "failed to run `gh api`");
                return None;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(
                endpoint = endpoint,
                error = stderr.trim(),
                "`gh api` failed"
            );
            return None;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(value) => Some(value),
            Err(e) => {
                tracing::error!(endpoint = endpoint, error = %e, "failed to parse JSON from `gh api`");
                None
            }
        }
    }

    /// Fetch notifications/events for a single repo and convert them to signals.
    async fn poll_repo(&self, repo: &str) -> Vec<Signal> {
        let mut signals = Vec::new();

        // 1. Poll open issues assigned to the authenticated user.
        if let Some(ref username) = self.username
            && let Some(issues_value) = self
                .gh_api(&format!(
                    "repos/{repo}/issues?state=open&assignee={username}&per_page=10"
                ))
                .await
            && let Some(issues) = issues_value.as_array()
        {
            for issue in issues {
                if let Some(signal) = self.issue_to_signal(repo, issue) {
                    signals.push(signal);
                }
            }
        }

        // 2. Poll PRs where a review is requested from the authenticated user.
        //    The search API lets us find PRs with "review-requested:@me".
        if let Some(search_value) = self
            .gh_api(&format!(
                "search/issues?q=repo:{repo}+type:pr+state:open+review-requested:@me&per_page=10"
            ))
            .await
            && let Some(items) = search_value.get("items").and_then(|v| v.as_array())
        {
            for item in items {
                if let Some(signal) = self.review_request_to_signal(repo, item) {
                    signals.push(signal);
                }
            }
        }

        // 3. Poll issues with watched labels (if configured).
        for label in &self.config.watch_labels {
            if let Some(issues_value) = self
                .gh_api(&format!(
                    "repos/{repo}/issues?state=open&labels={label}&per_page=10"
                ))
                .await
                && let Some(issues) = issues_value.as_array()
            {
                for issue in issues {
                    if let Some(signal) = self.labeled_issue_to_signal(repo, issue, label) {
                        signals.push(signal);
                    }
                }
            }
        }

        // 4. Poll failed CI checks on the default branch.
        if let Some(response) = self
            .gh_api(&format!(
                "repos/{repo}/commits/HEAD/check-runs?status=completed&per_page=10"
            ))
            .await
            && let Some(check_runs) = response.get("check_runs").and_then(|v| v.as_array())
        {
            for run in check_runs {
                if let Some(signal) = self.check_run_to_signal(repo, run) {
                    signals.push(signal);
                }
            }
        }

        signals
    }

    /// Convert a GitHub issue JSON object to a Signal.
    fn issue_to_signal(&self, repo: &str, issue: &serde_json::Value) -> Option<Signal> {
        let number = issue.get("number")?.as_u64()?;
        let title = issue.get("title")?.as_str()?;
        let html_url = issue.get("html_url")?.as_str()?;
        let body = issue.get("body").and_then(|v| v.as_str()).unwrap_or("");

        let is_pr = issue.get("pull_request").is_some();
        let kind = if is_pr { "pr" } else { "issue" };

        let severity = if has_label(issue, "critical") || has_label(issue, "P0") {
            Severity::Critical
        } else if has_label(issue, "bug") || has_label(issue, "P1") {
            Severity::Warning
        } else {
            Severity::Info
        };

        let signal = Signal::new(
            "github",
            severity,
            format!("[{repo}] {kind} #{number}: {title}"),
            body,
        )
        .with_url(html_url)
        .with_dedup_key(format!("gh-{kind}-{repo}-{number}"))
        .with_tags([kind, repo]);

        Some(signal)
    }

    /// Convert a PR review request (from search API) to a Signal.
    fn review_request_to_signal(&self, repo: &str, item: &serde_json::Value) -> Option<Signal> {
        let number = item.get("number")?.as_u64()?;
        let title = item.get("title")?.as_str()?;
        let html_url = item.get("html_url")?.as_str()?;

        let signal = Signal::new(
            "github",
            Severity::Warning,
            format!("[{repo}] review requested: PR #{number}: {title}"),
            format!("Your review is requested on PR #{number} in {repo}"),
        )
        .with_url(html_url)
        .with_dedup_key(format!("gh-review-{repo}-{number}"))
        .with_tags(["review", "pr", repo]);

        Some(signal)
    }

    /// Convert a labeled issue to a Signal, tagging it with the watched label.
    fn labeled_issue_to_signal(
        &self,
        repo: &str,
        issue: &serde_json::Value,
        label: &str,
    ) -> Option<Signal> {
        let number = issue.get("number")?.as_u64()?;
        let title = issue.get("title")?.as_str()?;
        let html_url = issue.get("html_url")?.as_str()?;

        let is_pr = issue.get("pull_request").is_some();
        let kind = if is_pr { "pr" } else { "issue" };

        let severity = if label == "critical" || label == "P0" {
            Severity::Critical
        } else if label == "bug" || label == "P1" {
            Severity::Warning
        } else {
            Severity::Info
        };

        let signal = Signal::new(
            "github",
            severity,
            format!("[{repo}] [{label}] {kind} #{number}: {title}"),
            format!("{kind} #{number} has label '{label}' in {repo}"),
        )
        .with_url(html_url)
        .with_dedup_key(format!("gh-label-{label}-{repo}-{number}"))
        .with_tags([kind, label, repo]);

        Some(signal)
    }

    /// Convert a GitHub check run JSON object to a Signal if it failed.
    fn check_run_to_signal(&self, repo: &str, run: &serde_json::Value) -> Option<Signal> {
        let conclusion = run.get("conclusion")?.as_str()?;
        if conclusion != "failure" {
            return None;
        }

        let name = run.get("name")?.as_str()?;
        let html_url = run.get("html_url")?.as_str().unwrap_or("");
        let id = run.get("id")?.as_u64()?;

        let signal = Signal::new(
            "github",
            Severity::Warning,
            format!("[{repo}] CI failed: {name}"),
            format!("Check run '{name}' failed on {repo}"),
        )
        .with_url(html_url)
        .with_dedup_key(format!("gh-ci-{repo}-{id}"))
        .with_tags(["ci", repo]);

        Some(signal)
    }
}

/// Check if a GitHub issue/PR has a specific label.
fn has_label(issue: &serde_json::Value, label_name: &str) -> bool {
    issue
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|labels| {
            labels
                .iter()
                .any(|l| l.get("name").and_then(|n| n.as_str()) == Some(label_name))
        })
        .unwrap_or(false)
}

#[async_trait]
impl Watcher for GithubWatcher {
    fn name(&self) -> &str {
        "github"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn poll(&mut self) -> Result<Vec<Signal>> {
        if !self.ensure_gh_available().await {
            return Ok(Vec::new());
        }

        let mut all_signals = Vec::new();
        let mut current_keys: HashSet<String> = HashSet::new();

        for repo in &self.config.repos.clone() {
            let signals = self.poll_repo(repo).await;
            for signal in signals {
                if let Some(ref key) = signal.dedup_key {
                    current_keys.insert(key.clone());
                    // Only emit if we haven't already notified about this signal.
                    // Prevents re-firing every poll for long-lived conditions like
                    // a persistently broken CI check or an open assigned issue.
                    if !self.seen.contains(key) {
                        all_signals.push(signal);
                    }
                } else {
                    // No dedup key — always emit.
                    all_signals.push(signal);
                }
            }
        }

        // Prune seen to only currently-active signals. This means: if a CI run
        // passes (disappears from the failed list), its key leaves the seen set,
        // so a future failure on the same check will fire again.
        self.seen = current_keys;

        Ok(all_signals)
    }
}

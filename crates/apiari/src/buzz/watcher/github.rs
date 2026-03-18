//! GitHub watcher — polls GitHub for events using the `gh` CLI.
//!
//! Queries open issues, PR review requests, watched labels, and CI status.
//! Stateless — emits all signals every poll; the DB handles dedup via UNIQUE constraints.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use color_eyre::Result;
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::config::GithubWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

/// Watches GitHub repositories via the `gh` CLI.
pub struct GithubWatcher {
    config: GithubWatcherConfig,
    gh_available: Option<bool>,
    username: Option<String>,
    /// Last-seen release run ID per repo (cursor: github_release:{repo}).
    release_cursors: HashMap<String, u64>,
    /// Seen merged PR numbers per repo (cursor: github_merged_pr:{repo}).
    merged_pr_cursors: HashMap<String, HashSet<u64>>,
    /// PRs with passing CI per repo (cursor: github_ci_pass:{repo}).
    ci_pass_state: HashMap<String, HashSet<u64>>,
}

impl GithubWatcher {
    pub fn new(config: GithubWatcherConfig) -> Self {
        Self {
            config,
            gh_available: None,
            username: None,
            release_cursors: HashMap::new(),
            merged_pr_cursors: HashMap::new(),
            ci_pass_state: HashMap::new(),
        }
    }

    /// Pre-load cursor state from the signal store (called during daemon setup).
    pub fn load_cursors(&mut self, store: &SignalStore) {
        for repo in &self.config.repos {
            let rk = format!("github_release:{repo}");
            if let Ok(Some(val)) = store.get_cursor(&rk)
                && let Ok(id) = val.parse::<u64>()
            {
                self.release_cursors.insert(repo.clone(), id);
            }

            let mk = format!("github_merged_pr:{repo}");
            if let Ok(Some(val)) = store.get_cursor(&mk) {
                let seen: HashSet<u64> = val.split(',').filter_map(|n| n.parse().ok()).collect();
                if !seen.is_empty() {
                    self.merged_pr_cursors.insert(repo.clone(), seen);
                }
            }

            let ck = format!("github_ci_pass:{repo}");
            if let Ok(Some(val)) = store.get_cursor(&ck) {
                let state: HashSet<u64> = val.split(',').filter_map(|n| n.parse().ok()).collect();
                if !state.is_empty() {
                    self.ci_pass_state.insert(repo.clone(), state);
                }
            }
        }
    }

    async fn ensure_gh_available(&mut self) -> bool {
        if let Some(available) = self.gh_available {
            return available;
        }

        let which_result = tokio::process::Command::new("which")
            .arg("gh")
            .output()
            .await;

        match which_result {
            Ok(output) if output.status.success() => {}
            _ => {
                warn!("gh CLI is not installed or not on PATH");
                self.gh_available = Some(false);
                return false;
            }
        }

        let auth_result = tokio::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
            .await;

        match auth_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("gh is not authenticated: {}", stderr.trim());
                self.gh_available = Some(false);
                return false;
            }
            Err(e) => {
                warn!("failed to check gh auth status: {e}");
                self.gh_available = Some(false);
                return false;
            }
        }

        let user_result = tokio::process::Command::new("gh")
            .args(["api", "user", "--jq", ".login"])
            .output()
            .await;

        if let Ok(output) = user_result
            && output.status.success()
        {
            self.username = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }

        self.gh_available = Some(true);
        true
    }

    async fn gh_api(&self, endpoint: &str) -> Option<serde_json::Value> {
        let output = match tokio::process::Command::new("gh")
            .args(["api", endpoint])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                warn!("failed to run `gh api {endpoint}`: {e}");
                return None;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("`gh api {endpoint}` failed: {}", stderr.trim());
            return None;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(value) => Some(value),
            Err(e) => {
                warn!("failed to parse JSON from `gh api {endpoint}`: {e}");
                None
            }
        }
    }

    /// Fetch open pull requests for a repo. Returns (pr_number, pr_title, head_branch).
    async fn fetch_open_prs(&self, repo: &str) -> Vec<(u64, String, String)> {
        let mut result = Vec::new();
        if let Some(prs) = self
            .gh_api(&format!("repos/{repo}/pulls?state=open&per_page=20"))
            .await
            && let Some(prs) = prs.as_array()
        {
            for pr in prs {
                if let Some(number) = pr.get("number").and_then(|v| v.as_u64())
                    && let Some(title) = pr.get("title").and_then(|v| v.as_str())
                    && let Some(branch) = pr
                        .get("head")
                        .and_then(|v| v.get("ref"))
                        .and_then(|v| v.as_str())
                {
                    result.push((number, title.to_string(), branch.to_string()));
                }
            }
        }
        result
    }

    /// Fetch the latest workflow run for a branch using `gh run list`.
    async fn fetch_latest_run(&self, repo: &str, branch: &str) -> Option<serde_json::Value> {
        let output = tokio::process::Command::new("gh")
            .args([
                "run",
                "list",
                "--json",
                "status,conclusion,headBranch,databaseId,url",
                "--repo",
                repo,
                "--branch",
                branch,
                "--limit",
                "1",
            ])
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "`gh run list` for {repo}@{branch} failed: {}",
                stderr.trim()
            );
            return None;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        let runs: Vec<serde_json::Value> = serde_json::from_str(&body).ok()?;
        runs.into_iter().next()
    }

    /// Poll for completed Release workflow runs on a repo.
    /// Returns (signals, optional new max run ID for cursor update).
    async fn poll_release_runs(
        &self,
        repo: &str,
        last_seen_id: u64,
    ) -> (Vec<SignalUpdate>, Option<u64>) {
        let mut signals = Vec::new();

        let endpoint = format!("repos/{repo}/actions/runs?per_page=10&status=completed");
        let Some(response) = self.gh_api(&endpoint).await else {
            return (signals, None);
        };
        let Some(runs) = response.get("workflow_runs").and_then(|v| v.as_array()) else {
            return (signals, None);
        };

        let mut max_id = last_seen_id;

        for run in runs {
            let name = run.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let event = run.get("event").and_then(|v| v.as_str()).unwrap_or("");

            if name != "Release" || event != "push" {
                continue;
            }

            let run_id = run.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            if run_id <= last_seen_id {
                continue;
            }

            let conclusion = run.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
            let head_branch = run
                .get("head_branch")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let html_url = run.get("html_url").and_then(|v| v.as_str()).unwrap_or("");

            let (severity, title) = match conclusion {
                "success" => (
                    Severity::Info,
                    format!("\u{1f680} {head_branch} release succeeded"),
                ),
                "failure" => (
                    Severity::Critical,
                    format!("\u{1f4a5} {head_branch} release failed"),
                ),
                _ => continue,
            };

            let key = format!("release-{repo}-{run_id}");
            let mut signal = SignalUpdate::new("github_release", &key, &title, severity);
            if !html_url.is_empty() {
                signal = signal.with_url(html_url);
            }
            signals.push(signal);

            if run_id > max_id {
                max_id = run_id;
            }
        }

        let new_cursor = if max_id > last_seen_id {
            Some(max_id)
        } else {
            None
        };

        (signals, new_cursor)
    }

    /// Poll for recently merged PRs on a repo.
    /// Returns (signals, optional updated seen set for cursor update).
    async fn poll_merged_prs(
        &self,
        repo: &str,
        seen_prs: &HashSet<u64>,
    ) -> (Vec<SignalUpdate>, Option<HashSet<u64>>) {
        let mut signals = Vec::new();

        let output = tokio::process::Command::new("gh")
            .args([
                "pr",
                "list",
                "--repo",
                repo,
                "--state",
                "merged",
                "--limit",
                "10",
                "--json",
                "number,title,mergedAt,url",
            ])
            .output()
            .await;

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return (signals, None),
        };

        let body = String::from_utf8_lossy(&output.stdout);
        let prs: Vec<serde_json::Value> = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => return (signals, None),
        };

        let mut new_seen = seen_prs.clone();

        for pr in &prs {
            let number = pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
            if number == 0 || seen_prs.contains(&number) {
                continue;
            }

            // Always mark as seen to prevent future re-emission
            new_seen.insert(number);

            // On first run (empty cursor), only emit for recently merged PRs
            // to avoid flooding with old already-merged PRs.
            if seen_prs.is_empty() {
                let is_recent = pr
                    .get("mergedAt")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .is_some_and(|merged_at| {
                        let cutoff = chrono::Utc::now()
                            - chrono::Duration::seconds((self.config.interval_secs * 2) as i64);
                        merged_at >= cutoff
                    });
                if !is_recent {
                    continue;
                }
            }

            let title = pr.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = pr.get("url").and_then(|v| v.as_str()).unwrap_or("");

            let key = format!("merged-{repo}-{number}");
            let msg = format!("\u{2705} Merged: {title} #{number}");
            let mut signal = SignalUpdate::new("github_merged_pr", &key, &msg, Severity::Info);
            if !url.is_empty() {
                signal = signal.with_url(url);
            }
            signals.push(signal);
        }

        if new_seen != *seen_prs {
            (signals, Some(new_seen))
        } else {
            (signals, None)
        }
    }

    async fn poll_repo(&self, repo: &str) -> Vec<(String, SignalUpdate)> {
        let mut signals = Vec::new();

        // Open issues assigned to user
        if let Some(ref username) = self.username
            && let Some(issues_value) = self
                .gh_api(&format!(
                    "repos/{repo}/issues?state=open&assignee={username}&per_page=10"
                ))
                .await
            && let Some(issues) = issues_value.as_array()
        {
            for issue in issues {
                if let Some((key, signal)) = issue_to_signal(repo, issue) {
                    signals.push((key, signal));
                }
            }
        }

        // Watched labels
        for label in &self.config.watch_labels {
            if let Some(issues_value) = self
                .gh_api(&format!(
                    "repos/{repo}/issues?state=open&labels={label}&per_page=10"
                ))
                .await
                && let Some(issues) = issues_value.as_array()
            {
                for issue in issues {
                    if let Some((key, signal)) = labeled_issue_to_signal(repo, issue, label) {
                        signals.push((key, signal));
                    }
                }
            }
        }

        // Failed CI checks
        if let Some(response) = self
            .gh_api(&format!(
                "repos/{repo}/commits/HEAD/check-runs?status=completed&per_page=10"
            ))
            .await
            && let Some(check_runs) = response.get("check_runs").and_then(|v| v.as_array())
        {
            for run in check_runs {
                if let Some((key, signal)) = check_run_to_signal(repo, run) {
                    signals.push((key, signal));
                }
            }
        }

        signals
    }
}

#[async_trait]
impl Watcher for GithubWatcher {
    fn name(&self) -> &str {
        "github"
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        if !self.ensure_gh_available().await {
            return Ok(Vec::new());
        }

        let mut all_signals = Vec::new();
        let repos = self.config.repos.clone();

        for repo in &repos {
            let signals = self.poll_repo(repo).await;
            for (_key, signal) in signals {
                all_signals.push(signal);
            }
        }

        // Release workflow completion signals
        for repo in &repos {
            let last_seen_id = self.release_cursors.get(repo).copied().unwrap_or(0);
            let (signals, new_cursor) = self.poll_release_runs(repo, last_seen_id).await;
            all_signals.extend(signals);
            if let Some(max_id) = new_cursor {
                self.release_cursors.insert(repo.clone(), max_id);
            }
        }

        // Merged PR signals
        for repo in &repos {
            let empty = HashSet::new();
            let seen_prs = self.merged_pr_cursors.get(repo).unwrap_or(&empty).clone();
            let (signals, updated_seen) = self.poll_merged_prs(repo, &seen_prs).await;
            all_signals.extend(signals);
            if let Some(mut new_seen) = updated_seen {
                // Keep only the last 100 to prevent unbounded growth
                if new_seen.len() > 100 {
                    let mut sorted: Vec<u64> = new_seen.into_iter().collect();
                    sorted.sort_unstable();
                    new_seen = sorted[sorted.len() - 100..].iter().copied().collect();
                }
                self.merged_pr_cursors.insert(repo.clone(), new_seen);
            }
        }

        // PR CI failure and CI pass signals
        for repo in &repos {
            let mut ci_pass_prs = self.ci_pass_state.get(repo).cloned().unwrap_or_default();

            let prs = self.fetch_open_prs(repo).await;
            let open_pr_numbers: HashSet<u64> = prs.iter().map(|(n, _, _)| *n).collect();

            for (pr_number, pr_title, head_branch) in prs {
                if let Some(run) = self.fetch_latest_run(repo, &head_branch).await {
                    let conclusion = run.get("conclusion").and_then(|v| v.as_str());
                    let run_id = run.get("databaseId").and_then(|v| v.as_u64());
                    let run_url = run.get("url").and_then(|v| v.as_str());

                    match conclusion {
                        Some("failure") => {
                            // CI regressed — remove from pass tracking
                            ci_pass_prs.remove(&pr_number);

                            if let Some(run_id) = run_id {
                                let key = format!("ci-failure-{pr_number}-{run_id}");
                                let mut signal = SignalUpdate::new(
                                    "github",
                                    &key,
                                    format!("CI failed: {pr_title} (#{pr_number})"),
                                    Severity::Error,
                                );
                                if let Some(url) = run_url {
                                    signal = signal.with_body(url).with_url(url);
                                }
                                all_signals.push(signal);
                            }
                        }
                        Some("success") => {
                            // Only emit when CI transitions to passing.
                            // Include run_id in external_id so a fresh DB row is
                            // inserted if CI regresses then passes again on a new run.
                            if !ci_pass_prs.contains(&pr_number) {
                                ci_pass_prs.insert(pr_number);
                                let rid = run_id.unwrap_or(0);
                                let key = format!("ci-pass-{repo}-{pr_number}-{rid}");
                                let mut signal = SignalUpdate::new(
                                    "github_ci_pass",
                                    &key,
                                    format!("\u{2705} CI passed on PR #{pr_number}: {pr_title}"),
                                    Severity::Info,
                                );
                                if let Some(url) = run_url {
                                    signal = signal.with_url(url);
                                }
                                all_signals.push(signal);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Clean up closed PRs from tracking
            ci_pass_prs.retain(|n| open_pr_numbers.contains(n));
            self.ci_pass_state.insert(repo.clone(), ci_pass_prs);
        }

        if !all_signals.is_empty() {
            info!("github: {} signal(s)", all_signals.len());
        }

        Ok(all_signals)
    }

    /// Persist cursor state to the signal store (called synchronously after poll).
    /// Returns 0 so the framework still runs auto-reconcile for source "github"
    /// (stateless issue/label/check signals need stale resolution).
    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        for (repo, last_id) in &self.release_cursors {
            let key = format!("github_release:{repo}");
            if let Err(e) = store.set_cursor(&key, &last_id.to_string()) {
                warn!("failed to persist release cursor for {repo}: {e}");
            }
        }
        for (repo, seen) in &self.merged_pr_cursors {
            let key = format!("github_merged_pr:{repo}");
            let val: String = seen
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if let Err(e) = store.set_cursor(&key, &val) {
                warn!("failed to persist merged PR cursor for {repo}: {e}");
            }
        }
        for (repo, state) in &self.ci_pass_state {
            let key = format!("github_ci_pass:{repo}");
            let val: String = state
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if let Err(e) = store.set_cursor(&key, &val) {
                warn!("failed to persist CI pass cursor for {repo}: {e}");
            }
        }
        // Return 0: cursor persistence is done, but let the framework
        // auto-reconcile source "github" signals (issues/labels/checks).
        Ok(0)
    }
}

fn has_label(issue: &serde_json::Value, label_name: &str) -> bool {
    issue
        .get("labels")
        .and_then(|v| v.as_array())
        .is_some_and(|labels| {
            labels
                .iter()
                .any(|l| l.get("name").and_then(|n| n.as_str()) == Some(label_name))
        })
}

fn issue_to_signal(repo: &str, issue: &serde_json::Value) -> Option<(String, SignalUpdate)> {
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

    let key = format!("gh-{kind}-{repo}-{number}");
    let signal = SignalUpdate::new(
        "github",
        &key,
        format!("[{repo}] {kind} #{number}: {title}"),
        severity,
    )
    .with_body(body)
    .with_url(html_url);

    Some((key, signal))
}

fn labeled_issue_to_signal(
    repo: &str,
    issue: &serde_json::Value,
    label: &str,
) -> Option<(String, SignalUpdate)> {
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

    let key = format!("gh-label-{label}-{repo}-{number}");
    let signal = SignalUpdate::new(
        "github",
        &key,
        format!("[{repo}] [{label}] {kind} #{number}: {title}"),
        severity,
    )
    .with_body(format!("{kind} #{number} has label '{label}' in {repo}"))
    .with_url(html_url);

    Some((key, signal))
}

fn check_run_to_signal(repo: &str, run: &serde_json::Value) -> Option<(String, SignalUpdate)> {
    let conclusion = run.get("conclusion")?.as_str()?;
    if conclusion != "failure" {
        return None;
    }

    let name = run.get("name")?.as_str()?;
    let html_url = run.get("html_url").and_then(|v| v.as_str()).unwrap_or("");
    let id = run.get("id")?.as_u64()?;

    let key = format!("gh-ci-{repo}-{id}");
    let mut signal = SignalUpdate::new(
        "github",
        &key,
        format!("[{repo}] CI failed: {name}"),
        Severity::Warning,
    )
    .with_body(format!("Check run '{name}' failed on {repo}"));

    if !html_url.is_empty() {
        signal = signal.with_url(html_url);
    }

    Some((key, signal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_to_signal() {
        let issue = serde_json::json!({
            "number": 42,
            "title": "Something broke",
            "html_url": "https://github.com/org/repo/issues/42",
            "body": "It's broken",
            "labels": [{"name": "bug"}],
        });
        let (key, signal) = issue_to_signal("org/repo", &issue).unwrap();
        assert_eq!(key, "gh-issue-org/repo-42");
        assert_eq!(signal.severity, Severity::Warning); // "bug" label
        assert!(signal.title.contains("#42"));
    }

    #[test]
    fn test_issue_to_signal_pr() {
        let issue = serde_json::json!({
            "number": 10,
            "title": "Add feature",
            "html_url": "https://github.com/org/repo/pull/10",
            "pull_request": {},
            "labels": [],
        });
        let (key, _) = issue_to_signal("org/repo", &issue).unwrap();
        assert!(key.starts_with("gh-pr-"));
    }

    #[test]
    fn test_check_run_failure() {
        let run = serde_json::json!({
            "id": 999,
            "name": "CI Build",
            "conclusion": "failure",
            "html_url": "https://github.com/org/repo/actions/runs/999",
        });
        let (key, signal) = check_run_to_signal("org/repo", &run).unwrap();
        assert_eq!(key, "gh-ci-org/repo-999");
        assert!(signal.title.contains("CI failed"));
    }

    #[test]
    fn test_check_run_success_ignored() {
        let run = serde_json::json!({
            "id": 999,
            "name": "CI Build",
            "conclusion": "success",
        });
        assert!(check_run_to_signal("org/repo", &run).is_none());
    }

    #[test]
    fn test_has_label() {
        let issue = serde_json::json!({
            "labels": [{"name": "bug"}, {"name": "P1"}],
        });
        assert!(has_label(&issue, "bug"));
        assert!(has_label(&issue, "P1"));
        assert!(!has_label(&issue, "P0"));
    }

    #[test]
    fn test_ci_failure_signal() {
        let run_id = 456u64;
        let pr_title = "Add feature X";
        let pr_number = 42u64;
        let run_url = "https://github.com/org/repo/actions/runs/456";

        let key = format!("ci-failure-{pr_number}-{run_id}");
        let signal = SignalUpdate::new(
            "github",
            &key,
            format!("CI failed: {pr_title} (#{pr_number})"),
            Severity::Error,
        )
        .with_body(run_url)
        .with_url(run_url);

        assert_eq!(signal.external_id, "ci-failure-42-456");
        assert_eq!(signal.severity, Severity::Error);
        assert!(signal.title.contains("CI failed"));
        assert!(signal.title.contains("#42"));
        assert_eq!(signal.url.as_deref(), Some(run_url));
        assert_eq!(signal.body.as_deref(), Some(run_url));
    }

    #[test]
    fn test_ci_pass_signal() {
        let pr_title = "Add feature X";
        let pr_number = 42u64;
        let run_id = 789u64;
        let repo = "org/repo";
        let run_url = "https://github.com/org/repo/actions/runs/789";

        let key = format!("ci-pass-{repo}-{pr_number}-{run_id}");
        let signal = SignalUpdate::new(
            "github_ci_pass",
            &key,
            format!("\u{2705} CI passed on PR #{pr_number}: {pr_title}"),
            Severity::Info,
        )
        .with_url(run_url);

        assert_eq!(signal.source, "github_ci_pass");
        assert_eq!(signal.external_id, "ci-pass-org/repo-42-789");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("CI passed on PR #42"));
        assert!(signal.title.contains("Add feature X"));
    }

    #[test]
    fn test_release_success_signal() {
        let head_branch = "v0.1.4";
        let run_id = 12345u64;
        let repo = "org/repo";
        let html_url = "https://github.com/org/repo/actions/runs/12345";

        let key = format!("release-{repo}-{run_id}");
        let signal = SignalUpdate::new(
            "github_release",
            &key,
            format!("\u{1f680} {head_branch} release succeeded"),
            Severity::Info,
        )
        .with_url(html_url);

        assert_eq!(signal.source, "github_release");
        assert_eq!(signal.external_id, "release-org/repo-12345");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("v0.1.4"));
        assert!(signal.title.contains("release succeeded"));
    }

    #[test]
    fn test_release_failure_signal() {
        let head_branch = "v0.1.4";
        let run_id = 12345u64;
        let repo = "org/repo";

        let key = format!("release-{repo}-{run_id}");
        let signal = SignalUpdate::new(
            "github_release",
            &key,
            format!("\u{1f4a5} {head_branch} release failed"),
            Severity::Critical,
        );

        assert_eq!(signal.source, "github_release");
        assert_eq!(signal.severity, Severity::Critical);
        assert!(signal.title.contains("release failed"));
    }

    #[test]
    fn test_merged_pr_signal() {
        let pr_number = 53u64;
        let title = "Add fallback endpoint support";
        let repo = "org/repo";
        let url = "https://github.com/org/repo/pull/53";

        let key = format!("merged-{repo}-{pr_number}");
        let msg = format!("\u{2705} Merged: {title} #{pr_number}");
        let signal =
            SignalUpdate::new("github_merged_pr", &key, &msg, Severity::Info).with_url(url);

        assert_eq!(signal.source, "github_merged_pr");
        assert_eq!(signal.external_id, "merged-org/repo-53");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("Merged:"));
        assert!(signal.title.contains("#53"));
        assert_eq!(signal.url.as_deref(), Some(url));
    }

    #[test]
    fn test_labeled_issue_critical() {
        let issue = serde_json::json!({
            "number": 1,
            "title": "Critical issue",
            "html_url": "https://example.com",
            "labels": [],
        });
        let (_, signal) = labeled_issue_to_signal("org/repo", &issue, "critical").unwrap();
        assert_eq!(signal.severity, Severity::Critical);
    }
}

//! GitHub watcher — polls GitHub for events using the `gh` CLI.
//!
//! Queries open issues, PR review requests, watched labels, and CI status.
//! Stateless — emits all signals every poll; the DB handles dedup via UNIQUE constraints.

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
}

impl GithubWatcher {
    pub fn new(config: GithubWatcherConfig) -> Self {
        Self {
            config,
            gh_available: None,
            username: None,
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

        for repo in &self.config.repos.clone() {
            let signals = self.poll_repo(repo).await;
            for (_key, signal) in signals {
                all_signals.push(signal);
            }
        }

        // PR CI failure and pass signals
        for repo in &self.config.repos.clone() {
            let prs = self.fetch_open_prs(repo).await;
            for (pr_number, pr_title, head_branch) in prs {
                if let Some(run) = self.fetch_latest_run(repo, &head_branch).await {
                    let conclusion = run.get("conclusion").and_then(|v| v.as_str());
                    let run_id = run.get("databaseId").and_then(|v| v.as_u64());
                    let run_url = run.get("url").and_then(|v| v.as_str());

                    match conclusion {
                        Some("failure") => {
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
                            if let Some(run_id) = run_id {
                                let key = format!("ci-pass-{pr_number}-{run_id}");
                                let mut signal = SignalUpdate::new(
                                    "github",
                                    &key,
                                    format!("CI passed: {pr_title} (#{pr_number})"),
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
        }

        if !all_signals.is_empty() {
            info!("github: {} signal(s)", all_signals.len());
        }

        Ok(all_signals)
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
        let run_id = 789u64;
        let pr_title = "Add feature X";
        let pr_number = 42u64;
        let run_url = "https://github.com/org/repo/actions/runs/789";

        let key = format!("ci-pass-{pr_number}-{run_id}");
        let signal = SignalUpdate::new(
            "github",
            &key,
            format!("CI passed: {pr_title} (#{pr_number})"),
            Severity::Info,
        )
        .with_url(run_url);

        assert_eq!(signal.external_id, "ci-pass-42-789");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("CI passed"));
        assert!(signal.title.contains("#42"));
    }

    #[test]
    fn test_ci_pass_rerun_unique_external_ids() {
        // Two different run IDs for the same PR should produce different external_ids
        let pr_number = 30u64;
        let run_id_1 = 1000u64;
        let run_id_2 = 1001u64;

        let key1 = format!("ci-pass-{pr_number}-{run_id_1}");
        let key2 = format!("ci-pass-{pr_number}-{run_id_2}");

        assert_ne!(key1, key2, "re-runs should have unique external_ids");
        assert_eq!(key1, "ci-pass-30-1000");
        assert_eq!(key2, "ci-pass-30-1001");
    }

    #[test]
    fn test_ci_failure_includes_pr_number() {
        let pr_number = 42u64;
        let run_id = 999u64;
        let key = format!("ci-failure-{pr_number}-{run_id}");
        assert_eq!(key, "ci-failure-42-999");
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

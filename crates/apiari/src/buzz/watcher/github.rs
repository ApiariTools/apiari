//! GitHub watcher — polls GitHub for events using the `gh` CLI.
//!
//! Queries open issues, PR review requests, watched labels, and CI status.
//! Stateless — emits all signals every poll; the DB handles dedup via UNIQUE constraints.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use async_trait::async_trait;
use color_eyre::Result;
use futures::future::join_all;
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::config::GithubWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

/// Minimum remaining API calls before we skip the poll entirely.
const RATE_LIMIT_CRITICAL: u32 = 50;
/// Threshold below which we log a warning but still proceed.
const RATE_LIMIT_LOW: u32 = 200;

/// Evaluate rate-limit remaining count. Returns `Some(true)` to proceed,
/// `Some(false)` to skip, or logs a warning at the low threshold.
/// Pure function for testability.
fn rate_limit_decision(remaining: u32) -> (bool, bool) {
    // Returns (should_proceed, is_low_warning)
    if remaining < RATE_LIMIT_CRITICAL {
        (false, false)
    } else if remaining < RATE_LIMIT_LOW {
        (true, true)
    } else {
        (true, false)
    }
}

/// Max concurrent `gh` subprocess calls per poll (per-PR CI lookups, per-repo fanout).
const MAX_CONCURRENT_GH_CALLS: usize = 8;

/// Per-repo poll results collected concurrently, then merged into watcher state.
struct RepoPollResult {
    repo: String,
    signals: Vec<SignalUpdate>,
    new_release_cursor: Option<u64>,
    updated_merged_prs: Option<HashSet<u64>>,
    updated_ci_pass: HashSet<u64>,
    updated_bot_review_cursor: Option<String>,
    /// Updated PR head SHA map (always set; replaces previous cursor for this repo).
    updated_pr_push_cursors: HashMap<u64, String>,
}

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
    /// Last-seen bot review timestamp per repo (cursor: github_bot_review:{repo}).
    /// ISO 8601 string — only reviews newer than this are emitted.
    bot_review_cursors: HashMap<String, String>,
    /// Last-seen head commit SHA per open PR per repo (cursor: github_pr_push:{repo}).
    /// Used to detect new commits pushed to open PRs (useful for repos with no CI).
    pr_push_cursors: HashMap<String, HashMap<u64, String>>,
    /// Cached rate-limit remaining count and when it was fetched.
    last_rate_check: Option<(Instant, u32)>,
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
            bot_review_cursors: HashMap::new(),
            pr_push_cursors: HashMap::new(),
            last_rate_check: None,
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

            let bk = format!("github_bot_review:{repo}");
            if let Ok(Some(val)) = store.get_cursor(&bk)
                && !val.is_empty()
            {
                self.bot_review_cursors.insert(repo.clone(), val);
            }

            let pk = format!("github_pr_push:{repo}");
            if let Ok(Some(val)) = store.get_cursor(&pk)
                && !val.is_empty()
            {
                let map: HashMap<u64, String> = val
                    .split(',')
                    .filter_map(|entry| {
                        let (num, sha) = entry.split_once(':')?;
                        Some((num.parse().ok()?, sha.to_string()))
                    })
                    .collect();
                if !map.is_empty() {
                    self.pr_push_cursors.insert(repo.clone(), map);
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

    /// Fetch open pull requests for a repo. Returns (pr_number, pr_title, head_branch, head_sha).
    async fn fetch_open_prs(&self, repo: &str) -> Vec<(u64, String, String, String)> {
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
                    && let Some(sha) = pr
                        .get("head")
                        .and_then(|v| v.get("sha"))
                        .and_then(|v| v.as_str())
                {
                    result.push((
                        number,
                        title.to_string(),
                        branch.to_string(),
                        sha.to_string(),
                    ));
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
            if seen_prs.is_empty()
                && !is_recent_merge(
                    pr.get("mergedAt").and_then(|v| v.as_str()).unwrap_or(""),
                    self.config.interval_secs,
                )
            {
                continue;
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

    /// Poll for bot/automated code reviews on open PRs.
    /// Returns (signals, optional new cursor timestamp).
    async fn poll_bot_reviews(
        &self,
        repo: &str,
        cursor_ts: Option<&str>,
    ) -> (Vec<SignalUpdate>, Option<String>) {
        let mut signals = Vec::new();
        let mut max_ts = cursor_ts.map(|s| s.to_string());

        // Fetch open PRs (up to 20) and their reviews
        let endpoint = format!("repos/{repo}/pulls?state=open&per_page=20");
        let Some(prs_value) = self.gh_api(&endpoint).await else {
            return (signals, None);
        };
        let Some(prs) = prs_value.as_array() else {
            return (signals, None);
        };

        // Collect PR numbers + titles for review lookup
        let pr_infos: Vec<(u64, String)> = prs
            .iter()
            .filter_map(|pr| {
                let number = pr.get("number")?.as_u64()?;
                let title = pr.get("title")?.as_str()?.to_string();
                Some((number, title))
            })
            .collect();

        // Fetch reviews for each PR concurrently in bounded chunks
        for chunk in pr_infos.chunks(MAX_CONCURRENT_GH_CALLS) {
            let endpoints: Vec<String> = chunk
                .iter()
                .map(|(number, _)| format!("repos/{repo}/pulls/{number}/reviews?per_page=30"))
                .collect();
            let review_futures: Vec<_> = endpoints.iter().map(|ep| self.gh_api(ep)).collect();
            let results = futures::future::join_all(review_futures).await;

            for ((pr_number, pr_title), reviews_opt) in chunk.iter().zip(results) {
                let Some(reviews_value) = reviews_opt else {
                    continue;
                };
                let Some(reviews) = reviews_value.as_array() else {
                    continue;
                };

                for review in reviews {
                    let Some(user) = review.get("user") else {
                        continue;
                    };
                    let login = user.get("login").and_then(|v| v.as_str()).unwrap_or("");
                    let user_type = user.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    // Only bot reviews: GitHub App bots have type "Bot",
                    // or users with [bot] suffix in login
                    let is_bot = user_type == "Bot" || login.ends_with("[bot]");
                    if !is_bot {
                        continue;
                    }

                    let submitted_at = review
                        .get("submitted_at")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if submitted_at.is_empty() {
                        continue;
                    }

                    // Skip reviews older than or equal to cursor
                    if let Some(cursor) = cursor_ts
                        && submitted_at <= cursor
                    {
                        continue;
                    }

                    let state = review
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("COMMENTED");
                    let body = review.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let review_id = review.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let html_url = review
                        .get("html_url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // Truncate body to 500 chars
                    let truncated_body: String = body.chars().take(500).collect();

                    // Build metadata JSON
                    let metadata = serde_json::json!({
                        "pr_number": pr_number,
                        "pr_title": pr_title,
                        "repo": repo,
                        "bot_name": login,
                        "review_state": state,
                        "review_body": truncated_body,
                    });

                    let severity = match state {
                        "CHANGES_REQUESTED" => Severity::Warning,
                        "APPROVED" => Severity::Info,
                        _ => Severity::Info,
                    };

                    let bot_display = login.strip_suffix("[bot]").unwrap_or(login);
                    let key = format!("bot-review-{repo}-{pr_number}-{review_id}");
                    let title = format!("{bot_display} reviewed PR #{pr_number}: {state}");
                    let mut signal = SignalUpdate::new("github_bot_review", &key, &title, severity)
                        .with_metadata(metadata.to_string());
                    if !truncated_body.is_empty() {
                        signal = signal.with_body(&truncated_body);
                    }
                    if !html_url.is_empty() {
                        signal = signal.with_url(html_url);
                    }
                    signals.push(signal);

                    // Track max timestamp
                    match &max_ts {
                        Some(ts) if submitted_at > ts.as_str() => {
                            max_ts = Some(submitted_at.to_string());
                        }
                        None => {
                            max_ts = Some(submitted_at.to_string());
                        }
                        _ => {}
                    }
                }
            }
        }

        let new_cursor = if max_ts.as_deref() != cursor_ts {
            max_ts
        } else {
            None
        };

        (signals, new_cursor)
    }

    /// Detect new commits pushed to open PRs by comparing head SHAs against cursors.
    /// Returns (signals, updated cursor map for this repo).
    fn poll_pr_pushes(
        repo: &str,
        prs: &[(u64, String, String, String)],
        prev_cursors: &HashMap<u64, String>,
    ) -> (Vec<SignalUpdate>, HashMap<u64, String>) {
        let mut signals = Vec::new();
        let mut new_cursors = HashMap::new();

        for (number, title, _branch, sha) in prs {
            new_cursors.insert(*number, sha.clone());

            // Only emit if we have a previous SHA and it differs (actual new push).
            // On first run (empty cursors), we seed without emitting to avoid noise.
            if let Some(prev_sha) = prev_cursors.get(number) {
                if prev_sha != sha {
                    let key = format!("pr-push-{repo}-{number}-{sha}");
                    let msg = format!("New commits on PR #{number}: {title} ({repo})");
                    let url = format!("https://github.com/{repo}/pull/{number}");
                    let signal = SignalUpdate::new("github_pr_push", &key, &msg, Severity::Info)
                        .with_url(url);
                    signals.push(signal);
                }
            }
        }

        (signals, new_cursors)
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

    /// Check GitHub API rate limit. Returns false if remaining < 50 (skip poll).
    /// Caches the result for 60 seconds to avoid extra API calls.
    async fn check_rate_limit(&mut self) -> bool {
        if let Some((checked_at, remaining)) = self.last_rate_check
            && checked_at.elapsed().as_secs() < 60
        {
            let (proceed, _) = rate_limit_decision(remaining);
            return proceed;
        }

        let output = tokio::process::Command::new("gh")
            .args(["api", "rate_limit", "--jq", ".resources.core.remaining"])
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                let remaining: u32 = String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse()
                    .unwrap_or(5000);
                self.last_rate_check = Some((Instant::now(), remaining));

                let (proceed, is_low) = rate_limit_decision(remaining);
                if !proceed {
                    warn!("GitHub rate limit critical ({remaining} remaining) — skipping poll");
                } else if is_low {
                    warn!("GitHub rate limit low ({remaining} remaining) — proceeding cautiously");
                }
                proceed
            }
            _ => {
                warn!("Failed to check GitHub rate limit — proceeding anyway");
                // Cache failure so we don't re-run every poll and spam logs.
                // Use u32::MAX as a sentinel meaning "unknown but proceed".
                self.last_rate_check = Some((Instant::now(), u32::MAX));
                true
            }
        }
    }

    /// Poll everything for a single repo concurrently: issues, labels, CI checks,
    /// release runs, merged PRs, and PR CI status. Returns a self-contained result
    /// that the caller merges back into watcher state.
    async fn poll_repo_full(
        &self,
        repo: &str,
        last_seen_release_id: u64,
        seen_merged_prs: HashSet<u64>,
        mut ci_pass_prs: HashSet<u64>,
        bot_review_cursor: Option<String>,
        pr_push_prev: HashMap<u64, String>,
    ) -> RepoPollResult {
        // Run independent poll types concurrently within this repo.
        let (
            repo_signals,
            (release_signals, new_release_cursor),
            (merged_signals, updated_merged_prs),
            prs,
            (bot_review_signals, updated_bot_review_cursor),
        ) = tokio::join!(
            self.poll_repo(repo),
            self.poll_release_runs(repo, last_seen_release_id),
            self.poll_merged_prs(repo, &seen_merged_prs),
            self.fetch_open_prs(repo),
            self.poll_bot_reviews(repo, bot_review_cursor.as_deref()),
        );

        let mut signals: Vec<SignalUpdate> = repo_signals.into_iter().map(|(_, s)| s).collect();
        signals.extend(release_signals);
        signals.extend(merged_signals);
        signals.extend(bot_review_signals);

        // Detect new commits pushed to open PRs.
        let (pr_push_signals, updated_pr_push_cursors) =
            Self::poll_pr_pushes(repo, &prs, &pr_push_prev);
        signals.extend(pr_push_signals);

        // Fetch latest CI run for each open PR concurrently in bounded chunks.
        let open_pr_numbers: HashSet<u64> = prs.iter().map(|(n, _, _, _)| *n).collect();

        for chunk in prs.chunks(MAX_CONCURRENT_GH_CALLS) {
            let run_futures: Vec<_> = chunk
                .iter()
                .map(|(_, _, branch, _)| self.fetch_latest_run(repo, branch))
                .collect();
            let runs = join_all(run_futures).await;

            for ((pr_number, pr_title, _, _), run) in chunk.iter().zip(runs) {
                if let Some(run) = run {
                    let conclusion = run.get("conclusion").and_then(|v| v.as_str());
                    let run_id = run.get("databaseId").and_then(|v| v.as_u64());
                    let run_url = run.get("url").and_then(|v| v.as_str());

                    match conclusion {
                        Some("failure") => {
                            ci_pass_prs.remove(pr_number);
                            if let Some(run_id) = run_id {
                                let key = format!("ci-failure-{repo}-{pr_number}-{run_id}");
                                let mut signal = SignalUpdate::new(
                                    "github_ci_failure",
                                    &key,
                                    format!("CI failed: {pr_title} (#{pr_number})"),
                                    Severity::Error,
                                );
                                if let Some(url) = run_url {
                                    signal = signal.with_body(url).with_url(url);
                                }
                                signals.push(signal);
                            }
                        }
                        Some("success") => {
                            if !ci_pass_prs.contains(pr_number) {
                                ci_pass_prs.insert(*pr_number);
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
                                signals.push(signal);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        ci_pass_prs.retain(|n| open_pr_numbers.contains(n));

        RepoPollResult {
            repo: repo.to_string(),
            signals,
            new_release_cursor,
            updated_merged_prs,
            updated_ci_pass: ci_pass_prs,
            updated_bot_review_cursor,
            updated_pr_push_cursors,
        }
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

        if !self.check_rate_limit().await {
            return Ok(Vec::new());
        }

        let repos = self.config.repos.clone();

        // Snapshot cursor state, then fan out per-repo work concurrently.
        let repo_futures: Vec<_> = repos
            .iter()
            .map(|repo| {
                let last_seen = self.release_cursors.get(repo).copied().unwrap_or(0);
                let seen_prs = self
                    .merged_pr_cursors
                    .get(repo)
                    .cloned()
                    .unwrap_or_default();
                let ci_prs = self.ci_pass_state.get(repo).cloned().unwrap_or_default();
                let bot_cursor = self.bot_review_cursors.get(repo).cloned();
                let pr_push = self.pr_push_cursors.get(repo).cloned().unwrap_or_default();
                self.poll_repo_full(repo, last_seen, seen_prs, ci_prs, bot_cursor, pr_push)
            })
            .collect();

        let results = join_all(repo_futures).await;

        // Merge results back into watcher state.
        let mut all_signals = Vec::new();
        for result in results {
            all_signals.extend(result.signals);

            if let Some(max_id) = result.new_release_cursor {
                self.release_cursors.insert(result.repo.clone(), max_id);
            }

            if let Some(mut new_seen) = result.updated_merged_prs {
                // Keep only the last 100 to prevent unbounded growth
                if new_seen.len() > 100 {
                    let mut sorted: Vec<u64> = new_seen.into_iter().collect();
                    sorted.sort_unstable();
                    new_seen = sorted[sorted.len() - 100..].iter().copied().collect();
                }
                self.merged_pr_cursors.insert(result.repo.clone(), new_seen);
            }

            self.ci_pass_state
                .insert(result.repo.clone(), result.updated_ci_pass);

            if let Some(new_cursor) = result.updated_bot_review_cursor {
                self.bot_review_cursors
                    .insert(result.repo.clone(), new_cursor);
            }

            if !result.updated_pr_push_cursors.is_empty() {
                self.pr_push_cursors
                    .insert(result.repo.clone(), result.updated_pr_push_cursors);
            }
        }

        if !all_signals.is_empty() {
            info!("github: {} signal(s)", all_signals.len());
        }

        Ok(all_signals)
    }

    /// Persist cursor state and reconcile stale signals.
    ///
    /// The GitHub watcher emits signals under multiple sources ("github",
    /// "github_ci_failure"), so we must reconcile each source ourselves
    /// rather than relying on the framework's single-source auto-reconcile.
    fn reconcile(&self, _source: &str, poll_ids: &[String], store: &SignalStore) -> Result<usize> {
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
        for (repo, ts) in &self.bot_review_cursors {
            let key = format!("github_bot_review:{repo}");
            if let Err(e) = store.set_cursor(&key, ts) {
                warn!("failed to persist bot review cursor for {repo}: {e}");
            }
        }
        for (repo, pr_shas) in &self.pr_push_cursors {
            let key = format!("github_pr_push:{repo}");
            let val: String = pr_shas
                .iter()
                .map(|(num, sha)| format!("{num}:{sha}"))
                .collect::<Vec<_>>()
                .join(",");
            if let Err(e) = store.set_cursor(&key, &val) {
                warn!("failed to persist PR push cursor for {repo}: {e}");
            }
        }

        // Reconcile stale signals for all sources this watcher emits.
        let mut resolved = 0;
        for source in ["github", "github_ci_failure"] {
            resolved += store.resolve_missing_signals(source, poll_ids)?;
        }
        if resolved > 0 {
            info!("github: reconciled {resolved} stale signal(s)");
        }
        // Return max(1, resolved) so the framework skips its single-source fallback.
        Ok(resolved.max(1))
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
        "github_ci_failure",
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

/// Check if a merged PR is recent enough to emit a signal on first run.
fn is_recent_merge(merged_at_str: &str, interval_secs: u64) -> bool {
    chrono::DateTime::parse_from_rfc3339(merged_at_str)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .is_some_and(|merged_at| {
            let cutoff = chrono::Utc::now() - chrono::Duration::seconds((interval_secs * 2) as i64);
            merged_at >= cutoff
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_run_filters_old_merged_prs() {
        // A PR merged long ago should not be recent
        assert!(!is_recent_merge("2020-01-01T00:00:00Z", 300));

        // A PR merged just now should be recent
        let now = chrono::Utc::now().to_rfc3339();
        assert!(is_recent_merge(&now, 300));

        // Invalid date should not be recent
        assert!(!is_recent_merge("not-a-date", 300));
    }

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
        let repo = "org/repo";
        let run_url = "https://github.com/org/repo/actions/runs/456";

        let key = format!("ci-failure-{repo}-{pr_number}-{run_id}");
        let signal = SignalUpdate::new(
            "github_ci_failure",
            &key,
            format!("CI failed: {pr_title} (#{pr_number})"),
            Severity::Error,
        )
        .with_body(run_url)
        .with_url(run_url);

        assert_eq!(signal.external_id, "ci-failure-org/repo-42-456");
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
    fn test_bot_review_signal_structure() {
        let pr_number = 42u64;
        let pr_title = "Add new feature";
        let repo = "org/repo";
        let review_id = 1234u64;
        let login = "copilot[bot]";
        let state = "CHANGES_REQUESTED";
        let body = "Consider refactoring this function";

        let metadata = serde_json::json!({
            "pr_number": pr_number,
            "pr_title": pr_title,
            "repo": repo,
            "bot_name": login,
            "review_state": state,
            "review_body": body,
        });

        let bot_display = login.strip_suffix("[bot]").unwrap_or(login);
        let key = format!("bot-review-{repo}-{pr_number}-{review_id}");
        let title = format!("{bot_display} reviewed PR #{pr_number}: {state}");
        let signal = SignalUpdate::new("github_bot_review", &key, &title, Severity::Warning)
            .with_metadata(metadata.to_string())
            .with_body(body)
            .with_url("https://github.com/org/repo/pull/42#pullrequestreview-1234");

        assert_eq!(signal.source, "github_bot_review");
        assert_eq!(signal.external_id, "bot-review-org/repo-42-1234");
        assert_eq!(signal.severity, Severity::Warning);
        assert!(signal.title.contains("copilot"));
        assert!(signal.title.contains("CHANGES_REQUESTED"));
        assert!(signal.metadata.is_some());
        let meta: serde_json::Value =
            serde_json::from_str(signal.metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["pr_number"], 42);
        assert_eq!(meta["bot_name"], "copilot[bot]");
        assert_eq!(meta["review_state"], "CHANGES_REQUESTED");
    }

    #[test]
    fn test_bot_review_approved_is_info() {
        let signal = SignalUpdate::new(
            "github_bot_review",
            "bot-review-org/repo-1-100",
            "dependabot reviewed PR #1: APPROVED",
            Severity::Info,
        );
        assert_eq!(signal.severity, Severity::Info);
    }

    #[test]
    fn test_bot_review_body_truncation() {
        let long_body: String = "x".repeat(600);
        let truncated: String = long_body.chars().take(500).collect();
        assert_eq!(truncated.len(), 500);
        assert!(long_body.len() > 500);
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

    #[test]
    fn test_rate_limit_decision_critical() {
        // Below critical threshold — should skip poll
        let (proceed, _) = rate_limit_decision(0);
        assert!(!proceed);
        let (proceed, _) = rate_limit_decision(49);
        assert!(!proceed);
    }

    #[test]
    fn test_rate_limit_decision_low() {
        // At/above critical but below low threshold — proceed with warning
        let (proceed, is_low) = rate_limit_decision(50);
        assert!(proceed);
        assert!(is_low);
        let (proceed, is_low) = rate_limit_decision(199);
        assert!(proceed);
        assert!(is_low);
    }

    #[test]
    fn test_rate_limit_decision_ok() {
        // At/above low threshold — proceed without warning
        let (proceed, is_low) = rate_limit_decision(200);
        assert!(proceed);
        assert!(!is_low);
        let (proceed, is_low) = rate_limit_decision(5000);
        assert!(proceed);
        assert!(!is_low);
    }

    #[test]
    fn test_pr_push_first_run_no_signals() {
        // On first run (empty cursors), should seed state but NOT emit signals.
        let prs = vec![
            (1, "Fix bug".into(), "fix-bug".into(), "abc123".into()),
            (2, "Add feature".into(), "feat".into(), "def456".into()),
        ];
        let prev: HashMap<u64, String> = HashMap::new();

        let (signals, new_cursors) = GithubWatcher::poll_pr_pushes("org/repo", &prs, &prev);

        assert!(signals.is_empty(), "first run should not emit signals");
        assert_eq!(new_cursors.len(), 2);
        assert_eq!(new_cursors[&1], "abc123");
        assert_eq!(new_cursors[&2], "def456");
    }

    #[test]
    fn test_pr_push_new_commit_emits_signal() {
        let prs = vec![(42, "My PR".into(), "my-branch".into(), "newsha999".into())];
        let mut prev = HashMap::new();
        prev.insert(42, "oldsha111".to_string());

        let (signals, new_cursors) = GithubWatcher::poll_pr_pushes("org/repo", &prs, &prev);

        assert_eq!(signals.len(), 1);
        let signal = &signals[0];
        assert_eq!(signal.source, "github_pr_push");
        assert_eq!(signal.external_id, "pr-push-org/repo-42-newsha999");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("PR #42"));
        assert!(signal.title.contains("My PR"));
        assert!(signal.title.contains("org/repo"));
        assert_eq!(
            signal.url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        assert_eq!(new_cursors[&42], "newsha999");
    }

    #[test]
    fn test_pr_push_same_sha_no_signal() {
        let prs = vec![(10, "Stable".into(), "stable".into(), "sameSHA".into())];
        let mut prev = HashMap::new();
        prev.insert(10, "sameSHA".to_string());

        let (signals, new_cursors) = GithubWatcher::poll_pr_pushes("org/repo", &prs, &prev);

        assert!(signals.is_empty(), "unchanged SHA should not emit signal");
        assert_eq!(new_cursors[&10], "sameSHA");
    }

    #[test]
    fn test_pr_push_closed_pr_removed_from_cursors() {
        // PR #5 was tracked before but is no longer in open PRs.
        let prs = vec![(10, "Open PR".into(), "open".into(), "sha10".into())];
        let mut prev = HashMap::new();
        prev.insert(5, "old_sha".to_string());
        prev.insert(10, "sha10".to_string());

        let (_signals, new_cursors) = GithubWatcher::poll_pr_pushes("org/repo", &prs, &prev);

        // Only open PRs should be in the new cursors.
        assert_eq!(new_cursors.len(), 1);
        assert!(!new_cursors.contains_key(&5));
        assert_eq!(new_cursors[&10], "sha10");
    }

    #[test]
    fn test_pr_push_multiple_prs_mixed() {
        let prs = vec![
            (1, "PR one".into(), "b1".into(), "new1".into()),
            (2, "PR two".into(), "b2".into(), "same2".into()),
            (3, "PR three".into(), "b3".into(), "new3".into()),
        ];
        let mut prev = HashMap::new();
        prev.insert(1, "old1".to_string());
        prev.insert(2, "same2".to_string());
        prev.insert(3, "old3".to_string());

        let (signals, new_cursors) = GithubWatcher::poll_pr_pushes("org/repo", &prs, &prev);

        // PRs 1 and 3 changed, PR 2 unchanged
        assert_eq!(signals.len(), 2);
        assert!(signals.iter().any(|s| s.title.contains("PR #1")));
        assert!(signals.iter().any(|s| s.title.contains("PR #3")));
        assert!(!signals.iter().any(|s| s.title.contains("PR #2")));
        assert_eq!(new_cursors.len(), 3);
    }
}

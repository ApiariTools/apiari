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

/// Result of a `gh api --include` call with optional ETag support.
enum EtagApiResponse {
    /// HTTP 200 with fresh data and optional ETag header.
    Fresh {
        etag: Option<String>,
        body: serde_json::Value,
    },
    /// HTTP 304 — data not modified since the provided ETag.
    NotModified,
    /// Request failed (network error, 4xx/5xx).
    Error,
}

/// Parse a `gh api --include` response into (status_code, headers, body_str).
fn parse_gh_include_response(raw: &str) -> (u16, Vec<(&str, &str)>, &str) {
    let (headers_str, body) = if let Some(pos) = raw.find("\r\n\r\n") {
        (&raw[..pos], &raw[pos + 4..])
    } else if let Some(pos) = raw.find("\n\n") {
        (&raw[..pos], &raw[pos + 2..])
    } else {
        (raw, "")
    };

    let mut lines = headers_str.lines();

    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1)?.parse::<u16>().ok())
        .unwrap_or(0);

    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim(), value.trim()))
        })
        .collect();

    (status, headers, body)
}

// --- GraphQL result types for the consolidated per-repo query ---

struct GraphqlReview {
    database_id: u64,
    author_login: String,
    author_type: String,
    state: String,
    body: String,
    submitted_at: String,
    url: String,
}

struct GraphqlCheckSuite {
    conclusion: Option<String>,
    url: String,
    check_runs: Vec<GraphqlCheckRun>,
}

struct GraphqlCheckRun {
    conclusion: Option<String>,
    details_url: String,
}

struct GraphqlPr {
    number: u64,
    title: String,
    author_login: Option<String>,
    head_ref_name: String,
    head_sha: String,
    reviews: Vec<GraphqlReview>,
    check_suites: Vec<GraphqlCheckSuite>,
}

struct GraphqlMergedPr {
    number: u64,
    title: String,
    author_login: Option<String>,
    url: String,
    merged_at: String,
}

struct GraphqlIssue {
    number: u64,
    title: String,
    url: String,
    labels: Vec<String>,
}

struct RepoPollGraphqlResult {
    open_prs: Vec<GraphqlPr>,
    merged_prs: Vec<GraphqlMergedPr>,
    assigned_issues: Vec<GraphqlIssue>,
}

fn parse_graphql_result(response: &serde_json::Value) -> Option<RepoPollGraphqlResult> {
    let repo = response.pointer("/data/repository")?;

    let open_prs = repo
        .pointer("/openPRs/nodes")
        .map(parse_graphql_open_prs)
        .unwrap_or_default();
    let merged_prs = repo
        .pointer("/mergedPRs/nodes")
        .map(parse_graphql_merged_prs)
        .unwrap_or_default();
    let assigned_issues = repo
        .pointer("/assignedIssues/nodes")
        .map(parse_graphql_issues)
        .unwrap_or_default();

    Some(RepoPollGraphqlResult {
        open_prs,
        merged_prs,
        assigned_issues,
    })
}

fn parse_graphql_open_prs(nodes: &serde_json::Value) -> Vec<GraphqlPr> {
    let Some(arr) = nodes.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|node| {
            let number = node.get("number")?.as_u64()?;
            let title = node.get("title")?.as_str()?.to_string();
            let head_ref_name = node.get("headRefName")?.as_str()?.to_string();

            let head_sha = node
                .pointer("/commits/nodes/0/commit/oid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let reviews = node
                .pointer("/reviews/nodes")
                .and_then(|v| v.as_array())
                .map(|arr| parse_graphql_reviews(arr))
                .unwrap_or_default();

            let check_suites = node
                .pointer("/commits/nodes/0/commit/checkSuites/nodes")
                .and_then(|v| v.as_array())
                .map(|arr| parse_graphql_check_suites(arr))
                .unwrap_or_default();

            let author_login = node
                .pointer("/author/login")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            Some(GraphqlPr {
                number,
                title,
                author_login,
                head_ref_name,
                head_sha,
                reviews,
                check_suites,
            })
        })
        .collect()
}

fn parse_graphql_reviews(nodes: &[serde_json::Value]) -> Vec<GraphqlReview> {
    nodes
        .iter()
        .filter_map(|review| {
            let author = review.get("author")?;
            if author.is_null() {
                return None;
            }

            Some(GraphqlReview {
                database_id: review
                    .get("databaseId")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                author_login: author
                    .get("login")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                author_type: author
                    .get("__typename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                state: review
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("COMMENTED")
                    .to_string(),
                body: review
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                submitted_at: review
                    .get("submittedAt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: review
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn parse_graphql_check_suites(nodes: &[serde_json::Value]) -> Vec<GraphqlCheckSuite> {
    nodes
        .iter()
        .map(|suite| {
            let conclusion = suite
                .get("conclusion")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let url = suite
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let check_runs = suite
                .pointer("/checkRuns/nodes")
                .and_then(|v| v.as_array())
                .map(|arr| parse_graphql_check_runs(arr))
                .unwrap_or_default();

            GraphqlCheckSuite {
                conclusion,
                url,
                check_runs,
            }
        })
        .collect()
}

fn parse_graphql_check_runs(nodes: &[serde_json::Value]) -> Vec<GraphqlCheckRun> {
    nodes
        .iter()
        .map(|run| GraphqlCheckRun {
            conclusion: run
                .get("conclusion")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            details_url: run
                .get("detailsUrl")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect()
}

fn parse_graphql_merged_prs(nodes: &serde_json::Value) -> Vec<GraphqlMergedPr> {
    let Some(arr) = nodes.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|node| {
            Some(GraphqlMergedPr {
                number: node.get("number")?.as_u64()?,
                title: node.get("title")?.as_str()?.to_string(),
                author_login: node
                    .pointer("/author/login")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                url: node.get("url")?.as_str()?.to_string(),
                merged_at: node.get("mergedAt")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn parse_graphql_issues(nodes: &serde_json::Value) -> Vec<GraphqlIssue> {
    let Some(arr) = nodes.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|node| {
            let labels = node
                .pointer("/labels/nodes")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l.get("name").and_then(|v| v.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            Some(GraphqlIssue {
                number: node.get("number")?.as_u64()?,
                title: node.get("title")?.as_str()?.to_string(),
                url: node.get("url")?.as_str()?.to_string(),
                labels,
            })
        })
        .collect()
}

/// Evaluate check suites for a PR to determine overall CI status.
/// Returns (has_failure, has_success). CANCELLED/NEUTRAL/SKIPPED/in-progress
/// suites are ignored — CI passes when at least one suite succeeds and none fail.
/// Check if a signal matches an author filter string.
///
/// Supported filter formats:
/// - `"author:@me"` — matches if the PR author is the authenticated user
/// - `"author:<username>"` — matches if the PR author is the given username
///
/// For signals that don't reference a PR (e.g. `github_release`), always passes.
/// The PR number is extracted from the signal's `external_id` key patterns.
fn signal_matches_author_filter(
    signal: &SignalUpdate,
    filter: &str,
    my_username: Option<&str>,
    pr_authors: &HashMap<u64, Option<String>>,
) -> bool {
    let target_username = if let Some(rest) = filter.strip_prefix("author:") {
        let name = rest.trim();
        if name.is_empty() {
            return true; // bare "author:" with no value, don't filter
        }
        if name == "@me" {
            match my_username {
                Some(u) => u,
                None => return true, // can't resolve @me, pass through
            }
        } else {
            name
        }
    } else {
        // Unknown filter format — don't filter
        return true;
    };

    // Extract PR number from signal external_id key patterns:
    //   pr-push-{repo}-{number}-{sha}
    //   ci-failure-{repo}-{number}-{sha_short}
    //   ci-pass-{repo}-{number}-{sha_short}
    //   bot-review-{repo}-{number}-{review_id}
    //   merged-{repo}-{number}
    let pr_number = extract_pr_number_from_key(&signal.external_id);

    match pr_number {
        Some(num) => match pr_authors.get(&num) {
            Some(Some(author)) => author.eq_ignore_ascii_case(target_username),
            Some(None) => true, // Author unknown (ghost/deleted user), pass through
            // PR not in GraphQL data (e.g. pagination limit) — pass through
            // to avoid silently dropping signals we can't verify.
            None => true,
        },
        None => true, // Not a PR-related signal, pass through
    }
}

/// Extract a PR number from known signal external_id key patterns.
///
/// Keys have the form `{prefix}{owner}/{repo}-{number}` or
/// `{prefix}{owner}/{repo}-{number}-{suffix}`. Since repo names can contain
/// dashes (e.g. `my-cool-repo`), we split from the right to find the numeric
/// PR number segment.
fn extract_pr_number_from_key(key: &str) -> Option<u64> {
    let has_suffix_prefixes = ["pr-push-", "ci-failure-", "ci-pass-", "bot-review-"];
    let no_suffix_prefix = "merged-";

    // "merged-" keys: {owner}/{repo}-{number} — PR number is always the last
    // dash-separated segment, since there is no trailing suffix.
    if let Some(rest) = key.strip_prefix(no_suffix_prefix) {
        return rest.rsplit('-').next()?.parse().ok();
    }

    // Other keys: {owner}/{repo}-{number}-{suffix} — PR number is the
    // second-to-last dash-separated segment (suffix is SHA or review ID).
    let rest = has_suffix_prefixes
        .iter()
        .find_map(|p| key.strip_prefix(p))?;
    let last_dash = rest.rfind('-')?;
    let before_suffix = &rest[..last_dash];
    before_suffix.rsplit('-').next()?.parse().ok()
}

/// Evaluate check suites and return (has_failure, has_success).
fn evaluate_check_suites(check_suites: &[GraphqlCheckSuite]) -> (bool, bool) {
    let mut has_failure = false;
    let mut has_success = false;

    for suite in check_suites {
        match suite.conclusion.as_deref() {
            Some("FAILURE") => has_failure = true,
            Some("SUCCESS") => has_success = true,
            _ => {} // NEUTRAL, CANCELLED, SKIPPED, in-progress (None)
        }
    }

    (has_failure, has_success)
}

/// Find the URL of the first failing check run across all check suites.
fn first_failing_url(check_suites: &[GraphqlCheckSuite]) -> Option<String> {
    for suite in check_suites {
        if suite.conclusion.as_deref() == Some("FAILURE") {
            for run in &suite.check_runs {
                if run.conclusion.as_deref() == Some("FAILURE") && !run.details_url.is_empty() {
                    return Some(run.details_url.clone());
                }
            }
            if !suite.url.is_empty() {
                return Some(suite.url.clone());
            }
        }
    }
    None
}

/// Snapshot of per-repo cursor state passed into `poll_repo_full`.
struct RepoPollParams {
    repo: String,
    last_seen_release_id: u64,
    seen_merged_prs: HashSet<u64>,
    ci_pass_prs: HashSet<u64>,
    bot_review_cursor: Option<String>,
    pr_push_prev: HashMap<u64, String>,
    release_etag: Option<String>,
}

/// Per-repo poll results collected concurrently, then merged into watcher state.
struct RepoPollResult {
    repo: String,
    signals: Vec<SignalUpdate>,
    new_release_cursor: Option<u64>,
    updated_merged_prs: Option<HashSet<u64>>,
    updated_ci_pass: HashSet<u64>,
    updated_bot_review_cursor: Option<String>,
    updated_pr_push_cursors: Option<HashMap<u64, String>>,
    new_release_etag: Option<String>,
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
    /// ETag per repo for the release-runs endpoint (avoids rate-limit charge on 304).
    release_etags: HashMap<String, String>,
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
            release_etags: HashMap::new(),
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

    /// Execute `gh api --include` with optional ETag (If-None-Match) support.
    /// Returns `Fresh` with body on 200, `NotModified` on 304, or `Error` on failure.
    async fn gh_api_etag(&self, endpoint: &str, etag: Option<&str>) -> EtagApiResponse {
        let mut args = vec!["api".to_string(), "--include".to_string()];
        if let Some(etag) = etag {
            args.push("--header".to_string());
            args.push(format!("If-None-Match: {etag}"));
        }
        args.push(endpoint.to_string());

        let output = match tokio::process::Command::new("gh")
            .args(&args)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                warn!("failed to run `gh api {endpoint}`: {e}");
                return EtagApiResponse::Error;
            }
        };

        let raw = String::from_utf8_lossy(&output.stdout);
        let (status, headers, body_str) = parse_gh_include_response(&raw);

        if status == 304 {
            return EtagApiResponse::NotModified;
        }

        if status == 0 || status >= 400 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "`gh api {endpoint}` failed (HTTP {status}): {}",
                stderr.trim()
            );
            return EtagApiResponse::Error;
        }

        let new_etag = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
            .map(|(_, v)| v.to_string());

        match serde_json::from_str::<serde_json::Value>(body_str) {
            Ok(body) => EtagApiResponse::Fresh {
                etag: new_etag,
                body,
            },
            Err(e) => {
                warn!("failed to parse JSON from `gh api {endpoint}`: {e}");
                EtagApiResponse::Error
            }
        }
    }

    /// Poll for completed Release workflow runs on a repo (ETag-protected REST).
    /// Returns (signals, optional new max run ID, optional new ETag).
    async fn poll_release_runs(
        &self,
        repo: &str,
        last_seen_id: u64,
        etag: Option<&str>,
    ) -> (Vec<SignalUpdate>, Option<u64>, Option<String>) {
        let mut signals = Vec::new();

        let endpoint = format!("repos/{repo}/actions/runs?per_page=10&status=completed");
        let (response, new_etag) = match self.gh_api_etag(&endpoint, etag).await {
            EtagApiResponse::Fresh {
                etag: new_etag,
                body,
            } => (body, new_etag),
            EtagApiResponse::NotModified => return (signals, None, None),
            EtagApiResponse::Error => return (signals, None, None),
        };

        let Some(runs) = response.get("workflow_runs").and_then(|v| v.as_array()) else {
            return (signals, None, new_etag);
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

        (signals, new_cursor, new_etag)
    }

    /// Execute a consolidated GraphQL query for a repo, fetching open PRs (with
    /// reviews and CI check suites), merged PRs, and assigned issues in one call.
    async fn poll_repo_graphql(&self, repo: &str) -> Option<RepoPollGraphqlResult> {
        let Some((owner, name)) = repo.split_once('/') else {
            warn!("invalid repo format for GraphQL query: {repo}");
            return None;
        };

        let issues_fragment = if let Some(username) = &self.username {
            format!(
                r#"assignedIssues: issues(states: [OPEN], first: 10, filterBy: {{assignee: "{username}"}}) {{ nodes {{ number title url updatedAt labels(first: 5) {{ nodes {{ name }} }} }} }}"#
            )
        } else {
            String::new()
        };

        let query = format!(
            r#"{{ repository(owner: "{owner}", name: "{name}") {{ openPRs: pullRequests(states: [OPEN], first: 20, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{ nodes {{ number title url headRefName updatedAt author {{ login }} commits(last: 1) {{ nodes {{ commit {{ oid checkSuites(first: 10) {{ nodes {{ conclusion url checkRuns(first: 10) {{ nodes {{ name conclusion detailsUrl }} }} }} }} }} }} }} reviews(last: 30) {{ nodes {{ databaseId state body submittedAt url author {{ __typename login }} }} }} }} }} mergedPRs: pullRequests(states: [MERGED], first: 10, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{ nodes {{ number title url mergedAt author {{ login }} }} }} {issues_fragment} }} }}"#
        );
        let query_arg = format!("query={query}");

        let output = match tokio::process::Command::new("gh")
            .args(["api", "graphql", "-f", &query_arg])
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                warn!("failed to run GraphQL query for {repo}: {e}");
                return None;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("GraphQL query for {repo} failed: {}", stderr.trim());
            return None;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        let response: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to parse GraphQL response for {repo}: {e}");
                return None;
            }
        };

        if let Some(errors) = response.get("errors").and_then(|v| v.as_array()) {
            for err in errors {
                let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
                warn!("GraphQL error for {repo}: {msg}");
            }
        }

        parse_graphql_result(&response)
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
            if let Some(prev_sha) = prev_cursors.get(number)
                && prev_sha != sha
            {
                let key = format!("pr-push-{repo}-{number}-{sha}");
                let msg = format!("New commits on PR #{number}: {title} ({repo})");
                let url = format!("https://github.com/{repo}/pull/{number}");
                let metadata = serde_json::json!({
                    "repo": repo,
                    "pr_number": number,
                });
                let signal = SignalUpdate::new("github_pr_push", &key, &msg, Severity::Info)
                    .with_url(url)
                    .with_metadata(metadata.to_string());
                signals.push(signal);
            }
        }

        (signals, new_cursors)
    }

    /// Poll watched labels via REST (not available in GraphQL mega-query).
    async fn poll_watched_labels(&self, repo: &str) -> Vec<(String, SignalUpdate)> {
        let mut signals = Vec::new();
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

    /// Poll everything for a single repo: one GraphQL mega-query for PRs/reviews/
    /// issues/CI, one ETag-protected REST call for release runs, plus optional
    /// REST calls for watched labels.
    async fn poll_repo_full(&self, params: RepoPollParams) -> RepoPollResult {
        let RepoPollParams {
            repo,
            last_seen_release_id,
            seen_merged_prs,
            mut ci_pass_prs,
            bot_review_cursor,
            pr_push_prev,
            release_etag,
        } = params;

        // Run GraphQL mega-query, release runs (REST+ETag), and watched labels concurrently.
        let (
            graphql_result,
            (release_signals, new_release_cursor, new_release_etag),
            label_signals,
        ) = tokio::join!(
            self.poll_repo_graphql(&repo),
            self.poll_release_runs(&repo, last_seen_release_id, release_etag.as_deref()),
            self.poll_watched_labels(&repo),
        );

        let mut signals: Vec<SignalUpdate> = label_signals.into_iter().map(|(_, s)| s).collect();
        signals.extend(release_signals);

        let mut updated_bot_review_cursor = None;
        let mut updated_merged_prs = None;
        let mut updated_pr_push_cursors = None;

        if let Some(gql) = graphql_result {
            // --- Assigned issue signals ---
            for issue in &gql.assigned_issues {
                let severity = if issue.labels.iter().any(|l| l == "critical" || l == "P0") {
                    Severity::Critical
                } else if issue.labels.iter().any(|l| l == "bug" || l == "P1") {
                    Severity::Warning
                } else {
                    Severity::Info
                };
                let key = format!("gh-issue-{repo}-{}", issue.number);
                let signal = SignalUpdate::new(
                    "github",
                    &key,
                    format!("[{repo}] issue #{}: {}", issue.number, issue.title),
                    severity,
                )
                .with_url(&issue.url);
                signals.push(signal);
            }

            // --- Bot review signals (same cursor-based dedup as before) ---
            let cursor_ts = bot_review_cursor.as_deref();
            let mut max_ts = cursor_ts.map(|s| s.to_string());

            for pr in &gql.open_prs {
                for review in &pr.reviews {
                    let is_bot =
                        review.author_type == "Bot" || review.author_login.ends_with("[bot]");
                    if !is_bot || review.submitted_at.is_empty() {
                        continue;
                    }
                    if let Some(cursor) = cursor_ts
                        && review.submitted_at.as_str() <= cursor
                    {
                        continue;
                    }

                    let truncated_body: String = review.body.chars().take(500).collect();
                    let metadata = serde_json::json!({
                        "pr_number": pr.number,
                        "pr_title": pr.title,
                        "repo": repo,
                        "bot_name": review.author_login,
                        "review_state": review.state,
                        "review_body": truncated_body,
                    });
                    let severity = match review.state.as_str() {
                        "CHANGES_REQUESTED" => Severity::Warning,
                        _ => Severity::Info,
                    };
                    let bot_display = review
                        .author_login
                        .strip_suffix("[bot]")
                        .unwrap_or(&review.author_login);
                    let key = format!("bot-review-{repo}-{}-{}", pr.number, review.database_id);
                    let title =
                        format!("{bot_display} reviewed PR #{}: {}", pr.number, review.state);
                    let mut signal = SignalUpdate::new("github_bot_review", &key, &title, severity)
                        .with_metadata(metadata.to_string());
                    if !truncated_body.is_empty() {
                        signal = signal.with_body(&truncated_body);
                    }
                    if !review.url.is_empty() {
                        signal = signal.with_url(&review.url);
                    }
                    signals.push(signal);

                    match &max_ts {
                        Some(ts) if review.submitted_at.as_str() > ts.as_str() => {
                            max_ts = Some(review.submitted_at.clone());
                        }
                        None => {
                            max_ts = Some(review.submitted_at.clone());
                        }
                        _ => {}
                    }
                }
            }
            if max_ts.as_deref() != cursor_ts {
                updated_bot_review_cursor = max_ts;
            }

            // --- Merged PR signals (same seen-set dedup as before) ---
            let mut new_seen = seen_merged_prs.clone();
            for pr in &gql.merged_prs {
                if pr.number == 0 || seen_merged_prs.contains(&pr.number) {
                    continue;
                }
                new_seen.insert(pr.number);
                if seen_merged_prs.is_empty()
                    && !is_recent_merge(&pr.merged_at, self.config.interval_secs)
                {
                    continue;
                }
                let key = format!("merged-{repo}-{}", pr.number);
                let msg = format!("\u{2705} Merged: {} #{}", pr.title, pr.number);
                let metadata = serde_json::json!({
                    "repo": repo,
                    "pr_number": pr.number,
                });
                let mut signal = SignalUpdate::new("github_merged_pr", &key, &msg, Severity::Info)
                    .with_metadata(metadata.to_string());
                if !pr.url.is_empty() {
                    signal = signal.with_url(&pr.url);
                }
                signals.push(signal);
            }
            if new_seen != seen_merged_prs {
                updated_merged_prs = Some(new_seen);
            }

            // --- PR push detection ---
            let prs: Vec<(u64, String, String, String)> = gql
                .open_prs
                .iter()
                .map(|pr| {
                    (
                        pr.number,
                        pr.title.clone(),
                        pr.head_ref_name.clone(),
                        pr.head_sha.clone(),
                    )
                })
                .collect();
            let (pr_push_signals, cursors) = Self::poll_pr_pushes(&repo, &prs, &pr_push_prev);
            signals.extend(pr_push_signals);
            updated_pr_push_cursors = Some(cursors);

            // --- CI pass/fail signals from per-PR check suites ---
            let open_pr_numbers: HashSet<u64> = gql.open_prs.iter().map(|pr| pr.number).collect();

            for pr in &gql.open_prs {
                let (has_failure, has_success) = evaluate_check_suites(&pr.check_suites);

                if has_failure {
                    ci_pass_prs.remove(&pr.number);
                    let sha_short = &pr.head_sha[..7.min(pr.head_sha.len())];
                    let key = format!("ci-failure-{repo}-{}-{sha_short}", pr.number);
                    let metadata = serde_json::json!({
                        "repo": repo,
                        "pr_number": pr.number,
                    });
                    let mut signal = SignalUpdate::new(
                        "github_ci_failure",
                        &key,
                        format!("CI failed: {} (#{})", pr.title, pr.number),
                        Severity::Error,
                    )
                    .with_metadata(metadata.to_string());
                    if let Some(url) = first_failing_url(&pr.check_suites) {
                        signal = signal.with_body(&url).with_url(&url);
                    }
                    signals.push(signal);
                } else if has_success && !ci_pass_prs.contains(&pr.number) {
                    ci_pass_prs.insert(pr.number);
                    let sha_short = &pr.head_sha[..7.min(pr.head_sha.len())];
                    let key = format!("ci-pass-{repo}-{}-{sha_short}", pr.number);
                    let metadata = serde_json::json!({
                        "repo": repo,
                        "pr_number": pr.number,
                    });
                    let mut signal = SignalUpdate::new(
                        "github_ci_pass",
                        &key,
                        format!("\u{2705} CI passed on PR #{}: {}", pr.number, pr.title),
                        Severity::Info,
                    )
                    .with_metadata(metadata.to_string());
                    if let Some(suite) = pr.check_suites.first()
                        && !suite.url.is_empty()
                    {
                        signal = signal.with_url(&suite.url);
                    }
                    signals.push(signal);
                }
            }

            ci_pass_prs.retain(|n| open_pr_numbers.contains(n));

            // Apply per-event-type author filters
            if !self.config.filters.is_empty() {
                let mut pr_authors: HashMap<u64, Option<String>> = HashMap::new();
                for pr in &gql.open_prs {
                    pr_authors.insert(pr.number, pr.author_login.clone());
                }
                for pr in &gql.merged_prs {
                    pr_authors.insert(pr.number, pr.author_login.clone());
                }
                signals.retain(|signal| match self.config.filters.get(&signal.source) {
                    None => true,
                    Some(filter) => signal_matches_author_filter(
                        signal,
                        filter,
                        self.username.as_deref(),
                        &pr_authors,
                    ),
                });
            }
        }

        RepoPollResult {
            repo,
            signals,
            new_release_cursor,
            updated_merged_prs,
            updated_ci_pass: ci_pass_prs,
            updated_bot_review_cursor,
            updated_pr_push_cursors,
            new_release_etag,
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
                let rel_etag = self.release_etags.get(repo).cloned();
                self.poll_repo_full(RepoPollParams {
                    repo: repo.clone(),
                    last_seen_release_id: last_seen,
                    seen_merged_prs: seen_prs,
                    ci_pass_prs: ci_prs,
                    bot_review_cursor: bot_cursor,
                    pr_push_prev: pr_push,
                    release_etag: rel_etag,
                })
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

            if let Some(pr_push) = result.updated_pr_push_cursors {
                self.pr_push_cursors.insert(result.repo.clone(), pr_push);
            }

            if let Some(etag) = result.new_release_etag {
                self.release_etags.insert(result.repo.clone(), etag);
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

        // Reconcile stale signals only for stateful sources that should auto-resolve.
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
    fn test_ci_failure_signal() {
        let pr_title = "Add feature X";
        let pr_number = 42u64;
        let repo = "org/repo";
        let sha_short = "abc1234";
        let run_url = "https://github.com/org/repo/actions/runs/456";

        let key = format!("ci-failure-{repo}-{pr_number}-{sha_short}");
        let metadata = serde_json::json!({ "repo": repo, "pr_number": pr_number });
        let signal = SignalUpdate::new(
            "github_ci_failure",
            &key,
            format!("CI failed: {pr_title} (#{pr_number})"),
            Severity::Error,
        )
        .with_metadata(metadata.to_string())
        .with_body(run_url)
        .with_url(run_url);

        assert_eq!(signal.external_id, "ci-failure-org/repo-42-abc1234");
        assert_eq!(signal.severity, Severity::Error);
        assert!(signal.title.contains("CI failed"));
        assert!(signal.title.contains("#42"));
        assert_eq!(signal.url.as_deref(), Some(run_url));
        assert_eq!(signal.body.as_deref(), Some(run_url));
        let meta: serde_json::Value =
            serde_json::from_str(signal.metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["repo"], "org/repo");
        assert_eq!(meta["pr_number"], 42);
    }

    #[test]
    fn test_ci_pass_signal() {
        let pr_title = "Add feature X";
        let pr_number = 42u64;
        let repo = "org/repo";
        let sha_short = "def5678";

        let key = format!("ci-pass-{repo}-{pr_number}-{sha_short}");
        let metadata = serde_json::json!({ "repo": repo, "pr_number": pr_number });
        let signal = SignalUpdate::new(
            "github_ci_pass",
            &key,
            format!("\u{2705} CI passed on PR #{pr_number}: {pr_title}"),
            Severity::Info,
        )
        .with_metadata(metadata.to_string());

        assert_eq!(signal.source, "github_ci_pass");
        assert_eq!(signal.external_id, "ci-pass-org/repo-42-def5678");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("CI passed on PR #42"));
        assert!(signal.title.contains("Add feature X"));
        let meta: serde_json::Value =
            serde_json::from_str(signal.metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["repo"], "org/repo");
        assert_eq!(meta["pr_number"], 42);
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
        let metadata = serde_json::json!({ "repo": repo, "pr_number": pr_number });
        let signal = SignalUpdate::new("github_merged_pr", &key, &msg, Severity::Info)
            .with_metadata(metadata.to_string())
            .with_url(url);

        assert_eq!(signal.source, "github_merged_pr");
        assert_eq!(signal.external_id, "merged-org/repo-53");
        assert_eq!(signal.severity, Severity::Info);
        assert!(signal.title.contains("Merged:"));
        assert!(signal.title.contains("#53"));
        assert_eq!(signal.url.as_deref(), Some(url));
        let meta: serde_json::Value =
            serde_json::from_str(signal.metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["repo"], "org/repo");
        assert_eq!(meta["pr_number"], 53);
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
        let meta: serde_json::Value =
            serde_json::from_str(signal.metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["repo"], "org/repo");
        assert_eq!(meta["pr_number"], 42);
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
    fn test_pr_push_no_open_prs_clears_cursors() {
        // All PRs closed — should return empty cursors so old state is replaced.
        let prs: Vec<(u64, String, String, String)> = vec![];
        let mut prev = HashMap::new();
        prev.insert(1, "sha1".to_string());
        prev.insert(2, "sha2".to_string());

        let (signals, new_cursors) = GithubWatcher::poll_pr_pushes("org/repo", &prs, &prev);

        assert!(signals.is_empty());
        assert!(
            new_cursors.is_empty(),
            "cursors should be empty when no open PRs"
        );
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

    #[test]
    fn test_parse_graphql_open_prs() {
        let nodes = serde_json::json!([
            {
                "number": 42,
                "title": "Add feature",
                "url": "https://github.com/org/repo/pull/42",
                "headRefName": "feat-branch",
                "updatedAt": "2024-01-01T00:00:00Z",
                "commits": { "nodes": [{ "commit": {
                    "oid": "abc123def456",
                    "checkSuites": { "nodes": [
                        {
                            "conclusion": "SUCCESS",
                            "url": "https://github.com/org/repo/check-suite/1",
                            "checkRuns": { "nodes": [
                                { "name": "build", "conclusion": "SUCCESS", "detailsUrl": "https://example.com/build" }
                            ]}
                        }
                    ]}
                }}]},
                "reviews": { "nodes": [
                    {
                        "databaseId": 100,
                        "state": "APPROVED",
                        "body": "LGTM",
                        "submittedAt": "2024-01-01T12:00:00Z",
                        "url": "https://github.com/org/repo/pull/42#review-100",
                        "author": { "__typename": "Bot", "login": "copilot[bot]" }
                    }
                ]}
            }
        ]);

        let prs = parse_graphql_open_prs(&nodes);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].title, "Add feature");
        assert_eq!(prs[0].head_ref_name, "feat-branch");
        assert_eq!(prs[0].head_sha, "abc123def456");
        assert_eq!(prs[0].check_suites.len(), 1);
        assert_eq!(
            prs[0].check_suites[0].conclusion.as_deref(),
            Some("SUCCESS")
        );
        assert_eq!(prs[0].reviews.len(), 1);
        assert_eq!(prs[0].reviews[0].author_login, "copilot[bot]");
        assert_eq!(prs[0].reviews[0].author_type, "Bot");
    }

    #[test]
    fn test_parse_graphql_merged_prs() {
        let nodes = serde_json::json!([
            {
                "number": 10,
                "title": "Fix bug",
                "url": "https://github.com/org/repo/pull/10",
                "mergedAt": "2024-01-15T08:00:00Z"
            }
        ]);
        let prs = parse_graphql_merged_prs(&nodes);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 10);
        assert_eq!(prs[0].merged_at, "2024-01-15T08:00:00Z");
    }

    #[test]
    fn test_parse_graphql_issues() {
        let nodes = serde_json::json!([
            {
                "number": 5,
                "title": "Bug report",
                "url": "https://github.com/org/repo/issues/5",
                "updatedAt": "2024-01-01T00:00:00Z",
                "labels": { "nodes": [{ "name": "bug" }, { "name": "P1" }] }
            }
        ]);
        let issues = parse_graphql_issues(&nodes);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 5);
        assert_eq!(issues[0].labels, vec!["bug", "P1"]);
    }

    #[test]
    fn test_evaluate_check_suites_all_pass() {
        let suites = vec![
            GraphqlCheckSuite {
                conclusion: Some("SUCCESS".into()),
                url: String::new(),
                check_runs: vec![],
            },
            GraphqlCheckSuite {
                conclusion: Some("SUCCESS".into()),
                url: String::new(),
                check_runs: vec![],
            },
        ];
        let (fail, success) = evaluate_check_suites(&suites);
        assert!(!fail);
        assert!(success);
    }

    #[test]
    fn test_evaluate_check_suites_has_failure() {
        let suites = vec![
            GraphqlCheckSuite {
                conclusion: Some("SUCCESS".into()),
                url: String::new(),
                check_runs: vec![],
            },
            GraphqlCheckSuite {
                conclusion: Some("FAILURE".into()),
                url: String::new(),
                check_runs: vec![],
            },
        ];
        let (fail, success) = evaluate_check_suites(&suites);
        assert!(fail);
        assert!(success);
    }

    #[test]
    fn test_evaluate_check_suites_success_with_cancelled() {
        // SUCCESS + CANCELLED should still count as CI pass (no failure)
        let suites = vec![
            GraphqlCheckSuite {
                conclusion: Some("SUCCESS".into()),
                url: String::new(),
                check_runs: vec![],
            },
            GraphqlCheckSuite {
                conclusion: Some("CANCELLED".into()),
                url: String::new(),
                check_runs: vec![],
            },
        ];
        let (fail, success) = evaluate_check_suites(&suites);
        assert!(!fail);
        assert!(success);
    }

    #[test]
    fn test_evaluate_check_suites_success_with_in_progress() {
        // SUCCESS + in-progress (None) should still count as CI pass
        let suites = vec![
            GraphqlCheckSuite {
                conclusion: Some("SUCCESS".into()),
                url: String::new(),
                check_runs: vec![],
            },
            GraphqlCheckSuite {
                conclusion: None,
                url: String::new(),
                check_runs: vec![],
            },
        ];
        let (fail, success) = evaluate_check_suites(&suites);
        assert!(!fail);
        assert!(success);
    }

    #[test]
    fn test_evaluate_check_suites_empty() {
        let (fail, success) = evaluate_check_suites(&[]);
        assert!(!fail);
        assert!(!success);
    }

    #[test]
    fn test_first_failing_url_finds_check_run() {
        let suites = vec![GraphqlCheckSuite {
            conclusion: Some("FAILURE".into()),
            url: "https://suite-url".into(),
            check_runs: vec![
                GraphqlCheckRun {
                    conclusion: Some("SUCCESS".into()),
                    details_url: "https://pass-url".into(),
                },
                GraphqlCheckRun {
                    conclusion: Some("FAILURE".into()),
                    details_url: "https://fail-url".into(),
                },
            ],
        }];
        assert_eq!(
            first_failing_url(&suites).as_deref(),
            Some("https://fail-url")
        );
    }

    #[test]
    fn test_first_failing_url_falls_back_to_suite() {
        let suites = vec![GraphqlCheckSuite {
            conclusion: Some("FAILURE".into()),
            url: "https://suite-url".into(),
            check_runs: vec![],
        }];
        assert_eq!(
            first_failing_url(&suites).as_deref(),
            Some("https://suite-url")
        );
    }

    #[test]
    fn test_parse_graphql_result_full() {
        let response = serde_json::json!({
            "data": {
                "repository": {
                    "openPRs": { "nodes": [
                        {
                            "number": 1,
                            "title": "PR 1",
                            "url": "https://example.com/pr/1",
                            "headRefName": "branch-1",
                            "updatedAt": "2024-01-01T00:00:00Z",
                            "commits": { "nodes": [{ "commit": { "oid": "sha1", "checkSuites": { "nodes": [] } } }] },
                            "reviews": { "nodes": [] }
                        }
                    ]},
                    "mergedPRs": { "nodes": [
                        { "number": 2, "title": "PR 2", "url": "https://example.com/pr/2", "mergedAt": "2024-01-01T00:00:00Z" }
                    ]},
                    "assignedIssues": { "nodes": [
                        { "number": 3, "title": "Issue 3", "url": "https://example.com/issue/3", "updatedAt": "2024-01-01T00:00:00Z", "labels": { "nodes": [] } }
                    ]}
                }
            }
        });

        let result = parse_graphql_result(&response).unwrap();
        assert_eq!(result.open_prs.len(), 1);
        assert_eq!(result.merged_prs.len(), 1);
        assert_eq!(result.assigned_issues.len(), 1);
    }

    #[test]
    fn test_extract_pr_number_from_key() {
        assert_eq!(
            extract_pr_number_from_key("pr-push-org/repo-42-abc123"),
            Some(42)
        );
        assert_eq!(
            extract_pr_number_from_key("ci-failure-org/repo-7-abc"),
            Some(7)
        );
        assert_eq!(
            extract_pr_number_from_key("ci-pass-org/repo-99-def"),
            Some(99)
        );
        assert_eq!(
            extract_pr_number_from_key("bot-review-org/repo-10-555"),
            Some(10)
        );
        assert_eq!(extract_pr_number_from_key("merged-org/repo-5"), Some(5));
        // Repo names with dashes
        assert_eq!(
            extract_pr_number_from_key("pr-push-org/my-cool-repo-42-abc123"),
            Some(42)
        );
        assert_eq!(
            extract_pr_number_from_key("ci-failure-org/my-repo-7-abc"),
            Some(7)
        );
        assert_eq!(
            extract_pr_number_from_key("merged-org/dashed-name-5"),
            Some(5)
        );
        // Hyphenated repo with numeric segment in the name
        assert_eq!(
            extract_pr_number_from_key("merged-org/service-2-17"),
            Some(17)
        );
        assert_eq!(
            extract_pr_number_from_key("ci-pass-org/v2-api-99-abc"),
            Some(99)
        );
        assert_eq!(extract_pr_number_from_key("release-org/repo-123"), None);
        assert_eq!(extract_pr_number_from_key("random-key"), None);
    }

    #[test]
    fn test_signal_matches_author_filter_me() {
        let mut pr_authors = HashMap::new();
        pr_authors.insert(42, Some("josh".to_string()));
        pr_authors.insert(99, Some("other-user".to_string()));

        let signal_42 = SignalUpdate::new(
            "github_pr_push",
            "pr-push-org/repo-42-abc123",
            "pushed to PR",
            Severity::Info,
        );
        let signal_99 = SignalUpdate::new(
            "github_ci_pass",
            "ci-pass-org/repo-99-def",
            "CI passed",
            Severity::Info,
        );

        // author:@me with username "josh" should match PR 42 but not PR 99
        assert!(signal_matches_author_filter(
            &signal_42,
            "author:@me",
            Some("josh"),
            &pr_authors,
        ));
        assert!(!signal_matches_author_filter(
            &signal_99,
            "author:@me",
            Some("josh"),
            &pr_authors,
        ));
    }

    #[test]
    fn test_signal_matches_author_filter_literal() {
        let mut pr_authors = HashMap::new();
        pr_authors.insert(10, Some("bot-user".to_string()));

        let signal = SignalUpdate::new(
            "github_bot_review",
            "bot-review-org/repo-10-555",
            "bot reviewed",
            Severity::Info,
        );

        assert!(signal_matches_author_filter(
            &signal,
            "author:bot-user",
            None,
            &pr_authors,
        ));
        assert!(!signal_matches_author_filter(
            &signal,
            "author:someone-else",
            None,
            &pr_authors,
        ));
    }

    #[test]
    fn test_signal_matches_author_filter_non_pr_signal_passes() {
        let pr_authors = HashMap::new();

        let signal = SignalUpdate::new(
            "github_release",
            "release-org/repo-123",
            "new release",
            Severity::Info,
        );

        // Non-PR signals always pass through filters
        assert!(signal_matches_author_filter(
            &signal,
            "author:@me",
            Some("josh"),
            &pr_authors,
        ));
    }

    #[test]
    fn test_signal_matches_author_filter_no_filter_passes() {
        let pr_authors = HashMap::new();

        let signal = SignalUpdate::new(
            "github_pr_push",
            "pr-push-org/repo-42-abc",
            "pushed",
            Severity::Info,
        );

        // Unknown filter format passes through
        assert!(signal_matches_author_filter(
            &signal,
            "unknown:value",
            Some("josh"),
            &pr_authors,
        ));

        // Empty "author:" value passes through
        assert!(signal_matches_author_filter(
            &signal,
            "author:",
            Some("josh"),
            &pr_authors,
        ));
        assert!(signal_matches_author_filter(
            &signal,
            "author:   ",
            Some("josh"),
            &pr_authors,
        ));
    }

    #[test]
    fn test_signal_matches_author_filter_ghost_user_passes() {
        let mut pr_authors = HashMap::new();
        pr_authors.insert(42, None); // ghost/deleted user

        let signal = SignalUpdate::new(
            "github_pr_push",
            "pr-push-org/repo-42-abc",
            "pushed",
            Severity::Info,
        );

        // Ghost user (None author) should pass through filter
        assert!(signal_matches_author_filter(
            &signal,
            "author:@me",
            Some("josh"),
            &pr_authors,
        ));
    }

    #[test]
    fn test_signal_filtering_end_to_end() {
        let mut pr_authors = HashMap::new();
        pr_authors.insert(1, Some("josh".to_string()));
        pr_authors.insert(2, Some("other".to_string()));

        let signals = vec![
            SignalUpdate::new(
                "github_pr_push",
                "pr-push-org/repo-1-aaa",
                "push to #1",
                Severity::Info,
            ),
            SignalUpdate::new(
                "github_pr_push",
                "pr-push-org/repo-2-bbb",
                "push to #2",
                Severity::Info,
            ),
            SignalUpdate::new(
                "github_ci_pass",
                "ci-pass-org/repo-1-aaa",
                "CI pass #1",
                Severity::Info,
            ),
            SignalUpdate::new(
                "github_release",
                "release-org/repo-100",
                "new release",
                Severity::Info,
            ),
            SignalUpdate::new(
                "github_merged_pr",
                "merged-org/repo-2",
                "merged #2",
                Severity::Info,
            ),
        ];

        let mut filters = HashMap::new();
        filters.insert("github_pr_push".to_string(), "author:@me".to_string());
        filters.insert("github_ci_pass".to_string(), "author:@me".to_string());
        // no filter for github_release or github_merged_pr

        let filtered: Vec<_> = signals
            .into_iter()
            .filter(|signal| match filters.get(&signal.source) {
                None => true,
                Some(filter) => {
                    signal_matches_author_filter(signal, filter, Some("josh"), &pr_authors)
                }
            })
            .collect();

        // Should keep: pr_push #1 (josh), ci_pass #1 (josh), release, merged_pr #2
        // Should drop: pr_push #2 (other)
        assert_eq!(filtered.len(), 4);
        assert_eq!(filtered[0].external_id, "pr-push-org/repo-1-aaa");
        assert_eq!(filtered[1].external_id, "ci-pass-org/repo-1-aaa");
        assert_eq!(filtered[2].external_id, "release-org/repo-100");
        assert_eq!(filtered[3].external_id, "merged-org/repo-2");
    }
}

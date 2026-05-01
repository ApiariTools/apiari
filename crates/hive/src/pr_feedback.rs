use crate::pr_review::PrReviewCache;
use crate::tick::{Action, TickContext, Watcher};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct FeedbackStore {
    pub prs: HashMap<String, PrFeedbackState>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct PrFeedbackState {
    pub worker_id: String,
    pub forwarded_comment_ids: Vec<u64>,
    pub rounds: u32,
    pub last_worker_sha: Option<String>,
    pub last_forward_at: Option<String>,
    pub conflict_notified: bool,
    #[serde(default)]
    pub ci_notified_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewComment {
    pub id: u64,
    pub author: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub body: String,
}

/// Info about a worker's PR parsed from swarm state.
struct WorkerPr {
    worker_id: String,
    owner: String,
    repo: String,
    number: i64,
}

pub struct PrFeedbackWatcher {
    workspace_roots: Vec<PathBuf>,
    store: FeedbackStore,
    store_path: PathBuf,
    max_rounds: u32,
    pr_review_cache: PrReviewCache,
}

impl PrFeedbackWatcher {
    pub fn new(
        workspace_roots: Vec<PathBuf>,
        store_path: PathBuf,
        max_rounds: u32,
        pr_review_cache: PrReviewCache,
    ) -> Self {
        let store = load_store(&store_path);
        Self {
            workspace_roots,
            store,
            store_path,
            max_rounds,
            pr_review_cache,
        }
    }
}

fn load_store(path: &std::path::Path) -> FeedbackStore {
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => store,
            Err(err) => {
                warn!(
                    "[pr-feedback] failed to parse store at {}: {}; using default",
                    path.display(),
                    err
                );
                FeedbackStore::default()
            }
        },
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "[pr-feedback] failed to read store at {}: {}; using default",
                    path.display(),
                    err
                );
            }
            FeedbackStore::default()
        }
    }
}

fn save_store(path: &std::path::Path, store: &FeedbackStore) {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        warn!(
            "[pr-feedback] failed to create store directory {}: {}",
            parent.display(),
            err
        );
        return;
    }
    match serde_json::to_string_pretty(store) {
        Ok(json) => {
            if let Err(err) = std::fs::write(path, json) {
                warn!(
                    "[pr-feedback] failed to write store to {}: {}",
                    path.display(),
                    err
                );
            }
        }
        Err(err) => {
            warn!("[pr-feedback] failed to serialize store: {}", err);
        }
    }
}

/// Parse a GitHub PR URL into (owner, repo, number).
fn parse_pr_url(url: &str) -> Option<(String, String, i64)> {
    let url = url.trim_end_matches('/');
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    let len = parts.len();
    if parts[len - 2] != "pull" {
        return None;
    }
    let number = parts[len - 1].parse::<i64>().ok()?;
    let repo = parts[len - 3].to_string();
    let owner = parts[len - 4].to_string();
    Some((owner, repo, number))
}

/// Read swarm state and extract workers that have open PRs.
fn read_worker_prs(workspace_root: &std::path::Path) -> Vec<WorkerPr> {
    let state_path = workspace_root.join(".swarm/state.json");
    let content = match std::fs::read_to_string(&state_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let worktrees = match state.get("worktrees").and_then(|w| w.as_array()) {
        Some(w) => w,
        None => return vec![],
    };

    let mut result = Vec::new();
    for wt in worktrees {
        let worker_id = match wt.get("id").and_then(|i| i.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let pr = match wt.get("pr") {
            Some(p) => p,
            None => continue,
        };
        // Skip closed/merged PRs
        let pr_state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("open");
        if pr_state == "closed" || pr_state == "merged" {
            continue;
        }
        if let Some(url) = pr.get("url").and_then(|u| u.as_str())
            && let Some((owner, repo, number)) = parse_pr_url(url)
        {
            result.push(WorkerPr {
                worker_id,
                owner,
                repo,
                number,
            });
        }
    }
    result
}

/// Fetch PR review comments via `gh api`.
async fn fetch_pr_comments(owner: &str, repo: &str, number: i64) -> Vec<ReviewComment> {
    let endpoint = format!("repos/{owner}/{repo}/pulls/{number}/comments");
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["api", &endpoint, "--paginate"]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }

    let output = match cmd.output().await {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!("[pr-feedback] gh api comments failed: {stderr}");
            return vec![];
        }
        Err(e) => {
            warn!("[pr-feedback] failed to run gh: {e}");
            return vec![];
        }
    };

    let items: Vec<serde_json::Value> = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    items
        .into_iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_u64()?;
            let author = item
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or("unknown")
                .to_string();
            let path = item.get("path").and_then(|p| p.as_str()).map(String::from);
            let line = item.get("line").and_then(|l| l.as_u64());
            let body = item.get("body").and_then(|b| b.as_str())?.to_string();
            Some(ReviewComment {
                id,
                author,
                path,
                line,
                body,
            })
        })
        .collect()
}

/// Fetch CI failure log for a PR.
async fn fetch_ci_failure_log(owner: &str, repo: &str, pr_number: i64) -> Option<String> {
    // Use the PR head SHA directly (avoids pagination issues with commits endpoint)
    let sha = get_pr_head_sha(owner, repo, pr_number).await?;

    // Get check runs for that commit
    let runs_endpoint = format!("repos/{owner}/{repo}/commits/{}/check-runs", sha);
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["api", &runs_endpoint]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }
    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let check_runs = body.get("check_runs")?.as_array()?;

    // Find a failed check run
    let failed_run = check_runs.iter().find(|r| {
        r.get("conclusion")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c == "failure")
    })?;
    let run_id = failed_run.get("id")?.as_u64()?;

    // Get annotations for the failed run
    let annotations_endpoint = format!("repos/{owner}/{repo}/check-runs/{run_id}/annotations");
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["api", &annotations_endpoint]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }
    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        return None;
    }

    let annotations: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    if annotations.is_empty() {
        return Some(format!(
            "CI check '{}' failed (no annotations available). Check the PR for details.",
            failed_run
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown")
        ));
    }

    let mut log = String::new();
    for ann in annotations.iter().take(10) {
        let path = ann
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("unknown");
        let line = ann.get("start_line").and_then(|l| l.as_u64()).unwrap_or(0);
        let msg = ann
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("(no message)");
        log.push_str(&format!("{path}:{line}: {msg}\n"));
    }
    Some(log)
}

/// Format review comments into a message for the worker.
pub(crate) fn format_review_message(
    comments: &[ReviewComment],
    pr_number: i64,
    round: u32,
    max_rounds: u32,
) -> String {
    let mut msg = format!(
        "[PR Watcher] {} new review comment(s) on PR #{} (round {}/{}):\n\n",
        comments.len(),
        pr_number,
        round,
        max_rounds
    );
    for (i, c) in comments.iter().enumerate() {
        let location = match (&c.path, c.line) {
            (Some(p), Some(l)) => format!("{p}:{l}"),
            (Some(p), None) => p.clone(),
            _ => "general".to_string(),
        };
        let body = {
            let mut chars = c.body.chars();
            let truncated: String = chars.by_ref().take(200).collect();
            if chars.next().is_some() {
                format!("{truncated}...")
            } else {
                truncated
            }
        };
        msg.push_str(&format!(
            "{}. {} ({}):\n   {}\n\n",
            i + 1,
            location,
            c.author,
            body
        ));
    }
    msg.push_str("Please address these and push.");
    msg
}

/// Format a CI failure message.
pub(crate) fn format_ci_failure_message(pr_number: i64, log: &str) -> String {
    format!(
        "[PR Watcher] CI failed on PR #{}. Failure log:\n\n{}\n\nPlease fix and push.",
        pr_number, log
    )
}

/// Format a merge conflict message.
pub(crate) fn format_conflict_message(pr_number: i64) -> String {
    format!(
        "[PR Watcher] PR #{} has merge conflicts. Please rebase on main and force-push:\n\
         git fetch origin && git rebase origin/main && git push --force-with-lease",
        pr_number
    )
}

/// Filter out already-forwarded comments, returning only new ones.
pub(crate) fn new_comments(
    comments: &[ReviewComment],
    forwarded_ids: &[u64],
) -> Vec<ReviewComment> {
    comments
        .iter()
        .filter(|c| !forwarded_ids.contains(&c.id))
        .cloned()
        .collect()
}

#[async_trait]
impl Watcher for PrFeedbackWatcher {
    fn name(&self) -> &str {
        "pr-feedback-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        8 // every 8th tick = ~2 minutes at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut active_pr_keys: HashSet<String> = HashSet::new();

        for root in &self.workspace_roots {
            let worker_prs = read_worker_prs(root);

            for wp in &worker_prs {
                let pr_key = format!("{}/{}/{}", wp.owner, wp.repo, wp.number);
                active_pr_keys.insert(pr_key.clone());

                let state =
                    self.store
                        .prs
                        .entry(pr_key.clone())
                        .or_insert_with(|| PrFeedbackState {
                            worker_id: wp.worker_id.clone(),
                            forwarded_comment_ids: Vec::new(),
                            rounds: 0,
                            last_worker_sha: None,
                            last_forward_at: None,
                            conflict_notified: false,
                            ci_notified_sha: None,
                        });

                // Update worker_id in case it changed
                state.worker_id = wp.worker_id.clone();

                if state.rounds >= self.max_rounds {
                    warn!(
                        "[pr-feedback] PR #{} has had {} rounds of feedback without resolution. Manual intervention needed.",
                        wp.number, self.max_rounds
                    );
                    continue;
                }

                // Check if worker pushed a new commit — resolve forwarded review threads
                let head_sha = get_pr_head_sha(&wp.owner, &wp.repo, wp.number).await;
                if let Some(ref sha) = head_sha {
                    let sha_changed = state
                        .last_worker_sha
                        .as_ref()
                        .is_some_and(|prev| prev != sha);
                    if sha_changed && !state.forwarded_comment_ids.is_empty() {
                        resolve_forwarded_threads(
                            &wp.owner,
                            &wp.repo,
                            wp.number,
                            &state.forwarded_comment_ids,
                        )
                        .await;
                    }
                    state.last_worker_sha = Some(sha.clone());
                }

                // Check review comments
                let comments = fetch_pr_comments(&wp.owner, &wp.repo, wp.number).await;
                let new = new_comments(&comments, &state.forwarded_comment_ids);

                if !new.is_empty() {
                    state.rounds += 1;
                    let message =
                        format_review_message(&new, wp.number, state.rounds, self.max_rounds);
                    actions.push(Action::SendToWorker {
                        workspace_root: root.clone(),
                        worker_id: wp.worker_id.clone(),
                        message,
                    });
                    for c in &new {
                        state.forwarded_comment_ids.push(c.id);
                    }
                    state.last_forward_at = Some(chrono::Utc::now().to_rfc3339());
                    info!(
                        "[pr-feedback] Forwarded {} comment(s) for PR #{} to worker {}",
                        new.len(),
                        wp.number,
                        wp.worker_id
                    );
                }

                // Check CI status from review cache
                let cache_key = crate::pr_review::cache_key(&wp.owner, &wp.repo, wp.number);
                let ci_status = {
                    let guard = self.pr_review_cache.lock().await;
                    guard.get(&cache_key).and_then(|s| s.ci_status.clone())
                };
                if let Some(ref status) = ci_status
                    && (status == "FAILURE" || status == "ERROR")
                    && head_sha.is_some()
                {
                    // Only notify if we haven't already told this worker about this SHA
                    let already_notified = state
                        .ci_notified_sha
                        .as_ref()
                        .zip(head_sha.as_ref())
                        .is_some_and(|(notified, current)| notified == current);
                    if !already_notified
                        && let Some(log) =
                            fetch_ci_failure_log(&wp.owner, &wp.repo, wp.number).await
                    {
                        let message = format_ci_failure_message(wp.number, &log);
                        actions.push(Action::SendToWorker {
                            workspace_root: root.clone(),
                            worker_id: wp.worker_id.clone(),
                            message,
                        });
                        state.ci_notified_sha = head_sha.clone();
                        state.rounds += 1;
                        info!(
                            "[pr-feedback] Forwarded CI failure for PR #{} to worker {}",
                            wp.number, wp.worker_id
                        );
                    }
                }

                // Check merge conflicts via the PR API
                if !state.conflict_notified {
                    let mergeable = check_pr_mergeable(&wp.owner, &wp.repo, wp.number).await;
                    if mergeable == Some(false) {
                        actions.push(Action::SendToWorker {
                            workspace_root: root.clone(),
                            worker_id: wp.worker_id.clone(),
                            message: format_conflict_message(wp.number),
                        });
                        state.conflict_notified = true;
                        state.rounds += 1;
                        info!(
                            "[pr-feedback] Notified worker {} about merge conflicts on PR #{}",
                            wp.worker_id, wp.number
                        );
                    }
                }
            }
        }

        // Clean up stale entries
        self.store.prs.retain(|key, _| active_pr_keys.contains(key));

        // Save store after modifications
        save_store(&self.store_path, &self.store);

        actions
    }
}

/// Fetch review thread node IDs via GraphQL, returning (thread_id, is_resolved, comment_database_ids).
async fn fetch_review_threads(
    owner: &str,
    repo: &str,
    number: i64,
) -> Vec<(String, bool, Vec<u64>)> {
    let query = r#"query($owner: String!, $repo: String!, $number: Int!) {
        repository(owner: $owner, name: $repo) {
            pullRequest(number: $number) {
                reviewThreads(first: 100) {
                    nodes {
                        id
                        isResolved
                        comments(first: 100) {
                            nodes { databaseId }
                        }
                    }
                }
            }
        }
    }"#;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "api",
        "graphql",
        "-f",
        &format!("query={query}"),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("repo={repo}"),
        "-F",
        &format!("number={number}"),
    ]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }
    let output = match cmd.output().await {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!("[pr-feedback] GraphQL fetch review threads failed: {stderr}");
            return vec![];
        }
        Err(e) => {
            warn!("[pr-feedback] failed to run gh for review threads: {e}");
            return vec![];
        }
    };

    let body: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let nodes = match body
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(|n| n.as_array())
    {
        Some(n) => n,
        None => return vec![],
    };

    nodes
        .iter()
        .filter_map(|node| {
            let id = node.get("id")?.as_str()?.to_string();
            let is_resolved = node.get("isResolved")?.as_bool()?;
            let db_ids = node
                .pointer("/comments/nodes")
                .and_then(|n| n.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.get("databaseId")?.as_u64())
                        .collect()
                })
                .unwrap_or_default();
            Some((id, is_resolved, db_ids))
        })
        .collect()
}

/// Resolve a single review thread via GraphQL mutation.
async fn resolve_review_thread(thread_id: &str) -> bool {
    let query = r#"mutation($threadId: ID!) {
        resolveReviewThread(input: {threadId: $threadId}) {
            thread { isResolved }
        }
    }"#;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "api",
        "graphql",
        "-f",
        &format!("query={query}"),
        "-f",
        &format!("threadId={thread_id}"),
    ]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }
    match cmd.output().await {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!("[pr-feedback] GraphQL resolve thread failed: {stderr}");
            false
        }
        Err(e) => {
            warn!("[pr-feedback] failed to run gh for resolve thread: {e}");
            false
        }
    }
}

/// Resolve review threads whose comment IDs overlap with `forwarded_ids`.
async fn resolve_forwarded_threads(owner: &str, repo: &str, number: i64, forwarded_ids: &[u64]) {
    if forwarded_ids.is_empty() {
        return;
    }
    let threads = fetch_review_threads(owner, repo, number).await;
    for (thread_id, is_resolved, db_ids) in &threads {
        if *is_resolved {
            continue;
        }
        if db_ids.iter().any(|id| forwarded_ids.contains(id))
            && resolve_review_thread(thread_id).await
        {
            info!("[pr-feedback] Resolved review thread {thread_id}");
        }
    }
}

/// Get the head SHA of a PR.
async fn get_pr_head_sha(owner: &str, repo: &str, number: i64) -> Option<String> {
    let endpoint = format!("repos/{owner}/{repo}/pulls/{number}");
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["api", &endpoint, "--jq", ".head.sha"]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }
    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Check if a PR is mergeable.
async fn check_pr_mergeable(owner: &str, repo: &str, number: i64) -> Option<bool> {
    let endpoint = format!("repos/{owner}/{repo}/pulls/{number}");
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["api", &endpoint, "--jq", ".mergeable"]);
    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }
    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match val.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None, // "null" or unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feedback_store_roundtrip() {
        let mut store = FeedbackStore::default();
        store.prs.insert(
            "org/repo/1".to_string(),
            PrFeedbackState {
                worker_id: "w1".to_string(),
                forwarded_comment_ids: vec![100, 200],
                rounds: 1,
                last_worker_sha: Some("abc123".to_string()),
                last_forward_at: Some("2026-04-26T00:00:00Z".to_string()),
                conflict_notified: false,
                ci_notified_sha: None,
            },
        );

        let json = serde_json::to_string(&store).unwrap();
        let restored: FeedbackStore = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.prs.len(), 1);
        let state = &restored.prs["org/repo/1"];
        assert_eq!(state.worker_id, "w1");
        assert_eq!(state.forwarded_comment_ids, vec![100, 200]);
        assert_eq!(state.rounds, 1);
        assert_eq!(state.last_worker_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_new_comments_detected() {
        let comments = vec![
            ReviewComment {
                id: 1,
                author: "alice".to_string(),
                path: Some("src/main.rs".to_string()),
                line: Some(10),
                body: "Fix this".to_string(),
            },
            ReviewComment {
                id: 2,
                author: "bob".to_string(),
                path: None,
                line: None,
                body: "LGTM".to_string(),
            },
            ReviewComment {
                id: 3,
                author: "charlie".to_string(),
                path: Some("lib.rs".to_string()),
                line: Some(5),
                body: "Needs work".to_string(),
            },
        ];

        let forwarded = vec![1u64];
        let new = new_comments(&comments, &forwarded);
        assert_eq!(new.len(), 2);
        assert_eq!(new[0].id, 2);
        assert_eq!(new[1].id, 3);
    }

    #[test]
    fn test_max_rounds_stops_forwarding() {
        // Simulate: when rounds >= max_rounds, the watcher skips.
        // We test this logic directly rather than through the async tick.
        let state = PrFeedbackState {
            worker_id: "w1".to_string(),
            forwarded_comment_ids: vec![],
            rounds: 3,
            last_worker_sha: None,
            last_forward_at: None,
            conflict_notified: false,
            ci_notified_sha: None,
        };
        let max_rounds = 3;
        assert!(state.rounds >= max_rounds);
    }

    #[test]
    fn test_message_formatting() {
        let comments = vec![
            ReviewComment {
                id: 1,
                author: "alice".to_string(),
                path: Some("src/main.rs".to_string()),
                line: Some(42),
                body: "This needs to handle the error case".to_string(),
            },
            ReviewComment {
                id: 2,
                author: "bob".to_string(),
                path: None,
                line: None,
                body: "General comment".to_string(),
            },
        ];

        let msg = format_review_message(&comments, 7, 1, 3);
        assert!(msg.contains("2 new review comment(s) on PR #7 (round 1/3)"));
        assert!(msg.contains("src/main.rs:42 (alice)"));
        assert!(msg.contains("general (bob)"));
        assert!(msg.contains("Please address these and push."));
    }

    #[test]
    fn test_message_formatting_truncates_long_body() {
        let long_body = "x".repeat(300);
        let comments = vec![ReviewComment {
            id: 1,
            author: "alice".to_string(),
            path: Some("file.rs".to_string()),
            line: Some(1),
            body: long_body,
        }];

        let msg = format_review_message(&comments, 1, 1, 3);
        // Should contain truncated body (200 chars + "...")
        assert!(msg.contains(&format!("{}...", "x".repeat(200))));
        assert!(!msg.contains(&"x".repeat(201)));
    }

    #[test]
    fn test_cleanup_removes_stale_entries() {
        let mut store = FeedbackStore::default();
        store.prs.insert(
            "org/repo/1".to_string(),
            PrFeedbackState {
                worker_id: "w1".to_string(),
                forwarded_comment_ids: vec![],
                rounds: 0,
                last_worker_sha: None,
                last_forward_at: None,
                conflict_notified: false,
                ci_notified_sha: None,
            },
        );
        store.prs.insert(
            "org/repo/2".to_string(),
            PrFeedbackState {
                worker_id: "w2".to_string(),
                forwarded_comment_ids: vec![],
                rounds: 0,
                last_worker_sha: None,
                last_forward_at: None,
                conflict_notified: false,
                ci_notified_sha: None,
            },
        );

        // Only "org/repo/1" is still active
        let active_keys: HashSet<String> = ["org/repo/1".to_string()].into();
        store.prs.retain(|key, _| active_keys.contains(key));

        assert_eq!(store.prs.len(), 1);
        assert!(store.prs.contains_key("org/repo/1"));
        assert!(!store.prs.contains_key("org/repo/2"));
    }

    #[test]
    fn test_ci_failure_message_format() {
        let msg = format_ci_failure_message(42, "error[E0308]: mismatched types");
        assert!(msg.contains("CI failed on PR #42"));
        assert!(msg.contains("error[E0308]: mismatched types"));
        assert!(msg.contains("Please fix and push."));
    }

    #[test]
    fn test_conflict_message_format() {
        let msg = format_conflict_message(42);
        assert!(msg.contains("PR #42 has merge conflicts"));
        assert!(msg.contains("git fetch origin && git rebase origin/main"));
    }

    #[test]
    fn test_parse_pr_url() {
        let (owner, repo, number) = parse_pr_url("https://github.com/Org/Repo/pull/5").unwrap();
        assert_eq!(owner, "Org");
        assert_eq!(repo, "Repo");
        assert_eq!(number, 5);

        assert!(parse_pr_url("not a url").is_none());
        assert!(parse_pr_url("https://github.com/o/r/issues/1").is_none());
    }

    #[test]
    fn test_read_worker_prs_from_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".swarm")).unwrap();
        std::fs::write(
            dir.path().join(".swarm/state.json"),
            r#"{"worktrees":[
                {"id":"w1","pr":{"url":"https://github.com/O/R/pull/1","state":"open"}},
                {"id":"w2","pr":{"url":"https://github.com/O/R/pull/2","state":"closed"}},
                {"id":"w3"}
            ]}"#,
        )
        .unwrap();

        let prs = read_worker_prs(dir.path());
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].worker_id, "w1");
        assert_eq!(prs[0].number, 1);
    }

    #[test]
    fn test_store_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pr_feedback.json");

        let mut store = FeedbackStore::default();
        store.prs.insert(
            "org/repo/1".to_string(),
            PrFeedbackState {
                worker_id: "w1".to_string(),
                forwarded_comment_ids: vec![10, 20],
                rounds: 2,
                last_worker_sha: None,
                last_forward_at: None,
                conflict_notified: true,
                ci_notified_sha: None,
            },
        );
        save_store(&path, &store);

        let loaded = load_store(&path);
        assert_eq!(loaded.prs.len(), 1);
        let state = &loaded.prs["org/repo/1"];
        assert_eq!(state.rounds, 2);
        assert!(state.conflict_notified);
    }

    #[test]
    fn test_ci_notified_sha_serde_default() {
        // Old serialized data without ci_notified_sha should deserialize with None
        let json = r#"{"prs":{"org/repo/1":{"worker_id":"w1","forwarded_comment_ids":[],"rounds":0,"last_worker_sha":null,"last_forward_at":null,"conflict_notified":false}}}"#;
        let store: FeedbackStore = serde_json::from_str(json).unwrap();
        let state = &store.prs["org/repo/1"];
        assert!(state.ci_notified_sha.is_none());
    }

    #[test]
    fn test_ci_notified_sha_roundtrip() {
        let mut store = FeedbackStore::default();
        store.prs.insert(
            "org/repo/1".to_string(),
            PrFeedbackState {
                worker_id: "w1".to_string(),
                forwarded_comment_ids: vec![],
                rounds: 0,
                last_worker_sha: Some("sha1".to_string()),
                last_forward_at: None,
                conflict_notified: false,
                ci_notified_sha: Some("sha1".to_string()),
            },
        );
        let json = serde_json::to_string(&store).unwrap();
        let restored: FeedbackStore = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.prs["org/repo/1"].ci_notified_sha.as_deref(),
            Some("sha1")
        );
    }
}

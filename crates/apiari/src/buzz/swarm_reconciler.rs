//! Swarm reconciler — background task that polls `.swarm/state.json` every 5 seconds
//! and applies forward-only state transitions to the v2 worker DB table.
//!
//! # Rules (from spec)
//!
//! | Swarm signal                      | DB state  | Action                                       |
//! |-----------------------------------|-----------|----------------------------------------------|
//! | agent running                     | queued    | → running                                    |
//! | agent waiting (agent-status file) | running   | → waiting                                    |
//! | agent exited 0 (phase=="completed")| running   | → waiting, set branch_ready=true             |
//! | agent exited non-0 (phase failed) | running   | → waiting, set branch_ready=true             |
//! | pr.url appeared                   | any       | set pr_url property                          |
//! | worker vanished from state.json   | active    | check gh pr; → done or → abandoned           |
//! | DB=waiting, swarm=running         | waiting   | → running, increment revision_count          |
//! | last_output_at >10min + running   | —         | → stalled                                    |
//! | new output event                  | stalled   | → running, update last_output_at             |

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::Utc;
use color_eyre::eyre::Result;
use tracing::{debug, info, warn};

use crate::buzz::worker::{Worker, WorkerPropertyUpdate, WorkerState, WorkerStore};

// ── Swarm state JSON types ─────────────────────────────────────────────

/// Minimal deserialization of `.swarm/state.json`.
/// Shape can be an array (old format) or object with a `worktrees` key (new format).
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum SwarmStateJson {
    Array(Vec<SwarmWorktree>),
    Object { worktrees: Vec<SwarmWorktree> },
}

impl SwarmStateJson {
    fn into_worktrees(self) -> Vec<SwarmWorktree> {
        match self {
            SwarmStateJson::Array(v) => v,
            SwarmStateJson::Object { worktrees } => worktrees,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SwarmWorktree {
    id: String,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    agent_kind: Option<String>,
    #[serde(default)]
    pr: Option<SwarmPr>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    worktree_path: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SwarmPr {
    #[serde(default)]
    url: Option<String>,
}

// ── GraphQL batch helpers ──────────────────────────────────────────────

/// Sanitize a worker ID to a valid GraphQL alias (letters, digits, underscores).
fn gql_alias(worker_id: &str) -> String {
    let s = worker_id.replace(['-', '.'], "_");
    format!("wt_{s}")
}

/// Parse `(owner, repo, pr_number)` from a GitHub PR URL.
/// `https://github.com/ApiariTools/apiari/pull/21` → `("ApiariTools", "apiari", 21)`
fn parse_pr_url(url: &str) -> Option<(String, String, u64)> {
    let path = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = path.splitn(4, '/').collect();
    if parts.len() < 4 || parts[2] != "pull" {
        return None;
    }
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[3].parse().ok()?,
    ))
}

/// Fetch the GitHub owner+repo for the workspace root using `gh repo view`.
fn get_repo_nwo(workspace_root: &Path) -> Option<(String, String)> {
    let out = std::process::Command::new("gh")
        .args(["repo", "view", "--json", "owner,name"])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let val: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
    Some((
        val["owner"]["login"].as_str()?.to_string(),
        val["name"].as_str()?.to_string(),
    ))
}

/// Execute a GraphQL query via `gh api graphql -f query=...`.
fn run_graphql(query: &str) -> Option<serde_json::Value> {
    let out = std::process::Command::new("gh")
        .args(["api", "graphql", "-f", &format!("query={query}")])
        .output()
        .ok()?;
    if !out.status.success() {
        warn!(
            "[graphql] gh api graphql failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()
}

// ── Per-worker last-seen event timestamp tracking ──────────────────────

/// Read `.swarm/agents/{worker_id}/report.json` written by the worker to
/// explicitly report properties like `tests_passing` and `branch_ready`.
///
/// Format: `{"tests_passing": true, "branch_ready": true}`
fn read_worker_report(swarm_dir: &Path, worker_id: &str) -> Option<WorkerReport> {
    let path = swarm_dir.join("agents").join(worker_id).join("report.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

#[derive(Debug, serde::Deserialize)]
struct WorkerReport {
    #[serde(default)]
    tests_passing: Option<bool>,
    #[serde(default)]
    branch_ready: Option<bool>,
}

/// Read the most-recent `assistant_text` or `user_message` event timestamp
/// from `.swarm/agents/{worker_id}/events.jsonl`.
fn last_output_timestamp(swarm_dir: &Path, worker_id: &str) -> Option<chrono::DateTime<Utc>> {
    let path = swarm_dir
        .join("agents")
        .join(worker_id)
        .join("events.jsonl");

    let content = std::fs::read_to_string(&path).ok()?;

    // Walk lines from the end to find the latest timestamp quickly.
    let mut latest: Option<chrono::DateTime<Utc>> = None;

    for line in content.lines() {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event_type = val.get("type").or_else(|| val.get("event_type"));
        if let Some(t) = event_type
            && let Some(s) = t.as_str()
            && (s == "assistant_text" || s == "user_message" || s == "tool_use")
        {
            // Try common timestamp keys
            for key in ["timestamp", "created_at", "ts"] {
                if let Some(ts_val) = val.get(key)
                    && let Some(ts_str) = ts_val.as_str()
                    && let Ok(ts) = ts_str.parse::<chrono::DateTime<Utc>>()
                {
                    match latest {
                        None => latest = Some(ts),
                        Some(prev) if ts > prev => latest = Some(ts),
                        _ => {}
                    }
                }
            }
        }
    }

    latest
}

/// Read agent status from `.swarm/agent-status/{worker_id}`.
/// Returns `"waiting"`, `"running"`, or `None`.
fn agent_status(swarm_dir: &Path, worker_id: &str) -> Option<String> {
    let path = swarm_dir.join("agent-status").join(worker_id);
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── Reconciler ─────────────────────────────────────────────────────────

/// Configuration for the swarm reconciler.
pub struct SwarmReconcilerConfig {
    /// Workspace name (used for DB queries).
    pub workspace: String,
    /// Workspace root directory — `.swarm/` is relative to this.
    pub workspace_root: PathBuf,
    /// Broadcast sender for worker_v2_state WebSocket events.
    /// Optional — if not provided, events are not emitted.
    pub event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    /// DB path for opening TaskStore to auto-promote task stages.
    /// Optional — task promotion is skipped when not provided.
    pub db_path: Option<std::path::PathBuf>,
}

/// The actual reconciler logic — separated from the task for testability.
pub struct SwarmReconciler {
    workspace: String,
    swarm_dir: PathBuf,
    store: WorkerStore,
    event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    db_path: Option<std::path::PathBuf>,
    /// Workers queued for PR URL discovery. Value = attempts remaining (max 3).
    /// Populated by apply_rules when a Waiting worker has no pr_url.
    /// Drained by discover_prs_from_queue in the 5-min slow loop.
    pr_discovery_queue: std::sync::Mutex<HashMap<String, u32>>,
}

impl SwarmReconciler {
    pub fn new(
        config: SwarmReconcilerConfig,
        conn: Arc<Mutex<rusqlite::Connection>>,
    ) -> Result<Self> {
        let swarm_dir = config.workspace_root.join(".swarm");
        let store = WorkerStore::new(conn)?;
        Ok(Self {
            workspace: config.workspace,
            swarm_dir,
            store,
            event_tx: config.event_tx,
            db_path: config.db_path,
            pr_discovery_queue: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// On daemon startup, reset any Stalled workers whose swarm agent is still
    /// running back to Running. Stall is a continuous-observation state — after
    /// a restart we have no baseline, so assume alive and let stall detection
    /// re-fire naturally if the agent really is silent.
    pub fn reset_stalled_on_startup(&self) -> Result<()> {
        let worktrees = self.load_swarm_state();
        let workers = self.store.list(&self.workspace)?;
        let swarm_map: HashMap<String, &SwarmWorktree> =
            worktrees.iter().map(|wt| (wt.id.clone(), wt)).collect();

        for worker in &workers {
            if worker.state != WorkerState::Stalled {
                continue;
            }
            if let Some(wt) = swarm_map.get(&worker.id) {
                let phase = wt.phase.as_deref().unwrap_or("");
                if phase == "running" {
                    info!(
                        "[reconciler] startup: resetting {} stalled→running (agent alive)",
                        worker.id
                    );
                    self.do_transition(worker, WorkerState::Running)?;
                }
            }
        }
        Ok(())
    }

    /// Run one reconciliation cycle.
    pub fn reconcile_once(&self) -> Result<()> {
        let worktrees = self.load_swarm_state();
        let workers = self.store.list(&self.workspace)?;

        // Map existing workers by id for quick lookup.
        let worker_map: HashMap<String, &Worker> =
            workers.iter().map(|w| (w.id.clone(), w)).collect();

        // Map swarm worktrees by id.
        let swarm_map: HashMap<String, &SwarmWorktree> =
            worktrees.iter().map(|wt| (wt.id.clone(), wt)).collect();

        // Process each DB worker we know about.
        for worker in &workers {
            if let Some(swarm_wt) = swarm_map.get(&worker.id) {
                self.apply_rules(worker, swarm_wt)?;
            } else {
                // Worker is in DB but not in swarm — it disappeared (closed/merged).
                debug!("[reconciler] worker {} not in swarm state", worker.id);
                self.apply_disappeared(worker)?;
            }
        }

        // Check for swarm workers that exist but have no DB record (no-op in v2 —
        // workers are created through the API, not auto-imported from swarm).
        let _ = swarm_map;
        let _ = worker_map;

        Ok(())
    }

    /// Handle a worker that was in the DB but has disappeared from swarm state.json.
    ///
    /// Swarm removes worktrees on close — there is no "merged" phase. We detect
    /// merges by querying `gh pr view` and fall back to "abandoned" for workers
    /// without a merged PR (after a brief grace period to avoid false positives).
    fn apply_disappeared(&self, worker: &Worker) -> Result<()> {
        // Only act on non-terminal active states.
        match worker.state {
            WorkerState::Running
            | WorkerState::Waiting
            | WorkerState::Stalled
            | WorkerState::Queued => {}
            _ => return Ok(()), // already terminal, nothing to do
        }

        // Worker disappeared — check its PR state.
        if let Some(pr_url) = &worker.pr_url {
            let output = std::process::Command::new("gh")
                .args(["pr", "view", pr_url, "--json", "state", "--jq", ".state"])
                .output();
            if let Ok(out) = output {
                let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if state == "MERGED" {
                    info!("[reconciler] {} PR merged → done", worker.id);
                    self.do_transition(worker, WorkerState::Done)?;
                    return Ok(());
                }
                if state == "OPEN" {
                    // PR still open — worker just lost its worktree (daemon restart etc).
                    // Leave it alone; user can still see and interact with the PR.
                    return Ok(());
                }
            }
        }

        // No PR, or PR is closed/unknown — worker disappeared, treat as abandoned after grace period.
        if let Ok(entered) = worker
            .state_entered_at
            .parse::<chrono::DateTime<chrono::Utc>>()
        {
            let age_minutes = (chrono::Utc::now() - entered).num_minutes();
            if age_minutes >= 1 {
                info!(
                    "[reconciler] {} disappeared without merged PR → abandoned",
                    worker.id
                );
                self.do_transition(worker, WorkerState::Abandoned)?;
            }
        }

        Ok(())
    }

    /// Apply forward-only reconciliation rules for one worker.
    fn apply_rules(&self, worker: &Worker, swarm_wt: &SwarmWorktree) -> Result<()> {
        let phase = swarm_wt.phase.as_deref().unwrap_or("");
        let agent_status = agent_status(&self.swarm_dir, &worker.id);

        // 1a. branch appeared — sync to DB whenever state.json has it and DB doesn't
        if let Some(ref branch) = swarm_wt.branch
            && worker.branch.as_deref() != Some(branch.as_str())
        {
            self.store.update_properties(
                &self.workspace,
                &worker.id,
                WorkerPropertyUpdate {
                    branch: Some(branch.clone()),
                    ..Default::default()
                },
            )?;
        }

        // 1b. pr_url appeared — update property on any state
        let swarm_pr_url = swarm_wt.pr.as_ref().and_then(|p| p.url.as_deref());
        if let Some(url) = swarm_pr_url
            && worker.pr_url.as_deref() != Some(url)
        {
            info!("[reconciler] {} pr_url appeared: {}", worker.id, url);
            self.store.update_properties(
                &self.workspace,
                &worker.id,
                WorkerPropertyUpdate {
                    pr_url: Some(Some(url.to_string())),
                    ..Default::default()
                },
            )?;
            self.emit_event(worker)?;
        }

        // 2. State-specific transitions
        match worker.state {
            WorkerState::Queued => {
                if phase == "running" {
                    self.do_transition(worker, WorkerState::Running)?;
                }
            }
            WorkerState::Running => {
                // Agent paused waiting for input — detected via agent-status file
                if agent_status.as_deref() == Some("waiting") {
                    info!("[reconciler] {} agent waiting (agent-status)", worker.id);
                    self.do_transition(worker, WorkerState::Waiting)?;
                    return Ok(());
                }

                // Agent paused waiting for input — detected via swarm phase.
                // Handles the case where the swarm daemon wrote phase="waiting" but
                // the agent-status file is absent (e.g. daemon died before cleaning up).
                if phase == "waiting" {
                    info!("[reconciler] {} agent waiting (swarm phase)", worker.id);
                    self.do_transition(worker, WorkerState::Waiting)?;
                    return Ok(());
                }

                // Agent exited cleanly — move to waiting for review
                if phase == "completed" || phase == "failed" {
                    info!(
                        "[reconciler] {} agent exited (phase={phase}) → waiting",
                        worker.id
                    );
                    self.store.update_properties(
                        &self.workspace,
                        &worker.id,
                        WorkerPropertyUpdate {
                            branch_ready: Some(true),
                            branch: swarm_wt.branch.clone(),
                            ..Default::default()
                        },
                    )?;
                    self.do_transition(worker, WorkerState::Waiting)?;
                    return Ok(());
                }

                // Still running — check for stall
                self.check_stall_and_output(worker)?;
            }
            WorkerState::Stalled => {
                // Stalled → running if output resumes
                self.check_stall_and_output(worker)?;

                // Agent paused waiting for input while stalled
                if agent_status.as_deref() == Some("waiting") || phase == "waiting" {
                    info!("[reconciler] {} stalled agent now waiting", worker.id);
                    self.do_transition(worker, WorkerState::Waiting)?;
                    return Ok(());
                }

                // Agent exited from stalled — move to waiting
                if phase == "completed" || phase == "failed" {
                    info!("[reconciler] {} stalled agent exited → waiting", worker.id);
                    self.do_transition(worker, WorkerState::Waiting)?;
                    return Ok(());
                }
            }
            WorkerState::Waiting => {
                // Queue for PR discovery if we don't have a URL yet.
                if worker.pr_url.is_none() {
                    self.queue_pr_discovery(&worker.id);
                }

                // Resumed — human sent a message, agent is running again
                if phase == "running" && agent_status.as_deref() != Some("waiting") {
                    info!(
                        "[reconciler] {} resumed from waiting (revision {})",
                        worker.id,
                        worker.revision_count + 1
                    );
                    self.store.update_properties(
                        &self.workspace,
                        &worker.id,
                        WorkerPropertyUpdate {
                            increment_revision: true,
                            ..Default::default()
                        },
                    )?;
                    self.do_transition(worker, WorkerState::Running)?;
                }
            }
            // Terminal states — no forward transitions
            WorkerState::Done | WorkerState::Abandoned => {}
            // Pre-dispatch states — swarm hasn't started yet
            WorkerState::Created | WorkerState::Briefed => {}
        }

        // Apply any explicit property overrides from the worker's report.json
        self.apply_report(worker)?;

        Ok(())
    }

    /// Read `.swarm/agents/{id}/report.json` and apply reported properties to DB
    /// if they differ from current state.
    fn apply_report(&self, worker: &Worker) -> Result<()> {
        let Some(report) = read_worker_report(&self.swarm_dir, &worker.id) else {
            return Ok(());
        };

        let tests_changed = report
            .tests_passing
            .is_some_and(|v| v != worker.tests_passing);
        let branch_changed = report
            .branch_ready
            .is_some_and(|v| v != worker.branch_ready);

        if tests_changed || branch_changed {
            info!(
                "[reconciler] {} applying report: tests_passing={:?} branch_ready={:?}",
                worker.id, report.tests_passing, report.branch_ready
            );
            self.store.update_properties(
                &self.workspace,
                &worker.id,
                WorkerPropertyUpdate {
                    tests_passing: report.tests_passing,
                    branch_ready: if branch_changed {
                        report.branch_ready
                    } else {
                        None
                    },
                    ..Default::default()
                },
            )?;
            self.emit_event(worker)?;
        }

        Ok(())
    }

    /// Stall detection and last_output_at update for running/stalled workers.
    fn check_stall_and_output(&self, worker: &Worker) -> Result<()> {
        let latest = last_output_timestamp(&self.swarm_dir, &worker.id);

        if let Some(ts) = latest {
            let ts_str = ts.to_rfc3339();

            // New output arrived — update timestamp, un-stall if needed
            let is_new = worker
                .last_output_at
                .as_deref()
                .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
                .is_none_or(|prev| ts > prev);

            if is_new {
                self.store.update_properties(
                    &self.workspace,
                    &worker.id,
                    WorkerPropertyUpdate {
                        last_output_at: Some(ts_str),
                        ..Default::default()
                    },
                )?;
                if worker.state == WorkerState::Stalled {
                    info!("[reconciler] {} output resumed → running", worker.id);
                    self.do_transition(worker, WorkerState::Running)?;
                } else {
                    self.emit_event(worker)?;
                }
                return Ok(());
            }

            // No new output — check if stall threshold exceeded (10 min)
            let age_minutes = (Utc::now() - ts).num_minutes();
            if age_minutes >= 10 && worker.state == WorkerState::Running {
                info!(
                    "[reconciler] {} stalled (no output for {}min)",
                    worker.id, age_minutes
                );
                self.do_transition(worker, WorkerState::Stalled)?;
            }
        } else if let Ok(entered) = worker.state_entered_at.parse::<chrono::DateTime<Utc>>() {
            // No events file — use state_entered_at as proxy
            let age_minutes = (Utc::now() - entered).num_minutes();
            if age_minutes >= 10 && worker.state == WorkerState::Running {
                info!(
                    "[reconciler] {} stalled (no events, {}min in running)",
                    worker.id, age_minutes
                );
                self.do_transition(worker, WorkerState::Stalled)?;
            }
        }

        Ok(())
    }

    fn do_transition(&self, worker: &Worker, new_state: WorkerState) -> Result<()> {
        info!(
            "[reconciler] {} {:?} → {:?}",
            worker.id, worker.state, new_state
        );
        self.store
            .transition(&self.workspace, &worker.id, new_state.clone())?;
        // Re-fetch to get fresh label for the event
        if let Some(updated) = self.store.get(&self.workspace, &worker.id)? {
            self.emit_event(&updated)?;
        }
        // Auto-promote associated task stage based on the new worker state.
        self.advance_task_for_worker(&worker.id, new_state);
        Ok(())
    }

    /// Advance the task stage for any task linked to this worker.
    ///
    /// - Worker → Running:  Triage → InProgress (work has started)
    /// - Worker → Done:     InProgress → InAiReview (PR is open, needs review)
    /// - Worker → Waiting:  no stage change (interim state)
    fn advance_task_for_worker(&self, worker_id: &str, new_state: WorkerState) {
        use crate::buzz::task::{TaskStage, store::TaskStore};

        let target_from = match new_state {
            WorkerState::Running => TaskStage::Triage,
            WorkerState::Done => TaskStage::InProgress,
            _ => return,
        };
        let target_to = match new_state {
            WorkerState::Running => TaskStage::InProgress,
            WorkerState::Done => TaskStage::InAiReview,
            _ => return,
        };

        let Some(ref db_path) = self.db_path else {
            return;
        };

        let task_store = match TaskStore::open(db_path) {
            Ok(s) => s,
            Err(e) => {
                warn!("[reconciler] could not open task store for promotion: {e}");
                return;
            }
        };

        let task = match task_store.find_task_by_worker(&self.workspace, worker_id) {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                warn!("[reconciler] find_task_by_worker error for {worker_id}: {e}");
                return;
            }
        };

        if task.stage != target_from {
            return;
        }

        match task_store.update_task_stage(&task.id, &target_to) {
            Ok(()) => info!(
                "[reconciler] task {} promoted {:?} → {:?} (worker={worker_id})",
                task.id, target_from, target_to
            ),
            Err(e) => warn!(
                "[reconciler] failed to promote task {} for worker {worker_id}: {e}",
                task.id
            ),
        }
    }

    /// Check PR approval state for tasks in `InAiReview` and advance to `HumanReview`
    /// when the PR is approved.
    ///
    /// Uses a single batched GraphQL query (one alias per PR) instead of N separate
    /// `gh pr view` calls. Tasks are grouped by owner/repo so multi-repo workspaces
    /// each get one query.
    ///
    /// Called on a slow background interval (every 5 minutes), NOT in the hot
    /// 5-second reconciler loop.
    pub fn poll_pr_approvals(&self) {
        use crate::buzz::task::{TaskStage, store::TaskStore};

        let Some(ref db_path) = self.db_path else {
            return;
        };

        let task_store = match TaskStore::open(db_path) {
            Ok(s) => s,
            Err(e) => {
                warn!("[reconciler/pr-poll] could not open task store: {e}");
                return;
            }
        };

        let tasks = match task_store.get_tasks_by_stage(&self.workspace, &TaskStage::InAiReview) {
            Ok(t) => t,
            Err(e) => {
                warn!("[reconciler/pr-poll] get_tasks_by_stage error: {e}");
                return;
            }
        };
        if tasks.is_empty() {
            return;
        }

        // Parse PR info and group tasks by (owner, repo).
        let mut by_repo: HashMap<(String, String), Vec<(String, u64)>> = HashMap::new(); // (owner,repo) → [(task_id, pr_number)]
        for task in &tasks {
            let Some(ref url) = task.pr_url else { continue };
            if url.is_empty() {
                continue;
            }
            let Some((owner, repo, number)) = parse_pr_url(url) else {
                continue;
            };
            by_repo
                .entry((owner, repo))
                .or_default()
                .push((task.id.clone(), number));
        }

        for ((owner, repo), task_prs) in &by_repo {
            // One alias per PR: pr_{number}: pullRequest(number: N) { state reviewDecision }
            let aliases: String = task_prs
                .iter()
                .map(|(_, number)| {
                    format!(
                        r#"pr_{number}: pullRequest(number: {number}) {{ state reviewDecision }}"#
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");

            let query =
                format!(r#"{{ repository(owner: "{owner}", name: "{repo}") {{ {aliases} }} }}"#);

            let resp = match run_graphql(&query) {
                Some(v) => v,
                None => {
                    warn!("[reconciler/pr-poll] GraphQL call failed for {owner}/{repo} — skipping");
                    continue;
                }
            };

            for (task_id, number) in task_prs {
                let pr_data = &resp["data"]["repository"][format!("pr_{number}")];
                let state = pr_data["state"].as_str().unwrap_or("");
                let review_decision = pr_data["reviewDecision"].as_str().unwrap_or("");

                if review_decision == "APPROVED" && state == "OPEN" {
                    info!("[reconciler/pr-poll] task {task_id} PR approved → HumanReview");
                    if let Err(e) = task_store.update_task_stage(task_id, &TaskStage::HumanReview) {
                        warn!("[reconciler/pr-poll] task {task_id} → HumanReview failed: {e}");
                    }
                } else if state == "MERGED" {
                    // Belt-and-suspenders: apply_disappeared handles this when the worker
                    // closes, but catches the rare case where the PR was merged outside
                    // swarm (e.g. merged from GitHub UI after the worktree was already gone).
                    info!("[reconciler/pr-poll] task {task_id} PR merged → Merged");
                    if let Err(e) = task_store.update_task_stage(task_id, &TaskStage::Merged) {
                        warn!("[reconciler/pr-poll] task {task_id} → Merged failed: {e}");
                    }
                }
            }
        }
    }

    /// Add a worker to the PR discovery queue if not already present.
    /// Idempotent — existing entries keep their current attempt count.
    fn queue_pr_discovery(&self, worker_id: &str) {
        self.pr_discovery_queue
            .lock()
            .unwrap()
            .entry(worker_id.to_string())
            .or_insert(3);
    }

    /// Drain the PR discovery queue in a single batched GraphQL query.
    ///
    /// Looks up each queued worker's branch from SQLite, then fires one
    /// `gh api graphql` call with all branches as aliased `pullRequests` fields.
    /// Found URLs are written to DB immediately. Missed workers have their attempt
    /// counter decremented; they are dropped after 3 misses.
    ///
    /// If the GraphQL call itself fails (network/auth), all attempts are preserved
    /// and we retry next cycle — don't penalise workers for infrastructure hiccups.
    pub fn discover_prs_from_queue(&self) {
        let snapshot: Vec<(String, u32)> = {
            let q = self.pr_discovery_queue.lock().unwrap();
            q.iter().map(|(k, v)| (k.clone(), *v)).collect()
        };
        if snapshot.is_empty() {
            return;
        }

        // Resolve branch for each queued worker from SQLite.
        let jobs: Vec<(String, String, u32)> = snapshot
            .into_iter()
            .filter_map(|(worker_id, attempts)| {
                let branch = self
                    .store
                    .get(&self.workspace, &worker_id)
                    .ok()
                    .flatten()
                    .and_then(|w| w.branch)?;
                Some((worker_id, branch, attempts))
            })
            .collect();

        if jobs.is_empty() {
            return;
        }

        let workspace_root = self.swarm_dir.parent().unwrap_or(self.swarm_dir.as_path());

        let Some((owner, repo)) = get_repo_nwo(workspace_root) else {
            return;
        };

        // One alias per worker: wt_{id}: pullRequests(headRefName: "...") { nodes { url } }
        let aliases: String = jobs
            .iter()
            .map(|(id, branch, _)| {
                let alias = gql_alias(id);
                let branch = branch.replace('"', "");
                format!(
                    r#"{alias}: pullRequests(headRefName: "{branch}", first: 1, states: [OPEN, MERGED]) {{ nodes {{ url }} }}"#
                )
            })
            .collect::<Vec<_>>()
            .join(" ");

        let query =
            format!(r#"{{ repository(owner: "{owner}", name: "{repo}") {{ {aliases} }} }}"#);

        let resp = run_graphql(&query);

        if resp.is_none() {
            // Whole call failed — preserve all attempts and retry next cycle.
            warn!("[reconciler/pr-discovery] GraphQL call failed — attempts preserved");
            return;
        }

        let repo_data = resp
            .as_ref()
            .and_then(|v| v["data"]["repository"].as_object());

        for (worker_id, _branch, attempts) in &jobs {
            let alias = gql_alias(worker_id);
            let found_url = repo_data
                .and_then(|d| d.get(&alias))
                .and_then(|v| v["nodes"][0]["url"].as_str())
                .map(|s| s.to_string());

            match found_url {
                Some(url) => {
                    info!("[reconciler/pr-discovery] {worker_id} found PR {url} — writing to DB");
                    let _ = self.store.update_properties(
                        &self.workspace,
                        worker_id,
                        WorkerPropertyUpdate {
                            pr_url: Some(Some(url)),
                            ..Default::default()
                        },
                    );
                    if let Ok(Some(w)) = self.store.get(&self.workspace, worker_id) {
                        let _ = self.emit_event(&w);
                    }
                    self.pr_discovery_queue.lock().unwrap().remove(worker_id);
                }
                None => {
                    let mut q = self.pr_discovery_queue.lock().unwrap();
                    if *attempts <= 1 {
                        info!("[reconciler/pr-discovery] {worker_id} gave up after 3 attempts");
                        q.remove(worker_id);
                    } else {
                        q.insert(worker_id.clone(), attempts - 1);
                        debug!(
                            "[reconciler/pr-discovery] {worker_id} no PR yet, {} attempts left",
                            attempts - 1
                        );
                    }
                }
            }
        }
    }

    fn emit_event(&self, worker: &Worker) -> Result<()> {
        let Some(ref tx) = self.event_tx else {
            return Ok(());
        };
        // Fetch fresh state in case we just mutated it
        let fresh = match self.store.get(&self.workspace, &worker.id)? {
            Some(w) => w,
            None => return Ok(()),
        };
        let event = serde_json::json!({
            "type": "worker_v2_state",
            "workspace": fresh.workspace,
            "worker_id": fresh.id,
            "state": fresh.state.as_str(),
            "label": fresh.label,
            "properties": {
                "tests_passing": fresh.tests_passing,
                "branch_ready": fresh.branch_ready,
                "pr_url": fresh.pr_url,
                "pr_approved": fresh.pr_approved,
                "is_stalled": fresh.is_stalled,
                "revision_count": fresh.revision_count,
                "blocked_reason": fresh.blocked_reason,
                "last_output_at": fresh.last_output_at,
            }
        });
        let _ = tx.send(event); // ignore if no subscribers
        Ok(())
    }

    fn load_swarm_state(&self) -> Vec<SwarmWorktree> {
        let path = self.swarm_dir.join("state.json");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        match serde_json::from_str::<SwarmStateJson>(&content) {
            Ok(s) => s.into_worktrees(),
            Err(e) => {
                warn!("[reconciler] failed to parse state.json: {e}");
                Vec::new()
            }
        }
    }
}

// ── Background task ────────────────────────────────────────────────────

/// Spawn the reconciler as a background tokio task.
///
/// Returns immediately; the task runs until the process exits.
pub fn spawn_reconciler(config: SwarmReconcilerConfig, conn: Arc<Mutex<rusqlite::Connection>>) {
    tokio::spawn(async move {
        let reconciler = match SwarmReconciler::new(config, conn) {
            Ok(r) => std::sync::Arc::new(r),
            Err(e) => {
                warn!("[reconciler] failed to initialize: {e}");
                return;
            }
        };

        if let Err(e) = reconciler.reset_stalled_on_startup() {
            warn!("[reconciler] startup reset error: {e}");
        }

        // Fast loop: file reads only (swarm state.json, agent-status files).
        // No external subprocesses — must stay cheap.
        let mut fast_interval = tokio::time::interval(std::time::Duration::from_secs(5));
        fast_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Slow loop: PR approval polling via `gh pr view`.
        // Runs every 5 minutes. Only fires if there are tasks in InAiReview.
        let mut pr_interval = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
        pr_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = fast_interval.tick() => {
                    if let Err(e) = reconciler.reconcile_once() {
                        warn!("[reconciler] error: {e}");
                    }
                }
                _ = pr_interval.tick() => {
                    let r = std::sync::Arc::clone(&reconciler);
                    // Spawn a blocking task so `gh` subprocess calls don't block
                    // the async runtime while waiting on GitHub's API.
                    tokio::task::spawn_blocking(move || {
                        r.discover_prs_from_queue();
                        r.poll_pr_approvals();
                    });
                }
            }
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::worker::{Worker, WorkerState};
    use chrono::Utc;

    fn make_reconciler(tmp: &tempfile::TempDir) -> SwarmReconciler {
        let workspace_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace_root.join(".swarm")).unwrap();

        // Create a shared in-memory connection
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        let conn = Arc::new(Mutex::new(conn));

        let store = WorkerStore::new(Arc::clone(&conn)).unwrap();

        SwarmReconciler {
            workspace: "test".to_string(),
            swarm_dir: workspace_root.join(".swarm"),
            store,
            event_tx: None,
            db_path: None,
            pr_discovery_queue: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn default_worker(id: &str) -> Worker {
        let now = Utc::now().to_rfc3339();
        Worker {
            id: id.to_string(),
            workspace: "test".to_string(),
            state: WorkerState::Queued,
            brief: None,
            repo: None,
            branch: None,
            goal: None,
            tests_passing: false,
            branch_ready: false,
            pr_url: None,
            pr_approved: false,
            is_stalled: false,
            revision_count: 0,
            review_mode: "local_first".to_string(),
            blocked_reason: None,
            last_output_at: None,
            state_entered_at: now.clone(),
            created_at: now.clone(),
            updated_at: now,
            display_title: None,
            label: "Queued".to_string(),
            worktree_path: None,
            isolation_mode: None,
            agent_kind: None,
            repo_path: None,
        }
    }

    fn swarm_wt(id: &str, phase: &str, pr_url: Option<&str>) -> SwarmWorktree {
        SwarmWorktree {
            id: id.to_string(),
            phase: Some(phase.to_string()),
            agent_kind: Some("claude".to_string()),
            pr: pr_url.map(|url| SwarmPr {
                url: Some(url.to_string()),
            }),
            branch: None,
            worktree_path: None,
        }
    }

    #[test]
    fn rule_queued_running_to_running() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let w = default_worker("w1");
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "running", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Running);
    }

    #[test]
    fn rule_running_swarm_phase_waiting_transitions_to_waiting() {
        // swarm phase="waiting" with no agent-status file should still move to Waiting
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "waiting", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
    }

    #[test]
    fn rule_running_agent_waiting() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        // Write agent-status file
        let status_dir = tmp.path().join(".swarm").join("agent-status");
        std::fs::create_dir_all(&status_dir).unwrap();
        std::fs::write(status_dir.join("w1"), "waiting").unwrap();

        let wt = swarm_wt("w1", "running", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
    }

    #[test]
    fn rule_running_complete_sets_branch_ready_and_waiting() {
        // Legacy "complete" string — no longer matches; state should stay running.
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "complete", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        // "complete" (missing 'd') no longer triggers the rule
        assert_eq!(updated.state, WorkerState::Running);
        assert!(!updated.branch_ready);
    }

    #[test]
    fn rule_completed_phase_sets_branch_ready_and_waiting() {
        // "completed" is the actual string swarm serializes WorkerPhase::Completed as.
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "completed", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
        assert!(updated.branch_ready);
    }

    #[test]
    fn rule_running_failed_phase_moves_to_waiting() {
        // swarm phase="failed" means the agent exited — worker moves to Waiting (branch_ready=true)
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "failed", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
        assert!(updated.branch_ready);
    }

    #[test]
    fn rule_pr_url_property_set() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "running", Some("https://github.com/org/repo/pull/7"));
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(
            updated.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/7")
        );
    }

    #[test]
    fn rule_waiting_swarm_running_increments_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        w.revision_count = 0;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "running", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Running);
        assert_eq!(updated.revision_count, 1);
    }

    #[test]
    fn rule_no_backward_transition() {
        // Running should not go back to Queued
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        // Swarm phase is "queued" (shouldn't happen in practice, but guard anyway)
        let wt = swarm_wt("w1", "queued", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        // state should remain Running (no rule matches)
        assert_eq!(updated.state, WorkerState::Running);
    }

    #[test]
    fn load_swarm_state_array_format() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let json = r#"[{"id":"w1","phase":"running"}]"#;
        std::fs::write(tmp.path().join(".swarm/state.json"), json).unwrap();
        let wts = r.load_swarm_state();
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].id, "w1");
    }

    #[test]
    fn load_swarm_state_object_format() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let json = r#"{"session_name":"x","worktrees":[{"id":"w2","phase":"waiting"}]}"#;
        std::fs::write(tmp.path().join(".swarm/state.json"), json).unwrap();
        let wts = r.load_swarm_state();
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].id, "w2");
    }

    #[test]
    fn load_swarm_state_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let wts = r.load_swarm_state();
        assert!(wts.is_empty());
    }

    // ── apply_disappeared tests ─────────────────────────────────────────

    #[test]
    fn apply_disappeared_no_pr_abandoned_after_grace_period() {
        // Worker in waiting state with state_entered_at >1min ago, not in swarm → abandoned
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w-gone");
        w.state = WorkerState::Waiting;
        // Set state_entered_at to 2 minutes ago
        w.state_entered_at = (chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339();
        r.store.upsert(&w).unwrap();

        r.apply_disappeared(&w).unwrap();

        let updated = r.store.get("test", "w-gone").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Abandoned);
    }

    #[test]
    fn apply_disappeared_terminal_states_skipped() {
        // Workers in terminal states should not be touched
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        for terminal_state in [WorkerState::Done, WorkerState::Abandoned] {
            let mut w = default_worker("w-terminal");
            w.state = terminal_state.clone();
            w.state_entered_at = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
            r.store.upsert(&w).unwrap();

            r.apply_disappeared(&w).unwrap();

            let updated = r.store.get("test", "w-terminal").unwrap().unwrap();
            // State must not have changed
            assert_eq!(updated.state, terminal_state);
        }
    }

    // ── PR discovery queue tests ────────────────────────────────────────

    #[test]
    fn waiting_worker_with_no_pr_url_queued_for_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        w.pr_url = None;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "waiting", None);
        r.apply_rules(&w, &wt).unwrap();

        let queue = r.pr_discovery_queue.lock().unwrap();
        assert!(queue.contains_key("w1"), "should be queued for discovery");
        assert_eq!(queue["w1"], 3);
    }

    #[test]
    fn waiting_worker_with_pr_url_not_queued() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/org/repo/pull/1".to_string());
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "waiting", None);
        r.apply_rules(&w, &wt).unwrap();

        let queue = r.pr_discovery_queue.lock().unwrap();
        assert!(
            !queue.contains_key("w1"),
            "should not queue when pr_url already set"
        );
    }

    #[test]
    fn queue_pr_discovery_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        r.queue_pr_discovery("w1");
        r.queue_pr_discovery("w1"); // second call should not reset attempts
        r.queue_pr_discovery("w1");

        let queue = r.pr_discovery_queue.lock().unwrap();
        assert_eq!(
            queue["w1"], 3,
            "or_insert should not overwrite existing entry"
        );
    }

    #[test]
    fn discover_prs_from_queue_skips_workers_without_branch() {
        // Workers with no branch in DB are skipped — queue entry stays untouched
        // because get_repo_nwo will also fail (no git repo in tmp), so the whole
        // batch returns early before touching attempts.
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        // Worker exists in DB but has no branch
        let w = default_worker("w-no-branch");
        r.store.upsert(&w).unwrap();
        r.pr_discovery_queue
            .lock()
            .unwrap()
            .insert("w-no-branch".to_string(), 3);

        r.discover_prs_from_queue();

        // No branch → no jobs → early return → attempt count unchanged
        let queue = r.pr_discovery_queue.lock().unwrap();
        assert_eq!(queue.get("w-no-branch"), Some(&3));
    }

    #[test]
    fn parse_pr_url_extracts_owner_repo_number() {
        let (owner, repo, number) =
            parse_pr_url("https://github.com/ApiariTools/apiari/pull/21").unwrap();
        assert_eq!(owner, "ApiariTools");
        assert_eq!(repo, "apiari");
        assert_eq!(number, 21);
    }

    #[test]
    fn parse_pr_url_rejects_non_pr_url() {
        assert!(parse_pr_url("https://github.com/ApiariTools/apiari").is_none());
        assert!(parse_pr_url("https://github.com/ApiariTools/apiari/issues/5").is_none());
        assert!(parse_pr_url("not-a-url").is_none());
    }

    #[test]
    fn gql_alias_sanitizes_hyphens() {
        assert_eq!(gql_alias("apiari-e6bf"), "wt_apiari_e6bf");
        assert_eq!(gql_alias("worker.1"), "wt_worker_1");
    }

    #[test]
    fn apply_disappeared_fresh_worker_not_abandoned() {
        // Worker in waiting state with state_entered_at <1min ago → NOT abandoned (grace period)
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w-fresh");
        w.state = WorkerState::Waiting;
        // Only 10 seconds ago — within grace period
        w.state_entered_at = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        r.store.upsert(&w).unwrap();

        r.apply_disappeared(&w).unwrap();

        let updated = r.store.get("test", "w-fresh").unwrap().unwrap();
        // Still waiting — grace period not exceeded
        assert_eq!(updated.state, WorkerState::Waiting);
    }
}

//! Swarm reconciler — background task that drives forward-only state transitions
//! on v2 workers. Runs every 5 seconds (fast loop) and every 5 minutes (slow loop).
//!
//! Three independent passes per cycle:
//!
//! 1. `apply_rules` — for workers still present in state.json:
//!    drives running/waiting/stalled transitions based on swarm phase.
//!
//! 2. `apply_disappeared` — for workers no longer in state.json:
//!    marks them abandoned after a grace period (if no open PR).
//!
//! 3. `check_pr_status` — for ANY active worker with a pr_url:
//!    polls GitHub once per 60s; transitions to Done on merge,
//!    and keeps `ci_passing` current from the PR's statusCheckRollup.

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

#[derive(Debug, serde::Deserialize)]
struct SwarmStateJson {
    #[serde(default)]
    worktrees: Vec<SwarmWorktree>,
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

// ── GraphQL helpers ────────────────────────────────────────────────────

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

// ── Agent file readers ─────────────────────────────────────────────────

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

/// Count how many `assistant_text` events exist for a worker.
/// Used to decide when to refresh the display title.
fn count_assistant_text_events(swarm_dir: &Path, worker_id: &str) -> usize {
    let path = swarm_dir
        .join("agents")
        .join(worker_id)
        .join("events.jsonl");
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|line| line.contains("\"assistant_text\""))
        .count()
}

/// Collect the last ~800 chars of assistant_text content for title context.
fn recent_output_snippet(swarm_dir: &Path, worker_id: &str) -> Option<String> {
    let path = swarm_dir
        .join("agents")
        .join(worker_id)
        .join("events.jsonl");
    let content = std::fs::read_to_string(path).ok()?;

    let mut buf = String::new();
    for line in content.lines() {
        let val: serde_json::Value = serde_json::from_str(line).ok()?;
        if val.get("type").and_then(|t| t.as_str()) == Some("assistant_text")
            && let Some(text) = val.get("text").and_then(|t| t.as_str())
        {
            buf.push_str(text);
            buf.push('\n');
        }
    }

    if buf.is_empty() {
        return None;
    }
    // Keep only the tail
    let snippet = if buf.len() > 800 {
        buf[buf.len() - 800..].to_string()
    } else {
        buf
    };
    Some(snippet)
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
    /// Channel for emitting signals that should be routed through the orchestrator.
    /// Tuple is (workspace_name, signal_update).
    pub signal_tx:
        Option<tokio::sync::mpsc::UnboundedSender<(String, crate::buzz::signal::SignalUpdate)>>,
}

/// The actual reconciler logic — separated from the task for testability.
pub struct SwarmReconciler {
    workspace: String,
    swarm_dir: PathBuf,
    store: WorkerStore,
    event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    db_path: Option<std::path::PathBuf>,
    /// Channel for emitting signals to the orchestrator (e.g. swarm_branch_ready).
    signal_tx:
        Option<tokio::sync::mpsc::UnboundedSender<(String, crate::buzz::signal::SignalUpdate)>>,
    /// Last time we checked each waiting worker's PR for merge — throttled to
    /// at most once per 60 seconds to avoid hammering the GitHub API.
    pr_merge_checked: std::sync::Mutex<HashMap<String, std::time::Instant>>,
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
            signal_tx: config.signal_tx,
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
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

    /// On daemon startup, re-emit branch_ready signals for workers that are already
    /// in Waiting state with branch_ready=true. These workers may have transitioned
    /// while the daemon was down, so no signal was ever stored (or the signal was stored
    /// before the task had a linked review_mode). Deleting the stale signal and
    /// re-emitting ensures the orchestrator sees it as new and can create the PR.
    pub fn requeue_branch_ready_on_startup(&self) -> Result<()> {
        let workers = self.store.list(&self.workspace)?;
        for worker in &workers {
            if worker.state != WorkerState::Waiting || !worker.branch_ready {
                continue;
            }
            let branch = worker.branch.clone().unwrap_or_default();
            if branch.is_empty() {
                continue;
            }
            // Skip if the linked task already has a PR — the orchestrator already ran.
            if let Some(ref db_path) = self.db_path
                && let Ok(ts) = crate::buzz::task::store::TaskStore::open(db_path)
                && let Ok(Some(task)) = ts.find_task_by_worker(&self.workspace, &worker.id)
                && (task.pr_url.is_some()
                    || task.stage == crate::buzz::task::TaskStage::HumanReview
                    || task.stage == crate::buzz::task::TaskStage::Merged
                    || task.stage == crate::buzz::task::TaskStage::Dismissed)
            {
                info!(
                    "[reconciler] startup: skipping branch_ready re-queue for {} — PR already created",
                    worker.id
                );
                continue;
            }
            info!(
                "[reconciler] startup: re-queuing branch_ready signal for {}",
                worker.id
            );
            // Delete any stale signal so the next upsert treats it as new.
            if let Some(ref db_path) = self.db_path
                && let Ok(store) =
                    crate::buzz::signal::store::SignalStore::open(db_path, &self.workspace)
            {
                let _ = store.delete_signal(
                    "swarm_branch_ready",
                    &format!("swarm-branch-ready-{}", worker.id),
                );
            }
            self.emit_branch_ready_signal(&worker.id, &branch);
        }
        Ok(())
    }

    /// Run one reconciliation cycle.
    pub fn reconcile_once(&self) -> Result<()> {
        let worktrees = self.load_swarm_state();
        let workers = self.store.list(&self.workspace)?;

        let swarm_map: HashMap<String, &SwarmWorktree> =
            worktrees.iter().map(|wt| (wt.id.clone(), wt)).collect();

        for worker in &workers {
            if let Some(swarm_wt) = swarm_map.get(&worker.id) {
                self.apply_rules(worker, swarm_wt)?;
            } else {
                debug!("[reconciler] worker {} not in swarm state", worker.id);
                self.apply_disappeared(worker)?;
            }
        }

        // Runs independently of state.json — SQLite is the only source of truth for PR state.
        self.check_pr_status(&workers)?;

        Ok(())
    }

    /// Handle a worker that was in the DB but has disappeared from swarm state.json.
    ///
    /// Merge detection is handled by `check_merged_prs` (runs for all active workers
    /// regardless of state.json presence). Here we only handle the abandoned case:
    /// if the worker vanished without a merged PR, mark it abandoned after a grace period.
    fn apply_disappeared(&self, worker: &Worker) -> Result<()> {
        match worker.state {
            WorkerState::Running
            | WorkerState::Waiting
            | WorkerState::Stalled
            | WorkerState::Queued => {}
            _ => return Ok(()),
        }

        // If the PR is still open, the worktree just lost its entry (daemon restart etc) —
        // leave it alone. check_merged_prs will handle the MERGED case.
        if let Some(pr_url) = &worker.pr_url {
            let out = std::process::Command::new("gh")
                .args(["pr", "view", pr_url, "--json", "state", "--jq", ".state"])
                .output();
            if let Ok(out) = out
                && String::from_utf8_lossy(&out.stdout).trim() == "OPEN"
            {
                return Ok(());
            }
        }

        // No PR, or PR is closed/merged — abandon after grace period.
        if let Ok(entered) = worker
            .state_entered_at
            .parse::<chrono::DateTime<chrono::Utc>>()
            && (chrono::Utc::now() - entered).num_minutes() >= 1
        {
            info!(
                "[reconciler] {} disappeared without open PR → abandoned",
                worker.id
            );
            self.do_transition(worker, WorkerState::Abandoned)?;
        }

        Ok(())
    }

    /// Unified pass: for any active worker with a known PR URL, check if it was
    /// Check PR status (merged + CI) for all active workers with a pr_url.
    /// Throttled per-worker to at most once per 60 seconds.
    fn check_pr_status(&self, workers: &[Worker]) -> Result<()> {
        for worker in workers {
            match worker.state {
                WorkerState::Running | WorkerState::Waiting | WorkerState::Stalled => {}
                _ => continue,
            }
            let Some(pr_url) = &worker.pr_url else {
                continue;
            };
            let should_check = {
                let checked = self.pr_merge_checked.lock().unwrap();
                checked
                    .get(&worker.id)
                    .map(|t| t.elapsed().as_secs() >= 60)
                    .unwrap_or(true)
            };
            if !should_check {
                continue;
            }
            self.pr_merge_checked
                .lock()
                .unwrap()
                .insert(worker.id.clone(), std::time::Instant::now());

            let out = std::process::Command::new("gh")
                .args(["pr", "view", pr_url, "--json", "state,statusCheckRollup"])
                .output();

            let Ok(out) = out else { continue };
            let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
                continue;
            };

            // Merge detection
            if val["state"].as_str() == Some("MERGED") {
                info!("[reconciler] {} PR merged → done", worker.id);
                self.do_transition(worker, WorkerState::Done)?;
                continue;
            }

            // CI status from statusCheckRollup
            // Each entry has a `conclusion`: SUCCESS | FAILURE | NEUTRAL | CANCELLED |
            // SKIPPED | TIMED_OUT | ACTION_REQUIRED | null (still running)
            if let Some(checks) = val["statusCheckRollup"].as_array() {
                let ci_passing = if checks.is_empty() {
                    None // no CI configured
                } else {
                    let all_done = checks.iter().all(|c| {
                        !matches!(
                            c["status"].as_str(),
                            Some("IN_PROGRESS") | Some("QUEUED") | Some("PENDING")
                        )
                    });
                    if !all_done {
                        None // still running
                    } else {
                        let passing = checks.iter().all(|c| {
                            matches!(
                                c["conclusion"].as_str(),
                                Some("SUCCESS") | Some("NEUTRAL") | Some("SKIPPED")
                            )
                        });
                        Some(passing)
                    }
                };

                if ci_passing != worker.ci_passing {
                    debug!("[reconciler] {} CI status → {:?}", worker.id, ci_passing);
                    self.store.update_properties(
                        &self.workspace,
                        &worker.id,
                        WorkerPropertyUpdate {
                            ci_passing: Some(ci_passing),
                            ..Default::default()
                        },
                    )?;
                    self.emit_event(worker)?;
                }
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
                    if worker.branch_ready {
                        let branch = worker.branch.clone().unwrap_or_default();
                        self.emit_branch_ready_signal(&worker.id, &branch);
                    }
                    return Ok(());
                }

                // Agent paused waiting for input — detected via swarm phase.
                // Handles the case where the swarm daemon wrote phase="waiting" but
                // the agent-status file is absent (e.g. daemon died before cleaning up).
                if phase == "waiting" {
                    info!("[reconciler] {} agent waiting (swarm phase)", worker.id);
                    self.do_transition(worker, WorkerState::Waiting)?;
                    if worker.branch_ready {
                        let branch = worker.branch.clone().unwrap_or_default();
                        self.emit_branch_ready_signal(&worker.id, &branch);
                    }
                    return Ok(());
                }

                // Agent exited cleanly — move to waiting for review
                if phase == "completed" || phase == "failed" {
                    info!(
                        "[reconciler] {} agent exited (phase={phase}) → waiting",
                        worker.id
                    );
                    let branch = swarm_wt.branch.clone().unwrap_or_default();
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
                    self.emit_branch_ready_signal(&worker.id, &branch);
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
                    if worker.branch_ready {
                        let branch = worker.branch.clone().unwrap_or_default();
                        self.emit_branch_ready_signal(&worker.id, &branch);
                    }
                    return Ok(());
                }

                // Agent exited from stalled — move to waiting
                if phase == "completed" || phase == "failed" {
                    info!("[reconciler] {} stalled agent exited → waiting", worker.id);
                    let branch = swarm_wt.branch.clone().unwrap_or_default();
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
                    self.emit_branch_ready_signal(&worker.id, &branch);
                    return Ok(());
                }
            }
            WorkerState::Waiting => {
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
        let report_path = self
            .swarm_dir
            .join("agents")
            .join(&worker.id)
            .join("report.json");

        let Some(report) = read_worker_report(&self.swarm_dir, &worker.id) else {
            return Ok(());
        };

        // Always delete after reading — prevents infinite re-application on every
        // tick and prevents stale reports from being re-applied after a resume
        // clears branch_ready. The worker writes a fresh report.json on next completion.
        let _ = std::fs::remove_file(&report_path);

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

            // Safety net: if branch_ready just became true for the first time,
            // emit the signal here. The primary path (apply_report_to_db in
            // run_agent_task) should already have set branch_ready=true in the
            // DB before the Running→Waiting transition, so the signal fires there
            // with the correct worker state. This fallback covers daemon crashes
            // or other cases where apply_report_to_db didn't run.
            if branch_changed && report.branch_ready == Some(true) {
                let branch = worker.branch.clone().unwrap_or_default();
                self.emit_branch_ready_signal(&worker.id, &branch);
            }
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

                // Refresh display title every 3 assistant_text events while confidence < 85.
                let confidence = worker.title_confidence.unwrap_or(0);
                if confidence < 85 {
                    let count = count_assistant_text_events(&self.swarm_dir, &worker.id);
                    if count > 0 && count.is_multiple_of(3) {
                        let goal = worker.goal.clone().unwrap_or_default();
                        let snippet = recent_output_snippet(&self.swarm_dir, &worker.id);
                        let workspace = self.workspace.clone();
                        let worker_id = worker.id.clone();
                        let conn = self.store.conn_arc();
                        tokio::spawn(async move {
                            if let Some((title, conf)) =
                                crate::buzz::title_gen::generate_worker_title(
                                    &goal,
                                    snippet.as_deref(),
                                )
                                .await
                                && let Ok(store) = crate::buzz::worker::WorkerStore::new(conn)
                            {
                                let _ = store.update_title(&workspace, &worker_id, &title, conf);
                                info!("[title-gen] {worker_id} → {title:?} (confidence={conf})");
                            }
                        });
                    }
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

    /// Emit a `swarm_branch_ready` signal so the orchestrator's workflow chain fires.
    fn emit_branch_ready_signal(&self, worker_id: &str, branch: &str) {
        let Some(ref tx) = self.signal_tx else {
            warn!(
                "[reconciler] {} branch ready but no signal_tx — orchestrator won't fire",
                worker_id
            );
            return;
        };
        info!(
            "[reconciler] {} emitting swarm_branch_ready (branch={})",
            worker_id, branch
        );
        let update = crate::buzz::signal::SignalUpdate::new(
            "swarm_branch_ready",
            format!("swarm-branch-ready-{worker_id}"),
            format!("Branch ready: {branch}"),
            crate::buzz::signal::Severity::Info,
        )
        .with_metadata(
            serde_json::json!({
                "worker_id": worker_id,
                "branch_name": branch,
            })
            .to_string(),
        );
        let _ = tx.send((self.workspace.clone(), update));
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
            Ok(s) => s.worktrees,
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
        if let Err(e) = reconciler.requeue_branch_ready_on_startup() {
            warn!("[reconciler] startup branch_ready requeue error: {e}");
        }

        // Fast loop: runs in spawn_blocking (does file I/O + occasional `gh pr view`).
        let mut fast_interval = tokio::time::interval(std::time::Duration::from_secs(5));
        fast_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Slow loop: PR approval polling via GraphQL `gh api`.
        // Runs every 5 minutes. Only fires if there are tasks in InAiReview.
        let mut pr_interval = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
        pr_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = fast_interval.tick() => {
                    let r = std::sync::Arc::clone(&reconciler);
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = r.reconcile_once() {
                            warn!("[reconciler] error: {e}");
                        }
                    });
                }
                _ = pr_interval.tick() => {
                    let r = std::sync::Arc::clone(&reconciler);
                    tokio::task::spawn_blocking(move || {
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
            signal_tx: None,
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
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
            ci_passing: None,
            is_stalled: false,
            revision_count: 0,
            review_mode: "local_first".to_string(),
            blocked_reason: None,
            last_output_at: None,
            state_entered_at: now.clone(),
            created_at: now.clone(),
            updated_at: now,
            display_title: None,
            title_confidence: None,
            label: "Queued".to_string(),
            worktree_path: None,
            isolation_mode: None,
            agent_kind: None,
            model: None,
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
    fn rule_completed_phase_emits_branch_ready_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace_root.join(".swarm")).unwrap();

        let conn = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        conn.lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        let store = WorkerStore::new(Arc::clone(&conn)).unwrap();

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, crate::buzz::signal::SignalUpdate)>();
        let r = SwarmReconciler {
            workspace: "test".to_string(),
            swarm_dir: workspace_root.join(".swarm"),
            store,
            event_tx: None,
            db_path: None,
            signal_tx: Some(tx),
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
        };

        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let mut wt = swarm_wt("w1", "completed", None);
        wt.branch = Some("swarm/w1-fix".to_string());
        r.apply_rules(&w, &wt).unwrap();

        let (ws, signal) = rx.try_recv().expect("signal should have been sent");
        assert_eq!(ws, "test");
        assert_eq!(signal.source, "swarm_branch_ready");
        assert_eq!(signal.external_id, "swarm-branch-ready-w1");
        assert!(
            signal
                .metadata
                .as_deref()
                .unwrap_or("")
                .contains("swarm/w1-fix")
        );
    }

    #[test]
    fn rule_stalled_completed_phase_emits_branch_ready_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace_root.join(".swarm")).unwrap();

        let conn = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        conn.lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        let store = WorkerStore::new(Arc::clone(&conn)).unwrap();

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, crate::buzz::signal::SignalUpdate)>();
        let r = SwarmReconciler {
            workspace: "test".to_string(),
            swarm_dir: workspace_root.join(".swarm"),
            store,
            event_tx: None,
            db_path: None,
            signal_tx: Some(tx),
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
        };

        let mut w = default_worker("w1");
        w.state = WorkerState::Stalled;
        r.store.upsert(&w).unwrap();

        let mut wt = swarm_wt("w1", "completed", None);
        wt.branch = Some("swarm/w1-stalled-fix".to_string());
        r.apply_rules(&w, &wt).unwrap();

        let (ws, signal) = rx
            .try_recv()
            .expect("signal should have been sent from stalled exit");
        assert_eq!(ws, "test");
        assert_eq!(signal.source, "swarm_branch_ready");
        assert!(
            signal
                .metadata
                .as_deref()
                .unwrap_or("")
                .contains("swarm/w1-stalled-fix")
        );
    }

    #[test]
    fn rule_phase_waiting_with_branch_ready_emits_signal() {
        // Regression: agent sets branch_ready=true in report.json (applied on tick N),
        // then transitions via phase="waiting" on tick N+1. Signal must still fire.
        let tmp = tempfile::tempdir().unwrap();
        let workspace_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace_root.join(".swarm")).unwrap();

        let conn = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        conn.lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        let store = WorkerStore::new(Arc::clone(&conn)).unwrap();

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, crate::buzz::signal::SignalUpdate)>();
        let r = SwarmReconciler {
            workspace: "test".to_string(),
            swarm_dir: workspace_root.join(".swarm"),
            store,
            event_tx: None,
            db_path: None,
            signal_tx: Some(tx),
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
        };

        // Worker already has branch_ready=true (set by a previous apply_report tick)
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        w.branch_ready = true;
        w.branch = Some("swarm/w1-fix".to_string());
        r.store.upsert(&w).unwrap();

        // This tick: phase="waiting" (agent asking for input, not fully done)
        let mut wt = swarm_wt("w1", "waiting", None);
        wt.branch = Some("swarm/w1-fix".to_string());
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
        let (ws, signal) = rx
            .try_recv()
            .expect("signal should fire when branch_ready was pre-set");
        assert_eq!(ws, "test");
        assert_eq!(signal.source, "swarm_branch_ready");
        assert_eq!(signal.external_id, "swarm-branch-ready-w1");
    }

    #[test]
    fn rule_agent_status_waiting_with_branch_ready_emits_signal() {
        // Same regression via agent-status=waiting path
        let tmp = tempfile::tempdir().unwrap();
        let workspace_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace_root.join(".swarm")).unwrap();

        let conn = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        conn.lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        let store = WorkerStore::new(Arc::clone(&conn)).unwrap();

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, crate::buzz::signal::SignalUpdate)>();
        let r = SwarmReconciler {
            workspace: "test".to_string(),
            swarm_dir: workspace_root.join(".swarm"),
            store,
            event_tx: None,
            db_path: None,
            signal_tx: Some(tx),
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
        };

        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        w.branch_ready = true;
        w.branch = Some("swarm/w1-fix".to_string());
        r.store.upsert(&w).unwrap();

        // Write agent-status=waiting file
        let status_dir = workspace_root.join(".swarm").join("agent-status");
        std::fs::create_dir_all(&status_dir).unwrap();
        std::fs::write(status_dir.join("w1"), "waiting").unwrap();

        let mut wt = swarm_wt("w1", "running", None);
        wt.branch = Some("swarm/w1-fix".to_string());
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
        let (ws, signal) = rx
            .try_recv()
            .expect("signal should fire when branch_ready was pre-set");
        assert_eq!(ws, "test");
        assert_eq!(signal.source, "swarm_branch_ready");
        assert_eq!(signal.external_id, "swarm-branch-ready-w1");
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

    // ── State transition + signal matrix ───────────────────────────────────
    //
    // Every combination of (initial_state, phase, agent_status, branch_ready)
    // is checked for: resulting state AND whether swarm_branch_ready fires.
    //
    // Columns: initial_state | phase | agent_status | branch_ready_preset
    //        | expected_state | expect_signal
    #[test]
    fn matrix_state_transitions_and_signals() {
        #[derive(Debug)]
        struct Case {
            label: &'static str,
            initial: WorkerState,
            phase: &'static str,
            agent_status: Option<&'static str>,
            branch_ready_preset: bool,
            branch_preset: Option<&'static str>,
            expected_state: WorkerState,
            expect_signal: bool,
        }

        let cases = vec![
            // ── Queued ──────────────────────────────────────────────────────
            Case {
                label: "queued+running→running",
                initial: WorkerState::Queued,
                phase: "running",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Running,
                expect_signal: false,
            },
            Case {
                label: "queued+waiting→queued(no rule)",
                initial: WorkerState::Queued,
                phase: "waiting",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Queued,
                expect_signal: false,
            },
            // ── Running, no branch_ready ─────────────────────────────────
            Case {
                label: "running+completed→waiting+signal",
                initial: WorkerState::Running,
                phase: "completed",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            Case {
                label: "running+failed→waiting+signal",
                initial: WorkerState::Running,
                phase: "failed",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            Case {
                label: "running+phase-waiting→waiting,no signal",
                initial: WorkerState::Running,
                phase: "waiting",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Waiting,
                expect_signal: false,
            },
            Case {
                label: "running+agent-waiting→waiting,no signal",
                initial: WorkerState::Running,
                phase: "running",
                agent_status: Some("waiting"),
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Waiting,
                expect_signal: false,
            },
            Case {
                label: "running+running→stays running",
                initial: WorkerState::Running,
                phase: "running",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Running,
                expect_signal: false,
            },
            // ── Running, branch_ready already set (from prior apply_report) ──
            Case {
                label: "running+phase-waiting+br→waiting+signal",
                initial: WorkerState::Running,
                phase: "waiting",
                agent_status: None,
                branch_ready_preset: true,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            Case {
                label: "running+agent-waiting+br→waiting+signal",
                initial: WorkerState::Running,
                phase: "running",
                agent_status: Some("waiting"),
                branch_ready_preset: true,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            // ── Stalled ──────────────────────────────────────────────────
            Case {
                label: "stalled+completed→waiting+signal",
                initial: WorkerState::Stalled,
                phase: "completed",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            Case {
                label: "stalled+failed→waiting+signal",
                initial: WorkerState::Stalled,
                phase: "failed",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            Case {
                label: "stalled+phase-waiting,no br→waiting,no signal",
                initial: WorkerState::Stalled,
                phase: "waiting",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Waiting,
                expect_signal: false,
            },
            Case {
                label: "stalled+phase-waiting+br→waiting+signal",
                initial: WorkerState::Stalled,
                phase: "waiting",
                agent_status: None,
                branch_ready_preset: true,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            Case {
                label: "stalled+agent-waiting+br→waiting+signal",
                initial: WorkerState::Stalled,
                phase: "waiting",
                agent_status: Some("waiting"),
                branch_ready_preset: true,
                branch_preset: Some("feat/fix"),
                expected_state: WorkerState::Waiting,
                expect_signal: true,
            },
            // ── Waiting (already waiting, agent resumes) ─────────────────
            Case {
                label: "waiting+running→running(revision)",
                initial: WorkerState::Waiting,
                phase: "running",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Running,
                expect_signal: false,
            },
            Case {
                label: "waiting+completed→stays waiting",
                initial: WorkerState::Waiting,
                phase: "completed",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Waiting,
                expect_signal: false,
            },
            // ── Terminal states — nothing moves them ─────────────────────
            Case {
                label: "done+anything→stays done",
                initial: WorkerState::Done,
                phase: "running",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Done,
                expect_signal: false,
            },
            Case {
                label: "abandoned+anything→stays abandoned",
                initial: WorkerState::Abandoned,
                phase: "completed",
                agent_status: None,
                branch_ready_preset: false,
                branch_preset: None,
                expected_state: WorkerState::Abandoned,
                expect_signal: false,
            },
        ];

        for case in &cases {
            let tmp = tempfile::tempdir().unwrap();
            let workspace_root = tmp.path().to_path_buf();
            std::fs::create_dir_all(workspace_root.join(".swarm/agent-status")).unwrap();

            let conn = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
            conn.lock()
                .unwrap()
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
                .unwrap();
            let store = WorkerStore::new(Arc::clone(&conn)).unwrap();

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(
                String,
                crate::buzz::signal::SignalUpdate,
            )>();
            let r = SwarmReconciler {
                workspace: "test".to_string(),
                swarm_dir: workspace_root.join(".swarm"),
                store,
                event_tx: None,
                db_path: None,
                signal_tx: Some(tx),
                pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
            };

            let mut w = default_worker("w1");
            w.state = case.initial.clone();
            w.branch_ready = case.branch_ready_preset;
            w.branch = case.branch_preset.map(|s| s.to_string());
            r.store.upsert(&w).unwrap();

            if let Some(status) = case.agent_status {
                std::fs::write(workspace_root.join(".swarm/agent-status/w1"), status).unwrap();
            }

            let mut wt = swarm_wt("w1", case.phase, None);
            wt.branch = case.branch_preset.map(|s| s.to_string());
            r.apply_rules(&w, &wt).unwrap();

            let updated = r.store.get("test", "w1").unwrap().unwrap();
            assert_eq!(
                updated.state, case.expected_state,
                "[{}] expected state {:?} got {:?}",
                case.label, case.expected_state, updated.state
            );

            let got_signal = rx.try_recv().is_ok();
            assert_eq!(
                got_signal, case.expect_signal,
                "[{}] expect_signal={} but got_signal={}",
                case.label, case.expect_signal, got_signal
            );
        }
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

    // ── check_pr_status tests ───────────────────────────────────────────
    //
    // These use a fake `gh` binary (a shell script in a temp dir) injected via
    // PATH + a mutex, so parallel tests don't race.
    //
    // The fake `gh` outputs JSON matching `gh pr view --json state,statusCheckRollup`.
    // Helper `gh_json(state, ci)` builds common variants.

    /// Write a fake `gh` shell script that prints `output` and make it executable.
    fn write_fake_gh(dir: &std::path::Path, output: &str) -> std::path::PathBuf {
        let script = dir.join("gh");
        let escaped = output.replace('\'', "'\\''");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{escaped}'\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    /// Prepend `gh_path`'s parent dir to PATH for the duration of `f`.
    fn with_gh_on_path<F: FnOnce()>(gh_path: &std::path::Path, f: F) {
        let _guard = crate::test_env::lock();
        let original = std::env::var("PATH").unwrap_or_default();
        let dir = gh_path.parent().unwrap();
        unsafe { std::env::set_var("PATH", format!("{}:{original}", dir.display())) };
        f();
        unsafe { std::env::set_var("PATH", original) };
    }

    /// Build JSON matching `gh pr view --json state,statusCheckRollup`.
    /// `ci`: None = no checks, Some(true) = all SUCCESS, Some(false) = one FAILURE.
    fn gh_pr_json(state: &str, ci: Option<bool>) -> String {
        let checks = match ci {
            None => "[]".to_string(),
            Some(true) => {
                r#"[{"name":"CI","status":"COMPLETED","conclusion":"SUCCESS"}]"#.to_string()
            }
            Some(false) => {
                r#"[{"name":"CI","status":"COMPLETED","conclusion":"FAILURE"}]"#.to_string()
            }
        };
        format!(r#"{{"state":"{state}","statusCheckRollup":{checks}}}"#)
    }

    #[test]
    fn check_pr_status_merged_transitions_to_done() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w-merged");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/owner/repo/pull/1".to_string());
        r.store.upsert(&w).unwrap();

        let gh = write_fake_gh(tmp.path(), &gh_pr_json("MERGED", Some(true)));
        with_gh_on_path(&gh, || r.check_pr_status(&[w]).unwrap());

        assert_eq!(
            r.store.get("test", "w-merged").unwrap().unwrap().state,
            WorkerState::Done
        );
    }

    #[test]
    fn check_pr_status_open_ci_passing_updates_ci_passing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w-ci-pass");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/owner/repo/pull/2".to_string());
        w.ci_passing = None; // unknown initially
        r.store.upsert(&w).unwrap();

        let gh = write_fake_gh(tmp.path(), &gh_pr_json("OPEN", Some(true)));
        with_gh_on_path(&gh, || r.check_pr_status(&[w]).unwrap());

        let updated = r.store.get("test", "w-ci-pass").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting); // not merged
        assert_eq!(updated.ci_passing, Some(true));
    }

    #[test]
    fn check_pr_status_open_ci_failing_updates_ci_passing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w-ci-fail");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/owner/repo/pull/3".to_string());
        r.store.upsert(&w).unwrap();

        let gh = write_fake_gh(tmp.path(), &gh_pr_json("OPEN", Some(false)));
        with_gh_on_path(&gh, || r.check_pr_status(&[w]).unwrap());

        let updated = r.store.get("test", "w-ci-fail").unwrap().unwrap();
        assert_eq!(updated.ci_passing, Some(false));
    }

    #[test]
    fn check_pr_status_skips_workers_without_pr_url() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w-no-pr");
        w.state = WorkerState::Waiting;
        w.pr_url = None;
        r.store.upsert(&w).unwrap();

        // No gh needed — must not be called
        let gh = write_fake_gh(tmp.path(), &gh_pr_json("MERGED", Some(true)));
        with_gh_on_path(&gh, || r.check_pr_status(&[w]).unwrap());

        assert_eq!(
            r.store.get("test", "w-no-pr").unwrap().unwrap().state,
            WorkerState::Waiting
        );
    }

    #[test]
    fn check_pr_status_skips_terminal_workers() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w-done");
        w.state = WorkerState::Done;
        w.pr_url = Some("https://github.com/owner/repo/pull/4".to_string());
        r.store.upsert(&w).unwrap();

        let gh = write_fake_gh(tmp.path(), &gh_pr_json("MERGED", Some(true)));
        with_gh_on_path(&gh, || r.check_pr_status(&[w]).unwrap());

        // Already Done — must not be touched
        assert_eq!(
            r.store.get("test", "w-done").unwrap().unwrap().state,
            WorkerState::Done
        );
    }

    #[test]
    fn check_pr_status_throttles_per_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w-throttle");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/owner/repo/pull/5".to_string());
        r.store.upsert(&w).unwrap();

        // Seed the throttle map as if we just checked this worker.
        r.pr_merge_checked
            .lock()
            .unwrap()
            .insert("w-throttle".to_string(), std::time::Instant::now());

        // gh returns MERGED — but the check is throttled so it must not be called.
        let gh = write_fake_gh(tmp.path(), &gh_pr_json("MERGED", Some(true)));
        with_gh_on_path(&gh, || r.check_pr_status(&[w]).unwrap());

        // Still Waiting — throttle prevented the gh call
        assert_eq!(
            r.store.get("test", "w-throttle").unwrap().unwrap().state,
            WorkerState::Waiting
        );
    }

    // ── apply_report lifecycle ─────────────────────────────────────────────

    fn make_reconciler_with_signal(
        tmp: &tempfile::TempDir,
    ) -> (
        SwarmReconciler,
        tokio::sync::mpsc::UnboundedReceiver<(String, crate::buzz::signal::SignalUpdate)>,
    ) {
        let workspace_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace_root.join(".swarm")).unwrap();
        let conn = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        conn.lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        let store = WorkerStore::new(Arc::clone(&conn)).unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let r = SwarmReconciler {
            workspace: "test".to_string(),
            swarm_dir: workspace_root.join(".swarm"),
            store,
            event_tx: None,
            db_path: None,
            signal_tx: Some(tx),
            pr_merge_checked: std::sync::Mutex::new(HashMap::new()),
        };
        (r, rx)
    }

    fn write_report(
        swarm_dir: &std::path::Path,
        worker_id: &str,
        branch_ready: bool,
        tests_passing: bool,
    ) {
        let agents_dir = swarm_dir.join("agents").join(worker_id);
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("report.json"),
            format!(r#"{{"branch_ready": {branch_ready}, "tests_passing": {tests_passing}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn apply_report_deletes_file_after_reading() {
        // report.json must be deleted after apply_report reads it so it cannot
        // be re-applied on subsequent ticks.
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        r.store.upsert(&w).unwrap();

        write_report(&r.swarm_dir, "w1", true, true);
        let report_path = r.swarm_dir.join("agents").join("w1").join("report.json");
        assert!(report_path.exists(), "precondition: report.json must exist");

        let wt = swarm_wt("w1", "waiting", None);
        r.apply_rules(&w, &wt).unwrap();

        assert!(
            !report_path.exists(),
            "apply_report must delete report.json after reading"
        );
    }

    #[test]
    fn apply_report_does_not_reapply_after_deletion() {
        // After apply_report deletes report.json, a second apply_rules call must
        // not re-apply the values (file is gone so there's nothing to re-read).
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);

        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        r.store.upsert(&w).unwrap();

        write_report(&r.swarm_dir, "w1", true, true);

        let wt = swarm_wt("w1", "waiting", None);
        // First call: applies and deletes
        r.apply_rules(&w, &wt).unwrap();
        let after_first = r.store.get("test", "w1").unwrap().unwrap();
        assert!(after_first.branch_ready);

        // Manually reset branch_ready in DB (simulates v2_send_message clearing it)
        r.store
            .update_properties(
                "test",
                "w1",
                crate::buzz::worker::WorkerPropertyUpdate {
                    branch_ready: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();

        // Second call: report.json is gone — must NOT restore branch_ready=true
        r.apply_rules(&w, &wt).unwrap();
        let after_second = r.store.get("test", "w1").unwrap().unwrap();
        assert!(
            !after_second.branch_ready,
            "branch_ready must not be restored after file was deleted"
        );
    }

    #[test]
    fn apply_report_emits_signal_when_branch_ready_first_set() {
        // apply_report acts as a fallback signal emitter: if branch_ready transitions
        // false→true (e.g. apply_report_to_db didn't run before phase="waiting"),
        // it must emit swarm_branch_ready so the orchestrator can act.
        let tmp = tempfile::tempdir().unwrap();
        let (r, mut rx) = make_reconciler_with_signal(&tmp);

        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        w.branch = Some("swarm/w1-fix".to_string());
        w.branch_ready = false;
        r.store.upsert(&w).unwrap();

        write_report(&r.swarm_dir, "w1", true, true);

        let mut wt = swarm_wt("w1", "waiting", None);
        wt.branch = Some("swarm/w1-fix".to_string());
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert!(updated.branch_ready);

        let (ws, signal) = rx
            .try_recv()
            .expect("signal must be emitted when branch_ready first becomes true via report");
        assert_eq!(ws, "test");
        assert_eq!(signal.source, "swarm_branch_ready");
        assert_eq!(signal.external_id, "swarm-branch-ready-w1");
    }

    #[test]
    fn apply_report_no_signal_when_branch_ready_already_set() {
        // If branch_ready is already true in the DB and the report also says true,
        // no signal should be re-emitted (branch_changed = false).
        let tmp = tempfile::tempdir().unwrap();
        let (r, mut rx) = make_reconciler_with_signal(&tmp);

        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        w.branch = Some("swarm/w1-fix".to_string());
        w.branch_ready = true; // already set
        r.store.upsert(&w).unwrap();

        write_report(&r.swarm_dir, "w1", true, true);

        let mut wt = swarm_wt("w1", "waiting", None);
        wt.branch = Some("swarm/w1-fix".to_string());
        r.apply_rules(&w, &wt).unwrap();

        // branch_changed = false → no signal from apply_report
        assert!(
            rx.try_recv().is_err(),
            "no signal when branch_ready already was true"
        );
    }
}

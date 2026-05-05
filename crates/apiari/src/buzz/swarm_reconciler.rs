//! Swarm reconciler — background task that polls `.swarm/state.json` every 5 seconds
//! and applies forward-only state transitions to the v2 worker DB table.
//!
//! # Rules (from spec)
//!
//! | Swarm signal                      | DB state  | Action                                       |
//! |-----------------------------------|-----------|----------------------------------------------|
//! | agent running                     | queued    | → running                                    |
//! | agent waiting (agent-status file) | running   | → waiting                                    |
//! | agent exited 0 (phase=="complete")| running   | → waiting, set branch_ready=true             |
//! | agent exited non-0 (phase failed) | running   | → failed                                     |
//! | pr.url appeared                   | any       | set pr_url property                          |
//! | phase=="merged"                   | any       | → merged                                     |
//! | DB=waiting, swarm=running         | waiting   | → running, increment revision_count          |
//! | last_output_at >10min + running   | —         | set is_stalled=true                          |
//! | new output event                  | running   | clear is_stalled, update last_output_at      |

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
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SwarmPr {
    #[serde(default)]
    url: Option<String>,
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
}

/// The actual reconciler logic — separated from the task for testability.
pub struct SwarmReconciler {
    workspace: String,
    swarm_dir: PathBuf,
    store: WorkerStore,
    event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
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
        })
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
                // Worker is in DB but not in swarm — nothing to do (not an error).
                debug!("[reconciler] worker {} not in swarm state", worker.id);
            }
        }

        // Check for swarm workers that exist but have no DB record (no-op in v2 —
        // workers are created through the API, not auto-imported from swarm).
        let _ = swarm_map;
        let _ = worker_map;

        Ok(())
    }

    /// Apply forward-only reconciliation rules for one worker.
    fn apply_rules(&self, worker: &Worker, swarm_wt: &SwarmWorktree) -> Result<()> {
        let phase = swarm_wt.phase.as_deref().unwrap_or("");
        let agent_status = agent_status(&self.swarm_dir, &worker.id);

        // 1. phase=="merged" → merged (any state)
        if phase == "merged" && worker.state != WorkerState::Merged {
            self.do_transition(worker, WorkerState::Merged)?;
            return Ok(());
        }

        // 2. pr_url appeared (any state)
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

        // 3. State-specific transitions
        match worker.state {
            WorkerState::Queued => {
                // queued → running when swarm is running
                if phase == "running" {
                    self.do_transition(worker, WorkerState::Running)?;
                }
            }
            WorkerState::Running => {
                // Check for agent-status "waiting"
                if agent_status.as_deref() == Some("waiting") {
                    info!("[reconciler] {} agent waiting (agent-status)", worker.id);
                    self.do_transition(worker, WorkerState::Waiting)?;
                    return Ok(());
                }

                // phase=="complete" (agent exited 0)
                if phase == "complete" {
                    info!("[reconciler] {} agent completed", worker.id);
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

                // phase=="failed" (agent exited non-0)
                if phase == "failed" {
                    info!("[reconciler] {} agent failed", worker.id);
                    self.do_transition(worker, WorkerState::Failed)?;
                    return Ok(());
                }

                // Still running — check stall detection and update last_output_at
                self.check_stall_and_output(worker)?;
            }
            WorkerState::Waiting => {
                // waiting → running when swarm is running (human resumed)
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
            // Terminal states — no forward transitions from swarm
            WorkerState::Merged | WorkerState::Failed | WorkerState::Abandoned => {}
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

    /// Stall detection and last_output_at update for running workers.
    fn check_stall_and_output(&self, worker: &Worker) -> Result<()> {
        let latest = last_output_timestamp(&self.swarm_dir, &worker.id);

        if let Some(ts) = latest {
            let ts_str = ts.to_rfc3339();

            // New output arrived — clear stall, update timestamp
            let is_new = worker
                .last_output_at
                .as_deref()
                .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
                .is_none_or(|prev| ts > prev);

            if is_new {
                let mut update = WorkerPropertyUpdate {
                    last_output_at: Some(ts_str),
                    ..Default::default()
                };
                if worker.is_stalled {
                    update.is_stalled = Some(false);
                }
                self.store
                    .update_properties(&self.workspace, &worker.id, update)?;
                self.emit_event(worker)?;
                return Ok(());
            }

            // No new output — check if stall threshold exceeded (10 min)
            let age_minutes = (Utc::now() - ts).num_minutes();
            if age_minutes >= 10 && !worker.is_stalled {
                info!(
                    "[reconciler] {} stalled (no output for {}min)",
                    worker.id, age_minutes
                );
                self.store.update_properties(
                    &self.workspace,
                    &worker.id,
                    WorkerPropertyUpdate {
                        is_stalled: Some(true),
                        ..Default::default()
                    },
                )?;
                self.emit_event(worker)?;
            }
        } else if let Ok(entered) = worker.state_entered_at.parse::<chrono::DateTime<Utc>>() {
            // No events file — use state_entered_at as proxy
            let age_minutes = (Utc::now() - entered).num_minutes();
            if age_minutes >= 10 && !worker.is_stalled {
                info!(
                    "[reconciler] {} stalled (no events, {}min in running)",
                    worker.id, age_minutes
                );
                self.store.update_properties(
                    &self.workspace,
                    &worker.id,
                    WorkerPropertyUpdate {
                        is_stalled: Some(true),
                        ..Default::default()
                    },
                )?;
                self.emit_event(worker)?;
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
            .transition(&self.workspace, &worker.id, new_state)?;
        // Re-fetch to get fresh label for the event
        if let Some(updated) = self.store.get(&self.workspace, &worker.id)? {
            self.emit_event(&updated)?;
        }
        Ok(())
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
            Ok(r) => r,
            Err(e) => {
                warn!("[reconciler] failed to initialize: {e}");
                return;
            }
        };

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;
            if let Err(e) = reconciler.reconcile_once() {
                warn!("[reconciler] error: {e}");
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
            label: "Queued".to_string(),
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
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "complete", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Waiting);
        assert!(updated.branch_ready);
    }

    #[test]
    fn rule_running_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Running;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "failed", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Failed);
    }

    #[test]
    fn rule_any_phase_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let r = make_reconciler(&tmp);
        let mut w = default_worker("w1");
        w.state = WorkerState::Waiting;
        r.store.upsert(&w).unwrap();

        let wt = swarm_wt("w1", "merged", None);
        r.apply_rules(&w, &wt).unwrap();

        let updated = r.store.get("test", "w1").unwrap().unwrap();
        assert_eq!(updated.state, WorkerState::Merged);
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
}

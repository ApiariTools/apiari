//! Worker hook execution engine — Phase 5B of the v2 rebuild.
//!
//! Hooks let auto bots subscribe to worker conditions and react automatically.
//! The executor runs every 60 seconds, evaluates each (hook, worker) pair, and
//! fires the configured action when all conditions are met.
//!
//! # Condition semantics (AND logic)
//! - `trigger_state`: worker's current state string must match
//! - `trigger_property`: a specific worker boolean/optional property must match `trigger_value`
//! - `duration_minutes`: worker must have been in the current state for at least N minutes

use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::buzz::worker::{Worker, WorkerStore};

// ── Domain types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHook {
    pub id: i64,
    pub workspace: String,
    pub trigger_state: Option<String>,
    pub trigger_property: Option<String>,
    pub trigger_value: Option<String>,
    pub duration_minutes: Option<i64>,
    pub action: String,
    pub auto_bot_id: Option<i64>,
    pub created_at: String,
}

pub struct NewWorkerHook {
    pub workspace: String,
    pub trigger_state: Option<String>,
    pub trigger_property: Option<String>,
    pub trigger_value: Option<String>,
    pub duration_minutes: Option<i64>,
    pub action: String,
    pub auto_bot_id: Option<i64>,
}

// ── Store ──────────────────────────────────────────────────────────────

pub struct WorkerHookStore {
    conn: Arc<Mutex<Connection>>,
}

impl WorkerHookStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// List all hooks for a workspace.
    pub fn list(&self, workspace: &str) -> Result<Vec<WorkerHook>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, workspace, trigger_state, trigger_property, trigger_value,
                    duration_minutes, action, auto_bot_id, created_at
             FROM worker_hooks
             WHERE workspace = ?1
             ORDER BY id ASC",
        )?;
        let hooks = stmt
            .query_map(params![workspace], row_to_hook)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("list worker hooks")?;
        Ok(hooks)
    }

    /// Insert a new hook and return the full record.
    pub fn insert(&self, hook: &NewWorkerHook) -> Result<WorkerHook> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO worker_hooks
             (workspace, trigger_state, trigger_property, trigger_value,
              duration_minutes, action, auto_bot_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                hook.workspace,
                hook.trigger_state,
                hook.trigger_property,
                hook.trigger_value,
                hook.duration_minutes,
                hook.action,
                hook.auto_bot_id,
            ],
        )
        .wrap_err("insert worker hook")?;
        let id = conn.last_insert_rowid();
        let result = conn
            .query_row(
                "SELECT id, workspace, trigger_state, trigger_property, trigger_value,
                        duration_minutes, action, auto_bot_id, created_at
                 FROM worker_hooks WHERE id = ?1",
                params![id],
                row_to_hook,
            )
            .wrap_err("fetch inserted hook")?;
        Ok(result)
    }

    /// Delete a hook by (workspace, id).
    pub fn delete(&self, workspace: &str, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM worker_hooks WHERE workspace = ?1 AND id = ?2",
            params![workspace, id],
        )
        .wrap_err("delete worker hook")?;
        Ok(())
    }
}

fn row_to_hook(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkerHook> {
    Ok(WorkerHook {
        id: row.get(0)?,
        workspace: row.get(1)?,
        trigger_state: row.get(2)?,
        trigger_property: row.get(3)?,
        trigger_value: row.get(4)?,
        duration_minutes: row.get(5)?,
        action: row.get(6)?,
        auto_bot_id: row.get(7)?,
        created_at: row.get(8)?,
    })
}

// ── Default hooks ──────────────────────────────────────────────────────

/// Default hooks to seed when no hooks exist for a workspace.
fn default_hooks(workspace: &str) -> Vec<NewWorkerHook> {
    vec![
        // Hook 1: nudge stalled workers after 15 minutes
        NewWorkerHook {
            workspace: workspace.to_string(),
            trigger_state: Some("running".to_string()),
            trigger_property: Some("is_stalled".to_string()),
            trigger_value: Some("true".to_string()),
            duration_minutes: Some(15),
            action: "nudge".to_string(),
            auto_bot_id: None,
        },
        // Hook 2: notify when a worker has been waiting for input for 30 minutes
        NewWorkerHook {
            workspace: workspace.to_string(),
            trigger_state: Some("waiting".to_string()),
            trigger_property: None,
            trigger_value: None,
            duration_minutes: Some(30),
            action: "notify".to_string(),
            auto_bot_id: None,
        },
    ]
}

// ── Condition evaluation ───────────────────────────────────────────────

/// Check whether all set conditions for a hook are satisfied by the given worker.
///
/// Returns `true` only if every non-None condition field matches.
fn conditions_met(hook: &WorkerHook, worker: &Worker) -> bool {
    // 1. State condition
    if let Some(ref required_state) = hook.trigger_state
        && worker.state.as_str() != required_state.as_str()
    {
        return false;
    }

    // 2. Property condition
    if let Some(ref prop) = hook.trigger_property {
        let required_value = hook.trigger_value.as_deref().unwrap_or("true");
        let actual_value = match prop.as_str() {
            "is_stalled" => worker.is_stalled.to_string(),
            "branch_ready" => worker.branch_ready.to_string(),
            "tests_passing" => worker.tests_passing.to_string(),
            "pr_url" => worker.pr_url.is_some().to_string(),
            other => {
                warn!(
                    "[hook_executor] unknown trigger_property '{}' — skipping",
                    other
                );
                return false;
            }
        };
        if actual_value != required_value {
            return false;
        }
    }

    // 3. Duration condition
    if let Some(required_minutes) = hook.duration_minutes {
        if let Ok(entered) = worker.state_entered_at.parse::<DateTime<Utc>>() {
            let elapsed_minutes = (Utc::now() - entered).num_minutes();
            if elapsed_minutes < required_minutes {
                return false;
            }
        } else {
            // Can't parse the timestamp — skip the duration check rather than fire incorrectly.
            return false;
        }
    }

    true
}

// ── Executor ───────────────────────────────────────────────────────────

/// Background executor that evaluates hooks against workers every 60 seconds.
pub struct WorkerHookExecutor {
    pub hook_store: Arc<WorkerHookStore>,
    pub worker_store: Arc<WorkerStore>,
    pub event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    pub workspace: String,
    pub workspace_root: PathBuf,
    /// Tracks the last time each (hook_id, worker_id) pair fired.
    /// A hook won't re-fire for the same pair until `duration_minutes + 5` minutes have elapsed.
    last_fired: Mutex<HashMap<(i64, String), DateTime<Utc>>>,
}

impl WorkerHookExecutor {
    pub fn new(
        hook_store: Arc<WorkerHookStore>,
        worker_store: Arc<WorkerStore>,
        event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
        workspace: String,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            hook_store,
            worker_store,
            event_tx,
            workspace,
            workspace_root,
            last_fired: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn the executor as a background tokio task.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Seed defaults if no hooks exist
            if let Err(e) = self.seed_defaults_if_empty() {
                warn!(
                    "[hook_executor/{}] failed to seed default hooks: {e}",
                    self.workspace
                );
            }

            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                ticker.tick().await;
                if let Err(e) = self.tick() {
                    warn!("[hook_executor/{}] tick error: {e}", self.workspace);
                }
            }
        })
    }

    /// Seed default hooks when none exist for this workspace.
    fn seed_defaults_if_empty(&self) -> Result<()> {
        let existing = self.hook_store.list(&self.workspace)?;
        if !existing.is_empty() {
            return Ok(());
        }
        info!(
            "[hook_executor/{}] no hooks found — seeding defaults",
            self.workspace
        );
        for hook in default_hooks(&self.workspace) {
            self.hook_store.insert(&hook)?;
        }
        Ok(())
    }

    /// Run one evaluation cycle.
    fn tick(&self) -> Result<()> {
        let hooks = self.hook_store.list(&self.workspace)?;
        let workers = self.worker_store.list(&self.workspace)?;

        for hook in &hooks {
            for worker in &workers {
                // Skip terminal workers — no point nudging/notifying a merged/abandoned worker.
                if worker.state.is_terminal() {
                    continue;
                }

                if !conditions_met(hook, worker) {
                    continue;
                }

                // Deduplication: don't re-fire within the cooldown window.
                let cooldown_minutes = hook.duration_minutes.unwrap_or(0) + 5;
                let key = (hook.id, worker.id.clone());
                {
                    let map = self.last_fired.lock().unwrap();
                    if let Some(&last) = map.get(&key) {
                        let elapsed = (Utc::now() - last).num_minutes();
                        if elapsed < cooldown_minutes {
                            continue;
                        }
                    }
                }

                // Fire the action
                if let Err(e) = self.fire(hook, worker) {
                    warn!(
                        "[hook_executor/{}] hook {} worker {} fire error: {e}",
                        self.workspace, hook.id, worker.id
                    );
                    continue;
                }

                // Record the fire time
                {
                    let mut map = self.last_fired.lock().unwrap();
                    map.insert(key, Utc::now());
                }
            }
        }
        Ok(())
    }

    /// Execute the action for a (hook, worker) pair.
    fn fire(&self, hook: &WorkerHook, worker: &Worker) -> Result<()> {
        info!(
            "[hook_executor/{}] firing hook {} (action={}) for worker {}",
            self.workspace, hook.id, hook.action, worker.id
        );

        match hook.action.as_str() {
            "nudge" => {
                let msg = "Your last update was a while ago. Please provide a status update or continue with your work.";
                run_swarm_send(&self.workspace_root, &worker.id, msg);
            }
            "notify" => {
                if let Some(ref tx) = self.event_tx {
                    let event = serde_json::json!({
                        "type": "worker_hook_fired",
                        "workspace": self.workspace,
                        "worker_id": worker.id,
                        "hook_id": hook.id,
                        "action": "notify",
                    });
                    let _ = tx.send(event);
                }
            }
            "create_pr" => {
                let msg = "Please create a pull request for your current work.";
                run_swarm_send(&self.workspace_root, &worker.id, msg);
            }
            "dispatch_auto_bot" => {
                if let Some(bot_id) = hook.auto_bot_id {
                    warn!(
                        "[hook_executor/{}] dispatch_auto_bot for bot_id={bot_id} is not fully \
                         implemented in this context — use the HTTP trigger endpoint instead",
                        self.workspace
                    );
                } else {
                    warn!(
                        "[hook_executor/{}] hook {} has action=dispatch_auto_bot but no auto_bot_id",
                        self.workspace, hook.id
                    );
                }
            }
            other => {
                warn!(
                    "[hook_executor/{}] unknown action '{}' for hook {}",
                    self.workspace, other, hook.id
                );
            }
        }

        Ok(())
    }
}

/// Invoke `swarm --dir {root} send {worker_id} "{msg}"`.
fn run_swarm_send(workspace_root: &PathBuf, worker_id: &str, message: &str) {
    match Command::new("swarm")
        .arg("--dir")
        .arg(workspace_root)
        .arg("send")
        .arg(worker_id)
        .arg(message)
        .output()
    {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!(
                    "[hook_executor] swarm send to {} failed: {}",
                    worker_id, stderr
                );
            }
        }
        Err(e) => {
            warn!(
                "[hook_executor] failed to run swarm send to {}: {e}",
                worker_id
            );
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::worker::{Worker, WorkerState};

    // ── helpers ──────────────────────────────────────────────────────────

    fn open_memory_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn make_store(conn: Arc<Mutex<Connection>>) -> WorkerHookStore {
        // Ensure schema
        {
            let c = conn.lock().unwrap();
            crate::buzz::worker::ensure_schema(&c).unwrap();
        }
        WorkerHookStore::new(conn)
    }

    fn make_worker(id: &str, workspace: &str, state: WorkerState) -> Worker {
        let now = Utc::now().to_rfc3339();
        Worker {
            id: id.to_string(),
            workspace: workspace.to_string(),
            state,
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
            label: String::new(),
            worktree_path: None,
            isolation_mode: None,
            agent_kind: None,
            repo_path: None,
        }
    }

    fn make_hook(
        id: i64,
        workspace: &str,
        trigger_state: Option<&str>,
        trigger_property: Option<&str>,
        trigger_value: Option<&str>,
        duration_minutes: Option<i64>,
        action: &str,
    ) -> WorkerHook {
        WorkerHook {
            id,
            workspace: workspace.to_string(),
            trigger_state: trigger_state.map(String::from),
            trigger_property: trigger_property.map(String::from),
            trigger_value: trigger_value.map(String::from),
            duration_minutes,
            action: action.to_string(),
            auto_bot_id: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    // ── WorkerHookStore tests ─────────────────────────────────────────────

    #[test]
    fn store_insert_list_delete() {
        let conn = open_memory_conn();
        let store = make_store(conn);

        let new_hook = NewWorkerHook {
            workspace: "acme".to_string(),
            trigger_state: Some("running".to_string()),
            trigger_property: Some("is_stalled".to_string()),
            trigger_value: Some("true".to_string()),
            duration_minutes: Some(15),
            action: "nudge".to_string(),
            auto_bot_id: None,
        };

        let inserted = store.insert(&new_hook).unwrap();
        assert_eq!(inserted.workspace, "acme");
        assert_eq!(inserted.action, "nudge");
        assert_eq!(inserted.trigger_state.as_deref(), Some("running"));

        let hooks = store.list("acme").unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].id, inserted.id);

        store.delete("acme", inserted.id).unwrap();
        let hooks = store.list("acme").unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn store_list_empty() {
        let conn = open_memory_conn();
        let store = make_store(conn);
        let hooks = store.list("nope").unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn store_workspace_isolation() {
        let conn = open_memory_conn();
        let store = make_store(conn);

        store
            .insert(&NewWorkerHook {
                workspace: "a".to_string(),
                trigger_state: None,
                trigger_property: None,
                trigger_value: None,
                duration_minutes: None,
                action: "notify".to_string(),
                auto_bot_id: None,
            })
            .unwrap();

        store
            .insert(&NewWorkerHook {
                workspace: "b".to_string(),
                trigger_state: None,
                trigger_property: None,
                trigger_value: None,
                duration_minutes: None,
                action: "notify".to_string(),
                auto_bot_id: None,
            })
            .unwrap();

        assert_eq!(store.list("a").unwrap().len(), 1);
        assert_eq!(store.list("b").unwrap().len(), 1);
        assert_eq!(store.list("c").unwrap().len(), 0);
    }

    // ── conditions_met tests ──────────────────────────────────────────────

    #[test]
    fn condition_state_match() {
        let hook = make_hook(1, "ws", Some("running"), None, None, None, "notify");
        let mut w = make_worker("w1", "ws", WorkerState::Running);
        assert!(conditions_met(&hook, &w));
        w.state = WorkerState::Waiting;
        assert!(!conditions_met(&hook, &w));
    }

    #[test]
    fn condition_property_is_stalled_true() {
        let hook = make_hook(
            1,
            "ws",
            None,
            Some("is_stalled"),
            Some("true"),
            None,
            "nudge",
        );
        let mut w = make_worker("w1", "ws", WorkerState::Running);
        assert!(!conditions_met(&hook, &w));
        w.is_stalled = true;
        assert!(conditions_met(&hook, &w));
    }

    #[test]
    fn condition_property_branch_ready() {
        let hook = make_hook(
            1,
            "ws",
            None,
            Some("branch_ready"),
            Some("true"),
            None,
            "notify",
        );
        let mut w = make_worker("w1", "ws", WorkerState::Waiting);
        assert!(!conditions_met(&hook, &w));
        w.branch_ready = true;
        assert!(conditions_met(&hook, &w));
    }

    #[test]
    fn condition_property_pr_url() {
        let hook = make_hook(1, "ws", None, Some("pr_url"), Some("true"), None, "notify");
        let mut w = make_worker("w1", "ws", WorkerState::Waiting);
        assert!(!conditions_met(&hook, &w));
        w.pr_url = Some("https://github.com/org/repo/pull/1".to_string());
        assert!(conditions_met(&hook, &w));
    }

    #[test]
    fn condition_duration_not_elapsed() {
        // state_entered_at is "now" — duration of 15 minutes hasn't elapsed.
        let hook = make_hook(1, "ws", Some("running"), None, None, Some(15), "nudge");
        let w = make_worker("w1", "ws", WorkerState::Running);
        // state_entered_at is Utc::now() from make_worker — 0 minutes elapsed
        assert!(!conditions_met(&hook, &w));
    }

    #[test]
    fn condition_duration_elapsed() {
        let hook = make_hook(1, "ws", Some("running"), None, None, Some(15), "nudge");
        let mut w = make_worker("w1", "ws", WorkerState::Running);
        // Set state_entered_at to 20 minutes ago
        let twenty_ago = Utc::now() - chrono::Duration::minutes(20);
        w.state_entered_at = twenty_ago.to_rfc3339();
        assert!(conditions_met(&hook, &w));
    }

    #[test]
    fn condition_all_must_match() {
        // Hook requires state=running AND is_stalled=true AND duration>=15min
        let hook = make_hook(
            1,
            "ws",
            Some("running"),
            Some("is_stalled"),
            Some("true"),
            Some(15),
            "nudge",
        );

        let mut w = make_worker("w1", "ws", WorkerState::Running);
        let twenty_ago = Utc::now() - chrono::Duration::minutes(20);
        w.state_entered_at = twenty_ago.to_rfc3339();
        w.is_stalled = true;

        // All conditions met
        assert!(conditions_met(&hook, &w));

        // Fail: wrong state
        let mut w2 = w.clone();
        w2.state = WorkerState::Waiting;
        assert!(!conditions_met(&hook, &w2));

        // Fail: is_stalled = false
        let mut w3 = w.clone();
        w3.is_stalled = false;
        assert!(!conditions_met(&hook, &w3));

        // Fail: duration not elapsed
        let mut w4 = w.clone();
        w4.state_entered_at = Utc::now().to_rfc3339();
        assert!(!conditions_met(&hook, &w4));
    }

    // ── Deduplication test ───────────────────────────────────────────────

    #[test]
    fn dedup_no_double_fire() {
        let conn = open_memory_conn();
        let hook_store = Arc::new(make_store(Arc::clone(&conn)));
        let worker_store = Arc::new(WorkerStore::new(Arc::clone(&conn)).unwrap());

        let executor = Arc::new(WorkerHookExecutor::new(
            Arc::clone(&hook_store),
            Arc::clone(&worker_store),
            None,
            "ws".to_string(),
            PathBuf::from("/tmp"),
        ));

        // Insert a hook with 0 duration_minutes (no cooldown beyond the 5-minute buffer)
        let hook = hook_store
            .insert(&NewWorkerHook {
                workspace: "ws".to_string(),
                trigger_state: Some("waiting".to_string()),
                trigger_property: None,
                trigger_value: None,
                duration_minutes: Some(0),
                action: "notify".to_string(),
                auto_bot_id: None,
            })
            .unwrap();

        // Insert a waiting worker whose state_entered_at is 10 minutes ago
        let mut w = make_worker("w1", "ws", WorkerState::Waiting);
        let ten_ago = Utc::now() - chrono::Duration::minutes(10);
        w.state_entered_at = ten_ago.to_rfc3339();
        worker_store.upsert(&w).unwrap();

        // First tick — should fire and record last_fired
        executor.tick().unwrap();
        {
            let map = executor.last_fired.lock().unwrap();
            assert!(map.contains_key(&(hook.id, "w1".to_string())));
        }

        // Immediate second tick — should NOT fire again (within cooldown window of 5 min)
        // We verify by checking the last_fired timestamp hasn't changed.
        let first_fired = {
            let map = executor.last_fired.lock().unwrap();
            *map.get(&(hook.id, "w1".to_string())).unwrap()
        };
        executor.tick().unwrap();
        let second_fired = {
            let map = executor.last_fired.lock().unwrap();
            *map.get(&(hook.id, "w1".to_string())).unwrap()
        };
        assert_eq!(
            first_fired, second_fired,
            "hook should not re-fire within cooldown"
        );
    }

    // ── Default hook seeding ─────────────────────────────────────────────

    #[test]
    fn seed_defaults_when_empty() {
        let conn = open_memory_conn();
        let hook_store = Arc::new(make_store(Arc::clone(&conn)));
        let worker_store = Arc::new(WorkerStore::new(Arc::clone(&conn)).unwrap());

        let executor = Arc::new(WorkerHookExecutor::new(
            Arc::clone(&hook_store),
            Arc::clone(&worker_store),
            None,
            "ws".to_string(),
            PathBuf::from("/tmp"),
        ));

        assert!(hook_store.list("ws").unwrap().is_empty());
        executor.seed_defaults_if_empty().unwrap();

        let hooks = hook_store.list("ws").unwrap();
        assert_eq!(hooks.len(), 2, "should seed 2 default hooks");
        assert_eq!(hooks[0].action, "nudge");
        assert_eq!(hooks[1].action, "notify");
    }

    #[test]
    fn seed_defaults_skipped_when_hooks_exist() {
        let conn = open_memory_conn();
        let hook_store = Arc::new(make_store(Arc::clone(&conn)));
        let worker_store = Arc::new(WorkerStore::new(Arc::clone(&conn)).unwrap());

        // Pre-insert one hook
        hook_store
            .insert(&NewWorkerHook {
                workspace: "ws".to_string(),
                trigger_state: None,
                trigger_property: None,
                trigger_value: None,
                duration_minutes: None,
                action: "notify".to_string(),
                auto_bot_id: None,
            })
            .unwrap();

        let executor = Arc::new(WorkerHookExecutor::new(
            Arc::clone(&hook_store),
            Arc::clone(&worker_store),
            None,
            "ws".to_string(),
            PathBuf::from("/tmp"),
        ));

        executor.seed_defaults_if_empty().unwrap();

        // Should still be just 1, not 3
        let hooks = hook_store.list("ws").unwrap();
        assert_eq!(
            hooks.len(),
            1,
            "defaults should not be seeded when hooks exist"
        );
    }
}

//! Worker state machine — DB-backed worker records with derived labels.
//!
//! Implements Phase 1A of the v2 rebuild spec. Workers are tasks dispatched
//! to swarm worktrees; their state is tracked here and reconciled from
//! `.swarm/state.json` every 5 seconds.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use color_eyre::eyre::{Result, WrapErr, eyre};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

// ── State enum ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Created,
    Briefed,
    Queued,
    Running,
    Waiting,
    Merged,
    Failed,
    Abandoned,
}

impl WorkerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkerState::Created => "created",
            WorkerState::Briefed => "briefed",
            WorkerState::Queued => "queued",
            WorkerState::Running => "running",
            WorkerState::Waiting => "waiting",
            WorkerState::Merged => "merged",
            WorkerState::Failed => "failed",
            WorkerState::Abandoned => "abandoned",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "created" => Some(WorkerState::Created),
            "briefed" => Some(WorkerState::Briefed),
            "queued" => Some(WorkerState::Queued),
            "running" => Some(WorkerState::Running),
            "waiting" => Some(WorkerState::Waiting),
            "merged" => Some(WorkerState::Merged),
            "failed" => Some(WorkerState::Failed),
            "abandoned" => Some(WorkerState::Abandoned),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkerState::Merged | WorkerState::Failed | WorkerState::Abandoned
        )
    }
}

// ── Worker record ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub id: String,
    pub workspace: String,
    pub state: WorkerState,
    /// Brief JSON blob — the contract delivered to swarm.
    pub brief: Option<serde_json::Value>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub goal: Option<String>,
    pub tests_passing: bool,
    pub branch_ready: bool,
    pub pr_url: Option<String>,
    pub pr_approved: bool,
    pub is_stalled: bool,
    pub revision_count: i64,
    /// `local_first` | `pr_first`
    pub review_mode: String,
    pub blocked_reason: Option<String>,
    pub last_output_at: Option<String>,
    pub state_entered_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// Derived display label — computed, never stored in DB.
    #[serde(skip_deserializing)]
    pub label: String,
}

// ── Derived label ──────────────────────────────────────────────────────

/// Compute the display label for a worker according to the v2 spec label table.
///
/// Priority order within `running`:
///   is_stalled wins over revision_count
///
/// Priority order within `waiting`:
///   blocked_reason wins (Needs input)
///   then pr_url-based conditions
///   then branch_ready (no PR yet)
pub fn derived_label(worker: &Worker) -> String {
    match worker.state {
        WorkerState::Running => {
            if worker.is_stalled {
                "Stalled".to_string()
            } else if worker.revision_count > 0 {
                format!("Revising (pass {})", worker.revision_count)
            } else {
                "Working".to_string()
            }
        }
        WorkerState::Waiting => {
            if worker.blocked_reason.is_some() {
                "Needs input".to_string()
            } else if let Some(ref _url) = worker.pr_url {
                if worker.tests_passing && worker.pr_approved {
                    "Ready to merge".to_string()
                } else if !worker.tests_passing {
                    "Tests failing".to_string()
                } else {
                    // pr_url set, tests passing but not approved yet → has feedback pending
                    "Has feedback".to_string()
                }
            } else if worker.branch_ready {
                "Ready for local review".to_string()
            } else {
                "Waiting".to_string()
            }
        }
        WorkerState::Merged => "Merged".to_string(),
        WorkerState::Failed => "Failed".to_string(),
        WorkerState::Abandoned => "Abandoned".to_string(),
        WorkerState::Created => "Created".to_string(),
        WorkerState::Briefed => "Briefed".to_string(),
        WorkerState::Queued => "Queued".to_string(),
    }
}

// ── Property update ────────────────────────────────────────────────────

/// A partial update to worker properties (state machine does not change).
#[derive(Debug, Default)]
pub struct WorkerPropertyUpdate {
    pub tests_passing: Option<bool>,
    pub branch_ready: Option<bool>,
    /// `Some(Some(url))` to set, `Some(None)` to clear.
    pub pr_url: Option<Option<String>>,
    pub pr_approved: Option<bool>,
    pub is_stalled: Option<bool>,
    pub increment_revision: bool,
    /// `Some(Some(reason))` to set, `Some(None)` to clear.
    pub blocked_reason: Option<Option<String>>,
    pub last_output_at: Option<String>,
    pub branch: Option<String>,
}

// ── DB schema ──────────────────────────────────────────────────────────

/// Ensure the `workers` and `worker_hooks` tables exist on the given connection.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS workers (
            id TEXT PRIMARY KEY,
            workspace TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'created',
            brief TEXT,
            repo TEXT,
            branch TEXT,
            goal TEXT,
            tests_passing BOOLEAN NOT NULL DEFAULT FALSE,
            branch_ready BOOLEAN NOT NULL DEFAULT FALSE,
            pr_url TEXT,
            pr_approved BOOLEAN NOT NULL DEFAULT FALSE,
            is_stalled BOOLEAN NOT NULL DEFAULT FALSE,
            revision_count INTEGER NOT NULL DEFAULT 0,
            review_mode TEXT NOT NULL DEFAULT 'local_first',
            blocked_reason TEXT,
            last_output_at DATETIME,
            state_entered_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS worker_hooks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace TEXT NOT NULL,
            trigger_state TEXT,
            trigger_property TEXT,
            trigger_value TEXT,
            duration_minutes INTEGER,
            action TEXT NOT NULL,
            auto_bot_id INTEGER,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        ",
    )
    .wrap_err("failed to create worker tables")?;
    Ok(())
}

// ── Store ──────────────────────────────────────────────────────────────

/// SQLite-backed worker store. The connection is shared behind Arc<Mutex<>> so
/// the reconciler background task can hold a reference without lifetime issues.
pub struct WorkerStore {
    conn: Arc<Mutex<Connection>>,
}

impl WorkerStore {
    /// Create a new store backed by the given shared connection.
    /// Schema is created if it doesn't exist.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        {
            let c = conn.lock().unwrap();
            ensure_schema(&c)?;
        }
        Ok(Self { conn })
    }

    /// Open an in-memory database for testing.
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let conn = Arc::new(Mutex::new(conn));
        Self::new(conn)
    }

    /// Open a database at the given path.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .wrap_err_with(|| format!("failed to open {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let conn = Arc::new(Mutex::new(conn));
        Self::new(conn)
    }

    /// Clone the inner Arc for use in background tasks.
    pub fn conn_arc(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Insert or replace a worker record.
    pub fn upsert(&self, worker: &Worker) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let brief_str = worker.brief.as_ref().map(|v| v.to_string());
        conn.execute(
            "INSERT INTO workers
             (id, workspace, state, brief, repo, branch, goal,
              tests_passing, branch_ready, pr_url, pr_approved, is_stalled,
              revision_count, review_mode, blocked_reason,
              last_output_at, state_entered_at, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
             ON CONFLICT(id) DO UPDATE SET
               workspace       = excluded.workspace,
               state           = excluded.state,
               brief           = excluded.brief,
               repo            = excluded.repo,
               branch          = excluded.branch,
               goal            = excluded.goal,
               tests_passing   = excluded.tests_passing,
               branch_ready    = excluded.branch_ready,
               pr_url          = excluded.pr_url,
               pr_approved     = excluded.pr_approved,
               is_stalled      = excluded.is_stalled,
               revision_count  = excluded.revision_count,
               review_mode     = excluded.review_mode,
               blocked_reason  = excluded.blocked_reason,
               last_output_at  = excluded.last_output_at,
               state_entered_at= excluded.state_entered_at,
               updated_at      = excluded.updated_at",
            params![
                worker.id,
                worker.workspace,
                worker.state.as_str(),
                brief_str,
                worker.repo,
                worker.branch,
                worker.goal,
                worker.tests_passing as i64,
                worker.branch_ready as i64,
                worker.pr_url,
                worker.pr_approved as i64,
                worker.is_stalled as i64,
                worker.revision_count,
                worker.review_mode,
                worker.blocked_reason,
                worker.last_output_at,
                worker.state_entered_at,
                worker.created_at,
                worker.updated_at,
            ],
        )
        .wrap_err("upsert worker")?;
        Ok(())
    }

    /// Fetch one worker by (workspace, id). Returns the record with a derived label.
    pub fn get(&self, workspace: &str, id: &str) -> Result<Option<Worker>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id,workspace,state,brief,repo,branch,goal,
                    tests_passing,branch_ready,pr_url,pr_approved,is_stalled,
                    revision_count,review_mode,blocked_reason,
                    last_output_at,state_entered_at,created_at,updated_at
             FROM workers WHERE workspace=?1 AND id=?2",
            params![workspace, id],
            row_to_worker,
        );
        match result {
            Ok(mut w) => {
                w.label = derived_label(&w);
                Ok(Some(w))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).wrap_err("get worker"),
        }
    }

    /// List all workers for a workspace, most-recently-updated first.
    pub fn list(&self, workspace: &str) -> Result<Vec<Worker>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,workspace,state,brief,repo,branch,goal,
                    tests_passing,branch_ready,pr_url,pr_approved,is_stalled,
                    revision_count,review_mode,blocked_reason,
                    last_output_at,state_entered_at,created_at,updated_at
             FROM workers WHERE workspace=?1
             ORDER BY updated_at DESC",
        )?;
        let workers = stmt
            .query_map(params![workspace], row_to_worker)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("list workers")?;
        Ok(workers
            .into_iter()
            .map(|mut w| {
                w.label = derived_label(&w);
                w
            })
            .collect())
    }

    /// Transition a worker to a new state.
    /// Also updates `state_entered_at` and `updated_at`.
    /// This is a forward-only guard: callers should check before calling.
    pub fn transition(&self, workspace: &str, id: &str, new_state: WorkerState) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute(
                "UPDATE workers
                 SET state=?1, state_entered_at=?2, updated_at=?2
                 WHERE workspace=?3 AND id=?4",
                params![new_state.as_str(), now, workspace, id],
            )
            .wrap_err("transition worker state")?;
        if rows == 0 {
            return Err(eyre!("worker not found: {workspace}/{id}"));
        }
        Ok(())
    }

    /// Update individual properties without changing the state.
    /// Only fields set to `Some(...)` are modified.
    pub fn update_properties(
        &self,
        workspace: &str,
        id: &str,
        update: WorkerPropertyUpdate,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();

        // Build a dynamic UPDATE statement
        let mut sets = Vec::<String>::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut idx = 1usize;

        macro_rules! push_field {
            ($field:expr, $col:literal) => {
                sets.push(format!("{} = ?{}", $col, idx));
                values.push(Box::new($field));
                idx += 1;
            };
        }

        if let Some(v) = update.tests_passing {
            push_field!(v as i64, "tests_passing");
        }
        if let Some(v) = update.branch_ready {
            push_field!(v as i64, "branch_ready");
        }
        if let Some(v) = update.pr_url {
            // v is Option<String>; store null if None
            sets.push(format!("pr_url = ?{idx}"));
            values.push(Box::new(v));
            idx += 1;
        }
        if let Some(v) = update.pr_approved {
            push_field!(v as i64, "pr_approved");
        }
        if let Some(v) = update.is_stalled {
            push_field!(v as i64, "is_stalled");
        }
        if update.increment_revision {
            sets.push("revision_count = revision_count + 1".to_string());
        }
        if let Some(v) = update.blocked_reason {
            sets.push(format!("blocked_reason = ?{idx}"));
            values.push(Box::new(v));
            idx += 1;
        }
        if let Some(v) = update.last_output_at {
            push_field!(v, "last_output_at");
        }
        if let Some(v) = update.branch {
            push_field!(v, "branch");
        }

        if sets.is_empty() {
            // Nothing to update
            return Ok(());
        }

        // Always bump updated_at
        sets.push(format!("updated_at = ?{idx}"));
        values.push(Box::new(now.clone()));
        idx += 1;

        let sql = format!(
            "UPDATE workers SET {} WHERE workspace = ?{} AND id = ?{}",
            sets.join(", "),
            idx,
            idx + 1
        );
        values.push(Box::new(workspace.to_string()));
        values.push(Box::new(id.to_string()));

        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let rows = conn
            .execute(&sql, refs.as_slice())
            .wrap_err("update worker properties")?;
        if rows == 0 {
            return Err(eyre!("worker not found: {workspace}/{id}"));
        }
        Ok(())
    }

    /// Replace a worker's UUID with the swarm-assigned ID.
    /// Deletes the old record and upserts under the new ID.
    pub fn rekey(&self, old_id: &str, new_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM workers WHERE id = ?1", params![old_id])
            .wrap_err("rekey: delete old worker")?;
        drop(conn);
        // Caller is responsible for upserting the record with the new ID.
        let _ = new_id; // used by caller
        Ok(())
    }
}

// ── Row mapping ────────────────────────────────────────────────────────

fn row_to_worker(row: &rusqlite::Row<'_>) -> rusqlite::Result<Worker> {
    let state_str: String = row.get(2)?;
    let state = WorkerState::from_str(&state_str).unwrap_or(WorkerState::Created);
    let brief_str: Option<String> = row.get(3)?;
    let brief = brief_str.and_then(|s| serde_json::from_str(&s).ok());

    Ok(Worker {
        id: row.get(0)?,
        workspace: row.get(1)?,
        state,
        brief,
        repo: row.get(4)?,
        branch: row.get(5)?,
        goal: row.get(6)?,
        tests_passing: row.get::<_, i64>(7)? != 0,
        branch_ready: row.get::<_, i64>(8)? != 0,
        pr_url: row.get(9)?,
        pr_approved: row.get::<_, i64>(10)? != 0,
        is_stalled: row.get::<_, i64>(11)? != 0,
        revision_count: row.get(12)?,
        review_mode: row.get(13)?,
        blocked_reason: row.get(14)?,
        last_output_at: row.get(15)?,
        state_entered_at: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        // label is filled in by the caller
        label: String::new(),
    })
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_worker(id: &str, workspace: &str) -> Worker {
        let now = Utc::now().to_rfc3339();
        Worker {
            id: id.to_string(),
            workspace: workspace.to_string(),
            state: WorkerState::Created,
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
            label: String::new(),
        }
    }

    // ── derived_label tests ───────────────────────────────────────────

    #[test]
    fn label_running_first_pass() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Running;
        w.revision_count = 0;
        w.is_stalled = false;
        assert_eq!(derived_label(&w), "Working");
    }

    #[test]
    fn label_running_revision() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Running;
        w.revision_count = 2;
        w.is_stalled = false;
        assert_eq!(derived_label(&w), "Revising (pass 2)");
    }

    #[test]
    fn label_running_stalled() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Running;
        w.is_stalled = true;
        assert_eq!(derived_label(&w), "Stalled");
    }

    #[test]
    fn label_waiting_branch_ready_no_pr() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Waiting;
        w.branch_ready = true;
        w.pr_url = None;
        assert_eq!(derived_label(&w), "Ready for local review");
    }

    #[test]
    fn label_waiting_has_feedback() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/org/repo/pull/1".to_string());
        w.tests_passing = true;
        w.pr_approved = false;
        assert_eq!(derived_label(&w), "Has feedback");
    }

    #[test]
    fn label_waiting_ready_to_merge() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/org/repo/pull/1".to_string());
        w.tests_passing = true;
        w.pr_approved = true;
        assert_eq!(derived_label(&w), "Ready to merge");
    }

    #[test]
    fn label_waiting_tests_failing() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Waiting;
        w.pr_url = Some("https://github.com/org/repo/pull/1".to_string());
        w.tests_passing = false;
        assert_eq!(derived_label(&w), "Tests failing");
    }

    #[test]
    fn label_waiting_needs_input() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Waiting;
        w.blocked_reason = Some("stuck on auth".to_string());
        assert_eq!(derived_label(&w), "Needs input");
    }

    #[test]
    fn label_merged() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Merged;
        assert_eq!(derived_label(&w), "Merged");
    }

    #[test]
    fn label_failed() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Failed;
        assert_eq!(derived_label(&w), "Failed");
    }

    #[test]
    fn label_abandoned() {
        let mut w = make_worker("w1", "acme");
        w.state = WorkerState::Abandoned;
        assert_eq!(derived_label(&w), "Abandoned");
    }

    // ── WorkerStore tests ─────────────────────────────────────────────

    #[test]
    fn test_list_empty() {
        let store = WorkerStore::open_memory().unwrap();
        let workers = store.list("acme").unwrap();
        assert!(workers.is_empty());
    }

    #[test]
    fn test_upsert_and_get() {
        let store = WorkerStore::open_memory().unwrap();
        let w = make_worker("w1", "acme");
        store.upsert(&w).unwrap();

        let fetched = store.get("acme", "w1").unwrap().unwrap();
        assert_eq!(fetched.id, "w1");
        assert_eq!(fetched.workspace, "acme");
        assert_eq!(fetched.state, WorkerState::Created);
        assert_eq!(fetched.label, "Created");
    }

    #[test]
    fn test_get_not_found() {
        let store = WorkerStore::open_memory().unwrap();
        let result = store.get("acme", "nope").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_multiple_workspaces() {
        let store = WorkerStore::open_memory().unwrap();
        store.upsert(&make_worker("w1", "acme")).unwrap();
        store.upsert(&make_worker("w2", "beta")).unwrap();
        store.upsert(&make_worker("w3", "acme")).unwrap();

        let acme = store.list("acme").unwrap();
        assert_eq!(acme.len(), 2);
        let beta = store.list("beta").unwrap();
        assert_eq!(beta.len(), 1);
    }

    #[test]
    fn test_transition() {
        let store = WorkerStore::open_memory().unwrap();
        store.upsert(&make_worker("w1", "acme")).unwrap();
        store
            .transition("acme", "w1", WorkerState::Running)
            .unwrap();
        let w = store.get("acme", "w1").unwrap().unwrap();
        assert_eq!(w.state, WorkerState::Running);
    }

    #[test]
    fn test_transition_not_found() {
        let store = WorkerStore::open_memory().unwrap();
        let result = store.transition("acme", "ghost", WorkerState::Running);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_properties_tests_passing() {
        let store = WorkerStore::open_memory().unwrap();
        store.upsert(&make_worker("w1", "acme")).unwrap();
        store
            .update_properties(
                "acme",
                "w1",
                WorkerPropertyUpdate {
                    tests_passing: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        let w = store.get("acme", "w1").unwrap().unwrap();
        assert!(w.tests_passing);
    }

    #[test]
    fn test_update_properties_pr_url() {
        let store = WorkerStore::open_memory().unwrap();
        store.upsert(&make_worker("w1", "acme")).unwrap();
        store
            .update_properties(
                "acme",
                "w1",
                WorkerPropertyUpdate {
                    pr_url: Some(Some("https://github.com/org/repo/pull/42".to_string())),
                    ..Default::default()
                },
            )
            .unwrap();
        let w = store.get("acme", "w1").unwrap().unwrap();
        assert_eq!(
            w.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
    }

    #[test]
    fn test_update_properties_increment_revision() {
        let store = WorkerStore::open_memory().unwrap();
        store.upsert(&make_worker("w1", "acme")).unwrap();
        store
            .update_properties(
                "acme",
                "w1",
                WorkerPropertyUpdate {
                    increment_revision: true,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .update_properties(
                "acme",
                "w1",
                WorkerPropertyUpdate {
                    increment_revision: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let w = store.get("acme", "w1").unwrap().unwrap();
        assert_eq!(w.revision_count, 2);
    }

    #[test]
    fn test_update_properties_clear_stalled() {
        let store = WorkerStore::open_memory().unwrap();
        let mut w = make_worker("w1", "acme");
        w.is_stalled = true;
        store.upsert(&w).unwrap();

        store
            .update_properties(
                "acme",
                "w1",
                WorkerPropertyUpdate {
                    is_stalled: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        let fetched = store.get("acme", "w1").unwrap().unwrap();
        assert!(!fetched.is_stalled);
    }

    #[test]
    fn test_upsert_updates_existing() {
        let store = WorkerStore::open_memory().unwrap();
        store.upsert(&make_worker("w1", "acme")).unwrap();
        let mut w2 = make_worker("w1", "acme");
        w2.state = WorkerState::Running;
        store.upsert(&w2).unwrap();

        let w = store.get("acme", "w1").unwrap().unwrap();
        assert_eq!(w.state, WorkerState::Running);
    }
}

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde_json::Value;

use crate::types::{Result, RunStatus, WorkflowRun};

pub(crate) struct WorkflowDb {
    conn: Connection,
}

impl WorkflowDb {
    /// Open (or create) a workflow DB at `path`. Pass `":memory:"` for tests.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_runs (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                step_name    TEXT NOT NULL,
                status       TEXT NOT NULL,
                context      TEXT NOT NULL,
                waiting_for  TEXT,
                park_until   TEXT,
                attempt      INTEGER NOT NULL DEFAULT 0,
                revision     INTEGER NOT NULL DEFAULT 0,
                error        TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_status
                ON workflow_runs(status);
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_waiting
                ON workflow_runs(waiting_for) WHERE waiting_for IS NOT NULL;",
        )?;
        Ok(())
    }

    pub fn insert(&self, run: &WorkflowRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_runs
                (id, kind, step_name, status, context, waiting_for, park_until,
                 attempt, revision, error, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                run.id,
                run.kind,
                run.step_name,
                run.status.as_str(),
                serde_json::to_string(&run.context).unwrap_or_else(|_| "{}".into()),
                run.waiting_for,
                run.park_until.map(|t| t.to_rfc3339()),
                run.attempt,
                run.revision,
                run.error,
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update(&self, run: &WorkflowRun) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET
                step_name   = ?2,
                status      = ?3,
                context     = ?4,
                waiting_for = ?5,
                park_until  = ?6,
                attempt     = ?7,
                revision    = ?8,
                error       = ?9,
                updated_at  = ?10
             WHERE id = ?1",
            params![
                run.id,
                run.step_name,
                run.status.as_str(),
                serde_json::to_string(&run.context).unwrap_or_else(|_| "{}".into()),
                run.waiting_for,
                run.park_until.map(|t| t.to_rfc3339()),
                run.attempt,
                run.revision,
                run.error,
                run.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<WorkflowRun> {
        self.conn
            .query_row(
                "SELECT id, kind, step_name, status, context, waiting_for, park_until,
                    attempt, revision, error, created_at, updated_at
             FROM workflow_runs WHERE id = ?1",
                params![id],
                row_to_run,
            )
            .map_err(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    crate::types::WorkflowError::RunNotFound(id.to_string())
                } else {
                    crate::types::WorkflowError::Db(e)
                }
            })
    }

    /// Find all parked runs whose `waiting_for` matches `pattern`.
    pub fn find_parked_by_signal(&self, pattern: &str) -> Result<Vec<WorkflowRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, step_name, status, context, waiting_for, park_until,
                    attempt, revision, error, created_at, updated_at
             FROM workflow_runs
             WHERE status = 'parked' AND waiting_for = ?1",
        )?;
        let runs = stmt
            .query_map(params![pattern], row_to_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(runs)
    }

    /// Find all parked runs whose `park_until` has passed.
    pub fn find_timed_out(&self, now: &DateTime<Utc>) -> Result<Vec<WorkflowRun>> {
        let now_str = now.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, step_name, status, context, waiting_for, park_until,
                    attempt, revision, error, created_at, updated_at
             FROM workflow_runs
             WHERE status = 'parked' AND park_until IS NOT NULL AND park_until <= ?1",
        )?;
        let runs = stmt
            .query_map(params![now_str], row_to_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(runs)
    }

    pub fn list_by_status(&self, status: &RunStatus) -> Result<Vec<WorkflowRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, step_name, status, context, waiting_for, park_until,
                    attempt, revision, error, created_at, updated_at
             FROM workflow_runs WHERE status = ?1",
        )?;
        let runs = stmt
            .query_map(params![status.as_str()], row_to_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(runs)
    }
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRun> {
    let status_str: String = row.get(3)?;
    let context_str: String = row.get(4)?;
    let park_until_str: Option<String> = row.get(6)?;
    let created_str: String = row.get(10)?;
    let updated_str: String = row.get(11)?;

    Ok(WorkflowRun {
        id: row.get(0)?,
        kind: row.get(1)?,
        step_name: row.get(2)?,
        status: RunStatus::parse(&status_str).unwrap_or(RunStatus::Failed),
        context: serde_json::from_str(&context_str).unwrap_or(Value::Object(Default::default())),
        waiting_for: row.get(5)?,
        park_until: park_until_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc)),
        attempt: row.get(7)?,
        revision: row.get(8)?,
        error: row.get(9)?,
        created_at: DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        updated_at: DateTime::parse_from_rfc3339(&updated_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    })
}

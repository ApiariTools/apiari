//! Auto bot entity — DB-backed store for autonomous bots triggered by cron or signals.
//!
//! Implements Phase 3A of the v2 rebuild spec.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use color_eyre::eyre::{Result, WrapErr, eyre};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoBot {
    pub id: String,
    pub workspace: String,
    pub name: String,
    pub color: String,
    pub trigger_type: String,
    pub cron_schedule: Option<String>,
    pub signal_source: Option<String>,
    pub signal_filter: Option<String>,
    pub prompt: String,
    pub provider: String,
    pub model: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Derived — not stored. "idle" | "running" | "error"
    #[serde(skip_deserializing)]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoBotRun {
    pub id: String,
    pub auto_bot_id: String,
    pub workspace: String,
    pub triggered_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<String>,
    pub summary: Option<String>,
    pub worker_id: Option<String>,
    /// LLM cost in USD for this run, if the provider reported it.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

// ── Schema ─────────────────────────────────────────────────────────────

/// Ensure auto bot tables exist on the given connection.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS auto_bots (
            id TEXT PRIMARY KEY,
            workspace TEXT NOT NULL,
            name TEXT NOT NULL,
            color TEXT NOT NULL DEFAULT '#f5c542',
            trigger_type TEXT NOT NULL,
            cron_schedule TEXT,
            signal_source TEXT,
            signal_filter TEXT,
            prompt TEXT NOT NULL,
            provider TEXT NOT NULL DEFAULT 'claude',
            model TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS auto_bot_runs (
            id TEXT PRIMARY KEY,
            auto_bot_id TEXT NOT NULL REFERENCES auto_bots(id),
            workspace TEXT NOT NULL,
            triggered_by TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            outcome TEXT,
            summary TEXT,
            worker_id TEXT,
            cost_usd REAL
        );
        ",
    )
    .wrap_err("failed to create auto_bot tables")?;

    // Migrate existing tables that predate the cost_usd column.
    // ALTER TABLE ADD COLUMN fails if the column exists, so we ignore that error.
    let _ = conn.execute("ALTER TABLE auto_bot_runs ADD COLUMN cost_usd REAL", []);

    Ok(())
}

// ── Store ──────────────────────────────────────────────────────────────

/// SQLite-backed auto bot store. Shared behind Arc<Mutex<>>.
pub struct AutoBotStore {
    conn: Arc<Mutex<Connection>>,
}

impl AutoBotStore {
    /// Create a new store, ensuring schema exists.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Ensure the schema exists (idempotent).
    pub fn ensure_schema(&self) -> Result<()> {
        let c = self.conn.lock().unwrap();
        ensure_schema(&c)
    }

    /// Open an in-memory database for testing.
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert or replace an auto bot record.
    pub fn upsert(&self, bot: &AutoBot) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO auto_bots
             (id, workspace, name, color, trigger_type, cron_schedule,
              signal_source, signal_filter, prompt, provider, model,
              enabled, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(id) DO UPDATE SET
               workspace     = excluded.workspace,
               name          = excluded.name,
               color         = excluded.color,
               trigger_type  = excluded.trigger_type,
               cron_schedule = excluded.cron_schedule,
               signal_source = excluded.signal_source,
               signal_filter = excluded.signal_filter,
               prompt        = excluded.prompt,
               provider      = excluded.provider,
               model         = excluded.model,
               enabled       = excluded.enabled,
               updated_at    = excluded.updated_at",
            params![
                bot.id,
                bot.workspace,
                bot.name,
                bot.color,
                bot.trigger_type,
                bot.cron_schedule,
                bot.signal_source,
                bot.signal_filter,
                bot.prompt,
                bot.provider,
                bot.model,
                bot.enabled as i64,
                bot.created_at,
                bot.updated_at,
            ],
        )
        .wrap_err("upsert auto_bot")?;
        Ok(())
    }

    /// Fetch one auto bot by (workspace, id), with derived status.
    pub fn get(&self, workspace: &str, id: &str) -> Result<Option<AutoBot>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id,workspace,name,color,trigger_type,cron_schedule,
                    signal_source,signal_filter,prompt,provider,model,enabled,
                    created_at,updated_at
             FROM auto_bots WHERE workspace=?1 AND id=?2",
            params![workspace, id],
            row_to_auto_bot,
        );
        match result {
            Ok(mut bot) => {
                bot.status = self.derive_status_locked(&conn, &bot.id)?;
                Ok(Some(bot))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).wrap_err("get auto_bot"),
        }
    }

    /// List all auto bots for a workspace, ordered by created_at DESC.
    pub fn list(&self, workspace: &str) -> Result<Vec<AutoBot>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,workspace,name,color,trigger_type,cron_schedule,
                    signal_source,signal_filter,prompt,provider,model,enabled,
                    created_at,updated_at
             FROM auto_bots WHERE workspace=?1
             ORDER BY created_at DESC",
        )?;
        let mut bots = stmt
            .query_map(params![workspace], row_to_auto_bot)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("list auto_bots")?;
        for bot in &mut bots {
            bot.status = self.derive_status_locked(&conn, &bot.id)?;
        }
        Ok(bots)
    }

    /// Delete an auto bot (and its runs) by (workspace, id).
    pub fn delete(&self, workspace: &str, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // Runs reference the bot via FK; delete runs first.
        conn.execute(
            "DELETE FROM auto_bot_runs WHERE auto_bot_id=?1",
            params![id],
        )
        .wrap_err("delete auto_bot_runs")?;
        let rows = conn
            .execute(
                "DELETE FROM auto_bots WHERE workspace=?1 AND id=?2",
                params![workspace, id],
            )
            .wrap_err("delete auto_bot")?;
        if rows == 0 {
            return Err(eyre!("auto_bot not found: {workspace}/{id}"));
        }
        Ok(())
    }

    /// Insert a new run record (started, no finished_at yet).
    pub fn insert_run(&self, run: &AutoBotRun) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO auto_bot_runs
             (id, auto_bot_id, workspace, triggered_by, started_at,
              finished_at, outcome, summary, worker_id, cost_usd)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                run.id,
                run.auto_bot_id,
                run.workspace,
                run.triggered_by,
                run.started_at,
                run.finished_at,
                run.outcome,
                run.summary,
                run.worker_id,
                run.cost_usd,
            ],
        )
        .wrap_err("insert auto_bot_run")?;
        Ok(())
    }

    /// Finish an existing run — set finished_at, outcome, summary, worker_id, cost_usd.
    pub fn finish_run(
        &self,
        run_id: &str,
        outcome: &str,
        summary: &str,
        worker_id: Option<&str>,
        cost_usd: Option<f64>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute(
                "UPDATE auto_bot_runs
                 SET finished_at=?1, outcome=?2, summary=?3, worker_id=?4, cost_usd=?5
                 WHERE id=?6",
                params![now, outcome, summary, worker_id, cost_usd, run_id],
            )
            .wrap_err("finish auto_bot_run")?;
        if rows == 0 {
            return Err(eyre!("auto_bot_run not found: {run_id}"));
        }
        Ok(())
    }

    /// Count how many of the most recent completed runs for a bot ended in `"error"`,
    /// stopping at the first non-error outcome. Used to trigger failure alerts.
    pub fn count_consecutive_failures(&self, auto_bot_id: &str) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT outcome FROM auto_bot_runs
             WHERE auto_bot_id=?1 AND finished_at IS NOT NULL
             ORDER BY started_at DESC LIMIT 10",
        )?;
        let outcomes: Vec<Option<String>> = stmt
            .query_map(params![auto_bot_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut count = 0u32;
        for outcome in &outcomes {
            if outcome.as_deref() == Some("error") {
                count += 1;
            } else {
                break;
            }
        }
        Ok(count)
    }

    /// List the N most recent runs for a given auto_bot_id.
    pub fn list_runs(&self, auto_bot_id: &str, limit: usize) -> Result<Vec<AutoBotRun>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,auto_bot_id,workspace,triggered_by,started_at,
                    finished_at,outcome,summary,worker_id,cost_usd
             FROM auto_bot_runs WHERE auto_bot_id=?1
             ORDER BY started_at DESC
             LIMIT ?2",
        )?;
        let runs = stmt
            .query_map(params![auto_bot_id, limit as i64], row_to_run)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("list auto_bot_runs")?;
        Ok(runs)
    }

    /// Get the single most recent run for an auto_bot_id, or None.
    pub fn latest_run(&self, auto_bot_id: &str) -> Result<Option<AutoBotRun>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id,auto_bot_id,workspace,triggered_by,started_at,
                    finished_at,outcome,summary,worker_id,cost_usd
             FROM auto_bot_runs WHERE auto_bot_id=?1
             ORDER BY started_at DESC
             LIMIT 1",
            params![auto_bot_id],
            row_to_run,
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).wrap_err("latest auto_bot_run"),
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Derive status from the latest run — must be called while holding the conn lock.
    fn derive_status_locked(&self, conn: &Connection, auto_bot_id: &str) -> Result<String> {
        let result = conn.query_row(
            "SELECT finished_at, outcome FROM auto_bot_runs
             WHERE auto_bot_id=?1
             ORDER BY started_at DESC LIMIT 1",
            params![auto_bot_id],
            |row| {
                let finished_at: Option<String> = row.get(0)?;
                let outcome: Option<String> = row.get(1)?;
                Ok((finished_at, outcome))
            },
        );
        match result {
            Ok((None, _)) => Ok("running".to_string()),
            Ok((Some(_), Some(o))) if o == "error" => Ok("error".to_string()),
            Ok(_) => Ok("idle".to_string()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok("idle".to_string()),
            Err(e) => Err(e).wrap_err("derive auto_bot status"),
        }
    }
}

// ── Row mapping ────────────────────────────────────────────────────────

fn row_to_auto_bot(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutoBot> {
    Ok(AutoBot {
        id: row.get(0)?,
        workspace: row.get(1)?,
        name: row.get(2)?,
        color: row.get(3)?,
        trigger_type: row.get(4)?,
        cron_schedule: row.get(5)?,
        signal_source: row.get(6)?,
        signal_filter: row.get(7)?,
        prompt: row.get(8)?,
        provider: row.get(9)?,
        model: row.get(10)?,
        enabled: row.get::<_, i64>(11)? != 0,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        status: String::new(), // filled by caller
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutoBotRun> {
    Ok(AutoBotRun {
        id: row.get(0)?,
        auto_bot_id: row.get(1)?,
        workspace: row.get(2)?,
        triggered_by: row.get(3)?,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        outcome: row.get(6)?,
        summary: row.get(7)?,
        worker_id: row.get(8)?,
        cost_usd: row.get(9).unwrap_or(None),
    })
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bot(id: &str, workspace: &str, trigger_type: &str) -> AutoBot {
        let now = Utc::now().to_rfc3339();
        AutoBot {
            id: id.to_string(),
            workspace: workspace.to_string(),
            name: format!("Bot {id}"),
            color: "#f5c542".to_string(),
            trigger_type: trigger_type.to_string(),
            cron_schedule: if trigger_type == "cron" {
                Some("0 9 * * 1-5".to_string())
            } else {
                None
            },
            signal_source: if trigger_type == "signal" {
                Some("github".to_string())
            } else {
                None
            },
            signal_filter: None,
            prompt: "Do something useful".to_string(),
            provider: "claude".to_string(),
            model: None,
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
            status: String::new(),
        }
    }

    fn make_run(id: &str, bot_id: &str, workspace: &str) -> AutoBotRun {
        let now = Utc::now().to_rfc3339();
        AutoBotRun {
            id: id.to_string(),
            auto_bot_id: bot_id.to_string(),
            workspace: workspace.to_string(),
            triggered_by: "cron".to_string(),
            started_at: now,
            finished_at: None,
            outcome: None,
            summary: None,
            worker_id: None,
            cost_usd: None,
        }
    }

    #[test]
    fn test_schema_creates_cleanly() {
        AutoBotStore::open_memory().unwrap();
    }

    #[test]
    fn test_list_empty() {
        let store = AutoBotStore::open_memory().unwrap();
        let bots = store.list("acme").unwrap();
        assert!(bots.is_empty());
    }

    #[test]
    fn test_upsert_and_get() {
        let store = AutoBotStore::open_memory().unwrap();
        let bot = make_bot("b1", "acme", "cron");
        store.upsert(&bot).unwrap();

        let fetched = store.get("acme", "b1").unwrap().unwrap();
        assert_eq!(fetched.id, "b1");
        assert_eq!(fetched.workspace, "acme");
        assert_eq!(fetched.trigger_type, "cron");
        assert_eq!(fetched.status, "idle");
    }

    #[test]
    fn test_get_not_found() {
        let store = AutoBotStore::open_memory().unwrap();
        let result = store.get("acme", "nope").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_multiple_workspaces() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.upsert(&make_bot("b2", "beta", "signal")).unwrap();
        store.upsert(&make_bot("b3", "acme", "signal")).unwrap();

        let acme = store.list("acme").unwrap();
        assert_eq!(acme.len(), 2);
        let beta = store.list("beta").unwrap();
        assert_eq!(beta.len(), 1);
    }

    #[test]
    fn test_upsert_updates_existing() {
        let store = AutoBotStore::open_memory().unwrap();
        let bot = make_bot("b1", "acme", "cron");
        store.upsert(&bot).unwrap();

        let mut updated = bot.clone();
        updated.name = "Updated Bot".to_string();
        updated.enabled = false;
        updated.updated_at = Utc::now().to_rfc3339();
        store.upsert(&updated).unwrap();

        let fetched = store.get("acme", "b1").unwrap().unwrap();
        assert_eq!(fetched.name, "Updated Bot");
        assert!(!fetched.enabled);
    }

    #[test]
    fn test_delete() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.delete("acme", "b1").unwrap();
        assert!(store.get("acme", "b1").unwrap().is_none());
    }

    #[test]
    fn test_delete_not_found() {
        let store = AutoBotStore::open_memory().unwrap();
        let result = store.delete("acme", "ghost");
        assert!(result.is_err());
    }

    #[test]
    fn test_insert_run_and_list() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();

        let run = make_run("r1", "b1", "acme");
        store.insert_run(&run).unwrap();

        let runs = store.list_runs("b1", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, "r1");
        assert!(runs[0].finished_at.is_none());
    }

    #[test]
    fn test_finish_run() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();

        store
            .finish_run("r1", "notified", "Sent summary.", None, None)
            .unwrap();

        let runs = store.list_runs("b1", 10).unwrap();
        assert_eq!(runs[0].outcome.as_deref(), Some("notified"));
        assert_eq!(runs[0].summary.as_deref(), Some("Sent summary."));
        assert!(runs[0].finished_at.is_some());
    }

    #[test]
    fn test_finish_run_with_worker() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();

        store
            .finish_run("r1", "dispatched_worker", "Dispatched.", Some("w-42"), None)
            .unwrap();

        let runs = store.list_runs("b1", 10).unwrap();
        assert_eq!(runs[0].worker_id.as_deref(), Some("w-42"));
    }

    #[test]
    fn test_latest_run_none_when_empty() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        let latest = store.latest_run("b1").unwrap();
        assert!(latest.is_none());
    }

    #[test]
    fn test_latest_run_returns_most_recent() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();

        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();

        // Give second run a slightly later timestamp
        let mut r2 = make_run("r2", "b1", "acme");
        // bump started_at by faking it into the future
        let later = (Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
        r2.started_at = later;
        store.insert_run(&r2).unwrap();

        let latest = store.latest_run("b1").unwrap().unwrap();
        assert_eq!(latest.id, "r2");
    }

    // ── Status derivation ─────────────────────────────────────────────

    #[test]
    fn test_status_idle_no_runs() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        let bot = store.get("acme", "b1").unwrap().unwrap();
        assert_eq!(bot.status, "idle");
    }

    #[test]
    fn test_status_running_when_run_in_flight() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();
        // No finished_at → running
        let bot = store.get("acme", "b1").unwrap().unwrap();
        assert_eq!(bot.status, "running");
    }

    #[test]
    fn test_status_idle_after_finished_run() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();
        store
            .finish_run("r1", "notified", "ok", None, None)
            .unwrap();
        let bot = store.get("acme", "b1").unwrap().unwrap();
        assert_eq!(bot.status, "idle");
    }

    #[test]
    fn test_status_error_when_last_run_errored() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();
        store
            .finish_run("r1", "error", "something failed", None, None)
            .unwrap();
        let bot = store.get("acme", "b1").unwrap().unwrap();
        assert_eq!(bot.status, "error");
    }

    #[test]
    fn test_delete_cascades_runs() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();
        store.delete("acme", "b1").unwrap();
        assert!(store.get("acme", "b1").unwrap().is_none());
        // Runs should also be gone (no FK error on reinsert etc.)
    }

    #[test]
    fn test_list_runs_limit() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();

        for i in 0..5u8 {
            let mut r = make_run(&format!("r{i}"), "b1", "acme");
            let t = (Utc::now() + chrono::Duration::seconds(i as i64)).to_rfc3339();
            r.started_at = t;
            store.insert_run(&r).unwrap();
        }

        let runs = store.list_runs("b1", 3).unwrap();
        assert_eq!(runs.len(), 3);
    }

    // ── Consecutive failure counting ───────────────────────────────────

    #[test]
    fn test_consecutive_failures_zero_when_no_runs() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        assert_eq!(store.count_consecutive_failures("b1").unwrap(), 0);
    }

    #[test]
    fn test_consecutive_failures_counts_leading_errors() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();

        // oldest run: success
        let mut r0 = make_run("r0", "b1", "acme");
        r0.started_at = (Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        store.insert_run(&r0).unwrap();
        store
            .finish_run("r0", "notified", "ok", None, None)
            .unwrap();

        // two recent errors
        let mut r1 = make_run("r1", "b1", "acme");
        r1.started_at = (Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
        store.insert_run(&r1).unwrap();
        store.finish_run("r1", "error", "fail", None, None).unwrap();

        let mut r2 = make_run("r2", "b1", "acme");
        r2.started_at = Utc::now().to_rfc3339();
        store.insert_run(&r2).unwrap();
        store.finish_run("r2", "error", "fail", None, None).unwrap();

        // Streak = 2 (not 3 — r0 breaks it)
        assert_eq!(store.count_consecutive_failures("b1").unwrap(), 2);
    }

    #[test]
    fn test_consecutive_failures_resets_after_success() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();

        let mut r1 = make_run("r1", "b1", "acme");
        r1.started_at = (Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        store.insert_run(&r1).unwrap();
        store.finish_run("r1", "error", "fail", None, None).unwrap();

        // Success resets the streak
        let mut r2 = make_run("r2", "b1", "acme");
        r2.started_at = Utc::now().to_rfc3339();
        store.insert_run(&r2).unwrap();
        store
            .finish_run("r2", "notified", "ok", None, None)
            .unwrap();

        assert_eq!(store.count_consecutive_failures("b1").unwrap(), 0);
    }

    // ── Cost tracking ─────────────────────────────────────────────────

    #[test]
    fn test_cost_usd_stored_and_retrieved() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();
        store
            .finish_run("r1", "notified", "ok", None, Some(0.0042))
            .unwrap();

        let runs = store.list_runs("b1", 10).unwrap();
        assert!((runs[0].cost_usd.unwrap() - 0.0042).abs() < 1e-9);
    }

    #[test]
    fn test_cost_usd_none_when_not_set() {
        let store = AutoBotStore::open_memory().unwrap();
        store.upsert(&make_bot("b1", "acme", "cron")).unwrap();
        store.insert_run(&make_run("r1", "b1", "acme")).unwrap();
        store
            .finish_run("r1", "notified", "ok", None, None)
            .unwrap();

        let runs = store.list_runs("b1", 10).unwrap();
        assert!(runs[0].cost_usd.is_none());
    }
}

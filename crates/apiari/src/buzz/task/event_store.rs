//! SQLite-backed activity event store.
//!
//! Records task lifecycle events and workspace-level activity for the
//! per-task timeline and workspace activity feed.
//!
//! Uses a separate `activity_events` table (not `task_events`) so it can carry
//! richer fields: workspace, summary, source, and a JSON metadata blob.

use std::path::Path;

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr};
use rusqlite::{Connection, params};

/// A single activity event in the task lifecycle or workspace feed.
#[derive(Debug, Clone)]
pub struct ActivityEvent {
    pub id: i64,
    pub workspace: String,
    pub task_id: Option<String>,
    /// One of: `stage_change`, `signal`, `worker`, `review`, `pr`, `note`.
    pub event_type: String,
    pub summary: String,
    pub detail: Option<String>,
    pub source: Option<String>,
    pub signal_id: Option<i64>,
    /// JSON blob for structured data (from_stage, to_stage, verdict, etc.).
    pub metadata: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// SQLite-backed activity event store.
pub struct ActivityEventStore {
    conn: Connection,
}

impl ActivityEventStore {
    /// Open an activity event store against the given DB file.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .wrap_err_with(|| format!("failed to open {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self { conn };
        Self::ensure_schema(&store.conn)?;
        Ok(store)
    }

    /// Open an in-memory store (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        Self::ensure_schema(&store.conn)?;
        Ok(store)
    }

    /// Create the `activity_events` table if it doesn't exist.
    ///
    /// Called from `TaskStore::ensure_schema` so the table is always present
    /// on the shared database connection.
    pub fn ensure_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS activity_events (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace   TEXT NOT NULL,
                task_id     TEXT,
                event_type  TEXT NOT NULL,
                summary     TEXT NOT NULL,
                detail      TEXT,
                source      TEXT,
                signal_id   INTEGER,
                metadata    TEXT,
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_activity_events_task
                ON activity_events(workspace, task_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_activity_events_workspace
                ON activity_events(workspace, created_at);
            ",
        )
        .wrap_err("failed to create activity_events table")?;
        Ok(())
    }

    /// Log an activity event.
    #[allow(clippy::too_many_arguments)]
    pub fn log_event(
        &self,
        workspace: &str,
        task_id: Option<&str>,
        event_type: &str,
        summary: &str,
        detail: Option<&str>,
        source: Option<&str>,
        signal_id: Option<i64>,
        metadata: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO activity_events
             (workspace, task_id, event_type, summary, detail, source, signal_id, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                workspace, task_id, event_type, summary, detail, source, signal_id, metadata, now
            ],
        )?;
        Ok(())
    }

    /// Get all events for a specific task (chronological, oldest first).
    pub fn get_task_timeline(&self, workspace: &str, task_id: &str) -> Result<Vec<ActivityEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, task_id, event_type, summary, detail, source, signal_id, metadata, created_at
             FROM activity_events
             WHERE workspace = ?1 AND task_id = ?2
             ORDER BY created_at ASC",
        )?;
        let events = stmt
            .query_map(params![workspace, task_id], row_to_activity_event)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to query task timeline")?;
        Ok(events)
    }

    /// Get workspace-wide activity feed (newest first).
    pub fn get_activity_feed(
        &self,
        workspace: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ActivityEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, task_id, event_type, summary, detail, source, signal_id, metadata, created_at
             FROM activity_events
             WHERE workspace = ?1
             ORDER BY created_at DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let events = stmt
            .query_map(params![workspace, limit, offset], row_to_activity_event)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to query activity feed")?;
        Ok(events)
    }

    /// Delete events older than `retention_days` days for a workspace.
    pub fn prune(&self, workspace: &str, retention_days: u32) -> Result<()> {
        self.conn.execute(
            "DELETE FROM activity_events
             WHERE workspace = ?1
               AND created_at < datetime('now', ?2)",
            params![workspace, format!("-{retention_days} days")],
        )?;
        Ok(())
    }
}

fn row_to_activity_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityEvent> {
    let created_at: String = row.get(9)?;
    Ok(ActivityEvent {
        id: row.get(0)?,
        workspace: row.get(1)?,
        task_id: row.get(2)?,
        event_type: row.get(3)?,
        summary: row.get(4)?,
        detail: row.get(5)?,
        source: row.get(6)?,
        signal_id: row.get(7)?,
        metadata: row.get(8)?,
        created_at: created_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_and_get_activity_feed() {
        let store = ActivityEventStore::open_memory().unwrap();
        store
            .log_event(
                "acme",
                Some("task-1"),
                "stage_change",
                "Triage → In Progress",
                None,
                Some("swarm"),
                None,
                Some(r#"{"from":"Triage","to":"In Progress"}"#),
            )
            .unwrap();
        store
            .log_event(
                "acme",
                None,
                "signal",
                "New sentry error",
                None,
                Some("sentry"),
                Some(42),
                None,
            )
            .unwrap();

        let feed = store.get_activity_feed("acme", 50, 0).unwrap();
        assert_eq!(feed.len(), 2);
        // newest first
        assert_eq!(feed[0].summary, "New sentry error");
        assert_eq!(feed[1].summary, "Triage → In Progress");
    }

    #[test]
    fn test_get_task_timeline() {
        let store = ActivityEventStore::open_memory().unwrap();
        store
            .log_event(
                "acme",
                Some("task-1"),
                "stage_change",
                "Triage → In Progress",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .log_event(
                "acme",
                Some("task-1"),
                "worker",
                "Reviewer dispatched",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .log_event(
                "acme",
                Some("task-2"),
                "signal",
                "Other task event",
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let timeline = store.get_task_timeline("acme", "task-1").unwrap();
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].event_type, "stage_change");
        assert_eq!(timeline[1].event_type, "worker");
    }

    #[test]
    fn test_prune() {
        let store = ActivityEventStore::open_memory().unwrap();
        // Insert an old event directly with a past timestamp
        store
            .conn
            .execute(
                "INSERT INTO activity_events (workspace, event_type, summary, created_at)
                 VALUES ('acme', 'note', 'Old event', datetime('now', '-31 days'))",
                [],
            )
            .unwrap();
        store
            .log_event("acme", None, "note", "Recent event", None, None, None, None)
            .unwrap();

        store.prune("acme", 30).unwrap();

        let feed = store.get_activity_feed("acme", 50, 0).unwrap();
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].summary, "Recent event");
    }

    #[test]
    fn test_workspace_isolation() {
        let store = ActivityEventStore::open_memory().unwrap();
        store
            .log_event(
                "ws-a",
                None,
                "note",
                "Workspace A event",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .log_event(
                "ws-b",
                None,
                "note",
                "Workspace B event",
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let feed_a = store.get_activity_feed("ws-a", 50, 0).unwrap();
        assert_eq!(feed_a.len(), 1);
        assert_eq!(feed_a[0].summary, "Workspace A event");
    }
}

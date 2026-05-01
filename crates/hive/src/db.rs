use color_eyre::Result;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Database with separate read/write access.
/// SQLite WAL mode allows concurrent readers + one writer.
#[derive(Clone)]
pub struct Db {
    writer: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS conversations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        workspace TEXT NOT NULL,
        bot TEXT NOT NULL,
        role TEXT NOT NULL,
        content TEXT NOT NULL,
        attachments TEXT,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
    );

    CREATE TABLE IF NOT EXISTS sessions (
        workspace TEXT NOT NULL,
        bot TEXT NOT NULL,
        session_id TEXT NOT NULL,
        prompt_hash TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        PRIMARY KEY (workspace, bot)
    );

    CREATE TABLE IF NOT EXISTS bot_status (
        workspace TEXT NOT NULL,
        bot TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'idle',
        streaming_content TEXT NOT NULL DEFAULT '',
        tool_name TEXT,
        updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        PRIMARY KEY (workspace, bot)
    );

    CREATE TABLE IF NOT EXISTS signals (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        workspace TEXT NOT NULL,
        source TEXT NOT NULL,
        external_id TEXT NOT NULL,
        title TEXT NOT NULL,
        body TEXT,
        severity TEXT NOT NULL DEFAULT 'info',
        status TEXT NOT NULL DEFAULT 'open',
        url TEXT,
        metadata TEXT,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        UNIQUE(workspace, source, external_id)
    );

    CREATE TABLE IF NOT EXISTS last_seen (
        workspace TEXT NOT NULL,
        bot TEXT NOT NULL,
        message_id INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (workspace, bot)
    );

    CREATE TABLE IF NOT EXISTS schedule_last_run (
        workspace TEXT NOT NULL,
        bot TEXT NOT NULL,
        last_run_at TEXT NOT NULL,
        PRIMARY KEY (workspace, bot)
    );

    CREATE INDEX IF NOT EXISTS idx_conversations_ws_bot_role
        ON conversations(workspace, bot, role);
";

fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
    )?;
    Ok(conn)
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_conn(path)?;
        conn.execute_batch(SCHEMA)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(conn)),
            db_path: path.to_path_buf(),
        })
    }

    /// Open a fresh read-only connection. Doesn't block the writer.
    pub fn reader(&self) -> Result<Connection> {
        let conn = open_conn(&self.db_path)?;
        Ok(conn)
    }

    // ── Writes (use the shared writer) ──

    pub fn add_message(
        &self,
        workspace: &str,
        bot: &str,
        role: &str,
        content: &str,
        attachments: Option<&str>,
    ) -> Result<i64> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (workspace, bot, role, content, attachments)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workspace, bot, role, content, attachments],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_session(
        &self,
        workspace: &str,
        bot: &str,
        session_id: &str,
        prompt_hash: &str,
    ) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (workspace, bot, session_id, prompt_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               session_id = ?3, prompt_hash = ?4, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![workspace, bot, session_id, prompt_hash],
        )?;
        Ok(())
    }

    pub fn set_bot_status(
        &self,
        workspace: &str,
        bot: &str,
        status: &str,
        streaming_content: &str,
        tool_name: Option<&str>,
    ) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "INSERT INTO bot_status (workspace, bot, status, streaming_content, tool_name, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               status = ?3, streaming_content = ?4, tool_name = ?5, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![workspace, bot, status, streaming_content, tool_name],
        )?;
        Ok(())
    }

    pub fn set_schedule_last_run(
        &self,
        workspace: &str,
        bot: &str,
        last_run_at: &str,
    ) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "INSERT INTO schedule_last_run (workspace, bot, last_run_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace, bot) DO UPDATE SET last_run_at = ?3",
            params![workspace, bot, last_run_at],
        )?;
        Ok(())
    }

    pub fn get_schedule_last_run(&self, workspace: &str, bot: &str) -> Result<Option<String>> {
        let conn = self.reader()?;
        let result = conn.query_row(
            "SELECT last_run_at FROM schedule_last_run WHERE workspace = ?1 AND bot = ?2",
            params![workspace, bot],
            |row| row.get(0),
        );
        match result {
            Ok(ts) => Ok(Some(ts)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn append_streaming(&self, workspace: &str, bot: &str, text: &str) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "UPDATE bot_status SET streaming_content = streaming_content || ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE workspace = ?2 AND bot = ?3",
            params![text, workspace, bot],
        )?;
        Ok(())
    }

    // ── Reads (use fresh connections, never block the writer) ──

    pub fn get_conversations(
        &self,
        workspace: &str,
        bot: &str,
        limit: i64,
    ) -> Result<Vec<MessageRow>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT id, workspace, bot, role, content, attachments, created_at
             FROM conversations
             WHERE workspace = ?1 AND bot = ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![workspace, bot, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    bot: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    attachments: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
    }

    pub fn count_assistant_messages(&self, workspace: &str, bot: &str) -> Result<i64> {
        let conn = self.reader()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversations WHERE workspace = ?1 AND bot = ?2 AND role = 'assistant'",
            params![workspace, bot],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn get_message_content(&self, message_id: i64) -> Result<Option<String>> {
        let conn = self.reader()?;
        let result = conn.query_row(
            "SELECT content FROM conversations WHERE id = ?1",
            params![message_id],
            |row| row.get(0),
        );
        match result {
            Ok(content) => Ok(Some(content)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_session_id(
        &self,
        workspace: &str,
        bot: &str,
        current_hash: &str,
    ) -> Result<Option<String>> {
        let conn = self.reader()?;
        let result = conn.query_row(
            "SELECT session_id, prompt_hash FROM sessions WHERE workspace = ?1 AND bot = ?2",
            params![workspace, bot],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok((id, stored_hash)) => {
                if stored_hash == current_hash {
                    Ok(Some(id))
                } else {
                    tracing::info!(
                        "[session] prompt changed for {workspace}/{bot}, starting fresh"
                    );
                    let _ = self.add_message(
                        workspace,
                        bot,
                        "system",
                        "Session reset — bot configuration was updated.",
                        None,
                    );
                    Ok(None)
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_bot_status(&self, workspace: &str, bot: &str) -> Result<Option<BotStatus>> {
        let conn = self.reader()?;
        let result = conn.query_row(
            "SELECT status, streaming_content, tool_name FROM bot_status
             WHERE workspace = ?1 AND bot = ?2",
            params![workspace, bot],
            |row| {
                Ok(BotStatus {
                    status: row.get(0)?,
                    streaming_content: row.get(1)?,
                    tool_name: row.get(2)?,
                })
            },
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn search_conversations(
        &self,
        workspace: &str,
        bot: &str,
        query: &str,
        limit: i64,
    ) -> Result<Vec<MessageRow>> {
        let conn = self.reader()?;
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT id, workspace, bot, role, content, attachments, created_at
             FROM conversations
             WHERE workspace = ?1 AND bot = ?2 AND content LIKE ?3
             ORDER BY id DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![workspace, bot, pattern, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    bot: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    attachments: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
    }

    pub fn mark_seen(&self, workspace: &str, bot: &str) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "INSERT INTO last_seen (workspace, bot, message_id)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(id), 0) FROM conversations WHERE workspace = ?1 AND bot = ?2))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               message_id = (SELECT COALESCE(MAX(id), 0) FROM conversations WHERE workspace = ?1 AND bot = ?2)",
            params![workspace, bot],
        )?;
        Ok(())
    }

    pub fn get_unread_counts(&self, workspace: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT c.bot, COUNT(*) as unread
             FROM conversations c
             LEFT JOIN last_seen ls ON ls.workspace = c.workspace AND ls.bot = c.bot
             WHERE c.workspace = ?1
               AND c.id > COALESCE(ls.message_id, 0)
               AND c.role = 'assistant'
             GROUP BY c.bot",
        )?;
        let rows = stmt
            .query_map(params![workspace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_sentry_cursor(&self, workspace: &str, bot: &str) -> Result<Option<String>> {
        let conn = self.reader()?;
        let result = conn.query_row(
            "SELECT last_issue_id FROM sentry_cursors WHERE workspace = ?1 AND bot = ?2",
            params![workspace, bot],
            |row| row.get(0),
        );
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_sentry_cursor(
        &self,
        workspace: &str,
        bot: &str,
        last_issue_id: &str,
        last_poll_at: &str,
    ) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(
            "INSERT INTO sentry_cursors (workspace, bot, last_issue_id, last_poll_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace, bot) DO UPDATE SET
               last_issue_id = excluded.last_issue_id,
               last_poll_at = excluded.last_poll_at",
            params![workspace, bot, last_issue_id, last_poll_at],
        )?;
        Ok(())
    }

    /// Execute a raw SQL batch (for schema migrations from other modules).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute_batch(sql)?;
        Ok(())
    }

    /// Execute a parameterized SQL statement (for inserts/updates from other modules).
    pub fn execute_sql(&self, sql: &str, params_slice: &[&str]) -> Result<()> {
        let conn = self.writer.lock().unwrap();
        conn.execute(sql, rusqlite::params_from_iter(params_slice))?;
        Ok(())
    }

    /// Execute an INSERT and return last_insert_rowid.
    pub fn insert_returning_id(
        &self,
        sql: &str,
        params_slice: &[&dyn rusqlite::types::ToSql],
    ) -> Result<i64> {
        let conn = self.writer.lock().unwrap();
        conn.execute(sql, params_slice)?;
        Ok(conn.last_insert_rowid())
    }

    /// Query research tasks for a workspace.
    pub fn query_research_tasks(
        &self,
        workspace: &str,
    ) -> Result<Vec<crate::research::ResearchTask>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT id, workspace, topic, status, error, started_at, completed_at, output_file
             FROM research_tasks WHERE workspace = ?1 ORDER BY started_at DESC",
        )?;
        let rows = stmt
            .query_map(params![workspace], |row| {
                Ok(crate::research::ResearchTask {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    topic: row.get(2)?,
                    status: row.get(3)?,
                    error: row.get(4)?,
                    started_at: row.get(5)?,
                    completed_at: row.get(6)?,
                    output_file: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Query a single research task by ID.
    pub fn query_research_task(&self, id: &str) -> Option<crate::research::ResearchTask> {
        let conn = self.reader().ok()?;
        conn.query_row(
            "SELECT id, workspace, topic, status, error, started_at, completed_at, output_file
             FROM research_tasks WHERE id = ?1",
            params![id],
            |row| {
                Ok(crate::research::ResearchTask {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    topic: row.get(2)?,
                    status: row.get(3)?,
                    error: row.get(4)?,
                    started_at: row.get(5)?,
                    completed_at: row.get(6)?,
                    output_file: row.get(7)?,
                })
            },
        )
        .ok()
    }

    pub fn get_all_conversations(&self, workspace: &str, limit: i64) -> Result<Vec<MessageRow>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT id, workspace, bot, role, content, attachments, created_at
             FROM conversations
             WHERE workspace = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![workspace, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    bot: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    attachments: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
    }
}

#[derive(Debug, serde::Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub workspace: String,
    pub bot: String,
    pub role: String,
    pub content: String,
    pub attachments: Option<String>,
    pub created_at: String,
}

#[derive(Debug, serde::Serialize)]
pub struct BotStatus {
    pub status: String,
    pub streaming_content: String,
    pub tool_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamps_are_iso8601_with_z_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        db.add_message("ws", "bot", "user", "hello", None).unwrap();
        let msgs = db.get_conversations("ws", "bot", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        let ts = &msgs[0].created_at;
        assert!(ts.ends_with('Z'), "Timestamp should end with Z: {ts}");
        assert!(
            ts.contains('T'),
            "Timestamp should contain T separator: {ts}"
        );
    }
}

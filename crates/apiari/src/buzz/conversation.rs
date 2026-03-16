//! Conversation persistence — SQLite-backed chat history and session tokens.
//!
//! Stores user/assistant messages and provider-specific session tokens
//! for session resumption across daemon restarts.
//! All queries are scoped to a workspace.

use chrono::Utc;
use color_eyre::eyre::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// A provider-agnostic session token for resuming conversations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToken {
    /// Provider name: "claude", "codex", "gemini", etc.
    pub provider: String,
    /// The provider-specific resume token/ID.
    pub token: String,
}

/// A row from the conversations table.
#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub id: i64,
    pub workspace: String,
    pub role: String,
    pub content: String,
    pub source: Option<String>,
    pub provider: Option<String>,
    pub session_id: Option<String>,
    pub created_at: String,
}

/// SQLite-backed conversation store, scoped to a workspace.
pub struct ConversationStore<'a> {
    conn: &'a Connection,
    workspace: String,
}

impl<'a> ConversationStore<'a> {
    /// Create a ConversationStore backed by the given connection, scoped to a workspace.
    /// Assumes the `conversations` table already exists (created by SignalStore::init_schema).
    pub fn new(conn: &'a Connection, workspace: &str) -> Self {
        Self {
            conn,
            workspace: workspace.to_string(),
        }
    }

    /// Insert a conversation message.
    pub fn save_message(
        &self,
        role: &str,
        content: &str,
        source: Option<&str>,
        provider: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO conversations (workspace, role, content, source, provider, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![&self.workspace, role, content, source, provider, session_id, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Load the last N messages ordered by created_at.
    pub fn load_history(&self, limit: usize) -> Result<Vec<ConversationRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, role, content, source, provider, session_id, created_at
             FROM conversations
             WHERE workspace = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let mut rows: Vec<ConversationRow> = stmt
            .query_map(params![&self.workspace, limit as i64], |row| {
                Ok(ConversationRow {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    source: row.get(4)?,
                    provider: row.get(5)?,
                    session_id: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // Reverse so oldest first
        rows.reverse();
        Ok(rows)
    }

    /// Get the most recent (provider, session_id) pair for session resumption.
    pub fn last_session(&self) -> Result<Option<SessionToken>> {
        let result = self.conn.query_row(
            "SELECT provider, session_id FROM conversations
             WHERE workspace = ?1 AND provider IS NOT NULL AND session_id IS NOT NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            params![&self.workspace],
            |row| {
                Ok(SessionToken {
                    provider: row.get(0)?,
                    token: row.get(1)?,
                })
            },
        );

        match result {
            Ok(token) => Ok(Some(token)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id          INTEGER PRIMARY KEY,
                workspace   TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                source      TEXT,
                provider    TEXT,
                session_id  TEXT,
                created_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_workspace
                ON conversations(workspace, created_at);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_save_and_load() {
        let conn = test_conn();
        let store = ConversationStore::new(&conn, "test");

        store
            .save_message("user", "hello", Some("telegram"), None, None)
            .unwrap();
        store
            .save_message(
                "assistant",
                "hi there",
                Some("system"),
                Some("claude"),
                Some("sess-123"),
            )
            .unwrap();

        let history = store.load_history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].provider.as_deref(), Some("claude"));
        assert_eq!(history[1].session_id.as_deref(), Some("sess-123"));
    }

    #[test]
    fn test_last_session() {
        let conn = test_conn();
        let store = ConversationStore::new(&conn, "test");

        // No session yet
        assert!(store.last_session().unwrap().is_none());

        store
            .save_message("user", "msg1", None, None, None)
            .unwrap();
        // Still no session (user messages have no provider)
        assert!(store.last_session().unwrap().is_none());

        store
            .save_message("assistant", "resp1", None, Some("claude"), Some("sess-abc"))
            .unwrap();
        let token = store.last_session().unwrap().unwrap();
        assert_eq!(token.provider, "claude");
        assert_eq!(token.token, "sess-abc");

        // Newer session replaces
        store
            .save_message("assistant", "resp2", None, Some("claude"), Some("sess-xyz"))
            .unwrap();
        let token = store.last_session().unwrap().unwrap();
        assert_eq!(token.token, "sess-xyz");
    }

    #[test]
    fn test_workspace_isolation() {
        let conn = test_conn();
        let store_a = ConversationStore::new(&conn, "ws-a");
        let store_b = ConversationStore::new(&conn, "ws-b");

        store_a
            .save_message("user", "from A", None, None, None)
            .unwrap();
        store_b
            .save_message("user", "from B", None, None, None)
            .unwrap();

        let a_history = store_a.load_history(10).unwrap();
        assert_eq!(a_history.len(), 1);
        assert_eq!(a_history[0].content, "from A");

        let b_history = store_b.load_history(10).unwrap();
        assert_eq!(b_history.len(), 1);
        assert_eq!(b_history[0].content, "from B");
    }

    #[test]
    fn test_load_history_limit() {
        let conn = test_conn();
        let store = ConversationStore::new(&conn, "test");

        for i in 0..10 {
            store
                .save_message("user", &format!("msg {i}"), None, None, None)
                .unwrap();
        }

        let recent = store.load_history(3).unwrap();
        assert_eq!(recent.len(), 3);
        // Should be the last 3 messages, in chronological order
        assert!(recent[2].content.contains("msg 9"));
    }
}

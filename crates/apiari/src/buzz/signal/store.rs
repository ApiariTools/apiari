//! SQLite-backed signal store.
//!
//! Provides CRUD operations for signals and watcher cursor persistence.
//! The store uses WAL mode for concurrent readers.
//! All queries are scoped to a workspace name.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr};
use rusqlite::{Connection, params};

use super::{Severity, SignalRecord, SignalStatus, SignalUpdate};

#[derive(Debug, Clone)]
pub struct BotStatusRecord {
    pub status: String,
    pub streaming_content: String,
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FollowupRecord {
    pub id: String,
    pub bot: String,
    pub action: String,
    pub created_at: String,
    pub fires_at: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct BotTurnFailureRecord {
    pub id: i64,
    pub bot: String,
    pub provider: Option<String>,
    pub source: String,
    pub error_text: String,
    pub created_at: String,
}

/// SQLite signal store, scoped to a workspace.
pub struct SignalStore {
    conn: Connection,
    workspace: String,
    db_path: PathBuf,
}

impl SignalStore {
    /// Open (or create) a SQLite database at the given path, scoped to a workspace.
    pub fn open(path: &Path, workspace: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .wrap_err_with(|| format!("failed to open {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self {
            conn,
            workspace: workspace.to_string(),
            db_path: path.to_path_buf(),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Open an in-memory database (for testing), scoped to a workspace.
    pub fn open_memory(workspace: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn,
            workspace: workspace.to_string(),
            db_path: PathBuf::new(),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// The workspace this store is scoped to.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The path to the SQLite database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Create tables if they don't exist.
    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS signals (
                id INTEGER PRIMARY KEY,
                workspace TEXT NOT NULL,
                source TEXT NOT NULL,
                external_id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT,
                severity TEXT NOT NULL DEFAULT 'info',
                status TEXT NOT NULL DEFAULT 'open',
                url TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                resolved_at TEXT,
                metadata TEXT,
                UNIQUE(workspace, source, external_id)
            );

            CREATE TABLE IF NOT EXISTS watcher_cursors (
                workspace TEXT NOT NULL,
                watcher TEXT NOT NULL,
                cursor_value TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(workspace, watcher)
            );

            CREATE TABLE IF NOT EXISTS memory (
                id INTEGER PRIMARY KEY,
                workspace TEXT NOT NULL,
                category TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS conversations (
                id          INTEGER PRIMARY KEY,
                workspace   TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                attachments TEXT,
                source      TEXT,
                provider    TEXT,
                session_id  TEXT,
                created_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_workspace
                ON conversations(workspace, created_at);

            CREATE TABLE IF NOT EXISTS bot_status (
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'idle',
                streaming_content TEXT NOT NULL DEFAULT '',
                tool_name TEXT,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                PRIMARY KEY(workspace, bot)
            );

            CREATE TABLE IF NOT EXISTS bot_seen (
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                last_seen_message_id INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                PRIMARY KEY(workspace, bot)
            );

            CREATE TABLE IF NOT EXISTS followups (
                id TEXT PRIMARY KEY,
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                action TEXT NOT NULL,
                created_at TEXT NOT NULL,
                fires_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
            );

            CREATE TABLE IF NOT EXISTS bot_turn_failures (
                id INTEGER PRIMARY KEY,
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                provider TEXT,
                source TEXT NOT NULL,
                error_text TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );
            CREATE INDEX IF NOT EXISTS idx_bot_turn_failures_workspace_bot
                ON bot_turn_failures(workspace, bot, created_at DESC);
            ",
        )?;

        // Migrations for legacy signal schemas.
        // Ignore duplicate-column errors, propagate everything else.
        for (sql, column_name) in [
            (
                "ALTER TABLE signals ADD COLUMN resolved_at TEXT;",
                "resolved_at",
            ),
            (
                "ALTER TABLE signals ADD COLUMN snoozed_until TEXT;",
                "snoozed_until",
            ),
            (
                "ALTER TABLE conversations ADD COLUMN attachments TEXT;",
                "attachments",
            ),
        ] {
            if let Err(e) = self.conn.execute_batch(sql) {
                let msg = e.to_string();
                if !msg.contains("duplicate column") {
                    return Err(e).wrap_err(format!("failed to add {column_name} column"));
                }
            }
        }

        // Ensure task tables exist on the shared DB connection.
        crate::buzz::task::store::TaskStore::ensure_schema(&self.conn)?;

        Ok(())
    }

    /// Upsert a signal from a watcher update.
    ///
    /// Inserts a new signal or updates an existing one matched by (workspace, source, external_id).
    /// Returns the signal ID and whether it was newly inserted.
    pub fn upsert_signal(&self, update: &SignalUpdate) -> Result<(i64, bool)> {
        let now = Utc::now().to_rfc3339();

        // Check if signal already exists.
        let existing_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM signals WHERE workspace = ?1 AND source = ?2 AND external_id = ?3",
                params![self.workspace, update.source, update.external_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing_id {
            let resolved_at = if update.status == SignalStatus::Resolved {
                Some(now.clone())
            } else {
                None
            };
            self.conn.execute(
                "UPDATE signals SET title = ?1, body = ?2, severity = ?3, status = ?4,
                 url = ?5, updated_at = ?6, resolved_at = COALESCE(?7, resolved_at),
                 metadata = COALESCE(?8, metadata)
                 WHERE id = ?9",
                params![
                    update.title,
                    update.body,
                    update.severity.to_string(),
                    update.status.to_string(),
                    update.url,
                    now,
                    resolved_at,
                    update.metadata,
                    id,
                ],
            )?;
            Ok((id, false))
        } else {
            self.conn.execute(
                "INSERT INTO signals (workspace, source, external_id, title, body, severity, status,
                 url, created_at, updated_at, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)",
                params![
                    self.workspace,
                    update.source,
                    update.external_id,
                    update.title,
                    update.body,
                    update.severity.to_string(),
                    update.status.to_string(),
                    update.url,
                    now,
                    update.metadata,
                ],
            )?;
            let id = self.conn.last_insert_rowid();
            Ok((id, true))
        }
    }

    /// Get all signals with non-resolved/stale status for this workspace.
    pub fn get_open_signals(&self) -> Result<Vec<SignalRecord>> {
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, source, external_id, title, body, severity, status,
                    url, created_at, updated_at, resolved_at, metadata, snoozed_until
             FROM signals
             WHERE workspace = ?1 AND status IN ('open', 'updated')
               AND (snoozed_until IS NULL OR snoozed_until <= ?2)
             ORDER BY
                CASE severity
                    WHEN 'critical' THEN 0
                    WHEN 'error' THEN 1
                    WHEN 'warning' THEN 2
                    ELSE 3
                END,
                updated_at DESC",
        )?;

        let records = stmt
            .query_map(params![self.workspace, now], |row| {
                Ok(SignalRecord {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    body: row.get(4)?,
                    severity: Severity::from_str_loose(&row.get::<_, String>(5)?),
                    status: SignalStatus::from_str_loose(&row.get::<_, String>(6)?),
                    url: row.get(7)?,
                    created_at: parse_datetime(&row.get::<_, String>(8)?),
                    updated_at: parse_datetime(&row.get::<_, String>(9)?),
                    resolved_at: row
                        .get::<_, Option<String>>(10)?
                        .map(|s| parse_datetime(&s)),
                    metadata: row.get(11)?,
                    snoozed_until: row
                        .get::<_, Option<String>>(12)?
                        .map(|s| parse_datetime(&s)),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Get recent signal history for this workspace, including resolved and snoozed rows.
    pub fn get_signal_history(&self, limit: usize) -> Result<Vec<SignalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, external_id, title, body, severity, status,
                    url, created_at, updated_at, resolved_at, metadata, snoozed_until
             FROM signals
             WHERE workspace = ?1
             ORDER BY updated_at DESC, created_at DESC
             LIMIT ?2",
        )?;

        let records = stmt
            .query_map(params![self.workspace, limit as i64], |row| {
                Ok(SignalRecord {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    body: row.get(4)?,
                    severity: Severity::from_str_loose(&row.get::<_, String>(5)?),
                    status: SignalStatus::from_str_loose(&row.get::<_, String>(6)?),
                    url: row.get(7)?,
                    created_at: parse_datetime(&row.get::<_, String>(8)?),
                    updated_at: parse_datetime(&row.get::<_, String>(9)?),
                    resolved_at: row
                        .get::<_, Option<String>>(10)?
                        .map(|s| parse_datetime(&s)),
                    metadata: row.get(11)?,
                    snoozed_until: row
                        .get::<_, Option<String>>(12)?
                        .map(|s| parse_datetime(&s)),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Get a signal by ID (must belong to this workspace).
    pub fn get_signal(&self, id: i64) -> Result<Option<SignalRecord>> {
        let result = self.conn.query_row(
            "SELECT id, source, external_id, title, body, severity, status,
                    url, created_at, updated_at, resolved_at, metadata, snoozed_until
             FROM signals WHERE id = ?1 AND workspace = ?2",
            params![id, self.workspace],
            |row| {
                Ok(SignalRecord {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    body: row.get(4)?,
                    severity: Severity::from_str_loose(&row.get::<_, String>(5)?),
                    status: SignalStatus::from_str_loose(&row.get::<_, String>(6)?),
                    url: row.get(7)?,
                    created_at: parse_datetime(&row.get::<_, String>(8)?),
                    updated_at: parse_datetime(&row.get::<_, String>(9)?),
                    resolved_at: row
                        .get::<_, Option<String>>(10)?
                        .map(|s| parse_datetime(&s)),
                    metadata: row.get(11)?,
                    snoozed_until: row
                        .get::<_, Option<String>>(12)?
                        .map(|s| parse_datetime(&s)),
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_bot_status(
        &self,
        bot: &str,
        status: &str,
        streaming_content: &str,
        tool_name: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bot_status (workspace, bot, status, streaming_content, tool_name, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               status = ?3,
               streaming_content = ?4,
               tool_name = ?5,
               updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![self.workspace, bot, status, streaming_content, tool_name],
        )?;
        Ok(())
    }

    pub fn get_bot_status(&self, bot: &str) -> Result<Option<BotStatusRecord>> {
        let result = self.conn.query_row(
            "SELECT status, streaming_content, tool_name
             FROM bot_status
             WHERE workspace = ?1 AND bot = ?2",
            params![self.workspace, bot],
            |row| {
                Ok(BotStatusRecord {
                    status: row.get(0)?,
                    streaming_content: row.get(1)?,
                    tool_name: row.get(2)?,
                })
            },
        );

        match result {
            Ok(status) => Ok(Some(status)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn mark_bot_seen(&self, bot: &str, last_seen_message_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bot_seen (workspace, bot, last_seen_message_id, updated_at)
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               last_seen_message_id = ?3,
               updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![self.workspace, bot, last_seen_message_id],
        )?;
        Ok(())
    }

    pub fn get_bot_seen_message_id(&self, bot: &str) -> Result<Option<i64>> {
        let result = self.conn.query_row(
            "SELECT last_seen_message_id
             FROM bot_seen
             WHERE workspace = ?1 AND bot = ?2",
            params![self.workspace, bot],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn create_followup(
        &self,
        id: &str,
        bot: &str,
        action: &str,
        created_at: &str,
        fires_at: &str,
        status: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO followups (id, workspace, bot, action, created_at, fires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, self.workspace, bot, action, created_at, fires_at, status],
        )?;
        Ok(())
    }

    pub fn list_followups(&self) -> Result<Vec<FollowupRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, bot, action, created_at, fires_at, status
             FROM followups
             WHERE workspace = ?1
             ORDER BY fires_at ASC, created_at ASC",
        )?;

        let records = stmt
            .query_map(params![self.workspace], |row| {
                Ok(FollowupRecord {
                    id: row.get(0)?,
                    bot: row.get(1)?,
                    action: row.get(2)?,
                    created_at: row.get(3)?,
                    fires_at: row.get(4)?,
                    status: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    pub fn get_followup(&self, id: &str) -> Result<Option<FollowupRecord>> {
        let result = self.conn.query_row(
            "SELECT id, bot, action, created_at, fires_at, status
             FROM followups
             WHERE workspace = ?1 AND id = ?2",
            params![self.workspace, id],
            |row| {
                Ok(FollowupRecord {
                    id: row.get(0)?,
                    bot: row.get(1)?,
                    action: row.get(2)?,
                    created_at: row.get(3)?,
                    fires_at: row.get(4)?,
                    status: row.get(5)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_due_followups(&self) -> Result<Vec<FollowupRecord>> {
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, bot, action, created_at, fires_at, status
             FROM followups
             WHERE workspace = ?1 AND status = 'pending' AND fires_at <= ?2
             ORDER BY fires_at ASC, created_at ASC",
        )?;

        let records = stmt
            .query_map(params![self.workspace, now], |row| {
                Ok(FollowupRecord {
                    id: row.get(0)?,
                    bot: row.get(1)?,
                    action: row.get(2)?,
                    created_at: row.get(3)?,
                    fires_at: row.get(4)?,
                    status: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    pub fn log_bot_turn_failure(
        &self,
        bot: &str,
        provider: Option<&str>,
        source: &str,
        error_text: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO bot_turn_failures (workspace, bot, provider, source, error_text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![self.workspace, bot, provider, source, error_text],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_bot_turn_failures(
        &self,
        bot: &str,
        limit: usize,
    ) -> Result<Vec<BotTurnFailureRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, bot, provider, source, error_text, created_at
             FROM bot_turn_failures
             WHERE workspace = ?1 AND bot = ?2
             ORDER BY created_at DESC, id DESC
             LIMIT ?3",
        )?;

        let records = stmt
            .query_map(params![self.workspace, bot, limit as i64], |row| {
                Ok(BotTurnFailureRecord {
                    id: row.get(0)?,
                    bot: row.get(1)?,
                    provider: row.get(2)?,
                    source: row.get(3)?,
                    error_text: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    pub fn set_followup_status(&self, id: &str, status: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE followups
             SET status = ?3
             WHERE workspace = ?1 AND id = ?2",
            params![self.workspace, id, status],
        )?;
        Ok(changed > 0)
    }

    pub fn cancel_followup(&self, id: &str) -> Result<bool> {
        self.set_followup_status(id, "cancelled")
    }

    /// Resolve a signal by ID.
    pub fn resolve_signal(&self, id: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE signals SET status = 'resolved', resolved_at = ?1, updated_at = ?1
             WHERE id = ?2 AND workspace = ?3",
            params![now, id, self.workspace],
        )?;
        Ok(())
    }

    /// Snooze a signal until the given timestamp.
    pub fn snooze_signal(&self, id: i64, until: DateTime<Utc>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE signals SET snoozed_until = ?1, updated_at = ?2
             WHERE id = ?3 AND workspace = ?4",
            params![until.to_rfc3339(), now, id, self.workspace],
        )?;
        Ok(())
    }

    /// Resolve open signals from a given source whose external_id is NOT in `current_ids`.
    ///
    /// This is the reconciliation step: after a successful poll returns the full set of
    /// currently-active items, any DB signals not in that set must have been resolved upstream.
    /// An empty `current_ids` means the source has zero active items — all open signals get resolved.
    /// Callers should only invoke this after a successful poll (not on API errors).
    pub fn resolve_missing_signals(&self, source: &str, current_ids: &[String]) -> Result<usize> {
        let now = Utc::now().to_rfc3339();

        // Get all open signals for this source
        let mut stmt = self.conn.prepare(
            "SELECT id, external_id FROM signals
             WHERE workspace = ?1 AND source = ?2 AND status IN ('open', 'updated')",
        )?;
        let open_signals: Vec<(i64, String)> = stmt
            .query_map(params![self.workspace, source], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let current_set: std::collections::HashSet<&str> =
            current_ids.iter().map(|s| s.as_str()).collect();

        let mut resolved_count = 0;
        for (id, ext_id) in &open_signals {
            if !current_set.contains(ext_id.as_str()) {
                self.conn.execute(
                    "UPDATE signals SET status = 'resolved', resolved_at = ?1, updated_at = ?1
                     WHERE id = ?2 AND workspace = ?3",
                    params![now, id, self.workspace],
                )?;
                resolved_count += 1;
            }
        }

        Ok(resolved_count)
    }

    /// Mark signals older than the given threshold as stale.
    pub fn mark_stale(&self, older_than: DateTime<Utc>) -> Result<usize> {
        let threshold = older_than.to_rfc3339();
        let count = self.conn.execute(
            "UPDATE signals SET status = 'stale', updated_at = ?1
             WHERE workspace = ?2 AND status IN ('open', 'updated') AND updated_at < ?3",
            params![Utc::now().to_rfc3339(), self.workspace, threshold],
        )?;
        Ok(count)
    }

    /// Get the cursor value for a watcher in this workspace.
    pub fn get_cursor(&self, watcher: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT cursor_value FROM watcher_cursors WHERE workspace = ?1 AND watcher = ?2",
            params![self.workspace, watcher],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set the cursor value for a watcher in this workspace.
    pub fn set_cursor(&self, watcher: &str, value: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO watcher_cursors (workspace, watcher, cursor_value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace, watcher) DO UPDATE SET cursor_value = ?3, updated_at = ?4",
            params![self.workspace, watcher, value, now],
        )?;
        Ok(())
    }

    /// Get all signals for this workspace (for status display).
    pub fn get_all_signals(&self) -> Result<Vec<SignalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, external_id, title, body, severity, status,
                    url, created_at, updated_at, resolved_at, metadata, snoozed_until
             FROM signals WHERE workspace = ?1 ORDER BY updated_at DESC",
        )?;

        let records = stmt
            .query_map(params![self.workspace], |row| {
                Ok(SignalRecord {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    body: row.get(4)?,
                    severity: Severity::from_str_loose(&row.get::<_, String>(5)?),
                    status: SignalStatus::from_str_loose(&row.get::<_, String>(6)?),
                    url: row.get(7)?,
                    created_at: parse_datetime(&row.get::<_, String>(8)?),
                    updated_at: parse_datetime(&row.get::<_, String>(9)?),
                    resolved_at: row
                        .get::<_, Option<String>>(10)?
                        .map(|s| parse_datetime(&s)),
                    metadata: row.get(11)?,
                    snoozed_until: row
                        .get::<_, Option<String>>(12)?
                        .map(|s| parse_datetime(&s)),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Borrow the underlying connection (e.g. for MemoryStore).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Count signals per hour over the last 24 hours (for sparkline).
    /// Returns a 24-element vec where index 0 = 23 hours ago, index 23 = current hour.
    pub fn count_signals_by_hour(&self) -> Result<Vec<u64>> {
        let now = Utc::now();
        let cutoff = (now - chrono::Duration::hours(24)).to_rfc3339();

        let mut stmt = self
            .conn
            .prepare("SELECT created_at FROM signals WHERE workspace = ?1 AND created_at >= ?2")?;
        let timestamps: Vec<DateTime<Utc>> = stmt
            .query_map(params![self.workspace, cutoff], |row| {
                let s: String = row.get(0)?;
                Ok(parse_datetime(&s))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let current_hour = now.timestamp() / 3600;
        let mut buckets = vec![0u64; 24];
        for ts in &timestamps {
            let hour = ts.timestamp() / 3600;
            let offset = current_hour - hour;
            if (0..24).contains(&offset) {
                buckets[23 - offset as usize] += 1;
            }
        }
        Ok(buckets)
    }

    /// Get the N most recent signals (any status), ordered by updated_at DESC.
    pub fn get_recent_signals(&self, n: usize) -> Result<Vec<SignalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, external_id, title, body, severity, status,
                    url, created_at, updated_at, resolved_at, metadata, snoozed_until
             FROM signals WHERE workspace = ?1 ORDER BY updated_at DESC LIMIT ?2",
        )?;
        let records = stmt
            .query_map(params![self.workspace, n as i64], |row| {
                Ok(SignalRecord {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    body: row.get(4)?,
                    severity: Severity::from_str_loose(&row.get::<_, String>(5)?),
                    status: SignalStatus::from_str_loose(&row.get::<_, String>(6)?),
                    url: row.get(7)?,
                    created_at: parse_datetime(&row.get::<_, String>(8)?),
                    updated_at: parse_datetime(&row.get::<_, String>(9)?),
                    resolved_at: row
                        .get::<_, Option<String>>(10)?
                        .map(|s| parse_datetime(&s)),
                    metadata: row.get(11)?,
                    snoozed_until: row
                        .get::<_, Option<String>>(12)?
                        .map(|s| parse_datetime(&s)),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(records)
    }

    /// Get watcher cursor timestamps for health checking.
    pub fn get_watcher_cursors(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT watcher, updated_at FROM watcher_cursors WHERE workspace = ?1")?;
        let cursors = stmt
            .query_map(params![self.workspace], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(cursors)
    }

    /// Count signal activity in fine-grained time buckets.
    /// Returns `num_buckets` counts, each covering `bucket_minutes` minutes.
    /// Index 0 = oldest bucket, last index = most recent.
    /// Counts both created and updated signals (any event = activity).
    pub fn count_signal_activity(
        &self,
        bucket_minutes: i64,
        num_buckets: usize,
    ) -> Result<Vec<u64>> {
        let now = Utc::now();
        let total_minutes = bucket_minutes * num_buckets as i64;
        let cutoff = (now - chrono::Duration::minutes(total_minutes)).to_rfc3339();

        // Count both creations and updates as activity events
        let mut stmt = self.conn.prepare(
            "SELECT updated_at FROM signals WHERE workspace = ?1 AND updated_at >= ?2
             UNION ALL
             SELECT created_at FROM signals WHERE workspace = ?1 AND created_at >= ?2
               AND created_at != updated_at",
        )?;
        let timestamps: Vec<DateTime<Utc>> = stmt
            .query_map(params![self.workspace, cutoff], |row| {
                let s: String = row.get(0)?;
                Ok(parse_datetime(&s))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let now_ts = now.timestamp();
        let mut buckets = vec![0u64; num_buckets];
        for ts in &timestamps {
            let age_minutes = (now_ts - ts.timestamp()) / 60;
            let bucket_idx = num_buckets as i64 - 1 - (age_minutes / bucket_minutes);
            if bucket_idx >= 0 && (bucket_idx as usize) < num_buckets {
                buckets[bucket_idx as usize] += 1;
            }
        }
        Ok(buckets)
    }

    /// Count signals by status for this workspace.
    pub fn count_by_status(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT status, COUNT(*) FROM signals WHERE workspace = ?1 GROUP BY status")?;
        let counts = stmt
            .query_map(params![self.workspace], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(counts)
    }
}

fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SignalStore {
        SignalStore::open_memory("test").unwrap()
    }

    fn make_update(source: &str, ext_id: &str, title: &str, severity: Severity) -> SignalUpdate {
        SignalUpdate::new(source, ext_id, title, severity)
    }

    #[test]
    fn test_upsert_insert() {
        let store = test_store();
        let update = make_update("sentry", "issue-1", "Bug found", Severity::Error);
        let (id, is_new) = store.upsert_signal(&update).unwrap();
        assert!(id > 0);
        assert!(is_new);
    }

    #[test]
    fn test_upsert_update() {
        let store = test_store();
        let update1 = make_update("sentry", "issue-1", "Bug found", Severity::Error);
        let (id1, new1) = store.upsert_signal(&update1).unwrap();
        assert!(new1);

        let update2 = make_update(
            "sentry",
            "issue-1",
            "Bug found (updated)",
            Severity::Critical,
        );
        let (id2, new2) = store.upsert_signal(&update2).unwrap();
        assert!(!new2);
        assert_eq!(id1, id2);

        let record = store.get_signal(id1).unwrap().unwrap();
        assert_eq!(record.title, "Bug found (updated)");
        assert_eq!(record.severity, Severity::Critical);
    }

    #[test]
    fn test_upsert_different_sources_same_external_id() {
        let store = test_store();
        let u1 = make_update("sentry", "id-1", "Sentry bug", Severity::Error);
        let u2 = make_update("github", "id-1", "GitHub issue", Severity::Info);
        let (id1, _) = store.upsert_signal(&u1).unwrap();
        let (id2, _) = store.upsert_signal(&u2).unwrap();
        assert_ne!(id1, id2); // different sources = different signals
    }

    #[test]
    fn test_workspace_isolation() {
        let store_a = SignalStore::open_memory("workspace-a").unwrap();
        let store_b = SignalStore::open_memory("workspace-b").unwrap();

        store_a
            .upsert_signal(&make_update("sentry", "s1", "Bug A", Severity::Error))
            .unwrap();
        store_b
            .upsert_signal(&make_update("sentry", "s1", "Bug B", Severity::Warning))
            .unwrap();

        let a_signals = store_a.get_open_signals().unwrap();
        let b_signals = store_b.get_open_signals().unwrap();

        assert_eq!(a_signals.len(), 1);
        assert_eq!(a_signals[0].title, "Bug A");
        assert_eq!(b_signals.len(), 1);
        assert_eq!(b_signals[0].title, "Bug B");
    }

    #[test]
    fn test_get_open_signals() {
        let store = test_store();
        store
            .upsert_signal(&make_update("sentry", "s1", "Open bug", Severity::Warning))
            .unwrap();
        store
            .upsert_signal(
                &make_update("github", "g1", "Resolved issue", Severity::Info)
                    .with_status(SignalStatus::Resolved),
            )
            .unwrap();
        store
            .upsert_signal(&make_update(
                "sentry",
                "s2",
                "Critical bug",
                Severity::Critical,
            ))
            .unwrap();

        let open = store.get_open_signals().unwrap();
        assert_eq!(open.len(), 2);
        // Critical should come first
        assert_eq!(open[0].severity, Severity::Critical);
    }

    #[test]
    fn test_resolve_signal() {
        let store = test_store();
        let (id, _) = store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();
        store.resolve_signal(id).unwrap();

        let record = store.get_signal(id).unwrap().unwrap();
        assert_eq!(record.status, SignalStatus::Resolved);
        assert!(record.resolved_at.is_some());
    }

    #[test]
    fn test_mark_stale() {
        let store = test_store();
        store
            .upsert_signal(&make_update("sentry", "s1", "Old bug", Severity::Info))
            .unwrap();

        // Use a timestamp in the past — nothing should be stale yet
        let past = Utc::now() - chrono::Duration::hours(1);
        let stale_count = store.mark_stale(past).unwrap();
        assert_eq!(stale_count, 0);

        // Mark everything before far future as stale
        let future = Utc::now() + chrono::Duration::hours(1);
        let stale_count = store.mark_stale(future).unwrap();
        assert_eq!(stale_count, 1);
    }

    #[test]
    fn test_cursor_crud() {
        let store = test_store();

        assert!(store.get_cursor("github").unwrap().is_none());

        store.set_cursor("github", "cursor-abc").unwrap();
        assert_eq!(
            store.get_cursor("github").unwrap().as_deref(),
            Some("cursor-abc")
        );

        store.set_cursor("github", "cursor-xyz").unwrap();
        assert_eq!(
            store.get_cursor("github").unwrap().as_deref(),
            Some("cursor-xyz")
        );
    }

    #[test]
    fn test_cursor_multiple_watchers() {
        let store = test_store();
        store.set_cursor("github", "gh-123").unwrap();
        store.set_cursor("sentry", "sn-456").unwrap();

        assert_eq!(
            store.get_cursor("github").unwrap().as_deref(),
            Some("gh-123")
        );
        assert_eq!(
            store.get_cursor("sentry").unwrap().as_deref(),
            Some("sn-456")
        );
    }

    #[test]
    fn test_get_signal_not_found() {
        let store = test_store();
        assert!(store.get_signal(999).unwrap().is_none());
    }

    #[test]
    fn test_count_by_status() {
        let store = test_store();
        store
            .upsert_signal(&make_update("a", "1", "t1", Severity::Info))
            .unwrap();
        store
            .upsert_signal(&make_update("a", "2", "t2", Severity::Info))
            .unwrap();
        store
            .upsert_signal(
                &make_update("a", "3", "t3", Severity::Info).with_status(SignalStatus::Resolved),
            )
            .unwrap();

        let counts = store.count_by_status().unwrap();
        let open_count = counts
            .iter()
            .find(|(s, _)| s == "open")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let resolved_count = counts
            .iter()
            .find(|(s, _)| s == "resolved")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(open_count, 2);
        assert_eq!(resolved_count, 1);
    }

    #[test]
    fn test_upsert_with_body_and_url() {
        let store = test_store();
        let update = make_update("sentry", "s1", "Bug", Severity::Error)
            .with_body("Stack trace here")
            .with_url("https://sentry.io/issue/1");
        let (id, _) = store.upsert_signal(&update).unwrap();

        let record = store.get_signal(id).unwrap().unwrap();
        assert_eq!(record.body.as_deref(), Some("Stack trace here"));
        assert_eq!(record.url.as_deref(), Some("https://sentry.io/issue/1"));
    }

    #[test]
    fn test_upsert_idempotent() {
        let store = test_store();
        let update = make_update("github", "pr-1", "New PR", Severity::Info);

        let (id1, new1) = store.upsert_signal(&update).unwrap();
        let (id2, new2) = store.upsert_signal(&update).unwrap();
        let (id3, new3) = store.upsert_signal(&update).unwrap();

        assert!(new1);
        assert!(!new2);
        assert!(!new3);
        assert_eq!(id1, id2);
        assert_eq!(id2, id3);
    }

    #[test]
    fn test_status_transitions() {
        let store = test_store();

        // Open -> Updated -> Resolved
        let (id, _) = store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();
        let r = store.get_signal(id).unwrap().unwrap();
        assert_eq!(r.status, SignalStatus::Open);

        store
            .upsert_signal(
                &make_update("sentry", "s1", "Bug (worse)", Severity::Critical)
                    .with_status(SignalStatus::Updated),
            )
            .unwrap();
        let r = store.get_signal(id).unwrap().unwrap();
        assert_eq!(r.status, SignalStatus::Updated);
        assert_eq!(r.severity, Severity::Critical);

        store.resolve_signal(id).unwrap();
        let r = store.get_signal(id).unwrap().unwrap();
        assert_eq!(r.status, SignalStatus::Resolved);
        assert!(r.resolved_at.is_some());
    }

    #[test]
    fn test_open_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            store
                .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
                .unwrap();
            store.set_cursor("sentry", "cursor-1").unwrap();
        }

        // Reopen
        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            let signals = store.get_open_signals().unwrap();
            assert_eq!(signals.len(), 1);
            assert_eq!(signals[0].title, "Bug");

            let cursor = store.get_cursor("sentry").unwrap();
            assert_eq!(cursor.as_deref(), Some("cursor-1"));
        }
    }

    #[test]
    fn test_bot_status_and_followups_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            store
                .set_bot_status("Main", "streaming", "hello", Some("Bash"))
                .unwrap();
            store.mark_bot_seen("Main", 42).unwrap();
            store
                .create_followup(
                    "fu-1",
                    "Main",
                    "Check CI status",
                    "2026-05-02T00:00:00Z",
                    "2026-05-02T01:00:00Z",
                    "pending",
                )
                .unwrap();
        }

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            let status = store.get_bot_status("Main").unwrap().unwrap();
            assert_eq!(status.status, "streaming");
            assert_eq!(status.streaming_content, "hello");
            assert_eq!(status.tool_name.as_deref(), Some("Bash"));

            assert_eq!(store.get_bot_seen_message_id("Main").unwrap(), Some(42));

            let followup = store.get_followup("fu-1").unwrap().unwrap();
            assert_eq!(followup.bot, "Main");
            assert_eq!(followup.action, "Check CI status");
            assert_eq!(followup.status, "pending");
        }
    }

    #[test]
    fn test_due_followups_survive_reopen_and_can_be_fired() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let past = (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            store
                .create_followup(
                    "fu-due",
                    "Main",
                    "Check deploy status",
                    "2026-05-02T00:00:00Z",
                    &past,
                    "pending",
                )
                .unwrap();
        }

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            let due = store.list_due_followups().unwrap();
            assert_eq!(due.len(), 1);
            assert_eq!(due[0].id, "fu-due");
            assert_eq!(due[0].status, "pending");

            assert!(store.set_followup_status("fu-due", "fired").unwrap());
        }

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            let followup = store.get_followup("fu-due").unwrap().unwrap();
            assert_eq!(followup.status, "fired");
            assert!(store.list_due_followups().unwrap().is_empty());
        }
    }

    #[test]
    fn test_seen_checkpoint_survives_reopen_and_can_advance() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            store.mark_bot_seen("Main", 7).unwrap();
        }

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            assert_eq!(store.get_bot_seen_message_id("Main").unwrap(), Some(7));
            store.mark_bot_seen("Main", 9).unwrap();
        }

        {
            let store = SignalStore::open(&db_path, "test").unwrap();
            assert_eq!(store.get_bot_seen_message_id("Main").unwrap(), Some(9));
        }
    }

    #[test]
    fn test_resolve_missing_signals() {
        let store = test_store();
        store
            .upsert_signal(&make_update("sentry", "sentry-1", "Bug 1", Severity::Error))
            .unwrap();
        store
            .upsert_signal(&make_update(
                "sentry",
                "sentry-2",
                "Bug 2",
                Severity::Warning,
            ))
            .unwrap();
        store
            .upsert_signal(&make_update("sentry", "sentry-3", "Bug 3", Severity::Info))
            .unwrap();
        // Different source — should not be touched
        store
            .upsert_signal(&make_update("github", "gh-1", "Issue 1", Severity::Info))
            .unwrap();

        // Sentry poll returns only sentry-1 and sentry-3 (sentry-2 was resolved)
        let current = vec!["sentry-1".to_string(), "sentry-3".to_string()];
        let resolved = store.resolve_missing_signals("sentry", &current).unwrap();
        assert_eq!(resolved, 1);

        // sentry-2 should now be resolved
        let open = store.get_open_signals().unwrap();
        let open_ids: Vec<&str> = open.iter().map(|s| s.external_id.as_str()).collect();
        assert!(open_ids.contains(&"sentry-1"));
        assert!(!open_ids.contains(&"sentry-2"));
        assert!(open_ids.contains(&"sentry-3"));
        assert!(open_ids.contains(&"gh-1")); // untouched
    }

    #[test]
    fn test_resolve_missing_signals_empty_resolves_all() {
        let store = test_store();
        store
            .upsert_signal(&make_update("sentry", "sentry-1", "Bug 1", Severity::Error))
            .unwrap();

        // Empty current_ids means source has zero active items — resolve all
        let resolved = store.resolve_missing_signals("sentry", &[]).unwrap();
        assert_eq!(resolved, 1);

        let open = store.get_open_signals().unwrap();
        assert_eq!(open.len(), 0);
    }

    #[test]
    fn test_resolve_missing_signals_all_current() {
        let store = test_store();
        store
            .upsert_signal(&make_update("sentry", "sentry-1", "Bug 1", Severity::Error))
            .unwrap();
        store
            .upsert_signal(&make_update(
                "sentry",
                "sentry-2",
                "Bug 2",
                Severity::Warning,
            ))
            .unwrap();

        let current = vec!["sentry-1".to_string(), "sentry-2".to_string()];
        let resolved = store.resolve_missing_signals("sentry", &current).unwrap();
        assert_eq!(resolved, 0);

        let open = store.get_open_signals().unwrap();
        assert_eq!(open.len(), 2);
    }

    #[test]
    fn test_count_signals_by_hour() {
        let store = test_store();
        store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();
        store
            .upsert_signal(&make_update("sentry", "s2", "Bug2", Severity::Warning))
            .unwrap();

        let buckets = store.count_signals_by_hour().unwrap();
        assert_eq!(buckets.len(), 24);
        // Both signals created just now → bucket[23] should be 2
        assert_eq!(buckets[23], 2);
        // All other buckets should be 0
        assert_eq!(buckets[0..23].iter().sum::<u64>(), 0);
    }

    #[test]
    fn test_get_recent_signals() {
        let store = test_store();
        store
            .upsert_signal(&make_update("a", "1", "first", Severity::Info))
            .unwrap();
        store
            .upsert_signal(
                &make_update("a", "2", "second", Severity::Info)
                    .with_status(SignalStatus::Resolved),
            )
            .unwrap();
        store
            .upsert_signal(&make_update("a", "3", "third", Severity::Error))
            .unwrap();

        // Should return all statuses
        let recent = store.get_recent_signals(10).unwrap();
        assert_eq!(recent.len(), 3);

        // Limit works
        let limited = store.get_recent_signals(2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn test_get_watcher_cursors() {
        let store = test_store();
        store.set_cursor("github", "gh-123").unwrap();
        store.set_cursor("sentry", "sn-456").unwrap();

        let cursors = store.get_watcher_cursors().unwrap();
        assert_eq!(cursors.len(), 2);
        let names: Vec<&str> = cursors.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"github"));
        assert!(names.contains(&"sentry"));
    }

    #[test]
    fn test_ci_pass_different_run_ids_are_new() {
        let store = test_store();
        // Same PR, different run IDs — each should be a new signal
        let u1 = make_update(
            "github",
            "ci-pass-42-1000",
            "CI passed: feat (#42)",
            Severity::Info,
        );
        let u2 = make_update(
            "github",
            "ci-pass-42-1001",
            "CI passed: feat (#42)",
            Severity::Info,
        );

        let (id1, new1) = store.upsert_signal(&u1).unwrap();
        let (id2, new2) = store.upsert_signal(&u2).unwrap();

        assert!(new1, "first CI pass signal should be new");
        assert!(new2, "re-run CI pass signal should also be new");
        assert_ne!(
            id1, id2,
            "different run IDs should produce different signals"
        );
    }

    #[test]
    fn test_ci_pass_same_run_id_is_update() {
        let store = test_store();
        let u1 = make_update(
            "github",
            "ci-pass-42-1000",
            "CI passed: feat (#42)",
            Severity::Info,
        );
        let u2 = make_update(
            "github",
            "ci-pass-42-1000",
            "CI passed: feat (#42)",
            Severity::Info,
        );

        let (id1, new1) = store.upsert_signal(&u1).unwrap();
        let (id2, new2) = store.upsert_signal(&u2).unwrap();

        assert!(new1);
        assert!(!new2, "same run ID should be an update, not new");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_resolve_missing_signals_ci_run_transition() {
        let store = test_store();

        // Old CI run signal
        store
            .upsert_signal(&make_update(
                "github",
                "ci-pass-42-1000",
                "CI passed: feat (#42)",
                Severity::Info,
            ))
            .unwrap();

        // New CI run replaces old — reconciliation should resolve the old one
        let current_ids = vec!["ci-pass-42-1001".to_string()];
        let resolved = store
            .resolve_missing_signals("github", &current_ids)
            .unwrap();
        assert_eq!(resolved, 1, "old CI run signal should be resolved");

        let open = store.get_open_signals().unwrap();
        assert!(
            !open.iter().any(|s| s.external_id == "ci-pass-42-1000"),
            "old CI run should no longer be open"
        );
    }

    #[test]
    fn test_metadata_preserved_on_update() {
        let store = test_store();

        // Insert with metadata
        let u1 =
            make_update("sentry", "s1", "Bug", Severity::Error).with_metadata(r#"{"count": 5}"#);
        let (id, _) = store.upsert_signal(&u1).unwrap();

        // Update without metadata — should preserve original
        let u2 = make_update("sentry", "s1", "Bug (updated)", Severity::Critical);
        store.upsert_signal(&u2).unwrap();

        let record = store.get_signal(id).unwrap().unwrap();
        assert_eq!(record.title, "Bug (updated)");
        assert_eq!(record.metadata.as_deref(), Some(r#"{"count": 5}"#));
    }

    #[test]
    fn test_snooze_signal_hides_from_open() {
        let store = test_store();
        let (id, _) = store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();

        // Snooze 1 hour into the future
        let until = Utc::now() + chrono::Duration::hours(1);
        store.snooze_signal(id, until).unwrap();

        // Should not appear in open signals
        let open = store.get_open_signals().unwrap();
        assert!(open.is_empty());

        // But should still be retrievable by ID
        let record = store.get_signal(id).unwrap().unwrap();
        assert!(record.snoozed_until.is_some());
    }

    #[test]
    fn test_snooze_expired_reappears() {
        let store = test_store();
        let (id, _) = store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();

        // Snooze to the past — should reappear immediately
        let past = Utc::now() - chrono::Duration::hours(1);
        store.snooze_signal(id, past).unwrap();

        let open = store.get_open_signals().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
    }

    #[test]
    fn test_upsert_preserves_snooze() {
        let store = test_store();
        let (id, _) = store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();

        // Snooze the signal
        let until = Utc::now() + chrono::Duration::hours(2);
        store.snooze_signal(id, until).unwrap();

        // Re-upsert the same signal (as a watcher would)
        store
            .upsert_signal(&make_update(
                "sentry",
                "s1",
                "Bug (updated)",
                Severity::Error,
            ))
            .unwrap();

        // Snooze should be preserved
        let record = store.get_signal(id).unwrap().unwrap();
        assert!(record.snoozed_until.is_some());
        assert_eq!(record.title, "Bug (updated)");

        // Still hidden from open signals
        let open = store.get_open_signals().unwrap();
        assert!(open.is_empty());
    }

    #[test]
    fn test_snooze_replaces_existing() {
        let store = test_store();
        let (id, _) = store
            .upsert_signal(&make_update("sentry", "s1", "Bug", Severity::Error))
            .unwrap();

        // First snooze
        let until1 = Utc::now() + chrono::Duration::hours(1);
        store.snooze_signal(id, until1).unwrap();

        // Second snooze with different time
        let until2 = Utc::now() + chrono::Duration::hours(4);
        store.snooze_signal(id, until2).unwrap();

        let record = store.get_signal(id).unwrap().unwrap();
        let snoozed = record.snoozed_until.unwrap();
        // The second snooze should have replaced the first
        let diff = (snoozed - until2).num_seconds().abs();
        assert!(diff < 2, "snooze should be close to until2");
    }

    #[test]
    fn test_log_and_list_bot_turn_failures() {
        let store = test_store();
        let _ = store
            .log_bot_turn_failure(
                "Codex",
                Some("codex"),
                "dispatch",
                "provider returned empty response",
            )
            .unwrap();
        let _ = store
            .log_bot_turn_failure(
                "Codex",
                Some("codex"),
                "runtime",
                "unexpected argument '--approval-policy' found",
            )
            .unwrap();

        let failures = store.list_bot_turn_failures("Codex", 10).unwrap();
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].bot, "Codex");
        assert_eq!(failures[0].provider.as_deref(), Some("codex"));
        assert!(!failures[0].source.is_empty());
        assert!(!failures[0].error_text.is_empty());
    }
}

//! MemoryStore — persistent observations in SQLite.
//!
//! Stores categorized observations that the coordinator accumulates
//! over time, providing context across sessions.
//! All queries are scoped to a workspace via the parent SignalStore.

use chrono::Utc;
use color_eyre::eyre::Result;
use rusqlite::{Connection, params};

/// Categories of memory entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCategory {
    Observation,
    Decision,
    Preference,
}

impl MemoryCategory {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Observation => "observation",
            Self::Decision => "decision",
            Self::Preference => "preference",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "decision" => Self::Decision,
            "preference" => Self::Preference,
            _ => Self::Observation,
        }
    }
}

/// A memory entry.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: i64,
    pub category: MemoryCategory,
    pub content: String,
    pub created_at: String,
}

/// SQLite-backed memory store, scoped to a workspace.
pub struct MemoryStore<'a> {
    conn: &'a Connection,
    workspace: String,
}

impl<'a> MemoryStore<'a> {
    /// Create a MemoryStore backed by the given connection, scoped to a workspace.
    /// Assumes the `memory` table already exists (created by SignalStore::init_schema).
    pub fn new(conn: &'a Connection, workspace: &str) -> Self {
        Self {
            conn,
            workspace: workspace.to_string(),
        }
    }

    /// Add an observation to memory.
    pub fn add(&self, category: MemoryCategory, content: &str) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO memory (workspace, category, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![self.workspace, category.as_str(), content, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get the most recent N entries for this workspace.
    pub fn get_recent(&self, n: usize) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, category, content, created_at FROM memory
             WHERE workspace = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let entries = stmt
            .query_map(params![self.workspace, n as i64], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    category: MemoryCategory::from_str_loose(&row.get::<_, String>(1)?),
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Search memory entries containing the query string.
    pub fn search(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        let pattern = format!("%{query}%");
        let mut stmt = self.conn.prepare(
            "SELECT id, category, content, created_at FROM memory
             WHERE workspace = ?1 AND content LIKE ?2 ORDER BY id DESC LIMIT 20",
        )?;
        let entries = stmt
            .query_map(params![self.workspace, pattern], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    category: MemoryCategory::from_str_loose(&row.get::<_, String>(1)?),
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Get all entries in a category for this workspace.
    pub fn get_by_category(&self, category: MemoryCategory) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, category, content, created_at FROM memory
             WHERE workspace = ?1 AND category = ?2 ORDER BY id DESC",
        )?;
        let entries = stmt
            .query_map(params![self.workspace, category.as_str()], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    category: MemoryCategory::from_str_loose(&row.get::<_, String>(1)?),
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Delete a memory entry.
    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM memory WHERE id = ?1 AND workspace = ?2",
            params![id, self.workspace],
        )?;
        Ok(())
    }

    /// Count all entries for this workspace.
    pub fn count(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory WHERE workspace = ?1",
            params![self.workspace],
            |row| row.get(0),
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memory (
                id INTEGER PRIMARY KEY,
                workspace TEXT NOT NULL,
                category TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_add_and_get_recent() {
        let conn = test_conn();
        let store = MemoryStore::new(&conn, "test");

        store
            .add(MemoryCategory::Observation, "User prefers verbose output")
            .unwrap();
        store
            .add(MemoryCategory::Decision, "Switched to SQLite for signals")
            .unwrap();

        let entries = store.get_recent(10).unwrap();
        assert_eq!(entries.len(), 2);
        // Most recent first
        assert_eq!(entries[0].category, MemoryCategory::Decision);
        assert_eq!(entries[1].category, MemoryCategory::Observation);
    }

    #[test]
    fn test_search() {
        let conn = test_conn();
        let store = MemoryStore::new(&conn, "test");

        store
            .add(MemoryCategory::Observation, "User likes Rust")
            .unwrap();
        store
            .add(MemoryCategory::Observation, "User likes Python")
            .unwrap();
        store
            .add(MemoryCategory::Decision, "Deploy on Fridays")
            .unwrap();

        let results = store.search("likes").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_by_category() {
        let conn = test_conn();
        let store = MemoryStore::new(&conn, "test");

        store.add(MemoryCategory::Observation, "obs1").unwrap();
        store.add(MemoryCategory::Decision, "dec1").unwrap();
        store.add(MemoryCategory::Observation, "obs2").unwrap();

        let observations = store.get_by_category(MemoryCategory::Observation).unwrap();
        assert_eq!(observations.len(), 2);

        let decisions = store.get_by_category(MemoryCategory::Decision).unwrap();
        assert_eq!(decisions.len(), 1);
    }

    #[test]
    fn test_delete() {
        let conn = test_conn();
        let store = MemoryStore::new(&conn, "test");

        let id = store.add(MemoryCategory::Observation, "temp").unwrap();
        assert_eq!(store.count().unwrap(), 1);

        store.delete(id).unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_count() {
        let conn = test_conn();
        let store = MemoryStore::new(&conn, "test");

        assert_eq!(store.count().unwrap(), 0);
        store.add(MemoryCategory::Preference, "dark mode").unwrap();
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_workspace_isolation() {
        let conn = test_conn();
        let store_a = MemoryStore::new(&conn, "ws-a");
        let store_b = MemoryStore::new(&conn, "ws-b");

        store_a.add(MemoryCategory::Observation, "from A").unwrap();
        store_b.add(MemoryCategory::Observation, "from B").unwrap();

        assert_eq!(store_a.count().unwrap(), 1);
        assert_eq!(store_b.count().unwrap(), 1);

        let a_entries = store_a.get_recent(10).unwrap();
        assert_eq!(a_entries[0].content, "from A");

        let b_entries = store_b.get_recent(10).unwrap();
        assert_eq!(b_entries[0].content, "from B");
    }

    #[test]
    fn test_category_round_trip() {
        assert_eq!(
            MemoryCategory::from_str_loose("observation"),
            MemoryCategory::Observation
        );
        assert_eq!(
            MemoryCategory::from_str_loose("decision"),
            MemoryCategory::Decision
        );
        assert_eq!(
            MemoryCategory::from_str_loose("preference"),
            MemoryCategory::Preference
        );
        assert_eq!(
            MemoryCategory::from_str_loose("unknown"),
            MemoryCategory::Observation
        );
    }

    #[test]
    fn test_get_recent_limit() {
        let conn = test_conn();
        let store = MemoryStore::new(&conn, "test");

        for i in 0..10 {
            store
                .add(MemoryCategory::Observation, &format!("entry {i}"))
                .unwrap();
        }

        let recent = store.get_recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert!(recent[0].content.contains("entry 9"));
    }
}

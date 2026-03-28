//! SQLite-backed task store.
//!
//! Provides CRUD operations for tasks and task events.
//! The store uses the same DB connection as SignalStore.

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr, eyre};
use rusqlite::{Connection, params};

use super::{Task, TaskEvent, TaskStage};

/// SQLite task store.
pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    /// Open (or create) a SQLite database at the given path.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .wrap_err_with(|| format!("failed to open {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Open an in-memory database (for testing).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Create task tables if they don't exist. Also called from SignalStore::init_schema
    /// so the tables exist on the shared DB connection.
    pub fn ensure_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                workspace TEXT NOT NULL,
                title TEXT NOT NULL,
                stage TEXT NOT NULL,
                source TEXT,
                source_url TEXT,
                worker_id TEXT,
                pr_url TEXT,
                pr_number INTEGER,
                repo TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                resolved_at TEXT,
                metadata TEXT NOT NULL DEFAULT '{}'
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_workspace_stage ON tasks(workspace, stage);
            CREATE INDEX IF NOT EXISTS idx_tasks_workspace_worker ON tasks(workspace, worker_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_workspace_pr ON tasks(workspace, repo, pr_number);

            CREATE TABLE IF NOT EXISTS task_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                from_stage TEXT,
                to_stage TEXT,
                signal_id INTEGER,
                detail TEXT,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_task_events_task_id ON task_events(task_id);
            ",
        )
        .wrap_err("failed to create task tables")
    }

    fn init_schema(&self) -> Result<()> {
        Self::ensure_schema(&self.conn)
    }

    pub fn create_task(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks
             (id, workspace, title, stage, source, source_url, worker_id,
              pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                task.id,
                task.workspace,
                task.title,
                task.stage.as_str(),
                task.source,
                task.source_url,
                task.worker_id,
                task.pr_url,
                task.pr_number,
                task.repo,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
                task.resolved_at.map(|t| t.to_rfc3339()),
                task.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE id = ?1",
            params![id],
            row_to_task,
        );
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).wrap_err("failed to get task"),
        }
    }

    pub fn get_tasks_by_stage(&self, workspace: &str, stage: &TaskStage) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE workspace = ?1 AND stage = ?2
             ORDER BY created_at ASC",
        )?;
        let tasks = stmt
            .query_map(params![workspace, stage.as_str()], row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to query tasks by stage")?;
        Ok(tasks)
    }

    pub fn get_active_tasks(&self, workspace: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE workspace = ?1
             AND stage NOT IN ('Merged', 'Dismissed')
             ORDER BY created_at ASC",
        )?;
        let tasks = stmt
            .query_map(params![workspace], row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to query active tasks")?;
        Ok(tasks)
    }

    pub fn get_all_tasks(&self, workspace: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE workspace = ?1
             ORDER BY created_at ASC",
        )?;
        let tasks = stmt
            .query_map(params![workspace], row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to query all tasks")?;
        Ok(tasks)
    }

    pub fn update_task_stage(&self, id: &str, new_stage: &TaskStage) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let resolved_at = if new_stage.is_terminal() {
            Some(now.clone())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE tasks SET stage = ?1, updated_at = ?2,
             resolved_at = COALESCE(?3, resolved_at)
             WHERE id = ?4",
            params![new_stage.as_str(), now, resolved_at, id],
        )?;
        Ok(())
    }

    pub fn update_task_worker(&self, id: &str, worker_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET worker_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![worker_id, now, id],
        )?;
        Ok(())
    }

    pub fn update_task_pr(&self, id: &str, pr_url: &str, pr_number: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET pr_url = ?1, pr_number = ?2, updated_at = ?3 WHERE id = ?4",
            params![pr_url, pr_number, now, id],
        )?;
        Ok(())
    }

    pub fn find_task_by_pr(
        &self,
        workspace: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE workspace = ?1 AND repo = ?2 AND pr_number = ?3",
            params![workspace, repo, pr_number],
            row_to_task,
        );
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).wrap_err("failed to find task by PR"),
        }
    }

    pub fn update_task_repo(&self, id: &str, repo: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET repo = ?1, updated_at = ?2 WHERE id = ?3",
            params![repo, now, id],
        )?;
        Ok(())
    }

    pub fn find_task_by_worker(&self, workspace: &str, worker_id: &str) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE workspace = ?1 AND worker_id = ?2",
            params![workspace, worker_id],
            row_to_task,
        );
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).wrap_err("failed to find task by worker"),
        }
    }

    /// Find a task whose metadata JSON contains `reviewer_worker_id == reviewer_id`.
    pub fn find_task_by_reviewer_worker(
        &self,
        workspace: &str,
        reviewer_id: &str,
    ) -> Result<Option<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, title, stage, source, source_url, worker_id,
             pr_url, pr_number, repo, created_at, updated_at, resolved_at, metadata
             FROM tasks WHERE workspace = ?1
             AND json_extract(metadata, '$.reviewer_worker_id') = ?2",
        )?;
        let mut tasks = stmt
            .query_map(params![workspace, reviewer_id], row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to find task by reviewer worker")?;
        Ok(tasks.pop())
    }

    /// Update the task's metadata JSON blob.
    pub fn update_task_metadata(&self, id: &str, metadata: &serde_json::Value) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET metadata = ?1, updated_at = ?2 WHERE id = ?3",
            params![metadata.to_string(), now, id],
        )?;
        Ok(())
    }

    pub fn log_event(&self, event: &TaskEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_events
             (task_id, event_type, from_stage, to_stage, signal_id, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.task_id,
                event.event_type,
                event.from_stage,
                event.to_stage,
                event.signal_id,
                event.detail,
                event.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_task_events(&self, task_id: &str) -> Result<Vec<TaskEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, event_type, from_stage, to_stage, signal_id, detail, created_at
             FROM task_events WHERE task_id = ?1 ORDER BY id ASC",
        )?;
        let events = stmt
            .query_map(params![task_id], row_to_task_event)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to query task events")?;
        Ok(events)
    }

    /// Transition a task stage and log a stage_change event atomically.
    pub fn transition_task(
        &self,
        id: &str,
        from: &TaskStage,
        to: &TaskStage,
        detail: Option<String>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let current_stage: String = tx
            .query_row(
                "SELECT stage FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|_| eyre!("task not found: {}", id))?;

        if current_stage != from.as_str() {
            return Err(eyre!(
                "task {} is in stage {}, expected {}",
                id,
                current_stage,
                from.as_str()
            ));
        }

        let now = Utc::now().to_rfc3339();
        let resolved_at: Option<String> = if to.is_terminal() {
            Some(now.clone())
        } else {
            None
        };

        tx.execute(
            "UPDATE tasks SET stage = ?1, updated_at = ?2, resolved_at = ?3 WHERE id = ?4",
            params![to.as_str(), now, resolved_at, id],
        )?;

        tx.execute(
            "INSERT INTO task_events (task_id, event_type, from_stage, to_stage, detail, created_at)
             VALUES (?1, 'stage_change', ?2, ?3, ?4, ?5)",
            params![id, from.as_str(), to.as_str(), detail, now],
        )?;

        tx.commit()?;
        Ok(())
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let stage_str: String = row.get(3)?;
    let stage = TaskStage::from_str(&stage_str).unwrap_or(TaskStage::Triage);

    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;
    let resolved_at: Option<String> = row.get(12)?;
    let metadata_str: String = row.get(13)?;

    Ok(Task {
        id: row.get(0)?,
        workspace: row.get(1)?,
        title: row.get(2)?,
        stage,
        source: row.get(4)?,
        source_url: row.get(5)?,
        worker_id: row.get(6)?,
        pr_url: row.get(7)?,
        pr_number: row.get(8)?,
        repo: row.get(9)?,
        created_at: created_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now()),
        updated_at: updated_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now()),
        resolved_at: resolved_at.and_then(|s| s.parse::<DateTime<Utc>>().ok()),
        metadata: serde_json::from_str(&metadata_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
    })
}

fn row_to_task_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEvent> {
    let created_at: String = row.get(7)?;
    Ok(TaskEvent {
        id: row.get(0)?,
        task_id: row.get(1)?,
        event_type: row.get(2)?,
        from_stage: row.get(3)?,
        to_stage: row.get(4)?,
        signal_id: row.get(5)?,
        detail: row.get(6)?,
        created_at: created_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_task(workspace: &str, title: &str) -> Task {
        let now = Utc::now();
        Task {
            id: Uuid::new_v4().to_string(),
            workspace: workspace.to_string(),
            title: title.to_string(),
            stage: TaskStage::Triage,
            source: None,
            source_url: None,
            worker_id: None,
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    #[test]
    fn test_create_and_get_task() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", "Fix the bug");
        store.create_task(&task).unwrap();

        let fetched = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(fetched.id, task.id);
        assert_eq!(fetched.title, "Fix the bug");
        assert_eq!(fetched.stage, TaskStage::Triage);
    }

    #[test]
    fn test_get_task_not_found() {
        let store = TaskStore::open_memory().unwrap();
        let result = store.get_task("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_transition_task_logs_event() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", "Ship feature");
        store.create_task(&task).unwrap();

        store
            .transition_task(
                &task.id,
                &TaskStage::Triage,
                &TaskStage::InProgress,
                Some("assigned to worker-1".to_string()),
            )
            .unwrap();

        let updated = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::InProgress);

        let events = store.get_task_events(&task.id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "stage_change");
        assert_eq!(events[0].from_stage.as_deref(), Some("Triage"));
        assert_eq!(events[0].to_stage.as_deref(), Some("In Progress"));
        assert_eq!(events[0].detail.as_deref(), Some("assigned to worker-1"));
    }

    #[test]
    fn test_find_task_by_pr() {
        let store = TaskStore::open_memory().unwrap();
        let mut task = make_task("acme", "PR task");
        task.repo = Some("org/repo".to_string());
        task.pr_number = Some(42);
        task.pr_url = Some("https://github.com/org/repo/pull/42".to_string());
        store.create_task(&task).unwrap();

        let found = store
            .find_task_by_pr("acme", "org/repo", 42)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
        assert_eq!(found.pr_number, Some(42));
    }

    #[test]
    fn test_find_task_by_pr_not_found() {
        let store = TaskStore::open_memory().unwrap();
        let result = store.find_task_by_pr("acme", "org/repo", 99).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_find_task_by_worker() {
        let store = TaskStore::open_memory().unwrap();
        let mut task = make_task("acme", "Worker task");
        task.worker_id = Some("worker-abc".to_string());
        store.create_task(&task).unwrap();

        let found = store
            .find_task_by_worker("acme", "worker-abc")
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
    }

    #[test]
    fn test_get_active_tasks_excludes_terminal() {
        let store = TaskStore::open_memory().unwrap();

        let t1 = make_task("acme", "Active task");
        let mut t2 = make_task("acme", "Merged task");
        t2.stage = TaskStage::Merged;
        let mut t3 = make_task("acme", "Dismissed task");
        t3.stage = TaskStage::Dismissed;

        store.create_task(&t1).unwrap();
        store.create_task(&t2).unwrap();
        store.create_task(&t3).unwrap();

        let active = store.get_active_tasks("acme").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, t1.id);
    }

    #[test]
    fn test_get_all_tasks_includes_terminal() {
        let store = TaskStore::open_memory().unwrap();

        let t1 = make_task("acme", "Active task");
        let mut t2 = make_task("acme", "Merged task");
        t2.stage = TaskStage::Merged;

        store.create_task(&t1).unwrap();
        store.create_task(&t2).unwrap();

        let all = store.get_all_tasks("acme").unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_transition_to_terminal_sets_resolved_at() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", "Done task");
        store.create_task(&task).unwrap();

        assert!(
            store
                .get_task(&task.id)
                .unwrap()
                .unwrap()
                .resolved_at
                .is_none()
        );

        store
            .transition_task(&task.id, &TaskStage::Triage, &TaskStage::Merged, None)
            .unwrap();

        let updated = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::Merged);
        assert!(updated.resolved_at.is_some());
    }

    #[test]
    fn test_get_tasks_by_stage() {
        let store = TaskStore::open_memory().unwrap();

        let t1 = make_task("acme", "Triage 1");
        let t2 = make_task("acme", "Triage 2");
        let mut t3 = make_task("acme", "In Progress");
        t3.stage = TaskStage::InProgress;

        store.create_task(&t1).unwrap();
        store.create_task(&t2).unwrap();
        store.create_task(&t3).unwrap();

        let triage = store
            .get_tasks_by_stage("acme", &TaskStage::Triage)
            .unwrap();
        assert_eq!(triage.len(), 2);

        let in_progress = store
            .get_tasks_by_stage("acme", &TaskStage::InProgress)
            .unwrap();
        assert_eq!(in_progress.len(), 1);
    }

    #[test]
    fn test_backward_transition_clears_resolved_at() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", "Backward transition");
        store.create_task(&task).unwrap();

        // Transition to terminal (sets resolved_at)
        store
            .transition_task(&task.id, &TaskStage::Triage, &TaskStage::Merged, None)
            .unwrap();
        let updated = store.get_task(&task.id).unwrap().unwrap();
        assert!(
            updated.resolved_at.is_some(),
            "resolved_at should be set after Merged"
        );

        // Transition back to non-terminal (clears resolved_at)
        store
            .transition_task(&task.id, &TaskStage::Merged, &TaskStage::InProgress, None)
            .unwrap();
        let updated = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::InProgress);
        assert!(
            updated.resolved_at.is_none(),
            "resolved_at should be cleared after backward transition"
        );
    }

    #[test]
    fn test_transition_wrong_from_stage_rejected() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", "Stage check");
        store.create_task(&task).unwrap();

        // Try to transition from wrong stage (task is Triage, not InProgress)
        let result = store.transition_task(
            &task.id,
            &TaskStage::InProgress,
            &TaskStage::MergeReady,
            None,
        );
        assert!(result.is_err(), "should reject wrong from stage");

        // Task should be unchanged
        let unchanged = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(unchanged.stage, TaskStage::Triage);
        // No events should have been logged
        let events = store.get_task_events(&task.id).unwrap();
        assert!(
            events.is_empty(),
            "no events should be logged on rejected transition"
        );
    }
}

//! Quest and Task models with JSON persistence.
//!
//! Quests represent high-level goals that the coordinator breaks down into
//! tasks. Tasks are assigned to workers (swarm worktrees) for execution.
//! All quest state persists to `.hive/quests/<quest-id>.json`.

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where a quest/task originated from, for routing notifications back.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskOrigin {
    /// Channel the request came from: "telegram", "cli", "dashboard".
    pub channel: String,
    /// Telegram chat_id (for routing replies back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
    /// Human-readable user name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    /// Numeric user ID (e.g. Telegram user_id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
}

/// The status of a quest through its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestStatus {
    /// Quest is being planned; tasks are being defined.
    Planning,
    /// Quest is actively being worked on.
    Active,
    /// Quest is temporarily paused.
    Paused,
    /// All tasks are complete.
    Complete,
}

impl std::fmt::Display for QuestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Planning => write!(f, "planning"),
            Self::Active => write!(f, "active"),
            Self::Paused => write!(f, "paused"),
            Self::Complete => write!(f, "complete"),
        }
    }
}

/// The status of an individual task within a quest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Task is waiting to be started.
    Pending,
    /// Task is currently being worked on by a worker.
    InProgress,
    /// Task is finished.
    Done,
    /// Task is blocked by a dependency or issue.
    Blocked,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in-progress"),
            Self::Done => write!(f, "done"),
            Self::Blocked => write!(f, "blocked"),
        }
    }
}

/// A task is a unit of work within a quest, assignable to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task identifier.
    pub id: String,
    /// The quest this task belongs to.
    pub quest_id: String,
    /// Short description of what needs to be done.
    pub title: String,
    /// Current status.
    pub status: TaskStatus,
    /// Worker ID or swarm worktree this task is assigned to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// Git branch for this task's work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Where this task originated from (for notification routing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TaskOrigin>,
}

impl Task {
    /// Create a new pending task.
    pub fn new(quest_id: &str, title: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            quest_id: quest_id.to_owned(),
            title: title.into(),
            status: TaskStatus::Pending,
            assigned_to: None,
            branch: None,
            origin: None,
        }
    }
}

/// A quest is a high-level goal broken down into tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quest {
    /// Unique quest identifier.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Detailed description of the quest objective.
    pub description: String,
    /// Current status.
    pub status: QuestStatus,
    /// Tasks that make up this quest.
    pub tasks: Vec<Task>,
    /// When the quest was created.
    pub created_at: DateTime<Utc>,
    /// When the quest was last modified.
    pub updated_at: DateTime<Utc>,
    /// Associated GitHub issue in "owner/repo#123" format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_issue: Option<String>,
    /// Where this quest originated from (for notification routing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TaskOrigin>,
}

impl Quest {
    /// Create a new quest in the planning stage.
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            description: description.into(),
            status: QuestStatus::Planning,
            tasks: Vec::new(),
            created_at: now,
            updated_at: now,
            github_issue: None,
            origin: None,
        }
    }

    /// Add a task to this quest.
    pub fn add_task(&mut self, title: impl Into<String>) -> &Task {
        let task = Task::new(&self.id, title);
        self.tasks.push(task);
        self.updated_at = Utc::now();
        self.tasks.last().expect("just pushed")
    }

    /// Return a summary line for display.
    pub fn summary(&self) -> String {
        let total = self.tasks.len();
        let done = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        format!(
            "[{}] {} ({}/{} tasks) - {}",
            &self.id[..8],
            self.title,
            done,
            total,
            self.status
        )
    }
}

/// Manages quest persistence in the `.hive/quests/` directory.
pub struct QuestStore {
    dir: PathBuf,
}

impl QuestStore {
    /// Create a new store rooted at the given directory.
    ///
    /// The directory is typically `.hive/quests/` relative to the workspace root.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Return the file path for a given quest ID.
    fn quest_path(&self, quest_id: &str) -> PathBuf {
        self.dir.join(format!("{quest_id}.json"))
    }

    /// Save a quest to disk.
    pub fn save(&self, quest: &Quest) -> Result<()> {
        let path = self.quest_path(&quest.id);
        apiari_common::state::save_state(&path, quest)
            .wrap_err_with(|| format!("failed to save quest {}", quest.id))?;
        Ok(())
    }

    /// Load a quest by ID. Returns `None` if it does not exist.
    pub fn load(&self, quest_id: &str) -> Result<Option<Quest>> {
        let path = self.quest_path(quest_id);
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("failed to read quest file {}", path.display()))?;
        let quest: Quest = serde_json::from_str(&data)
            .wrap_err_with(|| format!("failed to parse quest {quest_id}"))?;
        Ok(Some(quest))
    }

    /// List all quests on disk.
    pub fn list(&self) -> Result<Vec<Quest>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }

        let mut quests = Vec::new();
        let entries = std::fs::read_dir(&self.dir).wrap_err("failed to read quests directory")?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                match std::fs::read_to_string(&path) {
                    Ok(data) => match serde_json::from_str::<Quest>(&data) {
                        Ok(quest) => quests.push(quest),
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "failed to parse quest file");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "failed to read quest file");
                    }
                }
            }
        }

        quests.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(quests)
    }

    /// Delete a quest from disk.
    #[allow(dead_code)]
    pub fn delete(&self, quest_id: &str) -> Result<()> {
        let path = self.quest_path(quest_id);
        if path.exists() {
            std::fs::remove_file(&path)
                .wrap_err_with(|| format!("failed to delete quest {quest_id}"))?;
        }
        Ok(())
    }
}

/// Return the default quest store path relative to a workspace root.
pub fn default_store_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".hive").join("quests")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ---- Status display ----

    #[test]
    fn quest_status_display() {
        assert_eq!(QuestStatus::Planning.to_string(), "planning");
        assert_eq!(QuestStatus::Active.to_string(), "active");
        assert_eq!(QuestStatus::Paused.to_string(), "paused");
        assert_eq!(QuestStatus::Complete.to_string(), "complete");
    }

    #[test]
    fn task_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::InProgress.to_string(), "in-progress");
        assert_eq!(TaskStatus::Done.to_string(), "done");
        assert_eq!(TaskStatus::Blocked.to_string(), "blocked");
    }

    // ---- Model construction ----

    #[test]
    fn quest_new_defaults() {
        let q = Quest::new("My Quest", "A description");
        assert_eq!(q.title, "My Quest");
        assert_eq!(q.description, "A description");
        assert_eq!(q.status, QuestStatus::Planning);
        assert!(q.tasks.is_empty());
        assert!(q.github_issue.is_none());
        assert!(!q.id.is_empty());
    }

    #[test]
    fn task_new_defaults() {
        let t = Task::new("quest-123", "Implement feature");
        assert_eq!(t.quest_id, "quest-123");
        assert_eq!(t.title, "Implement feature");
        assert_eq!(t.status, TaskStatus::Pending);
        assert!(t.assigned_to.is_none());
        assert!(t.branch.is_none());
        assert!(!t.id.is_empty());
    }

    #[test]
    fn quest_add_task() {
        let mut q = Quest::new("Quest", "Desc");
        q.add_task("First task");
        assert_eq!(q.tasks.len(), 1);
        assert_eq!(q.tasks[0].title, "First task");
        assert_eq!(q.tasks[0].quest_id, q.id);
        assert_eq!(q.tasks[0].status, TaskStatus::Pending);

        q.add_task("Second task");
        assert_eq!(q.tasks.len(), 2);
        assert_eq!(q.tasks[1].title, "Second task");
    }

    #[test]
    fn quest_summary_no_tasks() {
        let q = Quest::new("Test Quest", "desc");
        let s = q.summary();
        assert!(s.contains("Test Quest"));
        assert!(s.contains("0/0 tasks"));
        assert!(s.contains("planning"));
    }

    #[test]
    fn quest_summary_with_done_tasks() {
        let mut q = Quest::new("Quest", "desc");
        q.add_task("Task 1");
        q.add_task("Task 2");
        q.tasks[0].status = TaskStatus::Done;
        let s = q.summary();
        assert!(s.contains("1/2 tasks"));
    }

    // ---- JSON roundtrip ----

    #[test]
    fn quest_json_roundtrip() {
        let mut original = Quest::new("Roundtrip Quest", "Testing serde");
        original.add_task("Task alpha");
        original.add_task("Task beta");
        original.tasks[0].status = TaskStatus::InProgress;
        original.github_issue = Some("owner/repo#42".into());

        let json = serde_json::to_string(&original).unwrap();
        let restored: Quest = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, original.id);
        assert_eq!(restored.title, original.title);
        assert_eq!(restored.description, original.description);
        assert_eq!(restored.status, original.status);
        assert_eq!(restored.tasks.len(), 2);
        assert_eq!(restored.tasks[0].status, TaskStatus::InProgress);
        assert_eq!(restored.github_issue, Some("owner/repo#42".into()));
    }

    // ---- QuestStore ----

    fn make_store() -> (TempDir, QuestStore) {
        let dir = TempDir::new().unwrap();
        let quests_dir = dir.path().join("quests");
        std::fs::create_dir_all(&quests_dir).unwrap();
        (dir, QuestStore::new(quests_dir))
    }

    #[test]
    fn store_save_and_load() {
        let (_dir, store) = make_store();
        let quest = Quest::new("Test Quest", "Description");
        let quest_id = quest.id.clone();
        store.save(&quest).unwrap();

        let loaded = store.load(&quest_id).unwrap().unwrap();
        assert_eq!(loaded.id, quest_id);
        assert_eq!(loaded.title, "Test Quest");
    }

    #[test]
    fn store_load_missing_returns_none() {
        let (_dir, store) = make_store();
        let result = store.load("nonexistent-id").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn store_list_empty_dir() {
        let (_dir, store) = make_store();
        let quests = store.list().unwrap();
        assert!(quests.is_empty());
    }

    #[test]
    fn store_list_multiple_quests() {
        let (_dir, store) = make_store();
        let q1 = Quest::new("Quest One", "first");
        let q2 = Quest::new("Quest Two", "second");
        store.save(&q1).unwrap();
        store.save(&q2).unwrap();

        let quests = store.list().unwrap();
        assert_eq!(quests.len(), 2);
        let titles: Vec<&str> = quests.iter().map(|q| q.title.as_str()).collect();
        assert!(titles.contains(&"Quest One"));
        assert!(titles.contains(&"Quest Two"));
    }

    #[test]
    fn store_delete() {
        let (_dir, store) = make_store();
        let quest = Quest::new("Delete Me", "desc");
        let id = quest.id.clone();
        store.save(&quest).unwrap();
        assert!(store.load(&id).unwrap().is_some());

        store.delete(&id).unwrap();
        assert!(store.load(&id).unwrap().is_none());
    }

    #[test]
    fn store_list_returns_empty_for_nonexistent_dir() {
        let dir = TempDir::new().unwrap();
        let store = QuestStore::new(dir.path().join("nonexistent"));
        let quests = store.list().unwrap();
        assert!(quests.is_empty());
    }

    #[test]
    fn quest_without_origin_deserializes() {
        // Backward compat: old JSON files won't have the origin field.
        let json = r#"{
            "id": "abc",
            "title": "Old Quest",
            "description": "no origin",
            "status": "active",
            "tasks": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        let quest: Quest = serde_json::from_str(json).unwrap();
        assert!(quest.origin.is_none());
        assert!(quest.github_issue.is_none());
    }

    #[test]
    fn task_with_origin_roundtrip() {
        let origin = TaskOrigin {
            channel: "telegram".into(),
            chat_id: Some(100),
            user_name: Some("josh".into()),
            user_id: Some(42),
        };
        let mut task = Task::new("q1", "Test task");
        task.origin = Some(origin);

        let json = serde_json::to_string(&task).unwrap();
        let restored: Task = serde_json::from_str(&json).unwrap();
        let o = restored.origin.unwrap();
        assert_eq!(o.channel, "telegram");
        assert_eq!(o.chat_id, Some(100));
        assert_eq!(o.user_name.as_deref(), Some("josh"));
        assert_eq!(o.user_id, Some(42));
    }

    #[test]
    fn task_without_origin_deserializes() {
        let json = r#"{
            "id": "t1",
            "quest_id": "q1",
            "title": "Old Task",
            "status": "pending"
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.origin.is_none());
        assert!(task.branch.is_none());
    }

    #[test]
    fn default_store_path_returns_hive_quests() {
        let path = default_store_path(Path::new("/workspace"));
        assert_eq!(path, PathBuf::from("/workspace/.hive/quests"));
    }
}

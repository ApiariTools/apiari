//! Integration tests for quest store CRUD lifecycle.

use hive::quest::{Quest, QuestStatus, QuestStore, Task, TaskStatus, default_store_path};
use std::path::Path;
use tempfile::TempDir;

fn make_store() -> (TempDir, QuestStore) {
    let dir = TempDir::new().unwrap();
    let quests_dir = dir.path().join("quests");
    std::fs::create_dir_all(&quests_dir).unwrap();
    (dir, QuestStore::new(quests_dir))
}

// ---- Full CRUD lifecycle ----

#[test]
fn quest_create_save_reload_roundtrip() {
    let (_dir, store) = make_store();

    // Create a quest with multiple tasks.
    let mut quest = Quest::new("Deploy v2", "Ship version 2 to production");
    quest.add_task("Update dependencies");
    quest.add_task("Run migration");
    quest.add_task("Smoke test");
    quest.tasks[0].status = TaskStatus::Done;
    quest.tasks[1].status = TaskStatus::InProgress;
    quest.tasks[1].assigned_to = Some("hive-1".into());
    quest.tasks[1].branch = Some("feat/migration".into());
    quest.github_issue = Some("ApiariTools/hive#10".into());

    let quest_id = quest.id.clone();
    store.save(&quest).unwrap();

    // Reload from disk.
    let loaded = store.load(&quest_id).unwrap().expect("quest should exist");

    assert_eq!(loaded.id, quest_id);
    assert_eq!(loaded.title, "Deploy v2");
    assert_eq!(loaded.description, "Ship version 2 to production");
    assert_eq!(loaded.status, QuestStatus::Planning);
    assert_eq!(loaded.tasks.len(), 3);

    // Verify task states survived the round-trip.
    assert_eq!(loaded.tasks[0].title, "Update dependencies");
    assert_eq!(loaded.tasks[0].status, TaskStatus::Done);
    assert_eq!(loaded.tasks[1].status, TaskStatus::InProgress);
    assert_eq!(loaded.tasks[1].assigned_to.as_deref(), Some("hive-1"));
    assert_eq!(loaded.tasks[1].branch.as_deref(), Some("feat/migration"));
    assert_eq!(loaded.tasks[2].status, TaskStatus::Pending);
    assert_eq!(loaded.github_issue.as_deref(), Some("ApiariTools/hive#10"));

    // Timestamps survive.
    assert_eq!(loaded.created_at, quest.created_at);
}

// ---- Status transitions ----

#[test]
fn quest_status_transitions_persist() {
    let (_dir, store) = make_store();

    let mut quest = Quest::new("Refactor auth", "Clean up auth module");
    assert_eq!(quest.status, QuestStatus::Planning);

    // Planning → Active
    quest.status = QuestStatus::Active;
    store.save(&quest).unwrap();
    let loaded = store.load(&quest.id).unwrap().unwrap();
    assert_eq!(loaded.status, QuestStatus::Active);

    // Active → Paused
    quest.status = QuestStatus::Paused;
    store.save(&quest).unwrap();
    let loaded = store.load(&quest.id).unwrap().unwrap();
    assert_eq!(loaded.status, QuestStatus::Paused);

    // Paused → Active → Complete
    quest.status = QuestStatus::Active;
    store.save(&quest).unwrap();
    quest.status = QuestStatus::Complete;
    store.save(&quest).unwrap();
    let loaded = store.load(&quest.id).unwrap().unwrap();
    assert_eq!(loaded.status, QuestStatus::Complete);
}

// ---- UUID uniqueness ----

#[test]
fn multiple_quests_have_unique_ids() {
    let (_dir, store) = make_store();

    let q1 = Quest::new("Quest A", "first");
    let q2 = Quest::new("Quest B", "second");
    let q3 = Quest::new("Quest C", "third");

    assert_ne!(q1.id, q2.id);
    assert_ne!(q2.id, q3.id);
    assert_ne!(q1.id, q3.id);

    store.save(&q1).unwrap();
    store.save(&q2).unwrap();
    store.save(&q3).unwrap();

    // All three coexist.
    let quests = store.list().unwrap();
    assert_eq!(quests.len(), 3);

    // Each loads independently.
    assert!(store.load(&q1.id).unwrap().is_some());
    assert!(store.load(&q2.id).unwrap().is_some());
    assert!(store.load(&q3.id).unwrap().is_some());
}

// ---- Missing file handling ----

#[test]
fn load_nonexistent_quest_returns_none() {
    let (_dir, store) = make_store();
    let result = store.load("does-not-exist-12345").unwrap();
    assert!(result.is_none());
}

#[test]
fn list_nonexistent_directory_returns_empty() {
    let dir = TempDir::new().unwrap();
    let store = QuestStore::new(dir.path().join("nonexistent"));
    let quests = store.list().unwrap();
    assert!(quests.is_empty());
}

// ---- Delete ----

#[test]
fn delete_quest_removes_file() {
    let (_dir, store) = make_store();

    let quest = Quest::new("Ephemeral", "delete me");
    let id = quest.id.clone();
    store.save(&quest).unwrap();

    assert!(store.load(&id).unwrap().is_some());
    store.delete(&id).unwrap();
    assert!(store.load(&id).unwrap().is_none());

    // Deleting again is a no-op.
    store.delete(&id).unwrap();
}

// ---- Overwrite ----

#[test]
fn save_overwrites_existing_quest() {
    let (_dir, store) = make_store();

    let mut quest = Quest::new("Original", "v1");
    let id = quest.id.clone();
    store.save(&quest).unwrap();

    quest.title = "Updated".into();
    quest.description = "v2".into();
    quest.add_task("New task");
    store.save(&quest).unwrap();

    let loaded = store.load(&id).unwrap().unwrap();
    assert_eq!(loaded.title, "Updated");
    assert_eq!(loaded.description, "v2");
    assert_eq!(loaded.tasks.len(), 1);
}

// ---- List ordering ----

#[test]
fn list_returns_newest_first() {
    let (_dir, store) = make_store();

    let q1 = Quest::new("Old", "first");
    store.save(&q1).unwrap();

    // Small sleep to ensure updated_at differs.
    std::thread::sleep(std::time::Duration::from_millis(10));

    let q2 = Quest::new("New", "second");
    store.save(&q2).unwrap();

    let quests = store.list().unwrap();
    assert_eq!(quests.len(), 2);
    assert_eq!(quests[0].title, "New", "newest should come first");
    assert_eq!(quests[1].title, "Old");
}

// ---- Task ID uniqueness within a quest ----

#[test]
fn tasks_within_quest_have_unique_ids() {
    let mut quest = Quest::new("Multi-task", "many tasks");
    quest.add_task("Task 1");
    quest.add_task("Task 2");
    quest.add_task("Task 3");

    let ids: Vec<&str> = quest.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
    assert_ne!(ids[0], ids[2]);

    // All tasks share the quest ID.
    for task in &quest.tasks {
        assert_eq!(task.quest_id, quest.id);
    }
}

// ---- Default store path ----

#[test]
fn default_store_path_joins_correctly() {
    let path = default_store_path(Path::new("/my/workspace"));
    assert_eq!(path, Path::new("/my/workspace/.hive/quests"));
}

// ---- Task new creates pending task ----

#[test]
fn task_new_fields() {
    let task = Task::new("quest-abc", "Implement feature X");
    assert_eq!(task.quest_id, "quest-abc");
    assert_eq!(task.title, "Implement feature X");
    assert_eq!(task.status, TaskStatus::Pending);
    assert!(task.assigned_to.is_none());
    assert!(task.branch.is_none());
    assert!(task.origin.is_none());
    assert!(!task.id.is_empty());
}

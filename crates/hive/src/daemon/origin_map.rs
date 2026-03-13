//! Origin map — tracks which Telegram chat (or CLI) originated each swarm worktree.
//!
//! Persisted as `.hive/origins.json`. Written when SwarmWatcher detects a new
//! worktree, read when routing completion/error notifications back to the
//! originating chat.

use crate::quest::TaskOrigin;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maps worktree_id → origin info. Persisted to `.hive/origins.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OriginMap {
    pub entries: HashMap<String, OriginEntry>,
}

/// A single origin mapping for a worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginEntry {
    pub origin: TaskOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl OriginMap {
    /// Load the origin map from disk, or return an empty map if missing/corrupt.
    pub fn load(workspace_root: &Path) -> Self {
        let path = Self::path(workspace_root);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save the origin map to disk.
    pub fn save(&self, workspace_root: &Path) -> std::io::Result<()> {
        let path = Self::path(workspace_root);
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Insert an origin entry for a worktree.
    pub fn insert(&mut self, worktree_id: String, entry: OriginEntry) {
        self.entries.insert(worktree_id, entry);
    }

    /// Look up the origin for a worktree.
    pub fn get(&self, worktree_id: &str) -> Option<&OriginEntry> {
        self.entries.get(worktree_id)
    }

    /// Remove a worktree's origin entry (e.g. on cleanup).
    pub fn remove(&mut self, worktree_id: &str) -> Option<OriginEntry> {
        self.entries.remove(worktree_id)
    }

    /// Return the chat_id to route a notification to for a given worktree,
    /// or `None` if the origin has no chat_id (falls back to alert_chat_id).
    pub fn route_target(&self, worktree_id: &str) -> Option<i64> {
        self.get(worktree_id).and_then(|e| e.origin.chat_id)
    }

    fn path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".hive").join("origins.json")
    }
}

/// Return the chat_id to route a notification to for a given worktree.
///
/// Looks up the originating chat in the origin map. Falls back to
/// `alert_chat_id` when the origin is unknown or has no chat_id.
pub fn route_notification(origin_map: &OriginMap, worktree_id: &str, alert_chat_id: i64) -> i64 {
    origin_map
        .route_target(worktree_id)
        .unwrap_or(alert_chat_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let hive_dir = root.join(".hive");
        std::fs::create_dir_all(&hive_dir).unwrap();
        (dir, root)
    }

    #[test]
    fn load_missing_returns_empty() {
        let (dir, root) = setup();
        let map = OriginMap::load(&root);
        assert!(map.entries.is_empty());
        drop(dir);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let (dir, root) = setup();
        let mut map = OriginMap::default();
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(100),
                    user_name: Some("josh".into()),
                    user_id: Some(42),
                },
                quest_id: Some("q1".into()),
                task_id: None,
                branch: Some("swarm/fix-auth".into()),
                created_at: Utc::now(),
            },
        );
        map.save(&root).unwrap();

        let loaded = OriginMap::load(&root);
        assert_eq!(loaded.entries.len(), 1);
        let entry = loaded.get("wt-1").unwrap();
        assert_eq!(entry.origin.channel, "telegram");
        assert_eq!(entry.origin.chat_id, Some(100));
        assert_eq!(entry.branch.as_deref(), Some("swarm/fix-auth"));
        drop(dir);
    }

    #[test]
    fn route_target_returns_chat_id() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(200),
                    user_name: None,
                    user_id: None,
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: Utc::now(),
            },
        );
        assert_eq!(map.route_target("wt-1"), Some(200));
    }

    #[test]
    fn route_target_returns_none_for_cli_origin() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-2".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "cli".into(),
                    chat_id: None,
                    user_name: None,
                    user_id: None,
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: Utc::now(),
            },
        );
        assert_eq!(map.route_target("wt-2"), None);
    }

    #[test]
    fn route_target_returns_none_for_unknown() {
        let map = OriginMap::default();
        assert_eq!(map.route_target("unknown"), None);
    }

    #[test]
    fn remove_entry() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin::default(),
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: Utc::now(),
            },
        );
        assert!(map.get("wt-1").is_some());
        let removed = map.remove("wt-1");
        assert!(removed.is_some());
        assert!(map.get("wt-1").is_none());
    }

    // --- route_notification tests ---

    #[test]
    fn test_route_returns_none_for_unknown_id() {
        let map = OriginMap::default();
        // Unknown worktree falls back to alert_chat_id.
        assert_eq!(super::route_notification(&map, "nonexistent", 999), 999);
    }

    #[test]
    fn test_insert_then_get_roundtrip() {
        let mut map = OriginMap::default();
        let entry = OriginEntry {
            origin: TaskOrigin {
                channel: "telegram".into(),
                chat_id: Some(42),
                user_name: Some("alice".into()),
                user_id: Some(7),
            },
            quest_id: Some("q-abc".into()),
            task_id: Some("t-123".into()),
            branch: Some("swarm/do-stuff".into()),
            created_at: Utc::now(),
        };
        map.insert("wt-99".into(), entry);

        let got = map.get("wt-99").unwrap();
        assert_eq!(got.origin.chat_id, Some(42));
        assert_eq!(got.origin.user_name.as_deref(), Some("alice"));
        assert_eq!(got.quest_id.as_deref(), Some("q-abc"));
        assert_eq!(got.task_id.as_deref(), Some("t-123"));
        assert_eq!(got.branch.as_deref(), Some("swarm/do-stuff"));
    }

    #[test]
    fn test_remove_clears_entry() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-x".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(1),
                    ..Default::default()
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: Utc::now(),
            },
        );
        assert!(map.get("wt-x").is_some());
        assert_eq!(map.route_target("wt-x"), Some(1));

        map.remove("wt-x");

        assert!(map.get("wt-x").is_none());
        assert_eq!(map.route_target("wt-x"), None);
        assert_eq!(super::route_notification(&map, "wt-x", 500), 500);
    }

    #[test]
    fn test_stale_entry_replaced_by_new_insert() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(100),
                    user_name: Some("old_user".into()),
                    ..Default::default()
                },
                quest_id: None,
                task_id: None,
                branch: Some("swarm/old-branch".into()),
                created_at: Utc::now(),
            },
        );
        assert_eq!(map.route_target("wt-1"), Some(100));

        // Overwrite with a new entry.
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(200),
                    user_name: Some("new_user".into()),
                    ..Default::default()
                },
                quest_id: None,
                task_id: None,
                branch: Some("swarm/new-branch".into()),
                created_at: Utc::now(),
            },
        );

        let entry = map.get("wt-1").unwrap();
        assert_eq!(entry.origin.chat_id, Some(200));
        assert_eq!(entry.origin.user_name.as_deref(), Some("new_user"));
        assert_eq!(entry.branch.as_deref(), Some("swarm/new-branch"));
        assert_eq!(map.entries.len(), 1, "should not create a duplicate");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut map = OriginMap::default();
        let now = Utc::now();
        map.insert(
            "wt-ser".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(555),
                    user_name: Some("bob".into()),
                    user_id: Some(77),
                },
                quest_id: Some("q-1".into()),
                task_id: None,
                branch: Some("swarm/serialize-test".into()),
                created_at: now,
            },
        );

        let json = serde_json::to_string(&map).unwrap();
        let deserialized: OriginMap = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.entries.len(), 1);
        let entry = deserialized.get("wt-ser").unwrap();
        assert_eq!(entry.origin.channel, "telegram");
        assert_eq!(entry.origin.chat_id, Some(555));
        assert_eq!(entry.origin.user_name.as_deref(), Some("bob"));
        assert_eq!(entry.origin.user_id, Some(77));
        assert_eq!(entry.quest_id.as_deref(), Some("q-1"));
        assert!(entry.task_id.is_none());
        assert_eq!(entry.branch.as_deref(), Some("swarm/serialize-test"));
        assert_eq!(entry.created_at, now);
    }

    #[test]
    fn test_route_notification_to_origin_chat() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(100),
                    user_name: Some("alice".into()),
                    user_id: Some(1),
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: Utc::now(),
            },
        );
        assert_eq!(route_notification(&map, "wt-1", 999), 100);
    }

    #[test]
    fn test_route_notification_falls_back_to_alert() {
        let map = OriginMap::default();
        assert_eq!(route_notification(&map, "unknown", 999), 999);
    }

    #[test]
    fn test_route_notification_cli_origin_falls_back() {
        let mut map = OriginMap::default();
        map.insert(
            "wt-1".into(),
            OriginEntry {
                origin: TaskOrigin {
                    channel: "cli".into(),
                    chat_id: None,
                    user_name: None,
                    user_id: None,
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: Utc::now(),
            },
        );
        // CLI origin has no chat_id, so falls back to alert_chat_id.
        assert_eq!(route_notification(&map, "wt-1", 999), 999);
    }
}

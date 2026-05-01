//! Per-chat Claude session tracking with persistence.
//!
//! Each Telegram chat gets its own Claude session. Sessions are persisted to
//! `.hive/sessions.json` so the daemon can resume after restarts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Persistent state for all chat sessions + buzz reader offset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStoreState {
    /// Active session per chat_id.
    #[serde(default)]
    pub active: HashMap<i64, ChatSession>,

    /// Archived (completed) sessions per chat_id.
    #[serde(default)]
    pub archived: HashMap<i64, Vec<ArchivedSession>>,

    /// Buzz JSONL reader byte offset — survives daemon restarts.
    #[serde(default)]
    pub buzz_reader_offset: u64,
}

/// A live Claude session associated with a chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub session_id: String,
    pub turn_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub nudge_sent: bool,
}

/// A previously completed session, kept for /history and /resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedSession {
    pub session_id: String,
    pub turn_count: u64,
    pub created_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

/// Manages session state with persistence to disk.
pub struct SessionStore {
    path: PathBuf,
    state: SessionStoreState,
}

impl SessionStore {
    /// Load or create a session store at the given path.
    pub fn load(workspace_root: &Path) -> Self {
        let path = workspace_root.join(".hive/sessions.json");
        let state: SessionStoreState = apiari_common::state::load_state(&path).unwrap_or_default();
        Self { path, state }
    }

    /// Persist current state to disk.
    pub fn save(&self) -> color_eyre::Result<()> {
        apiari_common::state::save_state(&self.path, &self.state)
            .map_err(|e| color_eyre::eyre::eyre!("failed to save sessions: {e}"))
    }

    /// Get the active session for a chat, if any.
    pub fn get_active(&self, chat_id: i64) -> Option<&ChatSession> {
        self.state.active.get(&chat_id)
    }

    /// Start a new session for a chat. Archives any existing active session.
    pub fn start_session(&mut self, chat_id: i64, session_id: String) {
        // Archive the current session if one exists.
        if let Some(old) = self.state.active.remove(&chat_id) {
            let archived = ArchivedSession {
                session_id: old.session_id,
                turn_count: old.turn_count,
                created_at: old.created_at,
                ended_at: Utc::now(),
            };
            self.state
                .archived
                .entry(chat_id)
                .or_default()
                .push(archived);
        }

        let session = ChatSession {
            session_id,
            turn_count: 0,
            created_at: Utc::now(),
            last_active: Utc::now(),
            nudge_sent: false,
        };
        self.state.active.insert(chat_id, session);
    }

    /// Increment turn count and update last_active for a chat's session.
    /// Returns whether a nudge should be sent (crosses threshold, not yet sent).
    pub fn record_turn(&mut self, chat_id: i64, nudge_threshold: u64) -> bool {
        if let Some(session) = self.state.active.get_mut(&chat_id) {
            session.turn_count += 1;
            session.last_active = Utc::now();

            if session.turn_count >= nudge_threshold && !session.nudge_sent {
                session.nudge_sent = true;
                return true;
            }
        }
        false
    }

    /// Archive the active session for a chat (/reset).
    /// Returns true if there was an active session to archive.
    pub fn reset_session(&mut self, chat_id: i64) -> bool {
        if let Some(old) = self.state.active.remove(&chat_id) {
            let archived = ArchivedSession {
                session_id: old.session_id,
                turn_count: old.turn_count,
                created_at: old.created_at,
                ended_at: Utc::now(),
            };
            self.state
                .archived
                .entry(chat_id)
                .or_default()
                .push(archived);
            true
        } else {
            false
        }
    }

    /// Get archived sessions for a chat (/history).
    pub fn history(&self, chat_id: i64) -> &[ArchivedSession] {
        self.state
            .archived
            .get(&chat_id)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    /// Find an archived session by ID prefix (/resume).
    /// Returns the session_id if found.
    pub fn find_archived(&self, chat_id: i64, id_prefix: &str) -> Option<String> {
        let archives = self.state.archived.get(&chat_id)?;
        let matches: Vec<_> = archives
            .iter()
            .filter(|a| a.session_id.starts_with(id_prefix))
            .collect();
        if matches.len() == 1 {
            Some(matches[0].session_id.clone())
        } else {
            None
        }
    }

    /// Resume an archived session — makes it the active session for a chat.
    /// Returns true if the session was found and resumed.
    pub fn resume_session(&mut self, chat_id: i64, session_id: &str) -> bool {
        let archives = match self.state.archived.get_mut(&chat_id) {
            Some(a) => a,
            None => return false,
        };

        let idx = archives.iter().position(|a| a.session_id == session_id);

        let Some(idx) = idx else { return false };

        let archived = archives.remove(idx);

        // Archive current active session if any.
        if let Some(old) = self.state.active.remove(&chat_id) {
            archives.push(ArchivedSession {
                session_id: old.session_id,
                turn_count: old.turn_count,
                created_at: old.created_at,
                ended_at: Utc::now(),
            });
        }

        // Restore the archived session as active.
        self.state.active.insert(
            chat_id,
            ChatSession {
                session_id: archived.session_id,
                turn_count: archived.turn_count,
                created_at: archived.created_at,
                last_active: Utc::now(),
                nudge_sent: false,
            },
        );

        true
    }

    /// Get/set the buzz reader byte offset.
    pub fn buzz_offset(&self) -> u64 {
        self.state.buzz_reader_offset
    }

    pub fn set_buzz_offset(&mut self, offset: u64) {
        self.state.buzz_reader_offset = offset;
    }

    /// Remove active session for a chat (e.g. when session dies unexpectedly).
    pub fn remove_active(&mut self, chat_id: i64) {
        self.state.active.remove(&chat_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_store() -> SessionStore {
        SessionStore {
            path: PathBuf::from("/tmp/test-sessions.json"),
            state: SessionStoreState::default(),
        }
    }

    #[test]
    fn test_start_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.turn_count, 0);
        assert!(!active.nudge_sent);
    }

    #[test]
    fn test_start_session_archives_previous() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.start_session(100, "sess-2".into());

        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-2");

        let history = store.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-1");
    }

    #[test]
    fn test_record_turn_and_nudge() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());

        // No nudge below threshold.
        for _ in 0..49 {
            assert!(!store.record_turn(100, 50));
        }

        // Nudge at threshold.
        assert!(store.record_turn(100, 50));

        // No second nudge.
        assert!(!store.record_turn(100, 50));

        assert_eq!(store.get_active(100).unwrap().turn_count, 51);
    }

    #[test]
    fn test_reset_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 50);

        assert!(store.reset_session(100));
        assert!(store.get_active(100).is_none());

        let history = store.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-1");
        assert_eq!(history[0].turn_count, 1);

        // Double reset is a no-op.
        assert!(!store.reset_session(100));
    }

    #[test]
    fn test_find_archived_by_prefix() {
        let mut store = empty_store();
        store.start_session(100, "abc-123-def".into());
        store.reset_session(100);
        store.start_session(100, "xyz-789-ghi".into());
        store.reset_session(100);

        assert_eq!(store.find_archived(100, "abc"), Some("abc-123-def".into()));
        assert_eq!(store.find_archived(100, "xyz"), Some("xyz-789-ghi".into()));
        assert_eq!(store.find_archived(100, "nope"), None);
    }

    #[test]
    fn test_resume_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 50);
        store.reset_session(100);
        store.start_session(100, "sess-2".into());

        assert!(store.resume_session(100, "sess-1"));

        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.turn_count, 1);

        // sess-2 should be archived now.
        let history = store.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-2");
    }

    #[test]
    fn test_buzz_offset() {
        let mut store = empty_store();
        assert_eq!(store.buzz_offset(), 0);
        store.set_buzz_offset(1234);
        assert_eq!(store.buzz_offset(), 1234);
    }

    #[test]
    fn test_history_empty() {
        let store = empty_store();
        assert!(store.history(999).is_empty());
    }

    #[test]
    fn test_remove_active() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        assert!(store.get_active(100).is_some());
        store.remove_active(100);
        assert!(store.get_active(100).is_none());
    }

    #[test]
    fn test_remove_active_noop_on_missing() {
        let mut store = empty_store();
        // Should not panic when removing from a chat with no session.
        store.remove_active(999);
        assert!(store.get_active(999).is_none());
    }

    #[test]
    fn test_record_turn_noop_on_missing_session() {
        let mut store = empty_store();
        // No active session for this chat — should return false, not panic.
        assert!(!store.record_turn(999, 5));
    }

    #[test]
    fn test_find_archived_ambiguous_prefix() {
        let mut store = empty_store();
        // Two sessions that share a prefix.
        store.start_session(100, "abc-111".into());
        store.reset_session(100);
        store.start_session(100, "abc-222".into());
        store.reset_session(100);

        // "abc" matches both — should return None (ambiguous).
        assert_eq!(store.find_archived(100, "abc"), None);

        // Exact-enough prefix should still work.
        assert_eq!(store.find_archived(100, "abc-111"), Some("abc-111".into()));
        assert_eq!(store.find_archived(100, "abc-222"), Some("abc-222".into()));
    }

    #[test]
    fn test_find_archived_no_archives() {
        let store = empty_store();
        // No sessions at all for this chat.
        assert_eq!(store.find_archived(100, "any"), None);
    }

    #[test]
    fn test_resume_nonexistent_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.reset_session(100);

        // Try to resume a session that doesn't exist.
        assert!(!store.resume_session(100, "nope"));
        // Original archived session is untouched.
        assert_eq!(store.history(100).len(), 1);
    }

    #[test]
    fn test_resume_from_chat_with_no_archives() {
        let mut store = empty_store();
        assert!(!store.resume_session(100, "anything"));
    }

    #[test]
    fn test_multiple_chats_isolated() {
        let mut store = empty_store();
        store.start_session(100, "sess-a".into());
        store.start_session(200, "sess-b".into());

        let a = store.get_active(100).unwrap();
        let b = store.get_active(200).unwrap();
        assert_eq!(a.session_id, "sess-a");
        assert_eq!(b.session_id, "sess-b");

        // Resetting one chat doesn't affect the other.
        store.reset_session(100);
        assert!(store.get_active(100).is_none());
        assert!(store.get_active(200).is_some());
    }

    #[test]
    fn test_multiple_resets_build_history() {
        let mut store = empty_store();
        store.start_session(100, "s1".into());
        store.reset_session(100);
        store.start_session(100, "s2".into());
        store.reset_session(100);
        store.start_session(100, "s3".into());
        store.reset_session(100);

        let history = store.history(100);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].session_id, "s1");
        assert_eq!(history[1].session_id, "s2");
        assert_eq!(history[2].session_id, "s3");
    }

    #[test]
    fn test_nudge_not_sent_below_threshold() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());

        // Record 9 turns with threshold of 10 — no nudge.
        for _ in 0..9 {
            assert!(!store.record_turn(100, 10));
        }
        assert_eq!(store.get_active(100).unwrap().turn_count, 9);
        assert!(!store.get_active(100).unwrap().nudge_sent);
    }

    #[test]
    fn test_nudge_fires_exactly_at_threshold() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());

        // Threshold of 1 means nudge on the very first turn.
        assert!(store.record_turn(100, 1));
        assert!(store.get_active(100).unwrap().nudge_sent);
        assert_eq!(store.get_active(100).unwrap().turn_count, 1);

        // Subsequent turns don't trigger again.
        assert!(!store.record_turn(100, 1));
    }

    #[test]
    fn test_start_session_resets_nudge_state() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 1); // triggers nudge
        assert!(store.get_active(100).unwrap().nudge_sent);

        // Starting a new session should have fresh nudge state.
        store.start_session(100, "sess-2".into());
        assert!(!store.get_active(100).unwrap().nudge_sent);
        assert_eq!(store.get_active(100).unwrap().turn_count, 0);
    }

    #[test]
    fn test_resume_preserves_turn_count() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        for _ in 0..5 {
            store.record_turn(100, 100);
        }
        store.reset_session(100);

        // Archived session should have 5 turns.
        assert_eq!(store.history(100)[0].turn_count, 5);

        // Resume it — turn count should be preserved.
        store.resume_session(100, "sess-1");
        assert_eq!(store.get_active(100).unwrap().turn_count, 5);
        // But nudge_sent should be reset on resume.
        assert!(!store.get_active(100).unwrap().nudge_sent);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        let mut store = SessionStore::load(root);
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 50);
        store.set_buzz_offset(42);
        store.save().unwrap();

        // Reload from disk.
        let store2 = SessionStore::load(root);
        let active = store2.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.turn_count, 1);
        assert_eq!(store2.buzz_offset(), 42);
    }

    #[test]
    fn test_load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::load(dir.path());
        assert!(store.get_active(100).is_none());
        assert_eq!(store.buzz_offset(), 0);
    }

    #[test]
    fn test_state_default_is_empty() {
        let state = SessionStoreState::default();
        assert!(state.active.is_empty());
        assert!(state.archived.is_empty());
        assert_eq!(state.buzz_reader_offset, 0);
    }

    // ---- additional edge cases ----

    #[test]
    fn test_resume_with_no_current_active_session() {
        let mut store = empty_store();
        // Create and archive a session.
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 100);
        store.record_turn(100, 100);
        store.reset_session(100);
        assert!(store.get_active(100).is_none());

        // Resume when there is no active session — should work fine.
        assert!(store.resume_session(100, "sess-1"));
        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.turn_count, 2);
        // No archived sessions should remain (it was the only one and it got resumed).
        assert!(store.history(100).is_empty());
    }

    #[test]
    fn test_nudge_threshold_zero() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        // Threshold of 0: turn_count starts at 0, after increment to 1 it is >= 0,
        // so nudge should fire on the very first turn.
        assert!(store.record_turn(100, 0));
        // Second turn should not re-nudge.
        assert!(!store.record_turn(100, 0));
    }

    #[test]
    fn test_start_session_with_empty_id() {
        let mut store = empty_store();
        store.start_session(100, String::new());
        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "");
        assert_eq!(active.turn_count, 0);
    }

    #[test]
    fn test_buzz_offset_max_value() {
        let mut store = empty_store();
        store.set_buzz_offset(u64::MAX);
        assert_eq!(store.buzz_offset(), u64::MAX);
    }

    #[test]
    fn test_save_and_load_with_archives() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        let mut store = SessionStore::load(root);
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 100);
        store.reset_session(100);
        store.start_session(100, "sess-2".into());
        store.save().unwrap();

        let store2 = SessionStore::load(root);
        // Active session should be sess-2.
        assert_eq!(store2.get_active(100).unwrap().session_id, "sess-2");
        // Archive should contain sess-1.
        let history = store2.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-1");
        assert_eq!(history[0].turn_count, 1);
    }

    #[test]
    fn test_remove_active_preserves_archives() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.reset_session(100);
        store.start_session(100, "sess-2".into());

        // Remove active without archiving.
        store.remove_active(100);
        assert!(store.get_active(100).is_none());
        // The previously archived session should still be there.
        assert_eq!(store.history(100).len(), 1);
        assert_eq!(store.history(100)[0].session_id, "sess-1");
    }

    #[test]
    fn test_find_archived_with_empty_prefix() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.reset_session(100);
        store.start_session(100, "sess-2".into());
        store.reset_session(100);

        // Empty prefix matches all — ambiguous when >1 archived.
        assert_eq!(store.find_archived(100, ""), None);
    }

    #[test]
    fn test_find_archived_empty_prefix_single_session() {
        let mut store = empty_store();
        store.start_session(100, "only-one".into());
        store.reset_session(100);

        // Empty prefix with exactly one archived session — should match.
        assert_eq!(store.find_archived(100, ""), Some("only-one".into()));
    }

    #[test]
    fn test_resume_archives_current_active_in_order() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.reset_session(100); // archived: [sess-1]
        store.start_session(100, "sess-2".into());
        store.reset_session(100); // archived: [sess-1, sess-2]
        store.start_session(100, "sess-3".into()); // active: sess-3

        // Resume sess-1: archives sess-3, removes sess-1 from archive, activates sess-1.
        store.resume_session(100, "sess-1");

        let history = store.history(100);
        // Should have sess-2 and sess-3 in archives (sess-1 is now active).
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].session_id, "sess-2");
        assert_eq!(history[1].session_id, "sess-3");
    }

    #[test]
    fn test_resume_across_chats_isolated() {
        let mut store = empty_store();
        store.start_session(100, "a-sess-1".into());
        store.reset_session(100);
        store.start_session(200, "b-sess-1".into());
        store.reset_session(200);

        // Resume in chat 100 should not affect chat 200.
        assert!(store.resume_session(100, "a-sess-1"));
        assert_eq!(store.get_active(100).unwrap().session_id, "a-sess-1");
        assert!(store.get_active(200).is_none());
        // Chat 200's archive is still intact.
        assert_eq!(store.history(200).len(), 1);
    }

    #[test]
    fn test_record_turn_updates_last_active() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        let created = store.get_active(100).unwrap().last_active;

        // Small sleep to ensure timestamp difference.
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.record_turn(100, 100);

        let updated = store.get_active(100).unwrap().last_active;
        assert!(updated >= created);
    }

    #[test]
    fn test_save_load_preserves_buzz_offset() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        let mut store = SessionStore::load(root);
        store.set_buzz_offset(9999);
        store.save().unwrap();

        let store2 = SessionStore::load(root);
        assert_eq!(store2.buzz_offset(), 9999);
    }

    #[test]
    fn test_state_serde_roundtrip() {
        let mut state = SessionStoreState {
            buzz_reader_offset: 42,
            ..SessionStoreState::default()
        };
        state.active.insert(
            100,
            ChatSession {
                session_id: "test-id".into(),
                turn_count: 7,
                created_at: Utc::now(),
                last_active: Utc::now(),
                nudge_sent: true,
            },
        );

        let json = serde_json::to_string(&state).unwrap();
        let restored: SessionStoreState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.buzz_reader_offset, 42);
        assert_eq!(restored.active.get(&100).unwrap().session_id, "test-id");
        assert_eq!(restored.active.get(&100).unwrap().turn_count, 7);
        assert!(restored.active.get(&100).unwrap().nudge_sent);
    }

    #[test]
    fn test_reset_then_start_fresh_session() {
        let mut store = empty_store();
        store.start_session(100, "old".into());
        for _ in 0..10 {
            store.record_turn(100, 5);
        }
        store.reset_session(100);
        store.start_session(100, "fresh".into());

        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "fresh");
        assert_eq!(active.turn_count, 0);
        assert!(!active.nudge_sent);

        // Old session is in history.
        assert_eq!(store.history(100)[0].session_id, "old");
        assert_eq!(store.history(100)[0].turn_count, 10);
    }
}

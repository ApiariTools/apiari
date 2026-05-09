//! Reminder storage with persistence to `.hive/reminders.json`.
//!
//! Uses `apiari_common::state::{load_state, save_state}` for atomic JSON
//! read/write, the same pattern as `session_store.rs`.

use super::types::{ReminderKind, StoredReminder};
use chrono::Utc;
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Errors returned by [`ReminderStore::cancel`].
#[derive(Debug)]
pub enum CancelError {
    /// No reminder matched the given ID prefix.
    NotFound,
    /// Multiple reminders matched the given ID prefix.
    Ambiguous(Vec<String>),
}

impl std::fmt::Display for CancelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no reminder found"),
            Self::Ambiguous(ids) => {
                write!(f, "ambiguous ID, matches: ")?;
                for (i, id) in ids.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", &id[..8.min(id.len())])?;
                }
                Ok(())
            }
        }
    }
}

/// On-disk format for the reminders file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReminderStoreState {
    #[serde(default)]
    reminders: Vec<StoredReminder>,
}

/// Manages user-facing reminders with persistence.
pub struct ReminderStore {
    path: PathBuf,
    state: ReminderStoreState,
}

impl ReminderStore {
    /// Load or create a reminder store at `<workspace_root>/.hive/reminders.json`.
    pub fn load(workspace_root: &Path) -> Self {
        let path = workspace_root.join(".hive/reminders.json");
        let mut state: ReminderStoreState =
            apiari_common::state::load_state(&path).unwrap_or_default();

        // Prune cancelled one-shot reminders on load.
        state
            .reminders
            .retain(|r| !r.cancelled || r.kind == ReminderKind::Cron);

        // Also prune cancelled cron reminders.
        state.reminders.retain(|r| !r.cancelled);

        Self { path, state }
    }

    /// Persist current state to disk.
    pub fn save(&self) -> color_eyre::Result<()> {
        apiari_common::state::save_state(&self.path, &self.state)
            .map_err(|e| color_eyre::eyre::eyre!("failed to save reminders: {e}"))
    }

    /// Add a new reminder. Returns the reminder's ID.
    pub fn add(&mut self, reminder: StoredReminder) -> String {
        let id = reminder.id.clone();
        self.state.reminders.push(reminder);
        id
    }

    /// Cancel a reminder by ID prefix. Returns the full ID on success.
    pub fn cancel(&mut self, id_prefix: &str) -> Result<String, CancelError> {
        let matches: Vec<usize> = self
            .state
            .reminders
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.cancelled && r.id.starts_with(id_prefix))
            .map(|(i, _)| i)
            .collect();

        match matches.len() {
            0 => Err(CancelError::NotFound),
            1 => {
                let r = &mut self.state.reminders[matches[0]];
                r.cancelled = true;
                let id = r.id.clone();
                // Remove it immediately.
                self.state.reminders.remove(matches[0]);
                Ok(id)
            }
            _ => {
                let ids: Vec<String> = matches
                    .iter()
                    .map(|&i| self.state.reminders[i].id.clone())
                    .collect();
                Err(CancelError::Ambiguous(ids))
            }
        }
    }

    /// Get all active (non-cancelled) reminders, sorted by next fire time.
    pub fn active(&self) -> Vec<&StoredReminder> {
        let mut active: Vec<&StoredReminder> = self
            .state
            .reminders
            .iter()
            .filter(|r| !r.cancelled)
            .collect();
        active.sort_by_key(|r| r.fire_at);
        active
    }

    /// Check for fired reminders and advance cron reminders.
    ///
    /// Returns the list of reminders that fired. One-shot reminders are removed;
    /// cron reminders have their `fire_at` updated to the next occurrence.
    pub fn check_and_advance(&mut self) -> Vec<StoredReminder> {
        let now = Utc::now();
        let mut fired = Vec::new();

        // Collect indices of fired reminders.
        let mut to_remove = Vec::new();
        for (i, r) in self.state.reminders.iter_mut().enumerate() {
            if r.cancelled {
                continue;
            }
            let Some(fire_at) = r.fire_at else {
                continue;
            };
            if fire_at > now {
                continue;
            }

            fired.push(r.clone());

            match r.kind {
                ReminderKind::Once => {
                    to_remove.push(i);
                }
                ReminderKind::Cron => {
                    // Advance to next occurrence.
                    if let Some(expr) = &r.cron_expr {
                        match advance_cron(expr, &now) {
                            Some(next) => {
                                r.fire_at = Some(next);
                                tracing::debug!(id = &r.id[..8.min(r.id.len())], next = %next.format("%Y-%m-%d %H:%M UTC"), "Cron reminder advanced");
                            }
                            None => {
                                // Cron expression has no future occurrences — remove it.
                                tracing::info!(
                                    id = &r.id[..8.min(r.id.len())],
                                    "Cron reminder has no future occurrences, removing"
                                );
                                to_remove.push(i);
                            }
                        }
                    } else {
                        // No cron_expr — shouldn't happen, but remove to be safe.
                        to_remove.push(i);
                    }
                }
            }
        }

        // Remove in reverse order to preserve indices.
        for i in to_remove.into_iter().rev() {
            self.state.reminders.remove(i);
        }

        fired
    }
}

/// Compute the next cron occurrence after `after`.
fn advance_cron(expr: &str, after: &chrono::DateTime<Utc>) -> Option<chrono::DateTime<Utc>> {
    let cron: Cron = expr.parse().ok()?;
    cron.find_next_occurrence(after, false).ok()
}

/// Compute the next cron occurrence from now (used at creation time).
pub fn next_cron_fire(expr: &str) -> Result<chrono::DateTime<Utc>, String> {
    let cron: Cron = expr
        .parse()
        .map_err(|e| format!("invalid cron expression: {e}"))?;
    cron.find_next_occurrence(&Utc::now(), false)
        .map_err(|e| format!("could not compute next occurrence: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_once(id: &str, message: &str, fire_at: chrono::DateTime<Utc>) -> StoredReminder {
        StoredReminder {
            id: id.into(),
            message: message.into(),
            kind: ReminderKind::Once,
            fire_at: Some(fire_at),
            cron_expr: None,
            created_at: Utc::now(),
            chat_id: None,
            cancelled: false,
        }
    }

    fn make_cron(
        id: &str,
        message: &str,
        cron_expr: &str,
        fire_at: chrono::DateTime<Utc>,
    ) -> StoredReminder {
        StoredReminder {
            id: id.into(),
            message: message.into(),
            kind: ReminderKind::Cron,
            fire_at: Some(fire_at),
            cron_expr: Some(cron_expr.into()),
            created_at: Utc::now(),
            chat_id: None,
            cancelled: false,
        }
    }

    fn empty_store() -> ReminderStore {
        ReminderStore {
            path: PathBuf::from("/tmp/test-reminders.json"),
            state: ReminderStoreState::default(),
        }
    }

    #[test]
    fn test_add_and_retrieve() {
        let mut store = empty_store();
        let r = make_once("abc-123", "test", Utc::now() + Duration::hours(1));
        store.add(r);
        let active = store.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "abc-123");
    }

    #[test]
    fn test_cancel_by_prefix() {
        let mut store = empty_store();
        store.add(make_once(
            "abc-123-def",
            "test",
            Utc::now() + Duration::hours(1),
        ));
        let id = store.cancel("abc").unwrap();
        assert_eq!(id, "abc-123-def");
        assert!(store.active().is_empty());
    }

    #[test]
    fn test_cancel_not_found() {
        let mut store = empty_store();
        store.add(make_once(
            "abc-123",
            "test",
            Utc::now() + Duration::hours(1),
        ));
        assert!(matches!(store.cancel("xyz"), Err(CancelError::NotFound)));
    }

    #[test]
    fn test_cancel_ambiguous() {
        let mut store = empty_store();
        store.add(make_once(
            "abc-111",
            "test1",
            Utc::now() + Duration::hours(1),
        ));
        store.add(make_once(
            "abc-222",
            "test2",
            Utc::now() + Duration::hours(2),
        ));
        match store.cancel("abc") {
            Err(CancelError::Ambiguous(ids)) => {
                assert_eq!(ids.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn test_active_excludes_cancelled() {
        let mut store = empty_store();
        store.add(make_once(
            "abc-123",
            "keep",
            Utc::now() + Duration::hours(1),
        ));
        store.add(make_once(
            "def-456",
            "cancel me",
            Utc::now() + Duration::hours(2),
        ));
        store.cancel("def").unwrap();
        let active = store.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "abc-123");
    }

    #[test]
    fn test_check_once_fires_and_removes() {
        let mut store = empty_store();
        // Fire time in the past.
        store.add(make_once(
            "abc-123",
            "overdue",
            Utc::now() - Duration::seconds(10),
        ));
        let fired = store.check_and_advance();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].message, "overdue");
        // Should be gone from active.
        assert!(store.active().is_empty());
    }

    #[test]
    fn test_check_not_yet_due() {
        let mut store = empty_store();
        store.add(make_once(
            "abc-123",
            "later",
            Utc::now() + Duration::hours(1),
        ));
        let fired = store.check_and_advance();
        assert!(fired.is_empty());
        assert_eq!(store.active().len(), 1);
    }

    #[test]
    fn test_check_cron_fires_and_advances() {
        let mut store = empty_store();
        // Cron that fires every minute, with fire_at in the past.
        store.add(make_cron(
            "cron-1",
            "every minute",
            "* * * * *",
            Utc::now() - Duration::seconds(10),
        ));
        let fired = store.check_and_advance();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].message, "every minute");
        // Should still be active with an advanced fire_at.
        let active = store.active();
        assert_eq!(active.len(), 1);
        assert!(active[0].fire_at.unwrap() > Utc::now());
    }

    #[test]
    fn test_empty_store_check() {
        let mut store = empty_store();
        let fired = store.check_and_advance();
        assert!(fired.is_empty());
    }

    #[test]
    fn test_active_sorted_by_fire_at() {
        let mut store = empty_store();
        let now = Utc::now();
        store.add(make_once("c", "third", now + Duration::hours(3)));
        store.add(make_once("a", "first", now + Duration::hours(1)));
        store.add(make_once("b", "second", now + Duration::hours(2)));
        let active = store.active();
        assert_eq!(active[0].message, "first");
        assert_eq!(active[1].message, "second");
        assert_eq!(active[2].message, "third");
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        let mut store = ReminderStore::load(root);
        store.add(make_once(
            "test-1",
            "hello",
            Utc::now() + Duration::hours(1),
        ));
        store.add(make_cron(
            "test-2",
            "daily",
            "0 9 * * *",
            Utc::now() + Duration::hours(2),
        ));
        store.save().unwrap();

        let store2 = ReminderStore::load(root);
        let active = store2.active();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, "test-1");
        assert_eq!(active[1].id, "test-2");
    }

    #[test]
    fn test_next_cron_fire_valid() {
        let next = next_cron_fire("* * * * *").unwrap();
        assert!(next > Utc::now());
    }

    #[test]
    fn test_next_cron_fire_invalid() {
        assert!(next_cron_fire("not a cron").is_err());
    }

    #[test]
    fn test_cancel_error_display() {
        let err = CancelError::NotFound;
        assert_eq!(err.to_string(), "no reminder found");

        let err = CancelError::Ambiguous(vec!["abc-123-def".into(), "abc-456-ghi".into()]);
        let s = err.to_string();
        assert!(s.contains("abc-123-"), "got: {s}");
        assert!(s.contains("abc-456-"), "got: {s}");
    }
}

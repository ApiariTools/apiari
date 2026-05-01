//! Integration tests for watcher cursor persistence.
//!
//! Tests that watcher cursors survive save/load cycles through
//! `.buzz/state.json` using the `save_cursors` / `create_watchers` APIs.

use apiari_common::state::{load_state, save_state};
use hive::buzz::config::{BuzzConfig, GithubConfig, SentryConfig};
use hive::buzz::watcher::{WatcherState, create_watchers, save_cursors};
use tempfile::TempDir;

// ---- save_cursors writes state file ----

#[test]
fn save_cursors_creates_state_file() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    // Create a config with a sentry watcher (it will have a cursor).
    let config = BuzzConfig {
        sentry: Some(SentryConfig {
            token: "test-token".into(),
            org: "test-org".into(),
            project: "test-project".into(),
            sweep: None,
        }),
        ..BuzzConfig::default()
    };

    let mut watchers = create_watchers(&config, base);

    // Simulate a watcher advancing its cursor.
    watchers[0].set_cursor("issue-999".into());

    save_cursors(&watchers, base);

    // Verify the state file was written.
    let state_path = base.join(".buzz/state.json");
    assert!(state_path.exists(), "state.json should exist after save");

    let state: WatcherState = load_state(&state_path).unwrap();
    assert_eq!(
        state.cursors.get("sentry"),
        Some(&"issue-999".to_string()),
        "sentry cursor should be persisted"
    );
}

// ---- load_cursors restores from state file ----

#[test]
fn create_watchers_restores_cursors_from_state() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    // Pre-seed a state file with a cursor.
    let mut state = WatcherState::default();
    state.cursors.insert("sentry".into(), "issue-42".into());
    let state_path = base.join(".buzz/state.json");
    save_state(&state_path, &state).unwrap();

    // Create watchers — they should pick up the persisted cursor.
    let config = BuzzConfig {
        sentry: Some(SentryConfig {
            token: "test-token".into(),
            org: "test-org".into(),
            project: "test-project".into(),
            sweep: None,
        }),
        ..BuzzConfig::default()
    };

    let watchers = create_watchers(&config, base);
    assert_eq!(watchers.len(), 1);
    assert_eq!(watchers[0].name(), "sentry");
    assert_eq!(
        watchers[0].cursor().as_deref(),
        Some("issue-42"),
        "cursor should be restored from state file"
    );
}

// ---- Multiple watchers coexist; only cursor-supporting watchers persist ----

#[test]
fn multiple_watchers_sentry_cursor_persists_github_does_not() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let config = BuzzConfig {
        sentry: Some(SentryConfig {
            token: "tok".into(),
            org: "org".into(),
            project: "proj".into(),
            sweep: None,
        }),
        github: Some(GithubConfig {
            repos: vec!["owner/repo".into()],
            watch_labels: vec![],
        }),
        ..BuzzConfig::default()
    };

    // Create watchers — sentry supports cursors, github does not.
    let mut watchers = create_watchers(&config, base);
    assert_eq!(watchers.len(), 2);

    watchers[0].set_cursor("sentry-cursor-100".into());
    // GitHub watcher ignores set_cursor (trait default is a no-op).

    save_cursors(&watchers, base);

    // Recreate watchers — sentry cursor restored, github has none.
    let watchers2 = create_watchers(&config, base);
    assert_eq!(watchers2.len(), 2);

    let sentry_w = watchers2.iter().find(|w| w.name() == "sentry").unwrap();
    let github_w = watchers2.iter().find(|w| w.name() == "github").unwrap();

    assert_eq!(sentry_w.cursor().as_deref(), Some("sentry-cursor-100"));
    assert!(
        github_w.cursor().is_none(),
        "github watcher does not support cursors"
    );
}

// ---- Missing state file returns no cursors (no panic) ----

#[test]
fn create_watchers_with_no_state_file_succeeds() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let config = BuzzConfig {
        sentry: Some(SentryConfig {
            token: "tok".into(),
            org: "org".into(),
            project: "proj".into(),
            sweep: None,
        }),
        ..BuzzConfig::default()
    };

    // No state file exists — should not panic.
    let watchers = create_watchers(&config, base);
    assert_eq!(watchers.len(), 1);
    assert!(
        watchers[0].cursor().is_none(),
        "no cursor without state file"
    );
}

// ---- WatcherState serialization round-trip ----

#[test]
fn watcher_state_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.json");

    let mut state = WatcherState::default();
    state.cursors.insert("sentry".into(), "s-100".into());
    state.cursors.insert("github".into(), "g-200".into());
    state.cursors.insert("webhook".into(), "w-300".into());

    save_state(&path, &state).unwrap();
    let loaded: WatcherState = load_state(&path).unwrap();

    assert_eq!(loaded.cursors.len(), 3);
    assert_eq!(loaded.cursors["sentry"], "s-100");
    assert_eq!(loaded.cursors["github"], "g-200");
    assert_eq!(loaded.cursors["webhook"], "w-300");
}

// ---- Empty watchers save empty state ----

#[test]
fn save_cursors_with_no_watchers_writes_empty_state() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let config = BuzzConfig::default();
    let watchers = create_watchers(&config, base);
    assert!(watchers.is_empty());

    save_cursors(&watchers, base);

    let state_path = base.join(".buzz/state.json");
    assert!(state_path.exists());

    let state: WatcherState = load_state(&state_path).unwrap();
    assert!(state.cursors.is_empty());
}

// ---- Cursor overwrite on re-save ----

#[test]
fn save_cursors_overwrites_previous_state() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let config = BuzzConfig {
        sentry: Some(SentryConfig {
            token: "tok".into(),
            org: "org".into(),
            project: "proj".into(),
            sweep: None,
        }),
        ..BuzzConfig::default()
    };

    let mut watchers = create_watchers(&config, base);
    watchers[0].set_cursor("v1".into());
    save_cursors(&watchers, base);

    // Update cursor and save again.
    watchers[0].set_cursor("v2".into());
    save_cursors(&watchers, base);

    let state_path = base.join(".buzz/state.json");
    let state: WatcherState = load_state(&state_path).unwrap();
    assert_eq!(state.cursors["sentry"], "v2");
}

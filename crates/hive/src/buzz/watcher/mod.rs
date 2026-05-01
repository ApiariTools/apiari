//! Watcher trait, registry, and state persistence.
//!
//! Each watcher polls an external source and returns normalized [`Signal`]s.
//! Watcher cursors (e.g. "last seen issue ID") are persisted to
//! `.buzz/state.json` so that buzz does not replay old signals between runs.

pub mod github;
pub mod sentry;
pub mod webhook;

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::signal::Signal;
use apiari_common::state::{load_state, save_state};
use async_trait::async_trait;
use color_eyre::Result;
use serde::{Deserialize, Serialize};

use super::config::BuzzConfig;

/// Relative path for persisted watcher state (joined onto a base/workspace root).
const STATE_REL: &str = ".buzz/state.json";

/// A pluggable source that can be polled for new signals.
#[async_trait]
pub trait Watcher: Send + Sync {
    /// Human-readable name for this watcher (used in logging and state keys).
    fn name(&self) -> &str;

    /// Poll the external source and return any new signals since the last poll.
    async fn poll(&mut self) -> Result<Vec<Signal>>;

    /// Run a full sweep (re-evaluate all known issues). Default: no-op.
    async fn sweep(&mut self) -> Result<Vec<Signal>> {
        Ok(Vec::new())
    }

    /// Whether this watcher supports sweep.
    fn has_sweep(&self) -> bool {
        false
    }

    /// Return the current cursor value for state persistence, if any.
    fn cursor(&self) -> Option<String> {
        None
    }

    /// Restore cursor from persisted state.
    fn set_cursor(&mut self, _cursor: String) {}

    /// Downcast to concrete type for type-specific state (e.g. SentryWatcher seen_issues).
    fn as_any(&self) -> &dyn Any;

    /// Mutable downcast to concrete type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Persisted state for all watchers — maps watcher name to its cursor string.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatcherState {
    /// Map of watcher name -> cursor value.
    pub cursors: HashMap<String, String>,
    /// Per-issue metadata for sweep re-triage (keyed by Sentry issue ID).
    #[serde(default)]
    pub seen_issues: HashMap<String, sentry::IssueMeta>,
    /// Dedup keys of currently-active GitHub signals (cross-poll dedup).
    #[serde(default)]
    pub seen_github: HashSet<String>,
}

/// Create all enabled watchers based on the configuration.
/// Restores persisted cursors from `<base>/.buzz/state.json` if available.
pub fn create_watchers(config: &BuzzConfig, base: &Path) -> Vec<Box<dyn Watcher>> {
    let mut watchers: Vec<Box<dyn Watcher>> = Vec::new();

    if let Some(sentry_config) = &config.sentry {
        watchers.push(Box::new(sentry::SentryWatcher::new(sentry_config.clone())));
    }

    if let Some(github_config) = &config.github {
        watchers.push(Box::new(github::GithubWatcher::new(github_config.clone())));
    }

    if let Some(webhook_config) = &config.webhook {
        watchers.push(Box::new(webhook::WebhookWatcher::new(
            webhook_config.clone(),
        )));
    }

    // Restore persisted cursors.
    load_cursors(&mut watchers, base);

    watchers
}

/// Load persisted watcher state and restore cursors + seen issues.
fn load_cursors(watchers: &mut [Box<dyn Watcher>], base: &Path) {
    let state_path = base.join(STATE_REL);
    let state: WatcherState = match load_state(&state_path) {
        Ok(state) => state,
        Err(e) => {
            tracing::error!(error = %e, "failed to load watcher state");
            WatcherState::default()
        }
    };

    for watcher in watchers.iter_mut() {
        if let Some(cursor) = state.cursors.get(watcher.name()) {
            tracing::debug!(
                name = watcher.name(),
                cursor = cursor.as_str(),
                "restored cursor"
            );
            watcher.set_cursor(cursor.clone());
        }
    }

    // Restore seen_issues into the sentry watcher (if present).
    if !state.seen_issues.is_empty()
        && let Some(sentry_watcher) = find_sentry_watcher_mut(watchers)
    {
        tracing::debug!(
            count = state.seen_issues.len(),
            "restored seen issue(s) for sweep"
        );
        sentry_watcher.restore_seen_issues(state.seen_issues);
    }

    // Restore seen_github into the github watcher (if present).
    if !state.seen_github.is_empty()
        && let Some(github_watcher) = find_github_watcher_mut(watchers)
    {
        tracing::debug!(
            count = state.seen_github.len(),
            "restored seen github signal(s)"
        );
        github_watcher.restore_seen(state.seen_github);
    }
}

/// Save all watcher cursors + seen issues to `<base>/.buzz/state.json`.
pub fn save_cursors(watchers: &[Box<dyn Watcher>], base: &Path) {
    let mut state = WatcherState::default();

    for watcher in watchers {
        if let Some(cursor) = watcher.cursor() {
            state.cursors.insert(watcher.name().to_string(), cursor);
        }
    }

    // Persist seen_issues from the sentry watcher (if present).
    if let Some(sw) = find_sentry_watcher(watchers) {
        state.seen_issues = sw.seen_issues().clone();
    }

    // Persist seen_github from the github watcher (if present).
    if let Some(gw) = find_github_watcher(watchers) {
        state.seen_github = gw.seen().clone();
    }

    let state_path = base.join(STATE_REL);
    if let Err(e) = save_state(&state_path, &state) {
        tracing::error!(error = %e, "failed to save watcher state");
    }
}

/// Find the SentryWatcher in a slice of boxed watchers (immutable).
fn find_sentry_watcher(watchers: &[Box<dyn Watcher>]) -> Option<&sentry::SentryWatcher> {
    watchers
        .iter()
        .find(|w| w.name() == "sentry")
        .and_then(|w| w.as_any().downcast_ref::<sentry::SentryWatcher>())
}

/// Find the SentryWatcher in a mutable slice of boxed watchers.
fn find_sentry_watcher_mut(
    watchers: &mut [Box<dyn Watcher>],
) -> Option<&mut sentry::SentryWatcher> {
    watchers
        .iter_mut()
        .find(|w| w.name() == "sentry")
        .and_then(|w| w.as_any_mut().downcast_mut::<sentry::SentryWatcher>())
}

/// Find the GithubWatcher in a slice of boxed watchers (immutable).
fn find_github_watcher(watchers: &[Box<dyn Watcher>]) -> Option<&github::GithubWatcher> {
    watchers
        .iter()
        .find(|w| w.name() == "github")
        .and_then(|w| w.as_any().downcast_ref::<github::GithubWatcher>())
}

/// Find the GithubWatcher in a mutable slice of boxed watchers.
fn find_github_watcher_mut(
    watchers: &mut [Box<dyn Watcher>],
) -> Option<&mut github::GithubWatcher> {
    watchers
        .iter_mut()
        .find(|w| w.name() == "github")
        .and_then(|w| w.as_any_mut().downcast_mut::<github::GithubWatcher>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Minimal mock watcher for cursor round-trip tests.
    struct MockWatcher {
        name: String,
        cursor: Mutex<Option<String>>,
    }

    impl MockWatcher {
        fn new(name: &str, cursor: Option<&str>) -> Self {
            Self {
                name: name.to_string(),
                cursor: Mutex::new(cursor.map(String::from)),
            }
        }
    }

    #[async_trait]
    impl Watcher for MockWatcher {
        fn name(&self) -> &str {
            &self.name
        }

        async fn poll(&mut self) -> Result<Vec<Signal>> {
            Ok(Vec::new())
        }

        fn cursor(&self) -> Option<String> {
            self.cursor.lock().unwrap().clone()
        }

        fn set_cursor(&mut self, cursor: String) {
            *self.cursor.lock().unwrap() = Some(cursor);
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_save_cursors_writes_state_to_base() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let w1: Box<dyn Watcher> = Box::new(MockWatcher::new("sentry", Some("cursor-abc")));
        let w2: Box<dyn Watcher> = Box::new(MockWatcher::new("github", Some("cursor-xyz")));
        let watchers: Vec<Box<dyn Watcher>> = vec![w1, w2];

        save_cursors(&watchers, base);

        let state_path = base.join(".buzz/state.json");
        assert!(state_path.exists(), "state file should be created");

        let state: WatcherState = load_state(&state_path).unwrap();
        assert_eq!(state.cursors.get("sentry").unwrap(), "cursor-abc");
        assert_eq!(state.cursors.get("github").unwrap(), "cursor-xyz");
    }

    #[test]
    fn test_load_cursors_restores_from_base() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Pre-populate state file.
        let mut state = WatcherState::default();
        state
            .cursors
            .insert("sentry".into(), "restored-cursor".into());
        save_state(&base.join(STATE_REL), &state).unwrap();

        let w: Box<dyn Watcher> = Box::new(MockWatcher::new("sentry", None));
        let mut watchers: Vec<Box<dyn Watcher>> = vec![w];
        load_cursors(&mut watchers, base);

        assert_eq!(
            watchers[0].cursor().as_deref(),
            Some("restored-cursor"),
            "cursor should be restored from state file"
        );
    }

    #[test]
    fn test_cursor_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Save cursors.
        let w: Box<dyn Watcher> = Box::new(MockWatcher::new("github", Some("issue-42")));
        save_cursors(&[w], base);

        // Load into fresh watchers.
        let mut fresh: Vec<Box<dyn Watcher>> = vec![Box::new(MockWatcher::new("github", None))];
        load_cursors(&mut fresh, base);

        assert_eq!(
            fresh[0].cursor().as_deref(),
            Some("issue-42"),
            "cursor should survive save/load round-trip"
        );
    }

    #[test]
    fn test_watcher_state_serialization() {
        let mut state = WatcherState::default();
        state.cursors.insert("sentry".into(), "abc123".into());
        state.cursors.insert("github".into(), "xyz789".into());

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: WatcherState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.cursors.get("sentry").unwrap(), "abc123");
        assert_eq!(deserialized.cursors.get("github").unwrap(), "xyz789");
    }

    #[test]
    fn test_save_cursors_skips_watchers_without_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let w1: Box<dyn Watcher> = Box::new(MockWatcher::new("with-cursor", Some("val")));
        let w2: Box<dyn Watcher> = Box::new(MockWatcher::new("no-cursor", None));

        save_cursors(&[w1, w2], base);

        let state: WatcherState = load_state(&base.join(STATE_REL)).unwrap();
        assert_eq!(state.cursors.len(), 1);
        assert!(state.cursors.contains_key("with-cursor"));
        assert!(!state.cursors.contains_key("no-cursor"));
    }

    #[test]
    fn test_load_cursors_missing_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // No state file exists.
        let mut watchers: Vec<Box<dyn Watcher>> = vec![Box::new(MockWatcher::new("sentry", None))];
        load_cursors(&mut watchers, base);

        // Should not panic, cursor stays None.
        assert!(watchers[0].cursor().is_none());
    }
}

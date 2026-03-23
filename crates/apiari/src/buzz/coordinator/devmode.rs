//! Dev-mode toggle — temporarily unlocks elevated Bash permissions.
//!
//! When dev-mode is active, the coordinator is allowed to run additional
//! commands that are normally blocked (e.g. `gh repo create`, `git clone`,
//! `git init`, `mkdir`, file writes under the workspace root).
//!
//! State is persisted as a JSON file at `~/.config/apiari/.devmode`.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::config_dir;

/// Default dev-mode duration: 30 minutes.
const DEFAULT_DURATION_MINUTES: i64 = 30;

/// The devmode state file contents.
#[derive(Debug, Serialize, Deserialize)]
pub struct DevModeState {
    pub enabled_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Path to the devmode state file.
///
/// In tests, set `APIARI_DEVMODE_PATH` to override the default location.
pub fn devmode_path() -> PathBuf {
    if let Ok(p) = std::env::var("APIARI_DEVMODE_PATH") {
        return PathBuf::from(p);
    }
    config_dir().join(".devmode")
}

/// Check if dev-mode is currently active (file exists and not expired).
pub fn is_active() -> bool {
    is_active_at(&devmode_path())
}

/// Check if dev-mode is active at a specific path.
fn is_active_at(path: &Path) -> bool {
    read_state_from(path)
        .map(|s| Utc::now() < s.expires_at)
        .unwrap_or(false)
}

/// Read the devmode state from disk, if present and valid.
pub fn read_state() -> Option<DevModeState> {
    read_state_from(&devmode_path())
}

fn read_state_from(path: &Path) -> Option<DevModeState> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Enable dev-mode with the default 30-minute timeout.
pub fn enable() -> DevModeState {
    enable_with_duration(DEFAULT_DURATION_MINUTES)
}

/// Enable dev-mode with a custom duration in minutes.
pub fn enable_with_duration(minutes: i64) -> DevModeState {
    enable_at(&devmode_path(), minutes)
}

fn enable_at(path: &Path, minutes: i64) -> DevModeState {
    let now = Utc::now();
    let state = DevModeState {
        enabled_at: now,
        expires_at: now + Duration::minutes(minutes),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(&state).unwrap());
    state
}

/// Disable dev-mode by removing the state file.
pub fn disable() {
    let _ = std::fs::remove_file(devmode_path());
}

/// Format the remaining time as a human-readable string.
pub fn remaining_str(state: &DevModeState) -> String {
    let remaining = state.expires_at - Utc::now();
    if remaining.num_seconds() <= 0 {
        return "expired".to_string();
    }
    let mins = remaining.num_minutes();
    let secs = remaining.num_seconds() % 60;
    if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enable_disable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".devmode");
        assert!(!is_active_at(&path));
        enable_at(&path, 30);
        assert!(is_active_at(&path));
        let _ = std::fs::remove_file(&path);
        assert!(!is_active_at(&path));
    }

    #[test]
    fn test_expired_is_not_active() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".devmode");
        enable_at(&path, 0);
        assert!(!is_active_at(&path));
    }

    #[test]
    fn test_remaining_str_format() {
        let now = Utc::now();
        let state = DevModeState {
            enabled_at: now,
            expires_at: now + Duration::minutes(15) + Duration::seconds(30),
        };
        let s = remaining_str(&state);
        assert!(s.contains("m"), "should contain minutes: {s}");
        assert!(s.contains("s"), "should contain seconds: {s}");
    }
}

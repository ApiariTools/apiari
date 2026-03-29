//! Dev-mode toggle — temporarily unlocks elevated Bash permissions.
//!
//! When dev-mode is active, the coordinator is allowed to run additional
//! commands that are normally blocked (e.g. `gh repo create`, `git clone`,
//! `git init`, `mkdir`, file writes under the workspace root).
//!
//! State is persisted as a JSON file at `~/.local/state/apiari/.devmode`.
//! This path is intentionally **outside** `~/.config/apiari/` (which is an
//! allowed Bash write target) so the coordinator cannot self-enable dev-mode.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

/// Default dev-mode duration: 30 minutes.
const DEFAULT_DURATION_MINUTES: i64 = 30;

/// Maximum allowed TTL in minutes. Prevents manual edits from keeping
/// dev-mode on indefinitely.
const MAX_TTL_MINUTES: i64 = 60;

/// The devmode state file contents.
#[derive(Debug, Serialize, Deserialize)]
pub struct DevModeState {
    pub enabled_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Path to the devmode state file.
///
/// Uses `~/.local/state/apiari/.devmode` — intentionally NOT under
/// `~/.config/apiari/` to prevent the coordinator from self-enabling
/// dev-mode via allowed Bash writes.
///
/// In tests, set `APIARI_DEVMODE_PATH` to override the default location.
pub fn devmode_path() -> PathBuf {
    if let Ok(p) = std::env::var("APIARI_DEVMODE_PATH") {
        return PathBuf::from(p);
    }
    state_dir().join(".devmode")
}

/// State directory for runtime files (not config).
fn state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".local/state/apiari")
}

/// Check if dev-mode is currently active (file exists, not expired, valid TTL).
pub fn is_active() -> bool {
    is_active_at(&devmode_path())
}

/// Check if dev-mode is active at a specific path.
fn is_active_at(path: &Path) -> bool {
    read_validated_state_from(path)
        .map(|s| Utc::now() < s.expires_at)
        .unwrap_or(false)
}

/// Read the devmode state from disk, if present and valid.
pub fn read_state() -> Option<DevModeState> {
    read_validated_state_from(&devmode_path())
}

/// Read and validate state from a path. Returns `None` if the file is
/// missing, unparseable, or has a TTL exceeding `MAX_TTL_MINUTES`.
fn read_validated_state_from(path: &Path) -> Option<DevModeState> {
    let contents = std::fs::read_to_string(path).ok()?;
    let state: DevModeState = serde_json::from_str(&contents).ok()?;
    // Enforce maximum TTL — reject tampered expires_at values.
    let max_expiry = state.enabled_at + Duration::minutes(MAX_TTL_MINUTES);
    if state.expires_at > max_expiry {
        // Tampered or invalid — remove and treat as off.
        let _ = std::fs::remove_file(path);
        return None;
    }
    Some(state)
}

/// Enable dev-mode with the default 30-minute timeout.
pub fn enable() -> Result<DevModeState> {
    enable_with_duration(DEFAULT_DURATION_MINUTES)
}

/// Enable dev-mode with a custom duration in minutes.
pub fn enable_with_duration(minutes: i64) -> Result<DevModeState> {
    enable_at(&devmode_path(), minutes)
}

fn enable_at(path: &Path, minutes: i64) -> Result<DevModeState> {
    let now = Utc::now();
    let state = DevModeState {
        enabled_at: now,
        expires_at: now + Duration::minutes(minutes),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create devmode dir: {}", parent.display()))?;
    }
    let json =
        serde_json::to_string_pretty(&state).wrap_err("failed to serialize devmode state")?;
    std::fs::write(path, json)
        .wrap_err_with(|| format!("failed to write devmode file: {}", path.display()))?;
    Ok(state)
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

/// Handle a `/devmode` subcommand. Returns the response text.
///
/// Shared by both the TUI and Telegram command handlers to avoid duplication.
pub fn handle_command(args: &str) -> String {
    match args.trim() {
        "on" => match enable() {
            Ok(state) => format!("\u{1f513} Dev mode enabled for {}.", remaining_str(&state)),
            Err(e) => format!("\u{274c} Failed to enable dev mode: {e}"),
        },
        "off" => {
            disable();
            "\u{1f512} Dev mode disabled.".to_string()
        }
        "" | "status" => {
            if let Some(state) = read_state() {
                if Utc::now() < state.expires_at {
                    format!(
                        "\u{1f513} Dev mode is ON — {} remaining.",
                        remaining_str(&state)
                    )
                } else {
                    disable();
                    "\u{1f512} Dev mode is OFF (expired).".to_string()
                }
            } else {
                "\u{1f512} Dev mode is OFF.".to_string()
            }
        }
        _ => "Usage: /devmode on|off|status".to_string(),
    }
}

/// Mutex shared by all tests that manipulate `APIARI_DEVMODE_PATH`.
#[cfg(test)]
pub static DEVMODE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard that removes the APIARI_DEVMODE_PATH env var on drop.
#[cfg(test)]
pub struct DevmodeTestGuard {
    pub _lock: std::sync::MutexGuard<'static, ()>,
    pub _tmp: tempfile::TempDir,
}

#[cfg(test)]
impl Drop for DevmodeTestGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("APIARI_DEVMODE_PATH") };
    }
}

/// Set up an isolated devmode env for testing. Returns an RAII guard.
#[cfg(test)]
pub fn setup_test_env() -> DevmodeTestGuard {
    let lock = DEVMODE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let devmode_file = tmp.path().join(".devmode");
    unsafe { std::env::set_var("APIARI_DEVMODE_PATH", &devmode_file) };
    DevmodeTestGuard {
        _lock: lock,
        _tmp: tmp,
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
        enable_at(&path, 30).unwrap();
        assert!(is_active_at(&path));
        let _ = std::fs::remove_file(&path);
        assert!(!is_active_at(&path));
    }

    #[test]
    fn test_expired_is_not_active() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".devmode");
        enable_at(&path, 0).unwrap();
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

    #[test]
    fn test_enable_returns_error_on_bad_path() {
        let result = enable_at(Path::new("/nonexistent/deeply/nested/.devmode"), 30);
        assert!(result.is_err(), "should fail on unwritable path");
    }

    #[test]
    fn test_tampered_ttl_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".devmode");
        let now = Utc::now();
        // Write a state with expires_at far in the future (beyond MAX_TTL_MINUTES)
        let tampered = DevModeState {
            enabled_at: now,
            expires_at: now + Duration::hours(24),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();
        // Should be treated as invalid
        assert!(!is_active_at(&path), "tampered TTL should be rejected");
        // File should have been removed
        assert!(!path.exists(), "tampered devmode file should be removed");
    }

    #[test]
    fn test_valid_ttl_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".devmode");
        let now = Utc::now();
        // Write a state within MAX_TTL_MINUTES
        let valid = DevModeState {
            enabled_at: now,
            expires_at: now + Duration::minutes(MAX_TTL_MINUTES),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&valid).unwrap()).unwrap();
        assert!(is_active_at(&path), "valid TTL should be accepted");
    }

    #[test]
    fn test_handle_command_off() {
        let _guard = setup_test_env();
        let text = handle_command("off");
        assert!(text.contains("disabled"));
    }

    #[test]
    fn test_handle_command_status_when_off() {
        let _guard = setup_test_env();
        let text = handle_command("status");
        assert!(text.contains("OFF"));
    }

    #[test]
    fn test_handle_command_no_args_shows_status() {
        let _guard = setup_test_env();
        let text = handle_command("");
        assert!(text.contains("OFF"), "no-arg should show status: {text}");
    }

    #[test]
    fn test_handle_command_invalid() {
        let text = handle_command("banana");
        assert!(text.contains("Usage"));
    }
}

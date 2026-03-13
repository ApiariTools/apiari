//! Channel presence state — tracks which UI channel was last active.
//!
//! The daemon and TUI both read/write `.hive/channel_state.json` to coordinate
//! notification routing. When the TUI is active it heartbeats every few seconds;
//! when stale (> 5 min) the daemon falls back to Telegram.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const STATE_FILE: &str = "channel_state.json";
const STALE_SECS: u64 = 300; // 5 minutes

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelEntry {
    #[serde(default)]
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelState {
    #[serde(default)]
    pub channels: HashMap<String, ChannelEntry>,
}

impl ChannelState {
    /// Returns the name of the most recently active channel,
    /// or `"telegram"` if none are active or all are stale (> `stale_secs`).
    pub fn active_channel(&self, stale_secs: u64) -> &str {
        let now = Utc::now();
        self.channels
            .iter()
            .filter(|(_, e)| (now - e.last_seen).num_seconds().unsigned_abs() < stale_secs)
            .max_by_key(|(_, e)| e.last_seen)
            .map(|(name, _)| name.as_str())
            .unwrap_or("telegram")
    }

    /// Update the last_seen timestamp for a channel.
    pub fn touch(&mut self, channel: &str) {
        self.channels.insert(
            channel.to_string(),
            ChannelEntry {
                last_seen: Utc::now(),
            },
        );
    }
}

/// Load `.hive/channel_state.json`, returning default if missing or malformed.
pub fn load(root: &Path) -> ChannelState {
    let path = root.join(".hive").join(STATE_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Atomically save `.hive/channel_state.json`.
///
/// Writes to a temp file then renames to avoid corruption from concurrent reads.
pub fn save(root: &Path, state: &ChannelState) -> std::io::Result<()> {
    let dir = root.join(".hive");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
    let final_path = dir.join(STATE_FILE);
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &final_path)
}

/// Convenience: touch a channel and save in one call.
pub fn touch_channel(root: &Path, channel: &str) -> std::io::Result<()> {
    let mut state = load(root);
    state.touch(channel);
    save(root, &state)
}

/// Returns the currently active channel name.
pub fn active_channel(root: &Path) -> String {
    load(root).active_channel(STALE_SECS).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_returns_telegram() {
        let state = ChannelState::default();
        assert_eq!(state.active_channel(300), "telegram");
    }

    #[test]
    fn fresh_touch_returns_channel() {
        let mut state = ChannelState::default();
        state.touch("ui");
        assert_eq!(state.active_channel(300), "ui");
    }

    #[test]
    fn stale_touch_returns_telegram() {
        let mut state = ChannelState::default();
        state.channels.insert(
            "ui".to_string(),
            ChannelEntry {
                last_seen: Utc::now() - chrono::Duration::seconds(600),
            },
        );
        assert_eq!(state.active_channel(300), "telegram");
    }

    #[test]
    fn most_recent_wins() {
        let mut state = ChannelState::default();
        state.channels.insert(
            "telegram".to_string(),
            ChannelEntry {
                last_seen: Utc::now() - chrono::Duration::seconds(10),
            },
        );
        state.touch("ui");
        assert_eq!(state.active_channel(300), "ui");
    }

    #[test]
    fn round_trip_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        touch_channel(root, "ui").unwrap();
        let loaded = load(root);
        assert_eq!(loaded.active_channel(300), "ui");
    }
}

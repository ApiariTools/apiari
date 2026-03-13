//! Chat history persistence — append-only JSONL in `~/.config/apiari/chat_history/`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default)]
    pub role: String, // "user" or "assistant"
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub ts: DateTime<Utc>,
    /// Origin of this message: "tui", "telegram", "system". None for legacy entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Path to chat history for a workspace.
fn history_path(workspace: &str) -> PathBuf {
    crate::config::config_dir()
        .join("chat_history")
        .join(format!("{workspace}.jsonl"))
}

/// Append a single message to the workspace's chat history.
pub fn save_message(workspace: &str, msg: &ChatMessage) -> std::io::Result<()> {
    let path = history_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
    let json = serde_json::to_string(msg).map_err(std::io::Error::other)?;
    writeln!(file, "{json}")
}

/// Load the last `limit` messages from the workspace's chat history.
///
/// Returns empty vec if file doesn't exist or is unreadable.
pub fn load_history(workspace: &str, limit: usize) -> Vec<ChatMessage> {
    let path = history_path(workspace);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = std::io::BufReader::new(file);
    let all: Vec<ChatMessage> = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    // Take the last `limit` messages.
    let skip = all.len().saturating_sub(limit);
    all.into_iter().skip(skip).collect()
}

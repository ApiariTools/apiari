//! Chat history persistence — append-only JSONL in `.hive/chat_history.jsonl`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::Path;

const HISTORY_FILE: &str = "chat_history.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default)]
    pub role: String, // "user" or "assistant"
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub ts: DateTime<Utc>,
}

/// Append a single message to `.hive/chat_history.jsonl`.
pub fn save_message(root: &Path, msg: &ChatMessage) -> std::io::Result<()> {
    let dir = root.join(".hive");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(HISTORY_FILE);
    let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
    let json = serde_json::to_string(msg).map_err(std::io::Error::other)?;
    writeln!(file, "{json}")
}

/// Load the last `limit` messages from `.hive/chat_history.jsonl`.
///
/// Returns empty vec if file doesn't exist or is unreadable.
pub fn load_history(root: &Path, limit: usize) -> Vec<ChatMessage> {
    let path = root.join(".hive").join(HISTORY_FILE);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        let msg1 = ChatMessage {
            role: "user".into(),
            content: "hello".into(),
            ts: Utc::now(),
        };
        let msg2 = ChatMessage {
            role: "assistant".into(),
            content: "hi there".into(),
            ts: Utc::now(),
        };

        save_message(root, &msg1).unwrap();
        save_message(root, &msg2).unwrap();

        let history = load_history(root, 10);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].role, "assistant");
    }

    #[test]
    fn limit_caps_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        for i in 0..10 {
            save_message(
                root,
                &ChatMessage {
                    role: "user".into(),
                    content: format!("msg {i}"),
                    ts: Utc::now(),
                },
            )
            .unwrap();
        }

        let history = load_history(root, 3);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "msg 7");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let history = load_history(dir.path(), 50);
        assert!(history.is_empty());
    }
}

//! UI inbox — daemon writes notification events here when the TUI is active.
//!
//! Uses `.hive/ui_inbox.jsonl` as an append-only event stream. The TUI polls
//! from a file position to pick up new events without re-reading old ones.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const INBOX_FILE: &str = "ui_inbox.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum UiEvent {
    PrOpened {
        worktree_id: String,
        pr_url: String,
        #[serde(default)]
        pr_title: String,
    },
    AgentWaiting {
        worktree_id: String,
    },
    AgentStalled {
        worktree_id: String,
    },
    AgentCompleted {
        worktree_id: String,
    },
    AgentClosed {
        worktree_id: String,
    },
}

impl UiEvent {
    pub fn display(&self) -> String {
        match self {
            UiEvent::PrOpened {
                worktree_id,
                pr_title,
                ..
            } => {
                if pr_title.is_empty() {
                    format!("\u{1f514} {worktree_id} opened a PR")
                } else {
                    format!("\u{1f514} {worktree_id} opened PR: {pr_title}")
                }
            }
            UiEvent::AgentWaiting { worktree_id } => {
                format!("\u{23f3} {worktree_id} is waiting for input")
            }
            UiEvent::AgentStalled { worktree_id } => {
                format!("\u{1f6a8} {worktree_id} appears stalled")
            }
            UiEvent::AgentCompleted { worktree_id } => {
                format!("\u{2705} {worktree_id} completed")
            }
            UiEvent::AgentClosed { worktree_id } => {
                format!("\u{1f5d1} {worktree_id} closed")
            }
        }
    }
}

/// Append a UiEvent to `.hive/ui_inbox.jsonl`.
pub fn push_event(root: &Path, event: &UiEvent) -> std::io::Result<()> {
    let dir = root.join(".hive");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(INBOX_FILE);
    let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
    let json = serde_json::to_string(event).map_err(std::io::Error::other)?;
    writeln!(file, "{json}")
}

/// Read new events from `.hive/ui_inbox.jsonl` since `pos`.
///
/// Updates `pos` to the new file position. Returns empty vec if the file
/// doesn't exist or there are no new events.
pub fn poll_events(root: &Path, pos: &mut u64) -> Vec<UiEvent> {
    let path = root.join(".hive").join(INBOX_FILE);
    let mut file = match OpenOptions::new().read(true).open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    // If the file shrank (was truncated), reset to start.
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file_len < *pos {
        *pos = 0;
    }

    if file_len == *pos {
        return Vec::new();
    }

    if file.seek(SeekFrom::Start(*pos)).is_err() {
        return Vec::new();
    }

    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return Vec::new();
    }

    let events: Vec<UiEvent> = buf
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    *pos = file_len;
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_poll() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        push_event(
            root,
            &UiEvent::PrOpened {
                worktree_id: "hive-1".into(),
                pr_url: "https://github.com/test/1".into(),
                pr_title: "Fix bug".into(),
            },
        )
        .unwrap();

        let mut pos = 0u64;
        let events = poll_events(root, &mut pos);
        assert_eq!(events.len(), 1);
        assert!(pos > 0);

        // No new events.
        let events2 = poll_events(root, &mut pos);
        assert!(events2.is_empty());

        // Push another.
        push_event(
            root,
            &UiEvent::AgentWaiting {
                worktree_id: "hive-2".into(),
            },
        )
        .unwrap();

        let events3 = poll_events(root, &mut pos);
        assert_eq!(events3.len(), 1);
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut pos = 0u64;
        let events = poll_events(dir.path(), &mut pos);
        assert!(events.is_empty());
    }

    #[test]
    fn display_formatting() {
        let e = UiEvent::PrOpened {
            worktree_id: "hive-1".into(),
            pr_url: "url".into(),
            pr_title: "My PR".into(),
        };
        assert!(e.display().contains("My PR"));

        let e2 = UiEvent::AgentStalled {
            worktree_id: "hive-2".into(),
        };
        assert!(e2.display().contains("stalled"));
    }
}

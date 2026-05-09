//! Parse agent events.jsonl into ConversationEntry items.
//!
//! The events file is written by swarm's agent TUI at
//! `.swarm/agents/{worktree_id}/events.jsonl`.

use crate::conversation::ConversationEntry;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::Path;

/// Format a UTC timestamp as local time for display.
fn fmt_ts(ts: &DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%-I:%M %p").to_string()
}

/// A structured event written to the agent's event log.
///
/// This is the canonical type for agent event serialization. Both swarm
/// (writing) and apiari (reading) use this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Session started.
    Start {
        timestamp: DateTime<Utc>,
        prompt: String,
        model: Option<String>,
    },
    /// User sent a follow-up message.
    UserMessage {
        timestamp: DateTime<Utc>,
        text: String,
    },
    /// Assistant emitted text.
    AssistantText {
        timestamp: DateTime<Utc>,
        text: String,
    },
    /// Assistant requested a tool call.
    ToolUse {
        timestamp: DateTime<Utc>,
        tool: String,
        input: String,
    },
    /// Tool execution completed.
    ToolResult {
        timestamp: DateTime<Utc>,
        tool: String,
        output: String,
        is_error: bool,
    },
    /// SDK returned a result — session is now idle and resumable.
    SessionResult {
        timestamp: DateTime<Utc>,
        turns: u64,
        cost_usd: Option<f64>,
        session_id: Option<String>,
    },
    /// Session errored.
    Error {
        timestamp: DateTime<Utc>,
        message: String,
    },
}

/// Parse an events.jsonl file into conversation entries.
///
/// Reads all events and converts them into a flat list of `ConversationEntry`
/// items suitable for rendering. Unlike `read_last_session`, this doesn't
/// require a completed session — it replays whatever events exist.
///
/// `SessionResult` events become status lines showing turn count and cost.
pub fn parse_events(path: &Path) -> Vec<ConversationEntry> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = std::io::BufReader::new(file);
    let mut entries: Vec<ConversationEntry> = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let event: AgentEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match event {
            AgentEvent::Start {
                prompt, timestamp, ..
            } => {
                entries.push(ConversationEntry::User {
                    text: prompt,
                    timestamp: fmt_ts(&timestamp),
                });
            }
            AgentEvent::UserMessage {
                text, timestamp, ..
            } => {
                entries.push(ConversationEntry::User {
                    text,
                    timestamp: fmt_ts(&timestamp),
                });
            }
            AgentEvent::AssistantText {
                text, timestamp, ..
            } => {
                // Merge consecutive assistant text chunks into a single entry.
                // The SDK streams text in small fragments; combining them produces
                // one coherent message for markdown rendering.
                if let Some(ConversationEntry::AssistantText {
                    text: prev_text, ..
                }) = entries.last_mut()
                {
                    prev_text.push_str(&text);
                } else {
                    entries.push(ConversationEntry::AssistantText {
                        text,
                        timestamp: fmt_ts(&timestamp),
                    });
                }
            }
            AgentEvent::ToolUse { tool, input, .. } => {
                entries.push(ConversationEntry::ToolCall {
                    tool,
                    input,
                    output: None,
                    is_error: false,
                    collapsed: true,
                });
            }
            AgentEvent::ToolResult {
                output, is_error, ..
            } => {
                // Update the last ToolCall entry with the result
                if let Some(ConversationEntry::ToolCall {
                    output: o,
                    is_error: e,
                    ..
                }) = entries.last_mut()
                {
                    *o = Some(output);
                    *e = is_error;
                }
            }
            AgentEvent::SessionResult {
                turns, cost_usd, ..
            } => {
                let cost_str = cost_usd.map(|c| format!(", ${:.2}", c)).unwrap_or_default();
                entries.push(ConversationEntry::Status {
                    text: format!("Session complete ({} turns{})", turns, cost_str),
                });
            }
            AgentEvent::Error { message, .. } => {
                entries.push(ConversationEntry::Status {
                    text: format!("Error: {}", message),
                });
            }
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_events(events: &[AgentEvent]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for ev in events {
            let json = serde_json::to_string(ev).unwrap();
            writeln!(f, "{}", json).unwrap();
        }
        f.flush().unwrap();
        f
    }

    fn ts() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn parse_basic_conversation() {
        let f = write_events(&[
            AgentEvent::Start {
                timestamp: ts(),
                prompt: "fix the bug".into(),
                model: Some("opus".into()),
            },
            AgentEvent::AssistantText {
                timestamp: ts(),
                text: "I'll fix it".into(),
            },
            AgentEvent::ToolUse {
                timestamp: ts(),
                tool: "Read".into(),
                input: "src/main.rs".into(),
            },
            AgentEvent::ToolResult {
                timestamp: ts(),
                tool: "Read".into(),
                output: "fn main() {}".into(),
                is_error: false,
            },
            AgentEvent::SessionResult {
                timestamp: ts(),
                turns: 3,
                cost_usd: Some(0.05),
                session_id: Some("s1".into()),
            },
        ]);

        let entries = parse_events(f.path());
        assert_eq!(entries.len(), 4); // User, AssistantText, ToolCall, Status

        assert!(matches!(
            &entries[0],
            ConversationEntry::User { text, .. } if text == "fix the bug"
        ));
        assert!(matches!(
            &entries[1],
            ConversationEntry::AssistantText { text, .. } if text == "I'll fix it"
        ));
        assert!(matches!(
            &entries[2],
            ConversationEntry::ToolCall { tool, output: Some(out), is_error: false, .. }
            if tool == "Read" && out == "fn main() {}"
        ));
        assert!(matches!(
            &entries[3],
            ConversationEntry::Status { text } if text.contains("3 turns") && text.contains("$0.05")
        ));
    }

    #[test]
    fn parse_empty_file() {
        let f = NamedTempFile::new().unwrap();
        let entries = parse_events(f.path());
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_nonexistent_file() {
        let entries = parse_events(Path::new("/nonexistent/events.jsonl"));
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_skips_corrupt_lines() {
        let mut f = NamedTempFile::new().unwrap();
        let ev = AgentEvent::Start {
            timestamp: ts(),
            prompt: "go".into(),
            model: None,
        };
        writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
        writeln!(f, "not json").unwrap();
        writeln!(f).unwrap();
        let ev2 = AgentEvent::AssistantText {
            timestamp: ts(),
            text: "done".into(),
        };
        writeln!(f, "{}", serde_json::to_string(&ev2).unwrap()).unwrap();
        f.flush().unwrap();

        let entries = parse_events(f.path());
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn parse_followup_messages() {
        let f = write_events(&[
            AgentEvent::Start {
                timestamp: ts(),
                prompt: "initial".into(),
                model: None,
            },
            AgentEvent::AssistantText {
                timestamp: ts(),
                text: "done".into(),
            },
            AgentEvent::SessionResult {
                timestamp: ts(),
                turns: 1,
                cost_usd: None,
                session_id: Some("s1".into()),
            },
            AgentEvent::UserMessage {
                timestamp: ts(),
                text: "follow up".into(),
            },
            AgentEvent::AssistantText {
                timestamp: ts(),
                text: "follow up answer".into(),
            },
            AgentEvent::SessionResult {
                timestamp: ts(),
                turns: 3,
                cost_usd: Some(0.10),
                session_id: Some("s1".into()),
            },
        ]);

        let entries = parse_events(f.path());
        // User, AssistantText, Status(session1), User, AssistantText, Status(session2)
        assert_eq!(entries.len(), 6);
        assert!(matches!(
            &entries[3],
            ConversationEntry::User { text, .. } if text == "follow up"
        ));
    }

    #[test]
    fn parse_error_events() {
        let f = write_events(&[
            AgentEvent::Start {
                timestamp: ts(),
                prompt: "go".into(),
                model: None,
            },
            AgentEvent::Error {
                timestamp: ts(),
                message: "rate limited".into(),
            },
        ]);

        let entries = parse_events(f.path());
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[1],
            ConversationEntry::Status { text } if text == "Error: rate limited"
        ));
    }
}

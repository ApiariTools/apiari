//! Protocol types for the Codex CLI `exec --json` JSONL output.
//!
//! When the CLI is invoked with `codex exec --json`, every line on stdout is a
//! JSON object whose `"type"` field determines the variant.
//!
//! | `type`             | Rust variant      | Description                              |
//! |--------------------|-------------------|------------------------------------------|
//! | `thread.started`   | `ThreadStarted`   | Thread ID assigned for this execution.   |
//! | `turn.started`     | `TurnStarted`     | A new turn has begun.                    |
//! | `turn.completed`   | `TurnCompleted`   | Turn finished successfully.              |
//! | `turn.failed`      | `TurnFailed`      | Turn failed with an error.               |
//! | `item.started`     | `ItemStarted`     | An item (message, command, etc.) began.  |
//! | `item.updated`     | `ItemUpdated`     | Incremental update to an in-flight item. |
//! | `item.completed`   | `ItemCompleted`   | An item finished.                        |
//! | `token_count`      | `TokenCount`      | Token usage statistics.                  |
//! | `error`            | `Error`           | Execution-level error.                   |

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level event envelope
// ---------------------------------------------------------------------------

/// A single JSONL event read from `codex exec --json` stdout.
///
/// Deserialized via `#[serde(tag = "type")]` so the `"type"` field selects
/// the variant. Unknown event types deserialize as [`Event::Unknown`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    /// A thread has been created for this execution.
    #[serde(rename = "thread.started")]
    ThreadStarted {
        /// The thread identifier.
        thread_id: String,
    },

    /// A new turn has started.
    #[serde(rename = "turn.started")]
    TurnStarted,

    /// A turn completed successfully.
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        /// Token usage for this turn.
        #[serde(default)]
        usage: Option<Usage>,
    },

    /// A turn failed.
    #[serde(rename = "turn.failed")]
    TurnFailed {
        /// Token usage for this turn (may still be reported on failure).
        #[serde(default)]
        usage: Option<Usage>,
        /// Error details.
        #[serde(default)]
        error: Option<ThreadError>,
    },

    /// An item has started (message, command, file change, etc.).
    #[serde(rename = "item.started")]
    ItemStarted {
        /// The item being started.
        item: Item,
    },

    /// Incremental update to an in-flight item.
    #[serde(rename = "item.updated")]
    ItemUpdated {
        /// The item with updated content.
        item: Item,
    },

    /// An item has completed.
    #[serde(rename = "item.completed")]
    ItemCompleted {
        /// The completed item.
        item: Item,
    },

    /// Token usage statistics.
    #[serde(rename = "token_count")]
    TokenCount {
        /// Number of input tokens.
        #[serde(default)]
        input_tokens: u64,
        /// Number of cached input tokens.
        #[serde(default)]
        cached_input_tokens: u64,
        /// Number of output tokens.
        #[serde(default)]
        output_tokens: u64,
    },

    /// An execution-level error.
    #[serde(rename = "error")]
    Error {
        /// Error message.
        #[serde(default)]
        message: Option<String>,
    },

    /// Forward-compatibility: any unrecognized event type.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// An item within a codex execution turn.
///
/// Items represent the model's actions: generating text, executing commands,
/// modifying files, etc. Each item goes through started -> updated* -> completed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    /// A text message from the agent.
    AgentMessage {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The message text.
        #[serde(default)]
        text: Option<String>,
    },

    /// Reasoning / chain-of-thought text.
    Reasoning {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The reasoning text.
        #[serde(default)]
        text: Option<String>,
    },

    /// A shell command execution.
    CommandExecution {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The command that was executed.
        #[serde(default)]
        command: Option<String>,
        /// Aggregated stdout/stderr output from the command.
        #[serde(default)]
        aggregated_output: Option<String>,
        /// Exit code of the command.
        #[serde(default)]
        exit_code: Option<i32>,
        /// Execution status (e.g. "completed", "running").
        #[serde(default)]
        status: Option<String>,
    },

    /// A file modification.
    FileChange {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The individual file changes.
        #[serde(default)]
        changes: Vec<FileUpdateChange>,
        /// Status of the file change.
        #[serde(default)]
        status: Option<String>,
    },

    /// An MCP tool invocation.
    McpToolCall {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The MCP server name.
        #[serde(default)]
        server: Option<String>,
        /// The tool name.
        #[serde(default)]
        tool: Option<String>,
        /// Execution status.
        #[serde(default)]
        status: Option<String>,
    },

    /// A web search query.
    WebSearch {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The search query.
        #[serde(default)]
        query: Option<String>,
    },

    /// A todo/task list.
    TodoList {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// The todo items.
        #[serde(default)]
        items: Vec<TodoItem>,
    },

    /// An item-level error.
    Error {
        /// Unique item identifier.
        #[serde(default)]
        id: Option<String>,
        /// Error message.
        #[serde(default)]
        message: Option<String>,
    },

    /// Forward-compatibility: any unrecognized item type.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Token usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Number of output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Number of cached input tokens.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Total tokens (input + output).
    #[serde(default)]
    pub total_tokens: u64,
}

/// Error information from a failed turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadError {
    /// Error message.
    #[serde(default)]
    pub message: Option<String>,
    /// Error code.
    #[serde(default)]
    pub code: Option<String>,
}

/// A single file change within a [`Item::FileChange`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUpdateChange {
    /// Path to the modified file.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Original file content (before the change).
    #[serde(default)]
    pub old_content: Option<String>,
    /// New file content (after the change).
    #[serde(default)]
    pub new_content: Option<String>,
}

/// A single item in a [`Item::TodoList`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// The todo item text.
    #[serde(default)]
    pub text: Option<String>,
    /// Whether the item is completed.
    #[serde(default)]
    pub completed: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl Event {
    /// Returns `true` if this is a [`Event::ThreadStarted`].
    pub fn is_thread_started(&self) -> bool {
        matches!(self, Event::ThreadStarted { .. })
    }

    /// Returns `true` if this is a [`Event::TurnCompleted`].
    pub fn is_turn_completed(&self) -> bool {
        matches!(self, Event::TurnCompleted { .. })
    }

    /// Returns `true` if this is a [`Event::TurnFailed`].
    pub fn is_turn_failed(&self) -> bool {
        matches!(self, Event::TurnFailed { .. })
    }

    /// Returns `true` if this is an [`Event::Error`].
    pub fn is_error(&self) -> bool {
        matches!(self, Event::Error { .. })
    }

    /// Returns `true` if this is an [`Event::ItemCompleted`].
    pub fn is_item_completed(&self) -> bool {
        matches!(self, Event::ItemCompleted { .. })
    }

    /// Extract the item from an ItemStarted, ItemUpdated, or ItemCompleted event.
    pub fn item(&self) -> Option<&Item> {
        match self {
            Event::ItemStarted { item }
            | Event::ItemUpdated { item }
            | Event::ItemCompleted { item } => Some(item),
            _ => None,
        }
    }
}

impl Item {
    /// Get the item ID, if present.
    pub fn id(&self) -> Option<&str> {
        match self {
            Item::AgentMessage { id, .. }
            | Item::Reasoning { id, .. }
            | Item::CommandExecution { id, .. }
            | Item::FileChange { id, .. }
            | Item::McpToolCall { id, .. }
            | Item::WebSearch { id, .. }
            | Item::TodoList { id, .. }
            | Item::Error { id, .. } => id.as_deref(),
            Item::Unknown => None,
        }
    }

    /// Get the text content for message/reasoning items.
    pub fn text(&self) -> Option<&str> {
        match self {
            Item::AgentMessage { text, .. } | Item::Reasoning { text, .. } => text.as_deref(),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_thread_started() {
        let json = r#"{"type":"thread.started","thread_id":"thread_abc123"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::ThreadStarted { thread_id } => assert_eq!(thread_id, "thread_abc123"),
            other => panic!("expected ThreadStarted, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_turn_started() {
        let json = r#"{"type":"turn.started"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(matches!(event, Event::TurnStarted));
    }

    #[test]
    fn deserialize_turn_started_with_extra_fields() {
        let json = r#"{"type":"turn.started","future_field":"hello"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(matches!(event, Event::TurnStarted));
    }

    #[test]
    fn deserialize_turn_completed_with_usage() {
        let json =
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::TurnCompleted {
                usage: Some(usage), ..
            } => {
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            other => panic!("expected TurnCompleted with usage, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_turn_completed_without_usage() {
        let json = r#"{"type":"turn.completed"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::TurnCompleted { usage: None } => {}
            other => panic!("expected TurnCompleted without usage, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_turn_failed() {
        let json = r#"{"type":"turn.failed","error":{"message":"rate limited","code":"rate_limit"}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::TurnFailed {
                error: Some(ref err),
                ..
            } => {
                assert_eq!(err.message.as_deref(), Some("rate limited"));
                assert_eq!(err.code.as_deref(), Some("rate_limit"));
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_item_completed_agent_message() {
        let json = r#"{"type":"item.completed","item":{"type":"agent_message","id":"msg_1","text":"Hello!"}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::ItemCompleted {
                item: Item::AgentMessage { id, text },
            } => {
                assert_eq!(id.as_deref(), Some("msg_1"));
                assert_eq!(text.as_deref(), Some("Hello!"));
            }
            other => panic!("expected ItemCompleted with AgentMessage, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_item_reasoning() {
        let json =
            r#"{"type":"item.started","item":{"type":"reasoning","id":"r_1","text":"Let me think..."}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::ItemStarted {
                item: Item::Reasoning { id, text },
            } => {
                assert_eq!(id.as_deref(), Some("r_1"));
                assert_eq!(text.as_deref(), Some("Let me think..."));
            }
            other => panic!("expected ItemStarted with Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_item_command_execution() {
        let json = r#"{"type":"item.completed","item":{"type":"command_execution","id":"cmd_1","command":"ls -la","aggregated_output":"total 42\n","exit_code":0,"status":"completed"}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::ItemCompleted {
                item:
                    Item::CommandExecution {
                        id,
                        command,
                        exit_code,
                        status,
                        ..
                    },
            } => {
                assert_eq!(id.as_deref(), Some("cmd_1"));
                assert_eq!(command.as_deref(), Some("ls -la"));
                assert_eq!(exit_code, Some(0));
                assert_eq!(status.as_deref(), Some("completed"));
            }
            other => panic!("expected CommandExecution, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_item_file_change() {
        let json = r#"{"type":"item.completed","item":{"type":"file_change","id":"fc_1","changes":[{"file_path":"src/main.rs","new_content":"fn main() {}"}],"status":"completed"}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::ItemCompleted {
                item: Item::FileChange {
                    id, changes, status,
                },
            } => {
                assert_eq!(id.as_deref(), Some("fc_1"));
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].file_path.as_deref(), Some("src/main.rs"));
                assert_eq!(status.as_deref(), Some("completed"));
            }
            other => panic!("expected FileChange, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_token_count() {
        let json = r#"{"type":"token_count","input_tokens":200,"cached_input_tokens":50,"output_tokens":100}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::TokenCount {
                input_tokens,
                cached_input_tokens,
                output_tokens,
            } => {
                assert_eq!(input_tokens, 200);
                assert_eq!(cached_input_tokens, 50);
                assert_eq!(output_tokens, 100);
            }
            other => panic!("expected TokenCount, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_error_event() {
        let json = r#"{"type":"error","message":"something went wrong"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::Error { message } => {
                assert_eq!(message.as_deref(), Some("something went wrong"))
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_unknown_event_type() {
        let json = r#"{"type":"future.event","some_field":"value"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(matches!(event, Event::Unknown));
    }

    #[test]
    fn deserialize_unknown_item_type() {
        let json = r#"{"type":"item.completed","item":{"type":"future_item","id":"x"}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::ItemCompleted {
                item: Item::Unknown,
            } => {}
            other => panic!("expected ItemCompleted with Unknown item, got {other:?}"),
        }
    }

    #[test]
    fn item_id_helper() {
        let item = Item::AgentMessage {
            id: Some("msg_1".into()),
            text: Some("hi".into()),
        };
        assert_eq!(item.id(), Some("msg_1"));
        assert_eq!(Item::Unknown.id(), None);
    }

    #[test]
    fn item_text_helper() {
        let item = Item::Reasoning {
            id: None,
            text: Some("thinking...".into()),
        };
        assert_eq!(item.text(), Some("thinking..."));

        let cmd = Item::CommandExecution {
            id: None,
            command: None,
            aggregated_output: None,
            exit_code: None,
            status: None,
        };
        assert_eq!(cmd.text(), None);
    }

    #[test]
    fn event_item_helper() {
        let event = Event::ItemCompleted {
            item: Item::AgentMessage {
                id: Some("m1".into()),
                text: Some("hello".into()),
            },
        };
        assert!(event.item().is_some());
        assert_eq!(event.item().unwrap().id(), Some("m1"));

        assert!(Event::TurnStarted.item().is_none());
    }
}

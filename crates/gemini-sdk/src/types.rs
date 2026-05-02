//! Protocol types for the Gemini CLI JSONL output.

use serde::{Deserialize, Serialize};

/// A single JSONL event read from Gemini CLI stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    /// Final headless JSON output object from the Gemini CLI.
    JsonOutput {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        response: Option<String>,
        #[serde(default)]
        stats: Option<ResultStats>,
        #[serde(default)]
        error: Option<JsonError>,
    },

    // Legacy schema.
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },

    #[serde(rename = "turn.started")]
    TurnStarted,

    #[serde(rename = "turn.completed")]
    TurnCompleted {
        #[serde(default)]
        usage: Option<Usage>,
    },

    #[serde(rename = "turn.failed")]
    TurnFailed {
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(default)]
        error: Option<ThreadError>,
    },

    #[serde(rename = "item.started")]
    ItemStarted { item: Item },

    #[serde(rename = "item.updated")]
    ItemUpdated { item: Item },

    #[serde(rename = "item.completed")]
    ItemCompleted { item: Item },

    #[serde(rename = "token_count")]
    TokenCount {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        cached_input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    },

    // Current stream-json schema.
    #[serde(rename = "init")]
    Init {
        #[serde(default)]
        model: Option<String>,
        session_id: String,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "agent_start")]
    AgentStart,

    #[serde(rename = "agent_end")]
    AgentEnd {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        data: Option<serde_json::Value>,
    },

    #[serde(rename = "session_update")]
    SessionUpdate {
        #[serde(default)]
        model: Option<String>,
    },

    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        content: MessageContent,
        #[serde(default)]
        delta: Option<bool>,
    },

    #[serde(rename = "tool_request")]
    ToolRequest {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        args: Option<serde_json::Value>,
    },

    #[serde(rename = "tool_response")]
    ToolResponse {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        content: MessageContent,
        #[serde(default)]
        is_error: Option<bool>,
    },

    #[serde(rename = "usage")]
    UsageEvent {
        #[serde(default)]
        model: Option<String>,
        #[serde(default, rename = "inputTokens")]
        input_tokens: Option<u64>,
        #[serde(default, rename = "outputTokens")]
        output_tokens: Option<u64>,
        #[serde(default, rename = "cachedTokens")]
        cached_tokens: Option<u64>,
    },

    #[serde(rename = "result")]
    Result {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        stats: Option<ResultStats>,
        #[serde(default)]
        timestamp: Option<String>,
    },

    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        fatal: Option<bool>,
    },

    #[serde(other)]
    Unknown,
}

/// An item within the legacy Gemini execution schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    AgentMessage {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },

    Reasoning {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },

    CommandExecution {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        aggregated_output: Option<String>,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        status: Option<String>,
    },

    FileChange {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        changes: Vec<FileUpdateChange>,
        #[serde(default)]
        status: Option<String>,
    },

    ToolCall {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },

    WebSearch {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        query: Option<String>,
    },

    TodoList {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        items: Vec<TodoItem>,
    },

    Error {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },

    #[serde(other)]
    Unknown,
}

/// Content payload within the current Gemini `message` / `tool_response`
/// schema. Recent CLI builds send a raw string for text deltas, while older
/// builds emitted structured parts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
    #[default]
    Empty,
}

/// A content part within the current Gemini `message` / `tool_response` schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "thought")]
    Thought { thought: String },

    #[serde(other)]
    Unknown,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// Aggregate stats from the current Gemini `result` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResultStats {
    #[serde(default)]
    pub cached: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub tool_calls: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub models: Option<serde_json::Value>,
}

/// Error payload from the final headless JSON output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JsonError {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

/// Error information from a failed legacy turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadError {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

/// A single file change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUpdateChange {
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub old_content: Option<String>,
    #[serde(default)]
    pub new_content: Option<String>,
}

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub completed: bool,
}

impl Event {
    pub fn is_thread_started(&self) -> bool {
        matches!(self, Event::ThreadStarted { .. } | Event::Init { .. })
    }

    pub fn is_turn_completed(&self) -> bool {
        matches!(self, Event::TurnCompleted { .. })
    }

    pub fn is_turn_failed(&self) -> bool {
        matches!(self, Event::TurnFailed { .. })
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Event::Error { .. })
    }

    pub fn is_item_completed(&self) -> bool {
        matches!(self, Event::ItemCompleted { .. })
    }

    pub fn item(&self) -> Option<&Item> {
        match self {
            Event::ItemStarted { item }
            | Event::ItemUpdated { item }
            | Event::ItemCompleted { item } => Some(item),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<String> {
        match self {
            Event::JsonOutput { response, .. } => response
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned),
            Event::Message { role, content, .. } => {
                if role.as_deref() == Some("user") {
                    return None;
                }
                content.text()
            }
            Event::ToolResponse { content, .. } => {
                content.text()
            }
            Event::AgentEnd { data, .. } => data
                .as_ref()
                .and_then(|value| value.get("message"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned),
            Event::ItemUpdated { item } | Event::ItemCompleted { item } => {
                item.text().map(ToOwned::to_owned)
            }
            _ => None,
        }
    }
}

impl MessageContent {
    pub fn text(&self) -> Option<String> {
        match self {
            MessageContent::Text(text) => {
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_owned())
            }
            MessageContent::Parts(parts) => {
                let text = parts
                    .iter()
                    .filter_map(ContentPart::text)
                    .collect::<Vec<_>>()
                    .join("");
                (!text.is_empty()).then_some(text)
            }
            MessageContent::Empty => None,
        }
    }
}

impl Item {
    pub fn id(&self) -> Option<&str> {
        match self {
            Item::AgentMessage { id, .. }
            | Item::Reasoning { id, .. }
            | Item::CommandExecution { id, .. }
            | Item::FileChange { id, .. }
            | Item::ToolCall { id, .. }
            | Item::WebSearch { id, .. }
            | Item::TodoList { id, .. }
            | Item::Error { id, .. } => id.as_deref(),
            Item::Unknown => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Item::AgentMessage { text, .. } | Item::Reasoning { text, .. } => text.as_deref(),
            _ => None,
        }
    }
}

impl ContentPart {
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentPart::Text { text } => Some(text),
            ContentPart::Thought { thought } => Some(thought),
            ContentPart::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_legacy_thread_started() {
        let json = r#"{"type":"thread.started","thread_id":"t123"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(event.is_thread_started());
    }

    #[test]
    fn test_json_output_text() {
        let event = Event::JsonOutput {
            session_id: Some("sess-1".into()),
            response: Some("hello".into()),
            stats: None,
            error: None,
        };
        assert_eq!(event.text().as_deref(), Some("hello"));
    }

    #[test]
    fn test_deserialize_init_event() {
        let json = r#"{"type":"init","model":"gemini-2.5-flash","session_id":"sess-123","timestamp":"2026-04-27T18:44:11.978Z"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(event.is_thread_started());
        match event {
            Event::Init {
                model,
                session_id,
                timestamp,
            } => {
                assert_eq!(model.as_deref(), Some("gemini-2.5-flash"));
                assert_eq!(session_id, "sess-123");
                assert_eq!(timestamp.as_deref(), Some("2026-04-27T18:44:11.978Z"));
            }
            _ => panic!("wrong event type"),
        }
    }

    #[test]
    fn test_deserialize_current_message_event() {
        let json =
            r#"{"type":"message","role":"agent","content":[{"type":"text","text":"hello"}]}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.text().as_deref(), Some("hello"));
    }

    #[test]
    fn test_deserialize_current_message_string_content() {
        let json = r#"{"type":"message","role":"assistant","content":"hello","delta":true}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.text().as_deref(), Some("hello"));
    }

    #[test]
    fn test_deserialize_current_error_event() {
        let json = r#"{"type":"error","status":"UNAUTHENTICATED","message":"login required","fatal":true}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(event.is_error());
        match event {
            Event::Error {
                status,
                message,
                fatal,
            } => {
                assert_eq!(status.as_deref(), Some("UNAUTHENTICATED"));
                assert_eq!(message.as_deref(), Some("login required"));
                assert_eq!(fatal, Some(true));
            }
            _ => panic!("wrong event type"),
        }
    }

    #[test]
    fn test_deserialize_tool_response_text() {
        let json = r#"{"type":"tool_response","name":"bash","content":[{"type":"text","text":"ok"}],"is_error":false}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.text().as_deref(), Some("ok"));
    }

    #[test]
    fn test_deserialize_result_event() {
        let json = r#"{"type":"result","status":"success","stats":{"output_tokens":9,"total_tokens":7711},"timestamp":"2026-04-27T18:46:16.098Z"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::Result {
                status,
                stats,
                timestamp,
            } => {
                assert_eq!(status.as_deref(), Some("success"));
                assert_eq!(stats.and_then(|s| s.output_tokens), Some(9));
                assert_eq!(timestamp.as_deref(), Some("2026-04-27T18:46:16.098Z"));
            }
            _ => panic!("wrong event type"),
        }
    }

    #[test]
    fn test_deserialize_unknown() {
        let json = r#"{"type":"future.event"}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert!(matches!(event, Event::Unknown));
    }

    #[test]
    fn test_agent_end_message_fallback() {
        let json = r#"{"type":"agent_end","reason":"completed","data":{"message":"final reply"}}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.text().as_deref(), Some("final reply"));
    }
}

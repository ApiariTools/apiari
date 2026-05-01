//! All message types from the Claude CLI stream-json protocol.
//!
//! When the CLI is invoked with `--output-format stream-json --verbose`, every
//! line on stdout is a JSON object whose `"type"` field determines the variant.
//!
//! The top-level message types are:
//!
//! | `type`               | Rust type            | Description                                       |
//! |----------------------|----------------------|---------------------------------------------------|
//! | `system`             | [`SystemMessage`]    | Metadata emitted at session start.                |
//! | `user`               | [`UserMessage`]      | Echo of the user turn.                            |
//! | `assistant`          | [`AssistantMessage`] | Model turn with text / tool-use content.          |
//! | `result`             | [`ResultMessage`]    | Final summary (cost, duration, session ID).       |
//! | `stream_event`       | [`StreamEvent`]      | Raw Anthropic API streaming event (partial data). |
//! | `rate_limit_event`   | [`RateLimitEvent`]   | Rate limit status information.                    |

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level message envelope
// ---------------------------------------------------------------------------

/// A single NDJSON message read from the Claude CLI stdout.
///
/// Deserialized via `#[serde(tag = "type")]` so the `"type"` field selects
/// the variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Session metadata emitted once at the start.
    System(SystemMessage),
    /// Echo of a user turn (our input reflected back).
    User(UserMessage),
    /// A model response turn.
    Assistant(AssistantMessage),
    /// Final summary after the conversation completes.
    Result(ResultMessage),
    /// A raw API streaming event (only when `--include-partial-messages`).
    StreamEvent(StreamEvent),
    /// Rate limit status information.
    RateLimitEvent(RateLimitEvent),
}

// ---------------------------------------------------------------------------
// System message
// ---------------------------------------------------------------------------

/// Metadata emitted once when the session starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    /// Sub-classification of the system message (e.g. `"init"`).
    pub subtype: String,

    /// The full raw JSON data for forward-compatibility.
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// User message
// ---------------------------------------------------------------------------

/// A user turn, either from our stdin input or echoed back by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    /// The message content (text string or structured content blocks).
    pub message: UserMessageContent,

    /// Session-unique identifier for this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// If this user message is a tool result, the parent tool-use ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,

    /// When the user message carries a tool result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<serde_json::Value>,
}

/// The inner content of a user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageContent {
    /// Always `"user"`.
    #[serde(default = "default_user_role")]
    pub role: String,

    /// Either a bare string or a vec of content blocks.
    pub content: UserContent,
}

fn default_user_role() -> String {
    "user".to_owned()
}

/// User content: either a simple text string or structured blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// A plain text string.
    Text(String),
    /// An array of content blocks.
    Blocks(Vec<ContentBlock>),
}

// ---------------------------------------------------------------------------
// Assistant message
// ---------------------------------------------------------------------------

/// The top-level assistant message envelope as emitted by the CLI.
///
/// The actual model content is nested inside the `message` field, with
/// metadata like `session_id` and `parent_tool_use_id` at the envelope level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// The inner message containing model output (content blocks, model name, etc.).
    pub message: AssistantMessageContent,

    /// If this turn was produced inside a tool-use (sub-agent), the parent ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,

    /// The session ID for this conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Session-unique identifier for this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// The inner content of an assistant message, containing the model's response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessageContent {
    /// The model that produced this turn (e.g. `"claude-opus-4-6"`).
    pub model: String,

    /// Content blocks: text, thinking, tool_use, or tool_result.
    #[serde(default)]
    pub content: Vec<ContentBlock>,

    /// Anthropic message ID (e.g. `"msg_01NwW..."`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// The role, always `"assistant"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Why the model stopped generating (e.g. `"end_turn"`, `"tool_use"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,

    /// Token usage for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,

    /// Forward-compatibility: capture any extra fields we don't know about.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Result message
// ---------------------------------------------------------------------------

/// Final summary emitted when the conversation finishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultMessage {
    /// Sub-classification (e.g. `"success"`, `"error"`, `"max_turns"`).
    pub subtype: String,

    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,

    /// API-only duration in milliseconds.
    pub duration_api_ms: u64,

    /// Whether the conversation ended in an error state.
    pub is_error: bool,

    /// How many agentic turns were executed.
    pub num_turns: u64,

    /// The session ID (useful for `--resume`).
    pub session_id: String,

    /// Total estimated cost in USD, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,

    /// Token usage breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,

    /// The final text result, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,

    /// Structured output when `--json-schema` was provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Stream event (partial messages)
// ---------------------------------------------------------------------------

/// A raw Anthropic API streaming event, emitted when
/// `--include-partial-messages` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Session-unique identifier.
    pub uuid: String,

    /// The session ID.
    pub session_id: String,

    /// The raw API event payload.
    pub event: StreamEventPayload,

    /// Parent tool-use ID if this event is from a sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Rate limit event
// ---------------------------------------------------------------------------

/// Rate limit status information emitted by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitEvent {
    /// Rate limit status details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_info: Option<serde_json::Value>,

    /// Session-unique identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// The session ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// The inner event payload from the Anthropic streaming API.
///
/// This mirrors the Server-Sent Events that the Anthropic API emits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEventPayload {
    /// Signals the start of a new message, includes message metadata.
    MessageStart {
        /// The message object with `id`, `role`, `model`, etc.
        message: serde_json::Value,
    },

    /// Signals the start of a content block at the given index.
    ContentBlockStart {
        /// Zero-based index of this content block.
        index: u64,
        /// The initial content block (type, and any initial data).
        content_block: ContentBlockInfo,
    },

    /// An incremental update to the content block at the given index.
    ContentBlockDelta {
        /// Zero-based index of the content block being updated.
        index: u64,
        /// The delta payload.
        delta: Delta,
    },

    /// Signals that the content block at the given index is complete.
    ContentBlockStop {
        /// Zero-based index of the completed content block.
        index: u64,
    },

    /// An update to the top-level message (e.g. `stop_reason`, usage).
    MessageDelta {
        /// The delta payload.
        delta: serde_json::Value,
        /// Updated usage statistics.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Value>,
    },

    /// Signals that the entire message is complete.
    MessageStop,

    /// Catch-all for forward-compatibility with unknown event types.
    #[serde(other)]
    Unknown,
}

/// Information about a content block at its start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockInfo {
    /// A text content block.
    Text {
        /// Initial text (usually empty at start).
        #[serde(default)]
        text: String,
    },
    /// A thinking content block.
    Thinking {
        /// Initial thinking text.
        #[serde(default)]
        thinking: String,
    },
    /// A tool-use content block.
    ToolUse {
        /// The tool-use ID.
        id: String,
        /// The tool name.
        name: String,
        /// Initial input (usually empty object or partial JSON).
        #[serde(default)]
        input: serde_json::Value,
    },
}

/// An incremental delta within a `content_block_delta` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    /// Incremental text.
    TextDelta {
        /// The text fragment.
        text: String,
    },
    /// Incremental thinking text.
    ThinkingDelta {
        /// The thinking fragment.
        thinking: String,
    },
    /// Incremental JSON for a tool-use input.
    InputJsonDelta {
        /// Partial JSON string to append.
        partial_json: String,
    },
}

// ---------------------------------------------------------------------------
// Content blocks (used in assistant & user messages)
// ---------------------------------------------------------------------------

/// A content block inside an assistant or user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text {
        /// The text content.
        text: String,
    },
    /// Model thinking / chain-of-thought (extended thinking).
    Thinking {
        /// The thinking text.
        thinking: String,
        /// Cryptographic signature for verification.
        signature: String,
    },
    /// A request for the SDK to execute a tool.
    ToolUse {
        /// Unique identifier for this tool invocation.
        id: String,
        /// The tool name (e.g. `"Bash"`, `"Edit"`, `"Read"`).
        name: String,
        /// The tool input parameters.
        input: serde_json::Value,
    },
    /// The result of a tool execution.
    ToolResult {
        /// The `id` from the corresponding `ToolUse` block.
        tool_use_id: String,
        /// The result content (text string, structured blocks, or null).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        /// Whether the tool execution failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

// ---------------------------------------------------------------------------
// Input messages (sent to stdin with --input-format stream-json)
// ---------------------------------------------------------------------------

/// A message we write to the CLI's stdin when using `--input-format stream-json`.
///
/// The CLI expects a JSON object per line. For user messages, the format is:
/// ```json
/// {"type":"user","message":{"role":"user","content":"Hello"}}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputMessage {
    /// A user message to send to the model.
    User {
        /// The message payload.
        message: InputMessageContent,
    },
}

/// The inner content of an input message sent to stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMessageContent {
    /// Always `"user"`.
    pub role: String,
    /// The content: text or content blocks.
    pub content: InputContent,
}

/// Content for an input message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputContent {
    /// A plain text string.
    Text(String),
    /// Structured content blocks (for tool results, images, etc.).
    Blocks(Vec<InputContentBlock>),
}

/// A content block within an input message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentBlock {
    /// Plain text.
    Text {
        /// The text content.
        text: String,
    },
    /// An image block (base64-encoded).
    Image {
        /// The image source.
        source: ImageSource,
    },
    /// A tool result block.
    ToolResult {
        /// The tool_use ID this result corresponds to.
        tool_use_id: String,
        /// The result content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        /// Whether the tool errored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Source data for an image content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    /// Always `"base64"`.
    #[serde(rename = "type")]
    pub source_type: String,
    /// MIME type (e.g. `"image/png"`, `"image/jpeg"`).
    pub media_type: String,
    /// Base64-encoded image data.
    pub data: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl InputMessage {
    /// Create a simple text user message.
    pub fn user_text(text: impl Into<String>) -> Self {
        InputMessage::User {
            message: InputMessageContent {
                role: "user".to_owned(),
                content: InputContent::Text(text.into()),
            },
        }
    }

    /// Create a user message with text and images.
    ///
    /// Images should be provided as `(media_type, base64_data)` tuples.
    pub fn user_with_images(
        text: impl Into<String>,
        images: Vec<(String, String)>,
    ) -> Self {
        let mut blocks: Vec<InputContentBlock> = images
            .into_iter()
            .map(|(media_type, data)| InputContentBlock::Image {
                source: ImageSource {
                    source_type: "base64".to_owned(),
                    media_type,
                    data,
                },
            })
            .collect();
        let text = text.into();
        if !text.is_empty() {
            blocks.push(InputContentBlock::Text { text });
        }
        InputMessage::User {
            message: InputMessageContent {
                role: "user".to_owned(),
                content: InputContent::Blocks(blocks),
            },
        }
    }

    /// Create a tool-result user message.
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        InputMessage::User {
            message: InputMessageContent {
                role: "user".to_owned(),
                content: InputContent::Blocks(vec![InputContentBlock::ToolResult {
                    tool_use_id: tool_use_id.into(),
                    content: Some(output.into()),
                    is_error: if is_error { Some(true) } else { None },
                }]),
            },
        }
    }
}

impl Message {
    /// Returns `true` if this is a [`ResultMessage`].
    pub fn is_result(&self) -> bool {
        matches!(self, Message::Result(_))
    }

    /// Returns `true` if this is a [`StreamEvent`].
    pub fn is_stream_event(&self) -> bool {
        matches!(self, Message::StreamEvent(_))
    }

    /// Returns `true` if this is an [`AssistantMessage`].
    pub fn is_assistant(&self) -> bool {
        matches!(self, Message::Assistant(_))
    }

    /// Try to extract a reference to the inner [`AssistantMessage`].
    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            Message::Assistant(m) => Some(m),
            _ => None,
        }
    }

    /// Try to extract a reference to the inner [`ResultMessage`].
    pub fn as_result(&self) -> Option<&ResultMessage> {
        match self {
            Message::Result(m) => Some(m),
            _ => None,
        }
    }

    /// Try to extract a reference to the inner [`StreamEvent`].
    pub fn as_stream_event(&self) -> Option<&StreamEvent> {
        match self {
            Message::StreamEvent(e) => Some(e),
            _ => None,
        }
    }
}

//! Tool use request/result protocol types.
//!
//! These are convenience wrappers around the raw [`ContentBlock::ToolUse`] and
//! [`ContentBlock::ToolResult`] variants, providing a more ergonomic API for
//! the common case of extracting tool calls from assistant messages and
//! constructing tool results to send back.

use serde::{Deserialize, Serialize};

/// A tool-use request extracted from an assistant message.
///
/// This is a flattened view of a [`ContentBlock::ToolUse`](crate::types::ContentBlock::ToolUse)
/// block, convenient for pattern-matching in application code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    /// Unique identifier for this tool invocation (assigned by the model).
    pub id: String,
    /// The tool name (e.g. `"Bash"`, `"Edit"`, `"Read"`, or an MCP tool).
    pub name: String,
    /// The tool input parameters as a JSON value.
    pub input: serde_json::Value,
}

/// A tool result to send back to the model.
///
/// Constructed by application code after executing the requested tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The `id` from the corresponding [`ToolUse`].
    pub tool_use_id: String,
    /// The result output (text content).
    pub output: String,
    /// Whether the tool execution failed.
    pub is_error: bool,
}

impl ToolUse {
    /// Extract all tool-use requests from an assistant message's content blocks.
    pub fn extract_from_content(content: &[crate::types::ContentBlock]) -> Vec<Self> {
        content
            .iter()
            .filter_map(|block| match block {
                crate::types::ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

impl ToolResult {
    /// Create a successful tool result.
    pub fn success(tool_use_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            output: output.into(),
            is_error: false,
        }
    }

    /// Create an error tool result.
    pub fn error(tool_use_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            output: message.into(),
            is_error: true,
        }
    }

    /// Convert this tool result into an [`InputMessage`](crate::types::InputMessage)
    /// suitable for writing to the CLI's stdin.
    pub fn into_input_message(self) -> crate::types::InputMessage {
        crate::types::InputMessage::tool_result(self.tool_use_id, self.output, self.is_error)
    }
}

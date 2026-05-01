//! Rust SDK for the Claude CLI.
//!
//! This crate wraps the `claude` command-line tool, communicating via
//! newline-delimited JSON (NDJSON) over stdin/stdout using the
//! `--input-format stream-json --output-format stream-json` protocol.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use apiari_claude_sdk::{ClaudeClient, SessionOptions};
//!
//! # async fn run() -> apiari_claude_sdk::error::Result<()> {
//! let client = ClaudeClient::new();
//! let mut session = client.spawn(SessionOptions {
//!     model: Some("sonnet".into()),
//!     allowed_tools: vec!["Bash".into(), "Read".into()],
//!     ..Default::default()
//! }).await?;
//!
//! session.send_message("List files in the current directory").await?;
//!
//! while let Some(event) = session.next_event().await? {
//!     match event {
//!         apiari_claude_sdk::Event::Assistant { message, tool_uses } => {
//!             for block in &message.message.content {
//!                 if let apiari_claude_sdk::types::ContentBlock::Text { text } = block {
//!                     print!("{text}");
//!                 }
//!             }
//!             // Handle tool_uses if needed...
//!             drop(tool_uses);
//!         }
//!         apiari_claude_sdk::Event::Result(result) => {
//!             println!("\nDone! Session: {}", result.session_id);
//!             break;
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod error;
pub mod session;
pub mod streaming;
pub mod tools;
pub mod transport;
pub mod types;

// Re-export the most commonly used types at the crate root.
pub use client::{ClaudeClient, Event, Session};
pub use error::{Result, SdkError};
pub use session::{PermissionMode, SessionOptions};
pub use streaming::{AssembledEvent, StreamAssembler};
pub use tools::{ToolResult, ToolUse};
pub use types::{
    AssistantMessage, AssistantMessageContent, ContentBlock, InputMessage, Message, RateLimitEvent,
    ResultMessage, StreamEvent, SystemMessage, UserMessage,
};

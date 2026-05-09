//! Rust SDK for the Codex CLI.
//!
//! This crate wraps the `codex` command-line tool, reading JSONL events from
//! stdout when invoked with `codex exec --json`. Unlike the Claude SDK, this
//! is **unidirectional** — the prompt goes as a CLI argument and stdin is
//! `/dev/null`.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use apiari_codex_sdk::{CodexClient, ExecOptions, Event, Item};
//!
//! # async fn run() -> apiari_codex_sdk::error::Result<()> {
//! let client = CodexClient::new();
//! let mut execution = client.exec("List files in the current directory", ExecOptions {
//!     model: Some("o4-mini".into()),
//!     full_auto: true,
//!     ..Default::default()
//! }).await?;
//!
//! while let Some(event) = execution.next_event().await? {
//!     match &event {
//!         Event::ItemCompleted { item: Item::AgentMessage { text, .. } } => {
//!             if let Some(text) = text {
//!                 println!("{text}");
//!             }
//!         }
//!         Event::TurnCompleted { usage } => {
//!             if let Some(usage) = usage {
//!                 println!("Tokens: {} in, {} out", usage.input_tokens, usage.output_tokens);
//!             }
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod error;
pub mod options;
pub mod transport;
pub mod types;

// Re-export the most commonly used types at the crate root.
pub use client::{CodexClient, Execution};
pub use error::{Result, SdkError};
pub use options::{ApprovalPolicy, ExecOptions, ResumeOptions, SandboxMode};
pub use types::{Event, FileUpdateChange, Item, ThreadError, TodoItem, Usage};

//! Rust SDK for the Gemini CLI.
pub mod client;
pub mod error;
pub mod options;
pub mod transport;
pub mod types;
pub use client::{Execution, GeminiClient};
pub use error::{Result, SdkError};
pub use options::{GeminiOptions, SessionOptions};
pub use types::{Event, FileUpdateChange, Item, ThreadError, TodoItem, Usage};

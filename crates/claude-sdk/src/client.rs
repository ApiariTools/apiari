//! High-level client for spawning and interacting with Claude sessions.
//!
//! [`ClaudeClient`] is the main entry point. Configure it once, then call
//! [`spawn`](ClaudeClient::spawn) to create a [`Session`] that communicates
//! with a running `claude` subprocess over NDJSON.
//!
//! # Example
//!
//! ```rust,no_run
//! # use apiari_claude_sdk::{ClaudeClient, SessionOptions};
//! # async fn example() -> apiari_claude_sdk::error::Result<()> {
//! let client = ClaudeClient::new();
//! let opts = SessionOptions {
//!     model: Some("sonnet".into()),
//!     allowed_tools: vec!["Bash".into(), "Read".into(), "Edit".into()],
//!     ..Default::default()
//! };
//! let mut session = client.spawn(opts).await?;
//!
//! session.send_message("What files are in the current directory?").await?;
//!
//! while let Some(event) = session.next_event().await? {
//!     println!("{event:?}");
//! }
//! # Ok(())
//! # }
//! ```

use crate::error::{Result, SdkError};
use crate::session::SessionOptions;
use crate::streaming::{AssembledEvent, StreamAssembler};
use crate::tools::{ToolResult, ToolUse};
use crate::transport::Transport;
use crate::types::{InputMessage, Message};

/// Builder / factory for Claude sessions.
///
/// Holds configuration that applies to every session spawned by this client,
/// such as the path to the `claude` binary.
#[derive(Debug, Clone)]
pub struct ClaudeClient {
    /// Path to the `claude` CLI binary.
    pub cli_path: String,
}

impl Default for ClaudeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeClient {
    /// Create a new client that will look for `claude` on `$PATH`.
    pub fn new() -> Self {
        Self {
            cli_path: "claude".to_owned(),
        }
    }

    /// Create a new client with a custom path to the Claude CLI binary.
    pub fn with_cli_path(cli_path: impl Into<String>) -> Self {
        Self {
            cli_path: cli_path.into(),
        }
    }

    /// Spawn a new Claude session with the given options.
    ///
    /// This starts the `claude` subprocess and returns a [`Session`] handle
    /// for sending messages and receiving events.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`] if the `claude` binary cannot be
    /// found or started.
    pub async fn spawn(&self, opts: SessionOptions) -> Result<Session> {
        let args = opts.to_cli_args();
        let transport = Transport::spawn(
            &self.cli_path,
            &args,
            opts.working_dir.as_deref(),
            &opts.env_vars,
        )?;

        Ok(Session {
            transport,
            assembler: StreamAssembler::new(),
            finished: false,
        })
    }
}

/// A live session with a running `claude` subprocess.
///
/// Provides methods for sending messages, sending tool results, reading
/// events, and interrupting the model.
pub struct Session {
    transport: Transport,
    assembler: StreamAssembler,
    finished: bool,
}

impl Session {
    /// Send a text message to the model.
    ///
    /// This writes a user message to the subprocess's stdin in NDJSON format.
    ///
    /// # Errors
    ///
    /// Returns an error if the session is not connected or writing fails.
    pub async fn send_message(&mut self, text: &str) -> Result<()> {
        if self.finished {
            return Err(SdkError::NotConnected);
        }
        let msg = InputMessage::user_text(text);
        self.transport.send(&msg).await
    }

    /// Send a message with text and images.
    ///
    /// Images are `(media_type, base64_data)` tuples.
    pub async fn send_message_with_images(
        &mut self,
        text: &str,
        images: Vec<(String, String)>,
    ) -> Result<()> {
        if self.finished {
            return Err(SdkError::NotConnected);
        }
        let msg = InputMessage::user_with_images(text, images);
        self.transport.send(&msg).await
    }

    /// Send a tool result back to the model.
    ///
    /// After receiving a tool-use request via [`next_event`](Self::next_event),
    /// execute the tool and send the result back with this method.
    ///
    /// # Errors
    ///
    /// Returns an error if the session is not connected or writing fails.
    pub async fn send_tool_result(&mut self, result: &ToolResult) -> Result<()> {
        if self.finished {
            return Err(SdkError::NotConnected);
        }
        let msg = InputMessage::tool_result(&result.tool_use_id, &result.output, result.is_error);
        self.transport.send(&msg).await
    }

    /// Get the next event from the stream.
    ///
    /// Returns `Ok(None)` when the session is complete (either the subprocess
    /// exited or a `result` message was received).
    ///
    /// # Errors
    ///
    /// Returns an error on I/O failure, JSON parse failure, or if the
    /// subprocess dies unexpectedly.
    pub async fn next_event(&mut self) -> Result<Option<Event>> {
        if self.finished {
            return Ok(None);
        }

        loop {
            let value = self.transport.recv().await?;

            let Some(value) = value else {
                // EOF — process exited.
                self.finished = true;
                return Ok(None);
            };

            // Try to parse as a typed Message.
            let message: Message = match serde_json::from_value(value.clone()) {
                Ok(m) => m,
                Err(e) => {
                    // If we can't parse it, log and skip (forward compatibility).
                    tracing::warn!(
                        error = %e,
                        line = %value,
                        "skipping unrecognized message from claude stdout"
                    );
                    continue;
                }
            };

            match message {
                Message::System(sys) => {
                    return Ok(Some(Event::System(sys)));
                }
                Message::User(user) => {
                    return Ok(Some(Event::User(user)));
                }
                Message::Assistant(assistant) => {
                    // Extract tool-use requests for convenience.
                    let tool_uses = ToolUse::extract_from_content(&assistant.message.content);
                    return Ok(Some(Event::Assistant {
                        message: assistant,
                        tool_uses,
                    }));
                }
                Message::RateLimitEvent(event) => {
                    return Ok(Some(Event::RateLimit(event)));
                }
                Message::Result(result) => {
                    self.finished = true;
                    return Ok(Some(Event::Result(result)));
                }
                Message::StreamEvent(stream_event) => {
                    // Process through the assembler.
                    let assembled = self.assembler.process(&stream_event.event);
                    return Ok(Some(Event::Stream {
                        raw: stream_event,
                        assembled,
                    }));
                }
            }
        }
    }

    /// Send an interrupt signal to the subprocess (SIGINT).
    ///
    /// This tells Claude to stop its current operation. The session remains
    /// open and can continue to receive events and send new messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be sent.
    pub async fn interrupt(&mut self) -> Result<()> {
        self.transport.interrupt()
    }

    /// Close stdin, signaling to the CLI that no more messages will be sent.
    ///
    /// After this call, [`send_message`](Self::send_message) and
    /// [`send_tool_result`](Self::send_tool_result) will return
    /// [`SdkError::NotConnected`]. The session remains open for reading
    /// events via [`next_event`](Self::next_event) until the CLI process
    /// finishes.
    ///
    /// This is useful when you have sent all your messages and want the CLI
    /// to process them and emit its response. In `--input-format stream-json`
    /// mode, the CLI may wait for EOF on stdin before finalizing.
    pub fn close_stdin(&mut self) {
        self.transport.close_stdin();
    }

    /// Close the session by closing stdin and waiting for the process to exit.
    ///
    /// Returns the exit code and any captured stderr output.
    pub async fn close(mut self) -> Result<(Option<i32>, Option<String>)> {
        self.transport.close_stdin();
        self.transport.wait_with_stderr().await
    }

    /// Wait for the subprocess to exit and return any captured stderr.
    ///
    /// Unlike [`close`](Self::close), this does not consume the session.
    /// Useful for retrieving stderr diagnostics after the session has finished.
    pub async fn wait_for_stderr(&mut self) -> Result<Option<String>> {
        let (_, stderr) = self.transport.wait_with_stderr().await?;
        Ok(stderr)
    }

    /// Kill the subprocess immediately.
    pub async fn kill(mut self) -> Result<()> {
        self.transport.kill().await
    }

    /// Returns `true` if the session has received a result message or the
    /// subprocess has exited.
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

/// A high-level event from a Claude session.
///
/// This is the primary type that callers iterate over via
/// [`Session::next_event`].
#[derive(Debug, Clone)]
pub enum Event {
    /// A system metadata message (emitted once at session start).
    System(crate::types::SystemMessage),

    /// An echo of a user turn.
    User(crate::types::UserMessage),

    /// An assistant response turn.
    ///
    /// The `tool_uses` field is pre-extracted for convenience — it contains
    /// the same tool-use blocks that appear in `message.content`, but as
    /// [`ToolUse`] structs for easier pattern matching.
    Assistant {
        /// The full assistant message.
        message: crate::types::AssistantMessage,
        /// Convenience: tool-use requests extracted from the content blocks.
        tool_uses: Vec<ToolUse>,
    },

    /// The final result message (session complete).
    Result(crate::types::ResultMessage),

    /// A raw streaming event with assembled content.
    ///
    /// Only emitted when `include_partial_messages` is set in [`SessionOptions`].
    Stream {
        /// The raw streaming event from the API.
        raw: crate::types::StreamEvent,
        /// Events assembled by the [`StreamAssembler`].
        assembled: Vec<AssembledEvent>,
    },

    /// Rate limit status information.
    RateLimit(crate::types::RateLimitEvent),
}

impl Event {
    /// Returns `true` if this is a [`Event::Result`] (session complete).
    pub fn is_result(&self) -> bool {
        matches!(self, Event::Result(_))
    }

    /// Returns `true` if this is an [`Event::Assistant`] variant.
    pub fn is_assistant(&self) -> bool {
        matches!(self, Event::Assistant { .. })
    }

    /// If this is an [`Event::Assistant`] with tool-use requests, return them.
    pub fn tool_uses(&self) -> Option<&[ToolUse]> {
        match self {
            Event::Assistant { tool_uses, .. } if !tool_uses.is_empty() => Some(tool_uses),
            _ => None,
        }
    }

    /// If this is a [`Event::Result`], return a reference to the result message.
    pub fn as_result(&self) -> Option<&crate::types::ResultMessage> {
        match self {
            Event::Result(r) => Some(r),
            _ => None,
        }
    }
}

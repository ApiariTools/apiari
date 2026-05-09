//! High-level client for spawning and streaming gemini executions.
//!
//! [`GeminiClient`] is the main entry point. Configure it once, then call
//! [`exec`](GeminiClient::exec) to start an [`Execution`] that reads JSONL
//! events from the gemini subprocess.
//!
//! # Example
//!
//! ```rust,no_run
//! # use apiari_gemini_sdk::{GeminiClient, GeminiOptions, Event, Item};
//! # async fn example() -> apiari_gemini_sdk::error::Result<()> {
//! let client = GeminiClient::new();
//! let mut execution = client.exec("List files in the current directory", GeminiOptions {
//!     model: Some("o4-mini".into()),
//!     ephemeral: false,
//!     ..Default::default()
//! }).await?;
//!
//! while let Some(event) = execution.next_event().await? {
//!     if let Event::ItemCompleted { item: Item::AgentMessage { text, .. } } = &event {
//!         if let Some(text) = text {
//!             println!("{text}");
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use crate::error::{Result, SdkError};
use crate::options::{GeminiOptions, SessionOptions};
use crate::transport::ReadOnlyTransport;
use crate::types::Event;

/// Builder / factory for gemini executions.
///
/// Holds configuration that applies to every execution, such as the path
/// to the `gemini` binary.
#[derive(Debug, Clone)]
pub struct GeminiClient {
    /// Path to the `gemini` CLI binary.
    pub cli_path: String,
}

impl Default for GeminiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiClient {
    /// Create a new client that will look for `gemini` on `$PATH`.
    pub fn new() -> Self {
        Self {
            cli_path: "gemini".to_owned(),
        }
    }

    /// Create a new client with a custom path to the gemini CLI binary.
    pub fn with_cli_path(path: impl Into<String>) -> Self {
        Self {
            cli_path: path.into(),
        }
    }

    /// Start a new gemini execution with the given prompt and options.
    ///
    /// This spawns the `gemini exec --json` subprocess and returns an
    /// [`Execution`] handle for reading events.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`](crate::error::SdkError::ProcessSpawn)
    /// if the `gemini` binary cannot be found or started.
    pub async fn exec(&self, prompt: &str, opts: GeminiOptions) -> Result<Execution> {
        let args = opts.to_cli_args();
        let transport = ReadOnlyTransport::spawn(
            &self.cli_path,
            "exec",
            &args,
            Some(prompt),
            opts.working_dir.as_deref(),
        )?;

        Ok(Execution {
            transport,
            thread_id: None,
            finished: false,
        })
    }

    /// Resume a previous gemini execution.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`](crate::error::SdkError::ProcessSpawn)
    /// if the `gemini` binary cannot be found or started.
    pub async fn exec_resume(&self, prompt: &str, opts: SessionOptions) -> Result<Execution> {
        let args = opts.to_cli_args();
        let transport = ReadOnlyTransport::spawn(
            &self.cli_path,
            "exec",
            &args,
            Some(prompt),
            opts.working_dir.as_deref(),
        )?;

        Ok(Execution {
            transport,
            thread_id: None,
            finished: false,
        })
    }
}

/// A live execution of a `gemini exec --json` subprocess.
///
/// Provides a read-only event stream. The gemini process handles tool execution
/// internally — there is no stdin interaction.
pub struct Execution {
    transport: ReadOnlyTransport,
    thread_id: Option<String>,
    finished: bool,
}

impl Execution {
    /// Get the next event from the execution.
    ///
    /// Returns `Ok(None)` when the execution is complete (subprocess exited).
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
                self.finished = true;
                let (exit_code, stderr) = self.transport.wait_with_stderr().await?;
                if exit_code.unwrap_or(0) != 0 {
                    return Err(SdkError::ProcessDied {
                        exit_code,
                        stderr: stderr.unwrap_or_default(),
                    });
                }
                return Ok(None);
            };

            if value.get("type").is_none() {
                let output = value.as_object().ok_or_else(|| {
                    SdkError::ProtocolError(format!("unexpected gemini json output: {value}"))
                })?;
                let event = Event::JsonOutput {
                    session_id: output
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned),
                    response: output
                        .get("response")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned),
                    stats: output
                        .get("stats")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| {
                            SdkError::ProtocolError(format!("invalid gemini stats payload: {e}"))
                        })?,
                    error: output
                        .get("error")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| {
                            SdkError::ProtocolError(format!("invalid gemini error payload: {e}"))
                        })?,
                };

                if let Event::JsonOutput {
                    error: Some(error), ..
                } = &event
                {
                    let msg = error.message.as_deref().unwrap_or("gemini error");
                    return Err(SdkError::ProtocolError(msg.to_string()));
                }

                if let Event::JsonOutput {
                    session_id: Some(session_id),
                    ..
                } = &event
                {
                    self.thread_id = Some(session_id.clone());
                }

                return Ok(Some(event));
            }

            // Try to parse as a typed Event.
            let event: Event = match serde_json::from_value(value.clone()) {
                Ok(e) => e,
                Err(e) => {
                    // If we can't parse it, log and skip (forward compatibility).
                    tracing::warn!(
                        error = %e,
                        line = %value,
                        "skipping unrecognized event from gemini stdout"
                    );
                    continue;
                }
            };

            if matches!(event, Event::Unknown) {
                return Err(SdkError::ProtocolError(format!(
                    "unknown gemini event: {}",
                    value
                )));
            }

            // Track session id from the first resumable session event.
            match &event {
                Event::ThreadStarted { thread_id } => {
                    self.thread_id = Some(thread_id.clone());
                }
                Event::Init { session_id, .. } => {
                    self.thread_id = Some(session_id.clone());
                }
                _ => {}
            }

            return Ok(Some(event));
        }
    }

    /// Get the thread ID assigned by gemini, if a `thread.started` event has
    /// been received.
    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    /// Returns `true` if the execution has finished (subprocess exited or EOF).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Send an interrupt signal to the subprocess (SIGINT).
    ///
    /// This tells gemini to stop its current operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be sent.
    pub fn interrupt(&self) -> Result<()> {
        self.transport.interrupt()
    }

    /// Kill the subprocess immediately.
    pub async fn kill(mut self) -> Result<()> {
        self.transport.kill().await
    }

    /// Wait for the subprocess to exit and return the exit code and stderr.
    pub async fn wait(mut self) -> Result<(Option<i32>, Option<String>)> {
        self.transport.wait_with_stderr().await
    }
}

//! High-level client for spawning and streaming codex executions.
//!
//! [`CodexClient`] is the main entry point. Configure it once, then call
//! [`exec`](CodexClient::exec) to start an [`Execution`] that reads JSONL
//! events from the codex subprocess.
//!
//! # Example
//!
//! ```rust,no_run
//! # use apiari_codex_sdk::{CodexClient, ExecOptions, Event, Item};
//! # async fn example() -> apiari_codex_sdk::error::Result<()> {
//! let client = CodexClient::new();
//! let mut execution = client.exec("List files in the current directory", ExecOptions {
//!     model: Some("o4-mini".into()),
//!     full_auto: true,
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

use crate::error::Result;
use crate::options::{ExecOptions, ResumeOptions};
use crate::transport::ReadOnlyTransport;
use crate::types::Event;

/// Builder / factory for codex executions.
///
/// Holds configuration that applies to every execution, such as the path
/// to the `codex` binary.
#[derive(Debug, Clone)]
pub struct CodexClient {
    /// Path to the `codex` CLI binary.
    pub cli_path: String,
}

impl Default for CodexClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexClient {
    /// Create a new client that will look for `codex` on `$PATH`.
    pub fn new() -> Self {
        Self {
            cli_path: "codex".to_owned(),
        }
    }

    /// Create a new client with a custom path to the codex CLI binary.
    pub fn with_cli_path(path: impl Into<String>) -> Self {
        Self {
            cli_path: path.into(),
        }
    }

    /// Start a new codex execution with the given prompt and options.
    ///
    /// This spawns the `codex exec --json` subprocess and returns an
    /// [`Execution`] handle for reading events.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`](crate::error::SdkError::ProcessSpawn)
    /// if the `codex` binary cannot be found or started.
    pub async fn exec(&self, prompt: &str, opts: ExecOptions) -> Result<Execution> {
        let args = opts.to_cli_args();
        let transport = ReadOnlyTransport::spawn(
            &self.cli_path,
            "exec",
            &args,
            Some(prompt),
            opts.working_dir.as_deref(),
            &opts.env_vars,
        )?;

        Ok(Execution {
            transport,
            thread_id: None,
            finished: false,
        })
    }

    /// Resume a previous codex execution.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`](crate::error::SdkError::ProcessSpawn)
    /// if the `codex` binary cannot be found or started.
    pub async fn exec_resume(&self, prompt: &str, opts: ResumeOptions) -> Result<Execution> {
        let args = opts.to_cli_args();
        let transport = ReadOnlyTransport::spawn(
            &self.cli_path,
            "exec",
            &args,
            Some(prompt),
            opts.working_dir.as_deref(),
            &opts.env_vars,
        )?;

        Ok(Execution {
            transport,
            thread_id: None,
            finished: false,
        })
    }
}

/// A live execution of a `codex exec --json` subprocess.
///
/// Provides a read-only event stream. The codex process handles tool execution
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
                // EOF — process exited.
                self.finished = true;
                return Ok(None);
            };

            // Try to parse as a typed Event.
            let event: Event = match serde_json::from_value(value.clone()) {
                Ok(e) => e,
                Err(e) => {
                    // If we can't parse it, log and skip (forward compatibility).
                    tracing::warn!(
                        error = %e,
                        line = %value,
                        "skipping unrecognized event from codex stdout"
                    );
                    continue;
                }
            };

            // Track thread_id from the first ThreadStarted event.
            if let Event::ThreadStarted { ref thread_id } = event {
                self.thread_id = Some(thread_id.clone());
            }

            return Ok(Some(event));
        }
    }

    /// Get the thread ID assigned by codex, if a `thread.started` event has
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
    /// This tells codex to stop its current operation.
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

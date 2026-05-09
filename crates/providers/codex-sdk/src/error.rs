//! SDK error types.
//!
//! Provides [`SdkError`], the unified error type for all operations in this crate.

use std::io;

/// Unified error type for all SDK operations.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// Failed to spawn the `codex` subprocess.
    #[error("failed to spawn codex process: {0}")]
    ProcessSpawn(#[source] io::Error),

    /// The `codex` process exited unexpectedly.
    #[error("codex process died (exit code: {exit_code:?}, stderr: {stderr})")]
    ProcessDied {
        /// Exit code, if available.
        exit_code: Option<i32>,
        /// Captured stderr output.
        stderr: String,
    },

    /// A line from stdout was not valid JSON.
    #[error("invalid JSON from codex stdout: {message}")]
    InvalidJson {
        /// Human-readable description of the parse failure.
        message: String,
        /// The raw line that failed to parse.
        line: String,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// The JSON was valid but did not match any expected protocol shape.
    #[error("protocol error: {0}")]
    ProtocolError(String),

    /// An operation exceeded its deadline.
    #[error("operation timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// Generic I/O error (stdout read, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The execution has already finished.
    #[error("execution is not running")]
    NotRunning,
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, SdkError>;

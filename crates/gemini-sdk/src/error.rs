//! SDK error types.
//!
//! Provides [`SdkError`], the unified error type for all operations in this crate.

use std::io;

/// Unified error type for all SDK operations.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// Failed to spawn the `gemini` subprocess.
    #[error("failed to spawn gemini process: {0}")]
    ProcessSpawn(#[source] io::Error),

    /// The `gemini` process exited unexpectedly.
    #[error("gemini process died (exit code: {exit_code:?}, stderr: {stderr})")]
    ProcessDied {
        /// Exit code, if available.
        exit_code: Option<i32>,
        /// Captured stderr output.
        stderr: String,
    },

    /// A line from stdout was not valid JSON.
    #[error("invalid JSON from gemini stdout: {message}")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_spawn_error() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "gemini not found");
        let sdk_err = SdkError::ProcessSpawn(io_err);
        assert!(
            sdk_err
                .to_string()
                .contains("failed to spawn gemini process")
        );
    }

    #[test]
    fn test_process_died_error() {
        let sdk_err = SdkError::ProcessDied {
            exit_code: Some(1),
            stderr: "fatal error".to_string(),
        };
        assert!(sdk_err.to_string().contains("gemini process died"));
        assert!(sdk_err.to_string().contains("exit code: Some(1)"));
        assert!(sdk_err.to_string().contains("fatal error"));
    }

    #[test]
    fn test_invalid_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{invalid").unwrap_err();
        let sdk_err = SdkError::InvalidJson {
            message: "unexpected EOF".to_string(),
            line: "{invalid".to_string(),
            source: json_err,
        };
        assert!(
            sdk_err
                .to_string()
                .contains("invalid JSON from gemini stdout")
        );
    }

    #[test]
    fn test_protocol_error() {
        let sdk_err = SdkError::ProtocolError("unexpected event type".to_string());
        assert_eq!(sdk_err.to_string(), "protocol error: unexpected event type");
    }

    #[test]
    fn test_timeout_error() {
        let duration = std::time::Duration::from_secs(30);
        let sdk_err = SdkError::Timeout(duration);
        assert!(sdk_err.to_string().contains("timed out"));
        assert!(sdk_err.to_string().contains("30s"));
    }

    #[test]
    fn test_not_running_error() {
        let sdk_err = SdkError::NotRunning;
        assert_eq!(sdk_err.to_string(), "execution is not running");
    }
}

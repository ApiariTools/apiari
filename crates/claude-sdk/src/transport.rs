//! NDJSON stdin/stdout transport over a subprocess.
//!
//! [`Transport`] wraps a `tokio::process::Child` running the `claude` CLI and
//! provides line-oriented JSON I/O over its stdin and stdout pipes.

use crate::error::{Result, SdkError};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tracing::{debug, warn};

/// NDJSON transport wrapping a `claude` subprocess.
///
/// Each line written to stdin is a complete JSON object (no trailing newline
/// within the object). Each line read from stdout is likewise a single JSON
/// object.
pub struct Transport {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_reader: BufReader<ChildStdout>,
    /// Buffer reused across reads to avoid allocations.
    line_buf: String,
    /// Handle to the stderr reader task.
    stderr_task: Option<tokio::task::JoinHandle<String>>,
}

impl Transport {
    /// Spawn a new `claude` process with the given extra arguments.
    ///
    /// The process is launched with `--print --output-format stream-json
    /// --input-format stream-json --verbose` as base arguments. Any
    /// additional arguments in `extra_args` are appended.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`] if the process cannot be started.
    pub fn spawn(
        cli_path: &str,
        extra_args: &[String],
        working_dir: Option<&std::path::Path>,
        env_vars: &[(String, String)],
    ) -> Result<Self> {
        let mut cmd = Command::new(cli_path);

        // Base arguments for the stream-json protocol.
        cmd.args([
            "--print",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
        ]);

        // Caller-supplied arguments (session options, model, etc.).
        cmd.args(extra_args);

        // Clear Claude Code environment variables to allow the SDK to spawn
        // claude from within a Claude Code agent session. CLAUDECODE blocks
        // nested sessions, and CLAUDE_CODE_ENTRYPOINT can alter startup
        // behavior (e.g. waiting for IPC handshake instead of streaming).
        cmd.env_remove("CLAUDECODE");
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");

        // Working directory.
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Environment variables.
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        // Pipe all three standard streams.
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // On Unix, close inherited file descriptors > 2 and reset signal
        // dispositions in the child before exec. This prevents leaked FDs from
        // parent daemon processes from confusing the child.
        #[cfg(unix)]
        {
            unsafe {
                cmd.pre_exec(|| {
                    // Reset SIGPIPE to default (parent daemon may have SIG_IGN).
                    libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                    Ok(())
                });
            }
        }

        let mut child = cmd.spawn().map_err(SdkError::ProcessSpawn)?;

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .expect("stdout was configured as piped but is None");
        let stderr = child.stderr.take();

        let stdout_reader = BufReader::new(stdout);

        // Spawn a background task to drain stderr so it doesn't block.
        let stderr_task = stderr.map(|se| tokio::spawn(drain_stderr(se)));

        Ok(Self {
            child,
            stdin,
            stdout_reader,
            line_buf: String::with_capacity(4096),
            stderr_task,
        })
    }

    /// Write a serializable message as a single NDJSON line to stdin.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::NotConnected`] if stdin has been closed.
    /// Returns [`SdkError::Io`] on write failure, or an error from
    /// `serde_json` serialization (wrapped as [`SdkError::ProtocolError`]).
    pub async fn send(&mut self, msg: &impl Serialize) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or(SdkError::NotConnected)?;
        let json = serde_json::to_string(msg)
            .map_err(|e| SdkError::ProtocolError(format!("failed to serialize message: {e}")))?;
        debug!(json = %json, "-> stdin");
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Read the next NDJSON line from stdout and parse it as a JSON value.
    ///
    /// Returns `Ok(None)` when stdout reaches EOF (process exited).
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidJson`] if a line is not valid JSON.
    /// Returns [`SdkError::Io`] on read failure.
    pub async fn recv(&mut self) -> Result<Option<serde_json::Value>> {
        loop {
            self.line_buf.clear();
            let n = self.stdout_reader.read_line(&mut self.line_buf).await?;
            if n == 0 {
                return Ok(None); // EOF
            }

            let line = self.line_buf.trim();
            if line.is_empty() {
                // Skip blank lines and try the next one.
                continue;
            }

            debug!(line = %line, "stdout <-");

            return serde_json::from_str(line)
                .map(Some)
                .map_err(|e| SdkError::InvalidJson {
                    message: e.to_string(),
                    line: line.to_owned(),
                    source: e,
                });
        }
    }

    /// Read the next line from stdout and deserialize it as a typed [`Message`](crate::types::Message).
    ///
    /// Returns `Ok(None)` at EOF.
    pub async fn recv_message(&mut self) -> Result<Option<crate::types::Message>> {
        let Some(value) = self.recv().await? else {
            return Ok(None);
        };

        serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|e| SdkError::InvalidJson {
                message: format!("failed to parse as Message: {e}"),
                line: value.to_string(),
                source: e,
            })
    }

    /// Close the stdin pipe, signaling EOF to the subprocess.
    ///
    /// After this call, [`send`](Self::send) will return [`SdkError::NotConnected`].
    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// Kill the subprocess.
    ///
    /// This sends SIGKILL on Unix. If the process has already exited, this
    /// is a no-op. Stdin is closed before killing.
    pub async fn kill(&mut self) -> Result<()> {
        self.close_stdin();
        self.child.kill().await.map_err(SdkError::Io)
    }

    /// Wait for the subprocess to exit and return the exit code and captured stderr.
    pub async fn wait_with_stderr(&mut self) -> Result<(Option<i32>, Option<String>)> {
        let status = self.child.wait().await?;
        let stderr = if let Some(task) = self.stderr_task.take() {
            task.await.ok()
        } else {
            None
        };
        Ok((status.code(), stderr))
    }

    /// Check whether the child process has exited without blocking.
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child.try_wait().map_err(SdkError::Io)
    }

    /// Send an interrupt signal (SIGINT on Unix) to the subprocess.
    ///
    /// This mimics Ctrl-C and tells Claude to stop its current operation.
    #[cfg(unix)]
    pub fn interrupt(&self) -> Result<()> {
        if let Some(pid) = self.child.id() {
            // Safety: sending SIGINT to a known child PID.
            let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
            if ret != 0 {
                return Err(SdkError::Io(std::io::Error::last_os_error()));
            }
        }
        Ok(())
    }

    /// Send an interrupt signal on non-Unix platforms (not supported).
    #[cfg(not(unix))]
    pub fn interrupt(&self) -> Result<()> {
        Err(SdkError::ProtocolError(
            "interrupt is not supported on this platform".to_owned(),
        ))
    }
}

/// Background task that drains stderr line by line, logging each line,
/// and returns the accumulated output.
async fn drain_stderr(stderr: ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut buf = String::new();
    let mut accumulated = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                let line = buf.trim_end();
                if !line.is_empty() {
                    // Log via tracing AND eprintln so stderr is visible
                    // even when no tracing subscriber is configured (e.g. daemon).
                    eprintln!("[claude stderr] {}", line);
                    warn!(target: "claude_stderr", "{}", line);
                    accumulated.push_str(line);
                    accumulated.push('\n');
                }
            }
            Err(e) => {
                warn!(target: "claude_stderr", "error reading stderr: {}", e);
                break;
            }
        }
    }
    accumulated
}

//! Read-only NDJSON transport over a subprocess.
//!
//! [`ReadOnlyTransport`] wraps a `tokio::process::Child` running the `codex`
//! CLI and reads JSONL lines from its stdout. Unlike the Claude SDK transport,
//! stdin is `/dev/null` — codex exec is unidirectional.

use crate::error::{Result, SdkError};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tracing::{debug, warn};

/// Read-only NDJSON transport wrapping a `codex` subprocess.
///
/// Each line read from stdout is a single JSON object. There is no stdin
/// writing — the prompt is passed as a CLI argument.
pub struct ReadOnlyTransport {
    child: Child,
    stdout_reader: BufReader<ChildStdout>,
    /// Buffer reused across reads to avoid allocations.
    line_buf: String,
    /// Handle to the stderr reader task.
    stderr_task: Option<tokio::task::JoinHandle<String>>,
}

impl ReadOnlyTransport {
    /// Spawn a new `codex` process.
    ///
    /// The process is launched as `<cli_path> <subcommand> --json [extra_args...] [prompt]`.
    /// Stdin is `/dev/null` — there is no send method.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ProcessSpawn`] if the process cannot be started.
    pub fn spawn(
        cli_path: &str,
        subcommand: &str,
        extra_args: &[String],
        prompt: Option<&str>,
        working_dir: Option<&std::path::Path>,
        env_vars: &[(String, String)],
    ) -> Result<Self> {
        let mut cmd = Command::new(cli_path);

        // Subcommand (e.g. "exec").
        cmd.arg(subcommand);

        // Always request JSON output.
        cmd.arg("--json");

        // Caller-supplied arguments (model, sandbox, etc.).
        cmd.args(extra_args);

        // Prompt as the final positional argument.
        if let Some(prompt) = prompt {
            cmd.arg(prompt);
        }

        // Clear the CLAUDECODE environment variable to allow the SDK to spawn
        // codex from within a Claude Code agent session.
        cmd.env_remove("CLAUDECODE");

        // Working directory.
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Environment variables.
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        // stdin is null — codex exec is unidirectional.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(SdkError::ProcessSpawn)?;

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
            stdout_reader,
            line_buf: String::with_capacity(4096),
            stderr_task,
        })
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

    /// Send an interrupt signal (SIGINT on Unix) to the subprocess.
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

    /// Kill the subprocess immediately (SIGKILL on Unix).
    pub async fn kill(&mut self) -> Result<()> {
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
                    warn!(target: "codex_stderr", "{}", line);
                    accumulated.push_str(line);
                    accumulated.push('\n');
                }
            }
            Err(e) => {
                warn!(target: "codex_stderr", "error reading stderr: {}", e);
                break;
            }
        }
    }
    accumulated
}

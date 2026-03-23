//! Script watcher — runs arbitrary shell commands on a configurable interval
//! and emits signals based on the result.

use std::process::Stdio;

use async_trait::async_trait;
use color_eyre::Result;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::Watcher;
use crate::buzz::config::ScriptWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalUpdate};

/// Maximum bytes of stdout/stderr to capture per script execution.
const MAX_OUTPUT_BYTES: usize = 10 * 1024;

/// Watches by running a shell command and emitting signals based on output/exit code.
pub struct ScriptWatcher {
    config: ScriptWatcherConfig,
    /// Last stdout output, used for change detection when `emit_on_change` is true.
    last_output: Option<String>,
    /// Signal source string: `script_{name}`.
    source: String,
}

impl ScriptWatcher {
    pub fn new(config: ScriptWatcherConfig) -> Self {
        let source = format!("script_{}", config.name);
        Self {
            config,
            last_output: None,
            source,
        }
    }

    /// Expand `~` at the start of a path to the user's home directory.
    fn expand_tilde(path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/")
            && let Some(home) = dirs::home_dir()
        {
            return format!("{}/{}", home.display(), rest);
        }
        path.to_string()
    }

    /// Read up to `MAX_OUTPUT_BYTES` from an async reader into a String.
    async fn read_capped(
        reader: &mut (impl tokio::io::AsyncRead + Unpin),
    ) -> std::io::Result<String> {
        let mut buf = vec![0u8; MAX_OUTPUT_BYTES + 1];
        let mut total = 0;
        loop {
            let remaining = buf.len() - total;
            if remaining == 0 {
                break;
            }
            let n = reader.read(&mut buf[total..total + remaining]).await?;
            if n == 0 {
                break;
            }
            total += n;
            if total > MAX_OUTPUT_BYTES {
                total = MAX_OUTPUT_BYTES;
                break;
            }
        }
        buf.truncate(total);
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

#[async_trait]
impl Watcher for ScriptWatcher {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn signal_source(&self) -> &str {
        &self.source
    }

    /// Disable auto-reconciliation — script watchers intentionally skip emitting
    /// signals on unchanged polls, so the framework should not resolve prior signals.
    fn reconcile(
        &self,
        _source: &str,
        _poll_ids: &[String],
        _store: &SignalStore,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        let command = Self::expand_tilde(&self.config.command);
        debug!("script '{}': running command", self.config.name);

        let child_result = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        let mut child = match child_result {
            Ok(child) => child,
            Err(e) => {
                warn!("script '{}': failed to spawn: {e}", self.config.name);
                let signal = SignalUpdate::new(
                    &self.source,
                    format!("{}_error", self.source),
                    format!("Script '{}' failed to execute", self.config.name),
                    Severity::from_str_loose(&self.config.severity_on_fail),
                )
                .with_body(format!("Error: {e}"));
                return Ok(vec![signal]);
            }
        };

        // Take ownership of stdout/stderr pipes before waiting
        let mut stdout_pipe = child.stdout.take().expect("stdout piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr piped");

        let timeout = std::time::Duration::from_secs(self.config.timeout_secs);

        // Read stdout and stderr concurrently, capped at MAX_OUTPUT_BYTES each
        let io_future = async {
            let (stdout_res, stderr_res) = tokio::join!(
                Self::read_capped(&mut stdout_pipe),
                Self::read_capped(&mut stderr_pipe),
            );
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((stdout_res?, stderr_res?, status))
        };

        let (stdout_raw, stderr_raw, status) = match tokio::time::timeout(timeout, io_future).await
        {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                warn!("script '{}': I/O error: {e}", self.config.name);
                // kill_on_drop handles cleanup
                let signal = SignalUpdate::new(
                    &self.source,
                    format!("{}_error", self.source),
                    format!("Script '{}' failed", self.config.name),
                    Severity::from_str_loose(&self.config.severity_on_fail),
                )
                .with_body(format!("Error: {e}"));
                return Ok(vec![signal]);
            }
            Err(_) => {
                warn!(
                    "script '{}': timed out after {}s",
                    self.config.name, self.config.timeout_secs
                );
                // Explicitly kill and reap the child to avoid zombies.
                // kill_on_drop is a safety net, but explicit is better.
                let _ = child.kill().await;
                let _ = child.wait().await;
                let signal = SignalUpdate::new(
                    &self.source,
                    format!("{}_timeout", self.source),
                    format!(
                        "Script '{}' timed out ({}s)",
                        self.config.name, self.config.timeout_secs
                    ),
                    Severity::from_str_loose(&self.config.severity_on_fail),
                )
                .with_body("Script exceeded timeout limit");
                return Ok(vec![signal]);
            }
        };

        let stdout_trimmed = stdout_raw.trim().to_string();
        let stderr_trimmed = stderr_raw.trim().to_string();
        let exit_code = status.code().unwrap_or(-1);

        // Exit code != 0 → always emit with severity_on_fail
        if exit_code != 0 {
            let mut body = String::new();
            if !stdout_trimmed.is_empty() {
                body.push_str(&stdout_trimmed);
            }
            if !stderr_trimmed.is_empty() {
                if !body.is_empty() {
                    body.push_str("\n---\n");
                }
                body.push_str(&stderr_trimmed);
            }
            if body.is_empty() {
                body = format!("Exit code: {exit_code}");
            }

            let signal = SignalUpdate::new(
                &self.source,
                format!("{}_fail", self.source),
                format!("Script '{}' failed (exit {})", self.config.name, exit_code),
                Severity::from_str_loose(&self.config.severity_on_fail),
            )
            .with_body(body);

            self.last_output = Some(stdout_trimmed);

            info!(
                "script '{}': exit code {exit_code}, emitting failure signal",
                self.config.name
            );
            return Ok(vec![signal]);
        }

        // Exit code 0 — check emit_on_change
        if self.config.emit_on_change {
            let changed = match &self.last_output {
                None => {
                    // First poll — store output, don't emit
                    self.last_output = Some(stdout_trimmed);
                    return Ok(Vec::new());
                }
                Some(prev) => *prev != stdout_trimmed,
            };

            self.last_output = Some(stdout_trimmed.clone());

            if !changed {
                return Ok(Vec::new());
            }

            info!(
                "script '{}': output changed, emitting signal",
                self.config.name
            );
            let signal = SignalUpdate::new(
                &self.source,
                format!("{}_changed", self.source),
                format!("Script '{}' output changed", self.config.name),
                Severity::Info,
            )
            .with_body(stdout_trimmed);

            Ok(vec![signal])
        } else {
            // emit_on_change = false → always emit
            self.last_output = Some(stdout_trimmed.clone());

            if stdout_trimmed.is_empty() {
                return Ok(Vec::new());
            }

            let signal = SignalUpdate::new(
                &self.source,
                format!("{}_output", self.source),
                format!("Script '{}'", self.config.name),
                Severity::Info,
            )
            .with_body(stdout_trimmed);

            Ok(vec![signal])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ScriptWatcherConfig {
        ScriptWatcherConfig {
            name: "test-script".to_string(),
            command: "echo hello".to_string(),
            interval_secs: 60,
            emit_on_change: false,
            severity_on_fail: "warning".to_string(),
            timeout_secs: 30,
        }
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = ScriptWatcher::expand_tilde("~/scripts/test.sh");
        if dirs::home_dir().is_some() {
            assert!(!expanded.starts_with('~'));
            assert!(expanded.ends_with("/scripts/test.sh"));
        } else {
            // No home dir available (e.g. CI container) — returns unchanged
            assert_eq!(expanded, "~/scripts/test.sh");
        }
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        let path = "/usr/bin/test";
        assert_eq!(ScriptWatcher::expand_tilde(path), path);
    }

    #[test]
    fn test_new_sets_source() {
        let watcher = ScriptWatcher::new(test_config());
        assert_eq!(watcher.signal_source(), "script_test-script");
    }

    #[test]
    fn test_name() {
        let watcher = ScriptWatcher::new(test_config());
        assert_eq!(watcher.name(), "test-script");
    }

    #[tokio::test]
    async fn test_poll_echo_emit_always() {
        let config = test_config();
        let mut watcher = ScriptWatcher::new(config);
        let store = SignalStore::open_memory("test").unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, "script_test-script");
        assert!(signals[0].body.as_ref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_poll_emit_on_change_first_silent() {
        let mut config = test_config();
        config.emit_on_change = true;
        let mut watcher = ScriptWatcher::new(config);
        let store = SignalStore::open_memory("test").unwrap();

        // First poll stores output, doesn't emit
        let signals = watcher.poll(&store).await.unwrap();
        assert!(signals.is_empty());

        // Second poll with same output — no signal
        let signals = watcher.poll(&store).await.unwrap();
        assert!(signals.is_empty());
    }

    #[tokio::test]
    async fn test_poll_failure_exit_code() {
        let mut config = test_config();
        config.command = "exit 1".to_string();
        let mut watcher = ScriptWatcher::new(config);
        let store = SignalStore::open_memory("test").unwrap();

        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].severity, Severity::Warning);
        assert!(signals[0].title.contains("failed"));
    }

    #[tokio::test]
    async fn test_poll_timeout() {
        let mut config = test_config();
        config.command = "sleep 10".to_string();
        config.timeout_secs = 1;
        let mut watcher = ScriptWatcher::new(config);
        let store = SignalStore::open_memory("test").unwrap();

        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("timed out"));
    }

    #[tokio::test]
    async fn test_read_capped_limits_output() {
        // Generate output larger than MAX_OUTPUT_BYTES
        let big_cmd = format!("python3 -c \"print('x' * {})\"", MAX_OUTPUT_BYTES + 5000);
        let mut config = test_config();
        config.command = big_cmd;
        let mut watcher = ScriptWatcher::new(config);
        let store = SignalStore::open_memory("test").unwrap();

        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        let body = signals[0].body.as_ref().unwrap();
        assert!(body.len() <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn test_reconcile_returns_zero() {
        let watcher = ScriptWatcher::new(test_config());
        let store = SignalStore::open_memory("test").unwrap();
        let result = watcher
            .reconcile("script_test-script", &[], &store)
            .unwrap();
        assert_eq!(result, 0);
    }
}

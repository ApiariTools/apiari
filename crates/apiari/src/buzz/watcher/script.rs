//! Script watcher — runs arbitrary shell commands on a configurable interval
//! and emits signals based on the result.

use async_trait::async_trait;
use color_eyre::Result;
use tokio::process::Command;
use tracing::{info, warn};

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

    /// Truncate output to MAX_OUTPUT_BYTES, preserving valid UTF-8.
    fn truncate_output(s: &str) -> &str {
        if s.len() <= MAX_OUTPUT_BYTES {
            return s;
        }
        // Find a valid char boundary at or before MAX_OUTPUT_BYTES
        let mut end = MAX_OUTPUT_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
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

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        let command = Self::expand_tilde(&self.config.command);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.timeout_secs),
            Command::new("sh").arg("-c").arg(&command).output(),
        )
        .await;

        let output = match result {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                warn!("script '{}': failed to execute: {e}", self.config.name);
                let signal = SignalUpdate::new(
                    &self.source,
                    format!("{}_error", self.source),
                    format!("Script '{}' failed to execute", self.config.name),
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

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout_trimmed = Self::truncate_output(stdout.trim());
        let stderr_trimmed = Self::truncate_output(stderr.trim());
        let exit_code = output.status.code().unwrap_or(-1);

        // Exit code != 0 → always emit with severity_on_fail
        if exit_code != 0 {
            let mut body = String::new();
            if !stdout_trimmed.is_empty() {
                body.push_str(stdout_trimmed);
            }
            if !stderr_trimmed.is_empty() {
                if !body.is_empty() {
                    body.push_str("\n---\n");
                }
                body.push_str(stderr_trimmed);
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

            // Update last_output even on failure
            self.last_output = Some(stdout_trimmed.to_string());

            info!(
                "script '{}': exit code {exit_code}, emitting failure signal",
                self.config.name
            );
            return Ok(vec![signal]);
        }

        // Exit code 0 — check emit_on_change
        let current_output = stdout_trimmed.to_string();

        if self.config.emit_on_change {
            let changed = match &self.last_output {
                None => {
                    // First poll — store output, don't emit
                    self.last_output = Some(current_output);
                    return Ok(Vec::new());
                }
                Some(prev) => prev != &current_output,
            };

            self.last_output = Some(current_output.clone());

            if !changed {
                // Silent heartbeat — no signal
                return Ok(Vec::new());
            }

            // Output changed → emit info signal
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
            .with_body(current_output);

            Ok(vec![signal])
        } else {
            // emit_on_change = false → always emit
            self.last_output = Some(current_output.clone());

            if current_output.is_empty() {
                return Ok(Vec::new());
            }

            let signal = SignalUpdate::new(
                &self.source,
                format!("{}_output", self.source),
                format!("Script '{}'", self.config.name),
                Severity::Info,
            )
            .with_body(current_output);

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
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("/scripts/test.sh"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        let path = "/usr/bin/test";
        assert_eq!(ScriptWatcher::expand_tilde(path), path);
    }

    #[test]
    fn test_truncate_output_short() {
        let s = "hello world";
        assert_eq!(ScriptWatcher::truncate_output(s), s);
    }

    #[test]
    fn test_truncate_output_long() {
        let s = "a".repeat(20_000);
        let truncated = ScriptWatcher::truncate_output(&s);
        assert_eq!(truncated.len(), MAX_OUTPUT_BYTES);
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
}

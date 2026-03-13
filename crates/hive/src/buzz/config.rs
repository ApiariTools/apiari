//! Configuration for buzz, loaded from TOML.
//!
//! # Configuration file format
//!
//! Buzz looks for `.buzz/config.toml` by default (override with `--config`).
//! All sections are optional; buzz runs with sensible defaults if the file
//! is missing or empty.
//!
//! ```toml
//! # How often to poll sources, in seconds (default: 60).
//! poll_interval_secs = 60
//!
//! # Output configuration.
//! [output]
//! mode = "stdout"          # "stdout" | "file" | "webhook"
//! # path = ".buzz/signals.jsonl"  # required for mode = "file"
//! # url  = "https://..."          # required for mode = "webhook"
//!
//! # Sentry watcher (optional — omit entire section to disable).
//! [sentry]
//! token   = "sntrys_..."   # Sentry auth token (required)
//! org     = "my-org"       # Sentry organization slug (required)
//! project = "my-project"   # Sentry project slug (required)
//!
//! # GitHub watcher (optional — omit entire section to disable).
//! # Requires `gh` CLI installed and authenticated (`gh auth login`).
//! [github]
//! repos = ["owner/repo1", "owner/repo2"]
//! watch_labels = ["critical", "P0", "incident"]  # optional
//!
//! # Webhook receiver (optional — omit entire section to disable).
//! # NOTE: not yet implemented; currently a stub.
//! [webhook]
//! port = 8088  # default: 8088
//!
//! # Scheduled reminders (optional, repeatable).
//! [[reminders]]
//! message       = "Daily standup"
//! interval_secs = 86400  # 24 hours
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level buzz configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuzzConfig {
    /// How often to poll sources, in seconds (default: 60).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,

    /// Output mode configuration (operational, not persisted to workspace.yaml).
    #[serde(default, skip_serializing)]
    pub output: OutputConfig,

    /// Sentry watcher configuration (optional — omit to disable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentry: Option<SentryConfig>,

    /// GitHub watcher configuration (optional — omit to disable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubConfig>,

    /// Webhook receiver configuration (optional — omit to disable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<WebhookConfig>,

    /// Scheduled reminders (optional, repeatable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminders: Vec<ReminderConfig>,
}

/// Sentry watcher configuration.
///
/// All three fields (token, org, project) are required when the `[sentry]`
/// section is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentryConfig {
    /// Sentry auth token (e.g. `sntrys_...`). Required.
    pub token: String,
    /// Sentry organization slug. Required.
    pub org: String,
    /// Sentry project slug. Required.
    pub project: String,
    /// Periodic sweep configuration (optional — omit to disable sweep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep: Option<SweepConfig>,
}

/// Configuration for periodic sweep of all unresolved Sentry issues.
///
/// The sweep re-evaluates all known issues on a cron schedule, surfacing
/// those that deserve attention (new, spiking, stale, severity changed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepConfig {
    /// Cron expression for when to run the sweep (e.g. "0 9 * * 1-5").
    pub schedule: String,
    /// Re-triage when event count >= ratio * count_at_last_triage (default: 2.0).
    #[serde(default = "default_spike_ratio")]
    pub event_spike_ratio: f64,
    /// Re-triage when issue has been unresolved for this many days (default: 3).
    #[serde(default = "default_stale_days")]
    pub stale_days: u64,
    /// Max issues to fetch per sweep (default: 50).
    #[serde(default = "default_max_sweep_issues")]
    pub max_issues: u32,
}

/// GitHub watcher configuration.
///
/// Requires the `gh` CLI to be installed and authenticated via `gh auth login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubConfig {
    /// List of repositories to watch, in "owner/repo" format.
    ///
    /// Example: `repos = ["myorg/backend", "myorg/frontend"]`
    pub repos: Vec<String>,

    /// Labels to watch for across all repos. Issues/PRs with any of these
    /// labels will generate signals even if they are not assigned to you.
    ///
    /// Example: `watch_labels = ["critical", "P0", "incident"]`
    #[serde(default)]
    pub watch_labels: Vec<String>,
}

/// Webhook receiver configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Port to listen on for incoming webhooks (default: 8088).
    #[serde(default = "default_webhook_port")]
    pub port: u16,
}

/// Configuration for a scheduled reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderConfig {
    /// Human-readable message for the reminder.
    pub message: String,
    /// Interval in seconds between reminder firings.
    pub interval_secs: u64,
}

/// Output configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Output mode: "stdout" (default), "file", or "webhook".
    #[serde(default = "default_output_mode")]
    pub mode: String,
    /// Path for file output mode. Defaults to `.buzz/signals.jsonl`.
    pub path: Option<PathBuf>,
    /// URL for webhook output mode. Required when mode = "webhook".
    pub url: Option<String>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mode: "stdout".to_string(),
            path: None,
            url: None,
        }
    }
}

impl Default for BuzzConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            output: OutputConfig::default(),
            sentry: None,
            github: None,
            webhook: None,
            reminders: Vec::new(),
        }
    }
}

impl BuzzConfig {
    /// Load configuration from a TOML file.
    ///
    /// - If the file does not exist, returns sensible defaults (no watchers
    ///   enabled, stdout output, 60s polling interval).
    /// - After loading, prints warnings for any configuration issues (e.g.
    ///   empty Sentry token, empty repo list) but does not fail.
    pub fn load(path: &std::path::Path) -> color_eyre::Result<Self> {
        let config = if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            let config: BuzzConfig = toml::from_str(&contents)?;
            tracing::info!(path = %path.display(), "loaded config from file");
            config
        } else {
            tracing::info!(path = %path.display(), "config file not found, using defaults");
            Self::default()
        };

        config.validate();
        Ok(config)
    }

    /// Resolve buzz configuration using the priority chain:
    /// 1. Explicit `--config <path>` CLI flag (if provided and exists)
    /// 2. `workspace.yaml` → `buzz:` section
    /// 3. `.buzz/config.toml` (legacy fallback)
    /// 4. Defaults (no watchers)
    pub fn resolve(
        cli_config_path: Option<&Path>,
        workspace: &crate::workspace::Workspace,
    ) -> color_eyre::Result<Self> {
        // 1. Explicit CLI path.
        if let Some(path) = cli_config_path
            && path.exists()
        {
            return Self::load(path);
        }

        // 2. workspace.yaml buzz section.
        if let Some(buzz) = &workspace.buzz {
            tracing::info!("loaded config from workspace.yaml");
            let config = buzz.clone();
            config.validate();
            return Ok(config);
        }

        // 3. Legacy .buzz/config.toml fallback.
        let legacy_path = workspace.root.join(".buzz/config.toml");
        Self::load(&legacy_path)
    }

    /// Print warnings for common configuration issues.
    /// Does not return errors — the tool should still run with partial config.
    pub fn validate(&self) {
        if let Some(sentry) = &self.sentry {
            if sentry.token.is_empty() {
                tracing::warn!("sentry.token is empty — Sentry API calls will fail");
            }
            if sentry.org.is_empty() {
                tracing::warn!("sentry.org is empty");
            }
            if sentry.project.is_empty() {
                tracing::warn!("sentry.project is empty");
            }
            if let Some(sweep) = &sentry.sweep
                && let Err(e) = sweep.schedule.parse::<croner::Cron>()
            {
                tracing::warn!(error = %e, "sentry.sweep.schedule is invalid cron");
            }
        }

        if let Some(github) = &self.github
            && github.repos.is_empty()
        {
            tracing::warn!("github.repos is empty — no repositories to watch");
        }

        if self.output.mode == "webhook" && self.output.url.is_none() {
            tracing::warn!("output.mode is 'webhook' but output.url is not set");
        }

        if self.output.mode == "file" && self.output.path.is_none() {
            tracing::info!(
                "output.mode is 'file' but output.path is not set, will use .buzz/signals.jsonl"
            );
        }

        if self.poll_interval_secs == 0 {
            tracing::warn!("poll_interval_secs is 0, this will poll as fast as possible");
        }

        let num_sources = self.sentry.is_some() as u8
            + self.github.is_some() as u8
            + self.webhook.is_some() as u8
            + (!self.reminders.is_empty()) as u8;

        if num_sources == 0 {
            tracing::info!("no watchers or reminders configured — nothing to poll");
        }
    }
}

fn default_poll_interval() -> u64 {
    60
}

fn default_output_mode() -> String {
    "stdout".to_string()
}

fn default_webhook_port() -> u16 {
    8088
}

fn default_spike_ratio() -> f64 {
    2.0
}

fn default_stale_days() -> u64 {
    3
}

fn default_max_sweep_issues() -> u32 {
    50
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_full_config_with_sentry_and_github() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(
            &path,
            r#"
poll_interval_secs = 120

[output]
mode = "file"
path = ".buzz/signals.jsonl"

[sentry]
token = "sntrys_test_token"
org = "my-org"
project = "my-project"

[github]
repos = ["owner/repo1", "owner/repo2"]
watch_labels = ["critical", "P0"]

[[reminders]]
message = "Daily standup"
interval_secs = 86400
"#,
        )
        .unwrap();

        let config = BuzzConfig::load(&path).unwrap();
        assert_eq!(config.poll_interval_secs, 120);
        assert_eq!(config.output.mode, "file");
        assert_eq!(
            config.output.path.as_deref(),
            Some(std::path::Path::new(".buzz/signals.jsonl"))
        );

        let sentry = config.sentry.unwrap();
        assert_eq!(sentry.token, "sntrys_test_token");
        assert_eq!(sentry.org, "my-org");
        assert_eq!(sentry.project, "my-project");

        let github = config.github.unwrap();
        assert_eq!(github.repos, vec!["owner/repo1", "owner/repo2"]);
        assert_eq!(github.watch_labels, vec!["critical", "P0"]);

        assert_eq!(config.reminders.len(), 1);
        assert_eq!(config.reminders[0].message, "Daily standup");
        assert_eq!(config.reminders[0].interval_secs, 86400);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let config = BuzzConfig::load(&path).unwrap();
        assert_eq!(config.poll_interval_secs, 60);
        assert_eq!(config.output.mode, "stdout");
        assert!(config.sentry.is_none());
        assert!(config.github.is_none());
        assert!(config.webhook.is_none());
        assert!(config.reminders.is_empty());
    }

    #[test]
    fn load_empty_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").unwrap();

        let config = BuzzConfig::load(&path).unwrap();
        assert_eq!(config.poll_interval_secs, 60);
        assert!(config.sentry.is_none());
    }

    #[test]
    fn validate_does_not_panic_with_empty_sentry_token() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(
            &path,
            r#"
[sentry]
token = ""
org = ""
project = ""
"#,
        )
        .unwrap();

        // Should not panic — just prints warnings to stderr.
        let config = BuzzConfig::load(&path).unwrap();
        assert!(config.sentry.is_some());
        assert!(config.sentry.unwrap().token.is_empty());
    }

    #[test]
    fn validate_does_not_panic_with_empty_github_repos() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(
            &path,
            r#"
[github]
repos = []
"#,
        )
        .unwrap();

        let config = BuzzConfig::load(&path).unwrap();
        assert!(config.github.unwrap().repos.is_empty());
    }

    #[test]
    fn validate_does_not_panic_with_zero_poll_interval() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "poll_interval_secs = 0").unwrap();

        let config = BuzzConfig::load(&path).unwrap();
        assert_eq!(config.poll_interval_secs, 0);
    }

    #[test]
    fn webhook_config_default_port() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[webhook]\n").unwrap();

        let config = BuzzConfig::load(&path).unwrap();
        let webhook = config.webhook.unwrap();
        assert_eq!(webhook.port, 8088);
    }

    // ---- resolve() tests ----

    #[test]
    fn resolve_prefers_cli_config_when_exists() {
        let dir = TempDir::new().unwrap();
        let cli_path = dir.path().join("explicit.toml");
        std::fs::write(&cli_path, "poll_interval_secs = 999").unwrap();

        let ws = crate::workspace::Workspace {
            buzz: Some(BuzzConfig {
                poll_interval_secs: 111,
                ..BuzzConfig::default()
            }),
            root: dir.path().to_path_buf(),
            ..crate::workspace::Workspace::default()
        };

        let config = BuzzConfig::resolve(Some(&cli_path), &ws).unwrap();
        assert_eq!(config.poll_interval_secs, 999);
    }

    #[test]
    fn resolve_falls_back_to_workspace_buzz() {
        let dir = TempDir::new().unwrap();

        let ws = crate::workspace::Workspace {
            buzz: Some(BuzzConfig {
                poll_interval_secs: 222,
                sentry: Some(SentryConfig {
                    token: "tok".into(),
                    org: "org".into(),
                    project: "proj".into(),
                    sweep: None,
                }),
                ..BuzzConfig::default()
            }),
            root: dir.path().to_path_buf(),
            ..crate::workspace::Workspace::default()
        };

        // No CLI path, no .buzz/config.toml — should use workspace.buzz.
        let config = BuzzConfig::resolve(None, &ws).unwrap();
        assert_eq!(config.poll_interval_secs, 222);
        assert!(config.sentry.is_some());
    }

    #[test]
    fn resolve_falls_back_to_legacy_config_toml() {
        let dir = TempDir::new().unwrap();
        let buzz_dir = dir.path().join(".buzz");
        std::fs::create_dir_all(&buzz_dir).unwrap();
        std::fs::write(buzz_dir.join("config.toml"), "poll_interval_secs = 333").unwrap();

        let ws = crate::workspace::Workspace {
            buzz: None, // no workspace buzz section
            root: dir.path().to_path_buf(),
            ..crate::workspace::Workspace::default()
        };

        let config = BuzzConfig::resolve(None, &ws).unwrap();
        assert_eq!(config.poll_interval_secs, 333);
    }

    #[test]
    fn resolve_returns_defaults_when_nothing_configured() {
        let dir = TempDir::new().unwrap();

        let ws = crate::workspace::Workspace {
            buzz: None,
            root: dir.path().to_path_buf(),
            ..crate::workspace::Workspace::default()
        };

        let config = BuzzConfig::resolve(None, &ws).unwrap();
        assert_eq!(config.poll_interval_secs, 60);
        assert!(config.sentry.is_none());
    }

    #[test]
    fn resolve_skips_missing_cli_path_and_uses_workspace() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist.toml");

        let ws = crate::workspace::Workspace {
            buzz: Some(BuzzConfig {
                poll_interval_secs: 444,
                ..BuzzConfig::default()
            }),
            root: dir.path().to_path_buf(),
            ..crate::workspace::Workspace::default()
        };

        let config = BuzzConfig::resolve(Some(&missing), &ws).unwrap();
        assert_eq!(config.poll_interval_secs, 444);
    }
}

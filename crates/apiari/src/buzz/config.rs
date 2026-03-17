//! Configuration types for buzz watchers, coordinator, and telegram.
//!
//! The caller (cli crate) is responsible for loading and providing config.

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Top-level buzz configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuzzConfig {
    /// Telegram bot configuration.
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,

    /// Watcher configurations.
    #[serde(default)]
    pub watchers: WatchersConfig,

    /// Coordinator configuration.
    #[serde(default)]
    pub coordinator: CoordinatorConfig,

    /// Morning brief configuration.
    #[serde(default)]
    pub morning_brief: Option<MorningBriefConfig>,
}

/// Telegram configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Bot token from @BotFather.
    pub bot_token: String,
    /// Chat ID to send unprompted notifications to.
    pub chat_id: i64,
    /// Forum topic ID (optional).
    #[serde(default)]
    pub topic_id: Option<i64>,
    /// Allowed user IDs (empty = allow all).
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
}

/// Watcher configurations (all optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchersConfig {
    /// GitHub watcher.
    #[serde(default)]
    pub github: Option<GithubWatcherConfig>,
    /// Sentry watcher.
    #[serde(default)]
    pub sentry: Option<SentryWatcherConfig>,
    /// Swarm watcher.
    #[serde(default)]
    pub swarm: Option<SwarmWatcherConfig>,
    /// Email watchers (multiple mailboxes).
    #[serde(default)]
    pub email: Vec<EmailMailboxConfig>,
}

/// GitHub watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubWatcherConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_watcher_interval")]
    pub interval_secs: u64,
    pub repos: Vec<String>,
    #[serde(default)]
    pub watch_labels: Vec<String>,
    /// Named priority queries for the review queue.
    #[serde(default)]
    pub review_queue: Vec<ReviewQueueEntry>,
}

/// A named review queue query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewQueueEntry {
    pub name: String,
    pub query: String,
}

/// Sentry watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentryWatcherConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_watcher_interval")]
    pub interval_secs: u64,
    pub org: String,
    pub project: String,
    pub token: String,
}

/// Swarm watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmWatcherConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_swarm_interval")]
    pub interval_secs: u64,
    pub state_path: std::path::PathBuf,
}

/// Email mailbox configuration (one per IMAP account).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMailboxConfig {
    /// Name used in source: "{name}_email_review_queue".
    pub name: String,
    /// IMAP server hostname.
    pub host: String,
    /// IMAP server port (typically 993 for TLS).
    #[serde(default = "default_imap_port")]
    pub port: u16,
    /// Use TLS.
    #[serde(default = "default_true")]
    pub tls: bool,
    /// IMAP username.
    pub username: String,
    /// IMAP password (app-specific password).
    pub password: String,
    /// IMAP folder to watch.
    #[serde(default = "default_folder")]
    pub folder: String,
    /// IMAP search criteria.
    #[serde(default = "default_filter")]
    pub filter: String,
    /// Include body preview in signal (default false for privacy).
    #[serde(default)]
    pub include_body: bool,
    /// Max emails per poll.
    #[serde(default = "default_max_fetch")]
    pub max_fetch: u32,
    /// Poll interval in seconds.
    #[serde(default = "default_email_interval")]
    pub interval_secs: u64,
    /// Optional Ollama/OpenAI-compatible summarizer.
    #[serde(default)]
    pub summarizer: Option<EmailSummarizerConfig>,
}

/// Ollama/OpenAI-compatible summarizer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummarizerConfig {
    /// Base URL (e.g. "http://localhost:11434").
    pub base_url: String,
    /// Model name (e.g. "llama3.2:3b").
    pub model: String,
}

fn default_imap_port() -> u16 {
    993
}

fn default_folder() -> String {
    "INBOX".to_string()
}

fn default_filter() -> String {
    "UNSEEN".to_string()
}

fn default_max_fetch() -> u32 {
    20
}

fn default_email_interval() -> u64 {
    300
}

/// Morning brief configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MorningBriefConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Time to send the brief in HH:MM format (24h).
    pub time: String,
    /// IANA timezone name (e.g. "America/Chicago").
    pub timezone: String,
}

/// Coordinator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            max_turns: default_max_turns(),
        }
    }
}

impl BuzzConfig {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            color_eyre::eyre::bail!("Config file not found: {}", path.display());
        }
        let contents = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let config: BuzzConfig = toml::from_str(&contents)
            .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }
}

fn default_true() -> bool {
    true
}

fn default_watcher_interval() -> u64 {
    120
}

fn default_swarm_interval() -> u64 {
    15
}

fn default_model() -> String {
    "sonnet".to_string()
}

fn default_max_turns() -> u32 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BuzzConfig::default();
        assert!(config.telegram.is_none());
        assert!(config.watchers.github.is_none());
        assert!(config.watchers.sentry.is_none());
        assert!(config.watchers.swarm.is_none());
        assert_eq!(config.coordinator.model, "sonnet");
        assert_eq!(config.coordinator.max_turns, 20);
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[telegram]
bot_token = "test-token"
chat_id = -123456

[watchers.github]
enabled = true
interval_secs = 60
repos = ["org/repo1"]
watch_labels = ["bug"]

[watchers.sentry]
enabled = true
interval_secs = 90
org = "test-org"
project = "test-proj"
token = "sntrys_test"

[watchers.swarm]
enabled = true
interval_secs = 10
state_path = "/tmp/state.json"

[coordinator]
model = "opus"
max_turns = 30
"#;
        let config: BuzzConfig = toml::from_str(toml_str).unwrap();

        let tg = config.telegram.unwrap();
        assert_eq!(tg.bot_token, "test-token");
        assert_eq!(tg.chat_id, -123456);

        let gh = config.watchers.github.unwrap();
        assert!(gh.enabled);
        assert_eq!(gh.interval_secs, 60);
        assert_eq!(gh.repos, vec!["org/repo1"]);

        let sentry = config.watchers.sentry.unwrap();
        assert_eq!(sentry.org, "test-org");

        let swarm = config.watchers.swarm.unwrap();
        assert_eq!(swarm.interval_secs, 10);

        assert_eq!(config.coordinator.model, "opus");
        assert_eq!(config.coordinator.max_turns, 30);
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml_str = r#"
[coordinator]
model = "haiku"
"#;
        let config: BuzzConfig = toml::from_str(toml_str).unwrap();
        assert!(config.telegram.is_none());
        assert!(config.watchers.github.is_none());
        assert_eq!(config.coordinator.model, "haiku");
        assert_eq!(config.coordinator.max_turns, 20); // default
    }

    #[test]
    fn test_load_missing_file_errors() {
        let result = BuzzConfig::load(Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
    }
}

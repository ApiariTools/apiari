//! Workspace configuration for the apiari CLI.
//!
//! Each workspace is a self-contained TOML file at `~/.config/apiari/workspaces/{name}.toml`.

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Root directory for all apiari config.
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".config/apiari")
}

/// Directory containing workspace TOML files.
pub fn workspaces_dir() -> PathBuf {
    config_dir().join("workspaces")
}

/// Path to the shared SQLite database.
pub fn db_path() -> PathBuf {
    config_dir().join("apiari.db")
}

/// Path to the daemon PID file.
pub fn pid_path() -> PathBuf {
    config_dir().join("daemon.pid")
}

/// Path to the daemon log file.
pub fn log_path() -> PathBuf {
    config_dir().join("daemon.log")
}

/// A fully self-contained workspace configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Absolute path to the workspace root.
    pub root: PathBuf,

    /// Repository slugs (e.g. ["ApiariTools/swarm"]).
    #[serde(default)]
    pub repos: Vec<String>,

    /// Telegram configuration.
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,

    /// Coordinator configuration.
    #[serde(default)]
    pub coordinator: CoordinatorConfig,

    /// Watcher configurations.
    #[serde(default)]
    pub watchers: WatchersConfig,

    /// Notification pipeline configuration.
    #[serde(default)]
    pub pipeline: PipelineConfig,
}

/// Telegram bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: i64,
    #[serde(default)]
    pub topic_id: Option<i64>,
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
}

/// Coordinator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorConfig {
    #[serde(default = "default_coordinator_name")]
    pub name: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            name: default_coordinator_name(),
            model: default_model(),
            max_turns: default_max_turns(),
        }
    }
}

/// Watcher configurations (all optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchersConfig {
    #[serde(default)]
    pub github: Option<GithubWatcherConfig>,
    #[serde(default)]
    pub sentry: Option<SentryWatcherConfig>,
    #[serde(default)]
    pub swarm: Option<SwarmWatcherConfig>,
}

/// GitHub watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubWatcherConfig {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default = "default_watcher_interval")]
    pub interval_secs: u64,
}

/// Sentry watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentryWatcherConfig {
    pub org: String,
    pub project: String,
    pub token: String,
    #[serde(default = "default_watcher_interval")]
    pub interval_secs: u64,
}

/// Swarm watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmWatcherConfig {
    pub state_path: PathBuf,
    #[serde(default = "default_swarm_interval")]
    pub interval_secs: u64,
}

/// A loaded workspace with its name.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub name: String,
    pub config: WorkspaceConfig,
}

/// Load a single workspace config from a TOML file.
pub fn load_workspace(path: &Path) -> Result<WorkspaceConfig> {
    let contents = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let config: WorkspaceConfig = toml::from_str(&contents)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    Ok(config)
}

/// Discover all workspace configs from `~/.config/apiari/workspaces/*.toml`.
pub fn discover_workspaces() -> Result<Vec<Workspace>> {
    let dir = workspaces_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut workspaces = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .wrap_err_with(|| format!("failed to read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        match load_workspace(&path) {
            Ok(config) => workspaces.push(Workspace { name, config }),
            Err(e) => {
                tracing::warn!("skipping {}: {e}", path.display());
            }
        }
    }

    Ok(workspaces)
}

/// Convert a WorkspaceConfig into a buzz BuzzConfig for watcher/coordinator use.
pub fn to_buzz_config(ws: &WorkspaceConfig) -> buzz::config::BuzzConfig {
    buzz::config::BuzzConfig {
        telegram: ws.telegram.as_ref().map(|t| buzz::config::TelegramConfig {
            bot_token: t.bot_token.clone(),
            chat_id: t.chat_id,
            topic_id: t.topic_id,
            allowed_user_ids: t.allowed_user_ids.clone(),
        }),
        watchers: buzz::config::WatchersConfig {
            github: ws
                .watchers
                .github
                .as_ref()
                .map(|g| buzz::config::GithubWatcherConfig {
                    enabled: true,
                    interval_secs: g.interval_secs,
                    repos: g.repos.clone(),
                    watch_labels: vec![],
                }),
            sentry: ws
                .watchers
                .sentry
                .as_ref()
                .map(|s| buzz::config::SentryWatcherConfig {
                    enabled: true,
                    interval_secs: s.interval_secs,
                    org: s.org.clone(),
                    project: s.project.clone(),
                    token: s.token.clone(),
                }),
            swarm: ws
                .watchers
                .swarm
                .as_ref()
                .map(|s| buzz::config::SwarmWatcherConfig {
                    enabled: true,
                    interval_secs: s.interval_secs,
                    state_path: s.state_path.clone(),
                }),
        },
        coordinator: buzz::config::CoordinatorConfig {
            model: ws.coordinator.model.clone(),
            max_turns: ws.coordinator.max_turns,
        },
    }
}

/// Notification pipeline configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Batch flush window in seconds (default: 60).
    #[serde(default = "default_batch_window")]
    pub batch_window_secs: u64,
    /// Pipeline rules. Empty = use defaults.
    #[serde(default)]
    pub rules: Vec<PipelineRuleConfig>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            batch_window_secs: default_batch_window(),
            rules: vec![],
        }
    }
}

/// A single pipeline rule in config TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRuleConfig {
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub id_prefix: Option<String>,
    pub action: String,
    #[serde(default)]
    pub rate_limit_secs: u64,
}

/// Convert pipeline config rules into buzz pipeline rules.
pub fn to_pipeline_rules(config: &PipelineConfig) -> Vec<buzz::pipeline::rule::PipelineRule> {
    use buzz::pipeline::rule::{PipelineAction, PipelineRule};
    use buzz::signal::Severity;

    config
        .rules
        .iter()
        .map(|r| PipelineRule {
            name: r.name.clone(),
            source: r.source.clone(),
            severity: r.severity.as_deref().map(Severity::from_str_loose),
            id_prefix: r.id_prefix.clone(),
            action: match r.action.as_str() {
                "notify" => PipelineAction::Notify,
                "batch" => PipelineAction::Batch,
                "drop" => PipelineAction::Drop,
                _ => PipelineAction::Batch,
            },
            rate_limit_secs: r.rate_limit_secs,
        })
        .collect()
}

fn default_batch_window() -> u64 {
    60
}

fn default_coordinator_name() -> String {
    "Bee".to_string()
}

fn default_model() -> String {
    "sonnet".to_string()
}

fn default_max_turns() -> u32 {
    20
}

fn default_watcher_interval() -> u64 {
    120
}

fn default_swarm_interval() -> u64 {
    15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_workspace_config() {
        let toml_str = r#"
root = "/Users/josh/Developer/apiari"
repos = ["ApiariTools/swarm"]

[telegram]
bot_token = "test-token"
chat_id = -123456
topic_id = 42

[coordinator]
model = "sonnet"
max_turns = 20

[watchers.github]
repos = ["ApiariTools/swarm"]
interval_secs = 120

[watchers.swarm]
state_path = "/Users/josh/Developer/apiari/.swarm/state.json"
interval_secs = 15
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.root, PathBuf::from("/Users/josh/Developer/apiari"));
        assert_eq!(config.repos, vec!["ApiariTools/swarm"]);
        assert!(config.telegram.is_some());
        assert!(config.watchers.github.is_some());
        assert!(config.watchers.swarm.is_some());
        assert!(config.watchers.sentry.is_none());
    }

    #[test]
    fn test_parse_minimal_workspace() {
        let toml_str = r#"
root = "/tmp/test"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.root, PathBuf::from("/tmp/test"));
        assert!(config.repos.is_empty());
        assert!(config.telegram.is_none());
        assert_eq!(config.coordinator.model, "sonnet");
        assert_eq!(config.coordinator.max_turns, 20);
    }

    #[test]
    fn test_to_buzz_config() {
        let ws = WorkspaceConfig {
            root: "/tmp".into(),
            repos: vec![],
            telegram: Some(TelegramConfig {
                bot_token: "tok".into(),
                chat_id: 123,
                topic_id: Some(42),
                allowed_user_ids: vec![],
            }),
            coordinator: CoordinatorConfig::default(),
            watchers: WatchersConfig::default(),
            pipeline: PipelineConfig::default(),
        };

        let buzz = to_buzz_config(&ws);
        assert!(buzz.telegram.is_some());
        assert_eq!(buzz.telegram.unwrap().chat_id, 123);
        assert_eq!(buzz.coordinator.model, "sonnet");
    }
}

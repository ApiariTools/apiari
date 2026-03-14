//! Workspace configuration for the apiari CLI.
//!
//! Each workspace is a self-contained TOML file at `~/.config/apiari/workspaces/{name}.toml`.

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Path to the daemon Unix socket (TUI ↔ daemon IPC).
pub fn socket_path() -> PathBuf {
    config_dir().join("daemon.sock")
}

/// A fully self-contained workspace configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Absolute path to the workspace root.
    pub root: PathBuf,

    /// Repository slugs (e.g. ["ApiariTools/swarm", "ApiariTools/apiari"]).
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

    /// Custom slash commands.
    #[serde(default)]
    pub commands: Vec<CommandConfig>,
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
    /// Custom prompt preamble (identity + role). If set, replaces the default.
    /// Signals, repos, and skills are still auto-appended.
    #[serde(default)]
    pub prompt: Option<String>,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            name: default_coordinator_name(),
            model: default_model(),
            max_turns: default_max_turns(),
            prompt: None,
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

/// Auto-discover git repos in immediate subdirectories of `root`.
///
/// Skips hidden dirs, `target/`, and `node_modules/`. For each dir containing `.git/`,
/// tries to extract a GitHub `org/repo` slug from the origin remote; falls back to the
/// directory name.
pub fn discover_repos(root: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut repos: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            e.file_type().is_ok_and(|ft| ft.is_dir())
                && !name.starts_with('.')
                && name != "target"
                && name != "node_modules"
        })
        .filter(|e| e.path().join(".git").exists())
        .map(|e| {
            extract_github_slug(&e.path())
                .unwrap_or_else(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect();

    repos.sort();
    repos.dedup();
    repos
}

/// Extract a GitHub `org/repo` slug from a repo's origin remote URL.
fn extract_github_slug(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout);
    parse_github_slug(url.trim())
}

/// Parse a GitHub `org/repo` slug from an HTTPS or SSH URL.
///
/// Handles:
/// - `https://github.com/Org/Repo.git`
/// - `https://github.com/Org/Repo`
/// - `git@github.com:Org/Repo.git`
///
/// Returns `None` for non-GitHub URLs or malformed input.
fn parse_github_slug(url: &str) -> Option<String> {
    let path = if let Some(rest) = url.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        return None;
    };

    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.splitn(3, '/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some(format!("{}/{}", parts[0], parts[1]))
    } else {
        None
    }
}

/// Resolve repos for a workspace: use explicit `repos` if non-empty, otherwise auto-discover.
pub fn resolve_repos(config: &WorkspaceConfig) -> Vec<String> {
    if !config.repos.is_empty() {
        return config.repos.clone();
    }
    discover_repos(&config.root)
}

/// Build a SkillContext from workspace config.
///
/// Uses `resolve_repos()` so auto-discovered repos flow into the coordinator prompt.
pub fn build_skill_context(
    workspace_name: &str,
    config: &WorkspaceConfig,
) -> crate::buzz::coordinator::skills::SkillContext {
    let repos = resolve_repos(config);
    crate::buzz::coordinator::skills::SkillContext {
        workspace_name: workspace_name.to_string(),
        workspace_root: config.root.clone(),
        config_path: workspaces_dir().join(format!("{workspace_name}.toml")),
        repos,
        has_sentry: config.watchers.sentry.is_some(),
        has_swarm: config.watchers.swarm.is_some(),
        prompt_preamble: config.coordinator.prompt.clone(),
    }
}

/// Convert a WorkspaceConfig into a buzz BuzzConfig for watcher/coordinator use.
pub fn to_buzz_config(ws: &WorkspaceConfig) -> crate::buzz::config::BuzzConfig {
    crate::buzz::config::BuzzConfig {
        telegram: ws.telegram.as_ref().map(|t| crate::buzz::config::TelegramConfig {
            bot_token: t.bot_token.clone(),
            chat_id: t.chat_id,
            topic_id: t.topic_id,
            allowed_user_ids: t.allowed_user_ids.clone(),
        }),
        watchers: crate::buzz::config::WatchersConfig {
            github: ws
                .watchers
                .github
                .as_ref()
                .map(|g| crate::buzz::config::GithubWatcherConfig {
                    enabled: true,
                    interval_secs: g.interval_secs,
                    repos: if g.repos.is_empty() {
                        discover_repos(&ws.root)
                    } else {
                        g.repos.clone()
                    },
                    watch_labels: vec![],
                }),
            sentry: ws
                .watchers
                .sentry
                .as_ref()
                .map(|s| crate::buzz::config::SentryWatcherConfig {
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
                .map(|s| crate::buzz::config::SwarmWatcherConfig {
                    enabled: true,
                    interval_secs: s.interval_secs,
                    state_path: s.state_path.clone(),
                }),
        },
        coordinator: crate::buzz::config::CoordinatorConfig {
            model: ws.coordinator.model.clone(),
            max_turns: ws.coordinator.max_turns,
        },
    }
}

/// A custom slash command executable via Telegram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandConfig {
    /// Command name (without the leading `/`).
    pub name: String,
    /// Shell script to run via `sh -c`.
    pub script: String,
    /// Description shown in `/help`.
    #[serde(default)]
    pub description: Option<String>,
    /// If true, daemon restarts itself after the script succeeds.
    #[serde(default)]
    pub restart: bool,
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
pub fn to_pipeline_rules(config: &PipelineConfig) -> Vec<crate::buzz::pipeline::rule::PipelineRule> {
    use crate::buzz::pipeline::rule::{PipelineAction, PipelineRule};
    use crate::buzz::signal::Severity;

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

/// Build the settings JSON for the coordinator (PreToolUse hook).
///
/// Shared by daemon, TUI, and CLI chat — configures the `apiari validate-bash`
/// hook so the coordinator's Bash calls are audited.
pub fn coordinator_settings_json() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe_str = exe.to_string_lossy();

    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": format!("{exe_str} validate-bash")
                }]
            }]
        }
    });

    Some(settings.to_string())
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
    fn test_parse_github_slug_https() {
        assert_eq!(
            parse_github_slug("https://github.com/ApiariTools/swarm.git"),
            Some("ApiariTools/swarm".to_string()),
        );
    }

    #[test]
    fn test_parse_github_slug_https_no_dot_git() {
        assert_eq!(
            parse_github_slug("https://github.com/ApiariTools/swarm"),
            Some("ApiariTools/swarm".to_string()),
        );
    }

    #[test]
    fn test_parse_github_slug_ssh() {
        assert_eq!(
            parse_github_slug("git@github.com:ApiariTools/swarm.git"),
            Some("ApiariTools/swarm".to_string()),
        );
    }

    #[test]
    fn test_parse_github_slug_non_github() {
        assert_eq!(parse_github_slug("https://gitlab.com/Org/Repo.git"), None,);
    }

    #[test]
    fn test_parse_github_slug_malformed() {
        assert_eq!(parse_github_slug("not-a-url"), None);
        assert_eq!(parse_github_slug("https://github.com/"), None);
        assert_eq!(parse_github_slug("https://github.com/only-org"), None);
    }

    #[test]
    fn test_resolve_repos_explicit_override() {
        let config = WorkspaceConfig {
            root: "/tmp/nonexistent".into(),
            repos: vec!["Org/Repo".to_string()],
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            watchers: WatchersConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
        };
        assert_eq!(resolve_repos(&config), vec!["Org/Repo"]);
    }

    #[test]
    fn test_resolve_repos_empty_discovers() {
        // With a non-existent root, discover_repos returns empty
        let config = WorkspaceConfig {
            root: "/tmp/nonexistent-dir-12345".into(),
            repos: vec![],
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            watchers: WatchersConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
        };
        assert!(resolve_repos(&config).is_empty());
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
            commands: vec![],
        };

        let buzz = to_buzz_config(&ws);
        assert!(buzz.telegram.is_some());
        assert_eq!(buzz.telegram.unwrap().chat_id, 123);
        assert_eq!(buzz.coordinator.model, "sonnet");
    }
}

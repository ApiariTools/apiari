//! Daemon configuration loaded from `.hive/daemon.toml`.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Top-level daemon configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    /// Model to use for coordinator sessions (default: "sonnet").
    #[serde(default = "default_model")]
    pub model: String,

    /// After this many turns, nudge the user to consider /reset.
    #[serde(default = "default_nudge_threshold")]
    pub nudge_turn_threshold: u64,

    /// Path to buzz signals JSONL file.
    #[serde(default = "default_buzz_signals_path")]
    pub buzz_signals_path: PathBuf,

    /// Max agentic turns per message (tool-use rounds). Keeps responses snappy.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// How often to poll buzz signals (seconds).
    #[serde(default = "default_buzz_poll_interval")]
    pub buzz_poll_interval_secs: u64,

    /// User IDs allowed to interact with the bot. Empty = allow all users.
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,

    /// Chat IDs (groups/DMs) the bot will respond in. Empty = allow all chats.
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,

    /// Telegram bot configuration.
    pub telegram: TelegramConfig,

    /// Inline buzz watcher configuration (optional).
    #[serde(default)]
    pub buzz: Option<BuzzDaemonConfig>,

    /// Swarm agent completion watcher (optional).
    #[serde(default)]
    pub swarm_watch: Option<SwarmWatchConfig>,

    /// Custom slash commands. Each key is the command name (without `/`).
    /// Example: `[commands.restart]` → `/restart` in Telegram.
    #[serde(default)]
    pub commands: HashMap<String, CustomCommand>,

    /// DEPRECATED: Extra workspaces previously configured inline. Now ignored —
    /// the global daemon discovers workspaces from `~/.config/hive/workspaces.toml`
    /// and each workspace has its own `.hive/daemon.toml`.
    #[serde(default)]
    pub workspaces: Vec<toml::Value>,
}

/// A user-defined slash command.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomCommand {
    /// Shell command to execute. Runs via `sh -c`. Working dir is workspace root.
    pub run: String,

    /// Optional description shown in `/help`.
    #[serde(default)]
    pub description: Option<String>,

    /// Post-action after the command finishes. Currently supported: `"restart"`.
    #[serde(default, rename = "then")]
    pub then_action: Option<String>,
}

/// Telegram-specific configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    /// Bot API token from @BotFather.
    pub bot_token: String,

    /// Chat ID where buzz triage alerts are sent.
    pub alert_chat_id: i64,

    /// Bot username (without @). Used in the system prompt so the coordinator
    /// knows its own identity when @mentioned in group chats.
    #[serde(default)]
    pub bot_username: Option<String>,

    /// Forum topic ID for notifications. When set, outbound alerts and
    /// auto-triage messages are sent to this topic within the alert chat.
    #[serde(default)]
    pub topic_id: Option<i64>,
}

/// Inline buzz watcher configuration for daemon mode.
///
/// When enabled, the daemon runs buzz watchers directly in its event loop
/// instead of reading from `.buzz/signals.jsonl`.
///
/// Config resolution order:
/// 1. `workspace.yaml` → `buzz:` section (preferred)
/// 2. `config_path` from this section (legacy `.buzz/config.toml`)
/// 3. Defaults (no watchers)
#[derive(Debug, Clone, Deserialize)]
pub struct BuzzDaemonConfig {
    /// Whether to run buzz watchers inline (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Path to `.buzz/config.toml` (default: ".buzz/config.toml").
    /// Only used as fallback when workspace.yaml has no `buzz:` section.
    #[serde(default = "default_buzz_config_path")]
    pub config_path: PathBuf,
}

/// Swarm agent completion watcher configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct SwarmWatchConfig {
    /// Whether to watch swarm state (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// How often to poll `.swarm/state.json` in seconds (default: 15).
    #[serde(default = "default_swarm_poll_interval")]
    pub poll_interval_secs: u64,

    /// Path to swarm state file (default: ".swarm/state.json").
    #[serde(default = "default_swarm_state_path")]
    pub state_path: PathBuf,

    /// Seconds of inactivity before an agent is considered stalled (default: 300, 0 = disabled).
    #[serde(default = "default_stall_timeout")]
    pub stall_timeout_secs: u64,

    /// DEPRECATED: Waiting debounce is no longer used (event-driven mode doesn't
    /// need it). Kept for config file backward compatibility — silently ignored.
    #[serde(default)]
    #[allow(dead_code)]
    pub waiting_debounce_secs: u64,

    /// When enabled, automatically invoke the coordinator after certain swarm
    /// notifications (PrOpened, AgentWaiting) to suggest a next action.
    #[serde(default)]
    pub auto_triage: bool,
}

fn default_model() -> String {
    "sonnet".into()
}

fn default_nudge_threshold() -> u64 {
    50
}

fn default_buzz_signals_path() -> PathBuf {
    PathBuf::from(".buzz/signals.jsonl")
}

fn default_max_turns() -> u32 {
    20
}

fn default_buzz_poll_interval() -> u64 {
    30
}

fn default_buzz_config_path() -> PathBuf {
    PathBuf::from(".buzz/config.toml")
}

fn default_true() -> bool {
    true
}

fn default_swarm_poll_interval() -> u64 {
    15
}

fn default_swarm_state_path() -> PathBuf {
    PathBuf::from(".swarm/state.json")
}

fn default_stall_timeout() -> u64 {
    300
}

impl DaemonConfig {
    /// Load config from `.hive/daemon.toml` under the given workspace root.
    pub fn load(workspace_root: &Path) -> color_eyre::Result<Self> {
        let path = workspace_root.join(".hive/daemon.toml");
        let content = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                color_eyre::eyre::eyre!(
                    "No daemon config found at {}\n\n\
                     To set up the Telegram bot:\n\
                     1. Message @BotFather on Telegram → /newbot\n\
                     2. Run: hive chat  (ask it to help set up the daemon)\n\
                     3. Or create .hive/daemon.toml manually:\n\n\
                     [telegram]\n\
                     bot_token = \"your-token-here\"\n\
                     alert_chat_id = 123456789\n",
                    path.display()
                )
            } else {
                color_eyre::eyre::eyre!("failed to read {}: {e}", path.display())
            }
        })?;
        let config: DaemonConfig = toml::from_str(&content)
            .map_err(|e| color_eyre::eyre::eyre!("failed to parse {}: {e}", path.display()))?;
        if !config.workspaces.is_empty() {
            tracing::warn!(path = %path.display(), "[[workspaces]] in daemon.toml is deprecated and ignored");
        }
        Ok(config)
    }

    /// Resolve the buzz signals path relative to the workspace root.
    pub fn resolved_buzz_path(&self, workspace_root: &Path) -> PathBuf {
        if self.buzz_signals_path.is_absolute() {
            self.buzz_signals_path.clone()
        } else {
            workspace_root.join(&self.buzz_signals_path)
        }
    }

    /// Resolve the buzz config path relative to the workspace root.
    #[allow(dead_code)]
    pub fn resolved_buzz_config_path(&self, workspace_root: &Path) -> Option<PathBuf> {
        let buzz = self.buzz.as_ref()?;
        if !buzz.enabled {
            return None;
        }
        Some(if buzz.config_path.is_absolute() {
            buzz.config_path.clone()
        } else {
            workspace_root.join(&buzz.config_path)
        })
    }

    /// Resolve the swarm state path relative to the workspace root.
    pub fn resolved_swarm_state_path(&self, workspace_root: &Path) -> Option<PathBuf> {
        let sw = self.swarm_watch.as_ref()?;
        if !sw.enabled {
            return None;
        }
        Some(if sw.state_path.is_absolute() {
            sw.state_path.clone()
        } else {
            workspace_root.join(&sw.state_path)
        })
    }

    /// Check if a user ID is allowed to interact with the bot.
    #[allow(dead_code)]
    pub fn is_user_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.is_empty() || self.allowed_user_ids.contains(&user_id)
    }

    /// Check if a chat ID is allowed.
    pub fn is_chat_allowed(&self, chat_id: i64) -> bool {
        self.allowed_chat_ids.is_empty() || self.allowed_chat_ids.contains(&chat_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
model = "opus"
nudge_turn_threshold = 25
max_turns = 5
buzz_signals_path = "/tmp/signals.jsonl"
buzz_poll_interval_secs = 10
allowed_user_ids = [111, 222]
allowed_chat_ids = [-100111, -100222]

[telegram]
bot_token = "7000000000:AAxxxxxxxxxxxxxxxxx"
alert_chat_id = 123456789
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.model, "opus");
        assert_eq!(config.nudge_turn_threshold, 25);
        assert_eq!(config.max_turns, 5);
        assert_eq!(
            config.buzz_signals_path,
            PathBuf::from("/tmp/signals.jsonl")
        );
        assert_eq!(config.buzz_poll_interval_secs, 10);
        assert_eq!(config.allowed_user_ids, vec![111, 222]);
        assert_eq!(config.allowed_chat_ids, vec![-100111, -100222]);
        assert_eq!(config.telegram.bot_token, "7000000000:AAxxxxxxxxxxxxxxxxx");
        assert_eq!(config.telegram.alert_chat_id, 123456789);
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.model, "sonnet");
        assert_eq!(config.nudge_turn_threshold, 50);
        assert_eq!(config.max_turns, 20);
        assert_eq!(
            config.buzz_signals_path,
            PathBuf::from(".buzz/signals.jsonl")
        );
        assert_eq!(config.buzz_poll_interval_secs, 30);
        assert!(config.allowed_user_ids.is_empty());
    }

    #[test]
    fn test_is_user_allowed_empty_list() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_user_allowed(999));
    }

    #[test]
    fn test_is_user_allowed_restricted() {
        let config: DaemonConfig = toml::from_str(
            r#"
allowed_user_ids = [100, 200]
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_user_allowed(100));
        assert!(config.is_user_allowed(200));
        assert!(!config.is_user_allowed(300));
    }

    #[test]
    fn test_is_chat_allowed_empty_list() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_chat_allowed(-100123));
    }

    #[test]
    fn test_is_chat_allowed_restricted() {
        let config: DaemonConfig = toml::from_str(
            r#"
allowed_chat_ids = [-100111, -100222]
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_chat_allowed(-100111));
        assert!(config.is_chat_allowed(-100222));
        assert!(!config.is_chat_allowed(-100333));
    }

    #[test]
    fn test_resolved_buzz_path_relative() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        let resolved = config.resolved_buzz_path(Path::new("/workspace"));
        assert_eq!(resolved, PathBuf::from("/workspace/.buzz/signals.jsonl"));
    }

    #[test]
    fn test_resolved_buzz_path_absolute() {
        let config: DaemonConfig = toml::from_str(
            r#"
buzz_signals_path = "/abs/signals.jsonl"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        let resolved = config.resolved_buzz_path(Path::new("/workspace"));
        assert_eq!(resolved, PathBuf::from("/abs/signals.jsonl"));
    }

    #[test]
    fn test_parse_buzz_config() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[buzz]
enabled = true
config_path = ".buzz/custom.toml"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let buzz = config.buzz.unwrap();
        assert!(buzz.enabled);
        assert_eq!(buzz.config_path, PathBuf::from(".buzz/custom.toml"));
    }

    #[test]
    fn test_parse_swarm_watch_config() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
enabled = true
poll_interval_secs = 30
state_path = ".swarm/custom.json"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let sw = config.swarm_watch.unwrap();
        assert!(sw.enabled);
        assert_eq!(sw.poll_interval_secs, 30);
        assert_eq!(sw.state_path, PathBuf::from(".swarm/custom.json"));
        assert_eq!(sw.stall_timeout_secs, 300); // default
    }

    #[test]
    fn test_stall_timeout_custom() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
stall_timeout_secs = 600
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let sw = config.swarm_watch.unwrap();
        assert_eq!(sw.stall_timeout_secs, 600);
    }

    #[test]
    fn test_auto_triage_default_false() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
enabled = true
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let sw = config.swarm_watch.unwrap();
        assert!(!sw.auto_triage, "auto_triage should default to false");
    }

    #[test]
    fn test_auto_triage_enabled() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
enabled = true
auto_triage = true
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let sw = config.swarm_watch.unwrap();
        assert!(sw.auto_triage);
    }

    #[test]
    fn test_buzz_and_swarm_watch_default_none() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(config.buzz.is_none());
        assert!(config.swarm_watch.is_none());
    }

    #[test]
    fn test_custom_commands_default_empty() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(config.commands.is_empty());
    }

    #[test]
    fn test_custom_commands_parsed() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[commands.restart]
run = "./install.sh"
description = "Pull, build, and restart"
then = "restart"

[commands.deploy]
run = "./deploy.sh"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.commands.len(), 2);

        let restart = &config.commands["restart"];
        assert_eq!(restart.run, "./install.sh");
        assert_eq!(
            restart.description.as_deref(),
            Some("Pull, build, and restart")
        );
        assert_eq!(restart.then_action.as_deref(), Some("restart"));

        let deploy = &config.commands["deploy"];
        assert_eq!(deploy.run, "./deploy.sh");
        assert!(deploy.description.is_none());
        assert!(deploy.then_action.is_none());
    }

    #[test]
    fn test_deprecated_workspaces_still_parse() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[[workspaces]]
name = "mgm"
path = "/Users/josh/Developer/mgm"
topic_id = 99
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.workspaces.len(),
            1,
            "deprecated field should still parse"
        );
    }

    #[test]
    fn test_workspaces_default_empty() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(config.workspaces.is_empty());
    }

    // ---- resolved_buzz_config_path ----

    #[test]
    fn resolved_buzz_config_path_none_when_buzz_absent() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.resolved_buzz_config_path(Path::new("/ws")).is_none());
    }

    #[test]
    fn resolved_buzz_config_path_none_when_disabled() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[buzz]
enabled = false
"#,
        )
        .unwrap();
        assert!(config.resolved_buzz_config_path(Path::new("/ws")).is_none());
    }

    #[test]
    fn resolved_buzz_config_path_relative_when_enabled() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[buzz]
enabled = true
config_path = ".buzz/config.toml"
"#,
        )
        .unwrap();
        let resolved = config.resolved_buzz_config_path(Path::new("/workspace"));
        assert_eq!(
            resolved,
            Some(PathBuf::from("/workspace/.buzz/config.toml"))
        );
    }

    #[test]
    fn resolved_buzz_config_path_absolute_when_enabled() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[buzz]
enabled = true
config_path = "/etc/buzz/config.toml"
"#,
        )
        .unwrap();
        let resolved = config.resolved_buzz_config_path(Path::new("/workspace"));
        assert_eq!(resolved, Some(PathBuf::from("/etc/buzz/config.toml")));
    }

    // ---- resolved_swarm_state_path ----

    #[test]
    fn resolved_swarm_state_path_none_when_absent() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.resolved_swarm_state_path(Path::new("/ws")).is_none());
    }

    #[test]
    fn resolved_swarm_state_path_none_when_disabled() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
enabled = false
"#,
        )
        .unwrap();
        assert!(config.resolved_swarm_state_path(Path::new("/ws")).is_none());
    }

    #[test]
    fn resolved_swarm_state_path_relative_when_enabled() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
enabled = true
state_path = ".swarm/state.json"
"#,
        )
        .unwrap();
        let resolved = config.resolved_swarm_state_path(Path::new("/workspace"));
        assert_eq!(
            resolved,
            Some(PathBuf::from("/workspace/.swarm/state.json"))
        );
    }

    #[test]
    fn resolved_swarm_state_path_absolute_when_enabled() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42

[swarm_watch]
enabled = true
state_path = "/var/swarm/state.json"
"#,
        )
        .unwrap();
        let resolved = config.resolved_swarm_state_path(Path::new("/workspace"));
        assert_eq!(resolved, Some(PathBuf::from("/var/swarm/state.json")));
    }

    // ---- DaemonConfig::load from temp file ----

    #[test]
    fn load_from_temp_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let hive_dir = dir.path().join(".hive");
        std::fs::create_dir_all(&hive_dir).unwrap();
        std::fs::write(
            hive_dir.join("daemon.toml"),
            r#"
model = "haiku"
max_turns = 10

[telegram]
bot_token = "test-token"
alert_chat_id = 999
"#,
        )
        .unwrap();

        let config = DaemonConfig::load(dir.path()).unwrap();
        assert_eq!(config.model, "haiku");
        assert_eq!(config.max_turns, 10);
        assert_eq!(config.telegram.bot_token, "test-token");
        assert_eq!(config.telegram.alert_chat_id, 999);
    }

    #[test]
    fn load_missing_file_returns_helpful_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = DaemonConfig::load(dir.path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No daemon config found"),
            "error should be helpful: {err_msg}"
        );
    }
}

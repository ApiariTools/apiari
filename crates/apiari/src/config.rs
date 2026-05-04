//! Workspace configuration for the apiari CLI.
//!
//! Supports both the legacy `~/.config/apiari/` layout and the current
//! `~/.config/hive/` layout.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

fn apiari_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".config/apiari")
}

fn hive_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".config/hive")
}

fn dir_has_workspace_state(dir: &Path) -> bool {
    dir.join("workspaces").is_dir()
        || dir.join("workspaces.toml").is_file()
        || dir.join("hive.db").is_file()
        || dir.join("apiari.db").is_file()
}

/// Root directory for all apiari config.
pub fn config_dir() -> PathBuf {
    let hive = hive_config_dir();
    if dir_has_workspace_state(&hive) {
        return hive;
    }

    apiari_config_dir()
}

/// Directory containing workspace TOML files.
pub fn workspaces_dir() -> PathBuf {
    config_dir().join("workspaces")
}

/// Path to the shared SQLite database.
pub fn db_path() -> PathBuf {
    let dir = config_dir();
    let hive_db = dir.join("hive.db");
    if hive_db.exists() {
        hive_db
    } else {
        dir.join("apiari.db")
    }
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

/// Workspace authority level — controls what the coordinator is allowed to do.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceAuthority {
    /// Read-only tools only (Read, Glob, Grep, WebSearch, WebFetch). No Bash, no swarm dispatch.
    Observe,
    /// Full toolset (current behavior). This is the default.
    #[default]
    Autonomous,
}

/// Per-bee execution policy — controls whether a bee may directly implement code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeeExecutionPolicy {
    /// Read-only. No Bash, no edits, no worker dispatch.
    Observe,
    /// Investigate and dispatch workers, but do not implement directly.
    DispatchOnly,
    /// May implement directly with provider-native write access.
    Autonomous,
}

impl Default for BeeExecutionPolicy {
    fn default() -> Self {
        Self::Autonomous
    }
}

impl BeeExecutionPolicy {
    /// Apply the workspace-level ceiling to a bee policy.
    pub fn resolved(self, authority: WorkspaceAuthority) -> Self {
        match authority {
            WorkspaceAuthority::Observe => Self::Observe,
            WorkspaceAuthority::Autonomous => self,
        }
    }
}

/// Workspace capabilities — fine-grained permission controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCapabilities {
    /// Whether the coordinator can dispatch swarm workers.
    /// Default: true in autonomous, false in observe.
    #[serde(default = "default_true")]
    pub dispatch_workers: bool,

    /// Whether and how the coordinator can merge PRs.
    /// Default: Never — must explicitly opt in.
    /// Accepts: `false`/`"never"` (Never), `true`/`"on_command"` (OnCommand), `"autonomous"` (Autonomous).
    #[serde(default)]
    pub merge_prs: MergePrsPolicy,
}

impl Default for WorkspaceCapabilities {
    fn default() -> Self {
        Self {
            dispatch_workers: true,
            merge_prs: MergePrsPolicy::default(),
        }
    }
}

impl WorkspaceCapabilities {
    /// Resolve capabilities for a given authority level.
    /// In observe mode, dispatch_workers is always false.
    pub fn resolved(&self, authority: WorkspaceAuthority) -> Self {
        match authority {
            WorkspaceAuthority::Observe => Self {
                dispatch_workers: false,
                merge_prs: MergePrsPolicy::Never,
            },
            WorkspaceAuthority::Autonomous => self.clone(),
        }
    }
}

/// Granular merge control policy for `merge_prs`.
///
/// TOML values accepted (backward compatible):
/// - `false` or `"never"` → `Never`
/// - `true` or `"on_command"` → `OnCommand`
/// - `"autonomous"` → `Autonomous`
/// - `[]` (empty legacy branch list) → `Never`
/// - `["branch", ...]` (non-empty legacy branch list) → `OnCommand`
/// - Any other string value → parse error
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(try_from = "MergePrsRaw")]
pub enum MergePrsPolicy {
    /// PR merging is never allowed. This is the default.
    #[default]
    Never,
    /// Merging is allowed only when the user explicitly requests it.
    OnCommand,
    /// Reserved for future autonomous merge behavior (not yet implemented).
    Autonomous,
}

impl Serialize for MergePrsPolicy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Never => s.serialize_bool(false),
            Self::OnCommand => s.serialize_str("on_command"),
            Self::Autonomous => s.serialize_str("autonomous"),
        }
    }
}

impl MergePrsPolicy {
    /// Returns true if merging is permitted under this policy.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::OnCommand | Self::Autonomous)
    }
}

/// Serde intermediate for deserializing `merge_prs` with backward compatibility.
#[derive(Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum MergePrsRaw {
    Bool(bool),
    Str(String),
    Branches(Vec<String>),
}

impl TryFrom<MergePrsRaw> for MergePrsPolicy {
    type Error = String;

    fn try_from(raw: MergePrsRaw) -> Result<Self, Self::Error> {
        match raw {
            MergePrsRaw::Bool(false) => Ok(MergePrsPolicy::Never),
            MergePrsRaw::Bool(true) => Ok(MergePrsPolicy::OnCommand),
            MergePrsRaw::Str(s) => match s.as_str() {
                "on_command" => Ok(MergePrsPolicy::OnCommand),
                "autonomous" => Ok(MergePrsPolicy::Autonomous),
                "never" | "false" => Ok(MergePrsPolicy::Never),
                other => Err(format!(
                    "unknown merge_prs value {:?}; expected false, true, \"never\", \"on_command\", or \"autonomous\"",
                    other
                )),
            },
            // Legacy branch-scoped lists: empty list → Never (no branches = no
            // merging), non-empty → OnCommand (merging was previously permitted).
            // Note: branch constraints from the old config are not enforced by
            // apiari; users relying on them should migrate to `"on_command"`.
            MergePrsRaw::Branches(branches) => {
                if branches.is_empty() {
                    Ok(MergePrsPolicy::Never)
                } else {
                    Ok(MergePrsPolicy::OnCommand)
                }
            }
        }
    }
}

/// Current workspace config version.
///
/// Bump this constant whenever you:
/// - Add, remove, or rename a config field
/// - Change the meaning or valid values of an existing field
/// - Change default behaviour in a way that affects existing configs
///
/// The daemon's doctor check (`apiari doctor`) compares the on-disk `config_version`
/// against this value and warns users whose configs are older than the current version.
pub const CURRENT_CONFIG_VERSION: u32 = 4;

/// Schedule configuration — defines when watchers and signal hooks are active.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schedule {
    /// Active time window in "HH:MM-HH:MM" 24h local time (e.g. "09:00-18:00").
    /// Supports overnight ranges like "22:00-06:00".
    /// If absent, all hours are active.
    #[serde(default)]
    pub active_hours: Option<String>,
    /// Active days of the week as lowercase 3-letter abbreviations
    /// (e.g. ["mon", "tue", "wed", "thu", "fri"]).
    /// If absent, all days are active.
    #[serde(default)]
    pub active_days: Option<Vec<String>>,
}

/// Activity feed / event retention configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityConfig {
    /// How many days to retain activity events. Default: 30.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

fn default_retention_days() -> u32 {
    30
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
        }
    }
}

/// A fully self-contained workspace configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Schema version for forward-compatible config evolution.
    /// `None` means pre-versioning (version 0).
    #[serde(default)]
    pub config_version: Option<u32>,

    /// Absolute path to the workspace root.
    pub root: PathBuf,

    /// Repository slugs (e.g. ["ApiariTools/swarm", "ApiariTools/apiari"]).
    #[serde(default)]
    pub repos: Vec<String>,

    /// Workspace authority level (observe or autonomous). Default: autonomous.
    #[serde(default)]
    pub authority: WorkspaceAuthority,

    /// Fine-grained capability controls.
    #[serde(default)]
    pub capabilities: WorkspaceCapabilities,

    /// Telegram configuration.
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,

    /// Coordinator configuration (single-bee shorthand).
    /// Ignored when `[[bees]]` is present.
    #[serde(default)]
    pub coordinator: CoordinatorConfig,

    /// Multiple specialized Bees (coordinators) for this workspace.
    /// When present, overrides `[coordinator]`.
    #[serde(default)]
    pub bees: Option<Vec<BeeConfig>>,

    /// Watcher configurations.
    #[serde(default)]
    pub watchers: WatchersConfig,

    /// Orchestrator configuration (replaces pipeline + signal_hooks).
    #[serde(default)]
    pub orchestrator: crate::buzz::orchestrator::OrchestratorConfig,

    /// Swarm agent configuration.
    #[serde(default)]
    pub swarm: SwarmConfig,

    /// Custom slash commands.
    #[serde(default)]
    pub commands: Vec<CommandConfig>,

    /// Morning brief configuration.
    #[serde(default)]
    pub morning_brief: Option<MorningBriefConfig>,

    /// Enable TCP listener for the daemon on this port (e.g. 7474).
    /// Disabled if not set.
    #[serde(default)]
    pub daemon_tcp_port: Option<u16>,

    /// Bind address for the TCP listener (default: "127.0.0.1").
    /// Set to "0.0.0.0" to listen on all interfaces.
    #[serde(default)]
    pub daemon_tcp_bind: Option<String>,

    /// Remote daemon host (Tailscale/LAN IP). When set with `daemon_port`,
    /// the TUI connects to this workspace via TCP instead of Unix socket.
    /// **Deprecated**: use `daemon_endpoints` instead.
    #[serde(default)]
    pub daemon_host: Option<String>,

    /// Remote daemon TCP port. Used with `daemon_host`.
    /// **Deprecated**: use `daemon_endpoints` instead.
    #[serde(default)]
    pub daemon_port: Option<u16>,

    /// Ordered list of daemon endpoints to try when connecting.
    /// The TUI tries each in order, using the first that responds.
    #[serde(default)]
    pub daemon_endpoints: Vec<DaemonEndpoint>,

    /// Shell management configuration (tmux integration).
    #[serde(default)]
    pub shells: ShellsConfig,

    /// Active-hours schedule — when absent, watchers and signal hooks run 24/7.
    #[serde(default)]
    pub schedule: Option<Schedule>,

    /// Activity feed / event retention configuration.
    #[serde(default)]
    pub activity: ActivityConfig,
}

/// A single daemon TCP endpoint (host + port).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonEndpoint {
    pub host: String,
    #[serde(default = "default_daemon_port")]
    pub port: u16,
}

fn default_daemon_port() -> u16 {
    7474
}

impl WorkspaceConfig {
    /// Resolve the ordered list of daemon endpoints.
    ///
    /// If `daemon_endpoints` is non-empty, returns it as-is.
    /// Otherwise, falls back to legacy `daemon_host` + `daemon_port` as a single entry.
    /// Returns an empty vec if neither is configured (local-only workspace).
    pub fn resolved_daemon_endpoints(&self) -> Vec<DaemonEndpoint> {
        if !self.daemon_endpoints.is_empty() {
            return self.daemon_endpoints.clone();
        }
        // Backward compat: legacy single-host config
        if let (Some(host), Some(port)) = (&self.daemon_host, self.daemon_port) {
            return vec![DaemonEndpoint {
                host: host.clone(),
                port,
            }];
        }
        Vec::new()
    }
}

/// Shell management configuration (tmux integration).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShellsConfig {
    /// Whether shell management is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Tmux session name. Defaults to "apiari-{workspace_name}".
    #[serde(default)]
    pub tmux_session: Option<String>,
    /// Automatically create/kill tmux windows with workers.
    #[serde(default)]
    pub auto_worker_shells: bool,
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
    /// LLM provider: "claude", "codex", or "gemini".
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    /// Custom prompt preamble (identity + role). If set, replaces the default.
    /// Signals, repos, and skills are still auto-appended.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Clear the coordinator session after this many turns (default: 50).
    /// Set to 0 to disable auto-compaction.
    #[serde(default = "default_max_session_turns")]
    pub max_session_turns: u32,
    /// Signal sources that trigger a coordinator follow-through.
    #[serde(default = "default_signal_hooks")]
    pub signal_hooks: Vec<SignalHookConfig>,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            name: default_coordinator_name(),
            provider: default_provider(),
            model: default_model(),
            max_turns: default_max_turns(),
            prompt: None,
            max_session_turns: default_max_session_turns(),
            signal_hooks: default_signal_hooks(),
        }
    }
}

/// Configuration for a single Bee (coordinator) in a multi-bee workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeeConfig {
    /// Display name for this Bee (e.g. "CodeBee", "CustomerBee").
    pub name: String,
    /// Short role label for UI and summaries.
    #[serde(default)]
    pub role: Option<String>,
    /// Optional accent color for UI surfaces.
    #[serde(default)]
    pub color: Option<String>,
    /// Whether this bee may observe only, dispatch only, or act autonomously.
    #[serde(default)]
    pub execution_policy: BeeExecutionPolicy,
    /// LLM provider: "claude", "codex", or "gemini".
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    /// Custom prompt preamble for this Bee's specialty.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default = "default_max_session_turns")]
    pub max_session_turns: u32,
    /// Signal sources that trigger this Bee's follow-through.
    #[serde(default)]
    pub signal_hooks: Vec<SignalHookConfig>,
    /// Telegram topic ID for this Bee (each Bee gets its own thread).
    #[serde(default)]
    pub topic_id: Option<i64>,
    /// Heartbeat interval (e.g. "30m", "1h", "4h"). None = no heartbeat.
    #[serde(default)]
    pub heartbeat: Option<String>,
    /// Prompt sent to the Bee on each heartbeat.
    #[serde(default)]
    pub heartbeat_prompt: Option<String>,
}

impl BeeConfig {
    /// Parse the heartbeat string (e.g. "30m", "1h", "4h", "30s") into a Duration.
    pub fn heartbeat_duration(&self) -> Option<std::time::Duration> {
        let s = self.heartbeat.as_deref()?.trim();
        if let Some(mins) = s.strip_suffix('m') {
            mins.trim()
                .parse::<u64>()
                .ok()
                .map(|m| std::time::Duration::from_secs(m * 60))
        } else if let Some(hours) = s.strip_suffix('h') {
            hours
                .trim()
                .parse::<u64>()
                .ok()
                .map(|h| std::time::Duration::from_secs(h * 3600))
        } else if let Some(secs) = s.strip_suffix('s') {
            secs.trim()
                .parse::<u64>()
                .ok()
                .map(std::time::Duration::from_secs)
        } else {
            None
        }
    }
}

impl WorkspaceConfig {
    /// Get the resolved list of Bees for this workspace.
    ///
    /// If `[[bees]]` is configured, returns those. Otherwise, converts the
    /// single `[coordinator]` into a one-element vec for backward compatibility.
    #[allow(dead_code)] // Used in Phase 2 (multi-bee daemon spawning)
    pub fn resolved_bees(&self) -> Vec<BeeConfig> {
        if let Some(ref bees) = self.bees
            && !bees.is_empty()
        {
            return bees
                .iter()
                .cloned()
                .map(|mut bee| {
                    bee.model = normalize_model_identifier(&bee.provider, &bee.model);
                    bee
                })
                .collect();
        }
        // Convert single coordinator config to a BeeConfig
        let c = &self.coordinator;
        vec![BeeConfig {
            name: c.name.clone(),
            role: None,
            color: None,
            execution_policy: BeeExecutionPolicy::default(),
            provider: c.provider.clone(),
            model: normalize_model_identifier(&c.provider, &c.model),
            max_turns: c.max_turns,
            prompt: c.prompt.clone(),
            max_session_turns: c.max_session_turns,
            signal_hooks: c.signal_hooks.clone(),
            topic_id: self.telegram.as_ref().and_then(|tg| tg.topic_id),
            heartbeat: None,
            heartbeat_prompt: None,
        }]
    }

    /// Resolve the swarm state path used by local worker features.
    ///
    /// When no explicit watcher path is configured, monorepo/local workflows
    /// still use the conventional workspace-root `.swarm/state.json`.
    pub fn resolved_swarm_state_path(&self) -> PathBuf {
        let path = self
            .watchers
            .swarm
            .as_ref()
            .map(|s| s.state_path.clone())
            .unwrap_or_else(|| self.root.join(".swarm/state.json"));

        if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        }
    }

    /// Resolve the swarm state directory that holds `state.json`, `agents/`,
    /// and `wt/`.
    pub fn resolved_swarm_dir(&self) -> PathBuf {
        self.resolved_swarm_state_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.join(".swarm"))
    }
}

/// Configuration for a signal hook — triggers coordinator follow-through when
/// signals from the specified source arrive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalHookConfig {
    /// Signal source to match (e.g. "swarm", "github_bot_review", "github").
    /// Matches exactly, or as a prefix with `_` separator (e.g. "github" matches
    /// "github_bot_review", "github_release", etc.).
    /// The first matching hook wins when multiple hooks could match a signal.
    pub source: String,
    /// Prompt template sent to coordinator. Supports {source} and {events} placeholders.
    /// Empty string = use default formatting.
    #[serde(default)]
    pub prompt: String,
    /// Natural-language action instruction for the coordinator.
    /// When set, appended to the hook prompt so the coordinator knows what to DO
    /// (e.g. dispatch a worker, forward a review, triage).
    /// If omitted, the coordinator just notifies (current default behavior).
    #[serde(default)]
    pub action: Option<String>,
    /// Max seconds to wait in queue before dropping. Default: 120.
    #[serde(default = "default_hook_ttl")]
    pub ttl_secs: u64,
    /// Playbook skills to load for this hook's coordinator session.
    /// Names reference files in `.apiari/skills/{name}.md`.
    #[serde(default)]
    pub skills: Vec<String>,
}

fn default_hook_ttl() -> u64 {
    120
}

fn default_signal_hooks() -> Vec<SignalHookConfig> {
    vec![
        SignalHookConfig {
            source: "swarm".into(),
            prompt: "Swarm activity: {events}".into(),
            action: Some("Assess the situation. If a worker opened a PR, check if Copilot has reviewed it and if so forward any comments to the worker. If a worker is stuck or failed, investigate and either send a fix or dispatch a new worker.".into()),
            ttl_secs: 300,
            skills: vec![],
        },
        SignalHookConfig {
            source: "github_bot_review".into(),
            prompt: "Bot review received: {events}".into(),
            action: Some("Find the swarm worker whose branch matches this PR and forward the review comments directly to it so it can address them.".into()),
            ttl_secs: 300,
            skills: vec![],
        },
        SignalHookConfig {
            source: "github".into(),
            prompt: "CI failed: {events}".into(),
            action: Some("Find the relevant swarm worker for this PR. If a worker exists, send it the CI error details so it can fix them. If no worker exists, dispatch a new one to fix the failure.".into()),
            ttl_secs: 300,
            skills: vec![],
        },
    ]
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
    /// Email watchers (multiple mailboxes via `[[watchers.email]]`).
    #[serde(default)]
    pub email: Vec<EmailMailboxConfig>,
    /// Notion watchers via `[[watchers.notion]]`.
    #[serde(default)]
    pub notion: Vec<NotionWatcherConfig>,
    /// Linear watchers via `[[watchers.linear]]`.
    #[serde(default)]
    pub linear: Vec<LinearWatcherConfig>,
    /// Script watchers via `[[watchers.script]]`.
    #[serde(default)]
    pub script: Vec<ScriptWatcherConfig>,
}

/// Email mailbox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMailboxConfig {
    pub name: String,
    pub host: String,
    #[serde(default = "default_imap_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub tls: bool,
    pub username: String,
    pub password: String,
    #[serde(default = "default_folder")]
    pub folder: String,
    #[serde(default = "default_filter")]
    pub filter: String,
    #[serde(default)]
    pub include_body: bool,
    #[serde(default = "default_max_fetch")]
    pub max_fetch: u32,
    #[serde(default = "default_email_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub summarizer: Option<EmailSummarizerConfig>,
    /// Per-watcher active hours override. Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

/// Ollama/OpenAI-compatible summarizer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummarizerConfig {
    pub base_url: String,
    pub model: String,
}

/// Notion watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionWatcherConfig {
    pub name: String,
    pub token: String,
    pub user_id: String,
    #[serde(default)]
    pub poll_database_ids: Option<Vec<String>>,
    #[serde(default = "default_notion_interval")]
    pub interval_secs: u64,
    /// Per-watcher active hours override. Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

/// Linear watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearWatcherConfig {
    pub name: String,
    pub api_key: String,
    #[serde(default = "default_linear_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub review_queue: Vec<LinearReviewQueueEntry>,
    /// Per-watcher active hours override. Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

/// A single review queue query for Linear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearReviewQueueEntry {
    pub name: String,
    pub query: String,
}

/// Script/command watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptWatcherConfig {
    pub name: String,
    pub command: String,
    #[serde(default = "default_script_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub emit_on_change: bool,
    #[serde(default = "default_severity_on_fail")]
    pub severity_on_fail: String,
    #[serde(default = "default_script_timeout")]
    pub timeout_secs: u64,
    /// Per-watcher active hours override. Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

fn default_script_interval() -> u64 {
    60
}

fn default_severity_on_fail() -> String {
    "warning".to_string()
}

fn default_script_timeout() -> u64 {
    30
}

fn default_linear_interval() -> u64 {
    60
}

fn default_notion_interval() -> u64 {
    120
}

fn default_true() -> bool {
    true
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

/// GitHub watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubWatcherConfig {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default = "default_watcher_interval")]
    pub interval_secs: u64,
    /// Named priority queries for the review queue.
    /// Order = priority order (first entry is highest priority).
    #[serde(default)]
    pub review_queue: Vec<ReviewQueueEntry>,
    /// Per-event-type filters (e.g. `github_pr_push = "author:@me"`).
    #[serde(default)]
    pub filters: HashMap<String, String>,
    /// Per-watcher active hours override (e.g. "09:00-17:00"). Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

/// A named review queue query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewQueueEntry {
    /// Human-readable name for this query (e.g. "My PRs", "Team Reviews").
    pub name: String,
    /// GitHub search query (e.g. "is:pr is:open review-requested:@me").
    pub query: String,
}

/// Sentry watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentryWatcherConfig {
    pub org: String,
    pub project: String,
    pub token: String,
    #[serde(default = "default_watcher_interval")]
    pub interval_secs: u64,
    /// Per-watcher active hours override. Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

/// Swarm watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmWatcherConfig {
    pub state_path: PathBuf,
    #[serde(default = "default_swarm_interval")]
    pub interval_secs: u64,
    /// Per-watcher active hours override. Overrides workspace schedule.
    #[serde(default)]
    pub active_hours: Option<String>,
}

/// Morning brief configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MorningBriefConfig {
    /// Whether the morning brief is enabled.
    #[serde(default = "default_morning_brief_enabled")]
    pub enabled: bool,
    /// Time to send the brief in HH:MM format (24h).
    pub time: String,
    /// IANA timezone name (e.g. "America/Chicago").
    pub timezone: String,
}

fn default_morning_brief_enabled() -> bool {
    true
}

/// A loaded workspace with its name.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub name: String,
    pub config: WorkspaceConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct HiveWorkspaceFile {
    workspace: HiveWorkspaceSection,
    #[serde(default)]
    bots: Vec<HiveBotConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFileFormat {
    Current,
    HiveCompat,
}

#[derive(Debug, Clone, Deserialize)]
struct HiveWorkspaceSection {
    root: PathBuf,
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    #[serde(default)]
    default_agent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HiveBotConfig {
    name: String,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    prompt_file: Option<String>,
    #[serde(default)]
    watch: Vec<String>,
    #[serde(default)]
    services: Vec<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    schedule_hours: Option<u64>,
    #[serde(default)]
    proactive_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyServicesFile {
    #[serde(default)]
    sentry: Option<LegacySentryServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyHiveWorkspaceYaml {
    #[serde(default)]
    buzz: Option<LegacyHiveBuzzConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyHiveBuzzConfig {
    #[serde(default)]
    sentry: Option<LegacySentryServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacySentryServiceConfig {
    org: String,
    project: String,
    token: String,
}

fn hive_workspace_to_current(value: &HiveWorkspaceFile) -> WorkspaceConfig {
    let bees = (!value.bots.is_empty()).then(|| {
        let root = value.workspace.root.clone();
        let mut bees = vec![BeeConfig {
            name: default_coordinator_name(),
            role: None,
            color: None,
            execution_policy: BeeExecutionPolicy::default(),
            provider: default_provider(),
            model: default_model(),
            max_turns: default_max_turns(),
            prompt: None,
            max_session_turns: default_max_session_turns(),
            signal_hooks: vec![],
            topic_id: None,
            heartbeat: None,
            heartbeat_prompt: None,
        }];

        bees.extend(value.bots.iter().map(|bot| BeeConfig {
            model: normalize_model_identifier(&bot.provider, &bot.model),
            name: bot.name.clone(),
            role: bot.role.clone(),
            color: bot.color.clone(),
            execution_policy: BeeExecutionPolicy::default(),
            provider: bot.provider.clone(),
            max_turns: default_max_turns(),
            prompt: hive_bot_prompt(&root, bot),
            max_session_turns: default_max_session_turns(),
            signal_hooks: hive_bot_signal_hooks(bot),
            topic_id: None,
            heartbeat: hive_bot_heartbeat(bot),
            heartbeat_prompt: bot.proactive_prompt.clone(),
        }));

        bees
    });

    WorkspaceConfig {
        config_version: None,
        root: value.workspace.root.clone(),
        repos: vec![],
        authority: WorkspaceAuthority::default(),
        capabilities: WorkspaceCapabilities::default(),
        telegram: None,
        coordinator: CoordinatorConfig::default(),
        bees,
        watchers: WatchersConfig::default(),
        orchestrator: crate::buzz::orchestrator::OrchestratorConfig::default(),
        swarm: SwarmConfig {
            default_agent: value
                .workspace
                .default_agent
                .clone()
                .unwrap_or_else(default_swarm_agent),
        },
        commands: vec![],
        morning_brief: None,
        daemon_tcp_port: None,
        daemon_tcp_bind: None,
        daemon_host: None,
        daemon_port: None,
        daemon_endpoints: vec![],
        shells: ShellsConfig::default(),
        schedule: None,
        activity: ActivityConfig::default(),
    }
}

impl From<HiveWorkspaceFile> for WorkspaceConfig {
    fn from(value: HiveWorkspaceFile) -> Self {
        hive_workspace_to_current(&value)
    }
}

/// Load a single workspace config from a TOML file.
pub fn load_workspace(path: &Path) -> Result<WorkspaceConfig> {
    let contents = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    if let Ok(config) = toml::from_str::<WorkspaceConfig>(&contents) {
        return Ok(config);
    }

    let hive_config: HiveWorkspaceFile = toml::from_str(&contents)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    let mut config = hive_workspace_to_current(&hive_config);
    hydrate_legacy_service_watchers(&mut config, &hive_config);
    Ok(config)
}

fn hive_bot_signal_hooks(bot: &HiveBotConfig) -> Vec<SignalHookConfig> {
    bot.watch
        .iter()
        .map(|source| SignalHookConfig {
            source: source.clone(),
            prompt: String::new(),
            action: None,
            ttl_secs: default_hook_ttl(),
            skills: vec![],
        })
        .collect()
}

fn hive_bot_heartbeat(bot: &HiveBotConfig) -> Option<String> {
    if let Some(hours) = bot.schedule_hours {
        return Some(format!("{hours}h"));
    }

    let schedule = bot.schedule.as_deref()?.trim();
    parse_simple_hive_schedule(schedule)
}

fn parse_simple_hive_schedule(schedule: &str) -> Option<String> {
    if let Some(rest) = schedule.strip_prefix("*/")
        && let Some((mins, tail)) = rest.split_once(' ')
        && tail == "* * * *"
        && let Ok(mins) = mins.parse::<u64>()
    {
        return Some(format!("{mins}m"));
    }

    if let Some(rest) = schedule.strip_prefix("0 */")
        && let Some((hours, tail)) = rest.split_once(' ')
        && tail == "* * *"
        && let Ok(hours) = hours.parse::<u64>()
    {
        return Some(format!("{hours}h"));
    }

    None
}

fn hive_bot_prompt(root: &Path, bot: &HiveBotConfig) -> Option<String> {
    let file_contents = bot
        .prompt_file
        .as_deref()
        .map(|path| root.join(path))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    match (bot.role.as_deref(), bot.color.as_deref(), file_contents) {
        (None, None, None) => None,
        (role, color, body) => {
            let mut sections = Vec::new();
            if let Some(role) = role
                && !role.trim().is_empty()
            {
                sections.push(format!("Role: {}", role.trim()));
            }
            if let Some(color) = color
                && !color.trim().is_empty()
            {
                sections.push(format!("Color: {}", color.trim()));
            }
            if let Some(body) = body {
                sections.push(body);
            }
            Some(sections.join("\n\n"))
        }
    }
}

pub fn detect_workspace_file_format(path: &Path) -> Result<WorkspaceFileFormat> {
    let contents = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    detect_workspace_file_format_from_str(&contents)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))
}

fn detect_workspace_file_format_from_str(contents: &str) -> Result<WorkspaceFileFormat> {
    if toml::from_str::<WorkspaceConfig>(contents).is_ok() {
        return Ok(WorkspaceFileFormat::Current);
    }
    if toml::from_str::<HiveWorkspaceFile>(contents).is_ok() {
        return Ok(WorkspaceFileFormat::HiveCompat);
    }
    color_eyre::eyre::bail!("workspace config does not match current or Hive-compatible schema");
}

fn hive_workspace_references_service(hive_config: &HiveWorkspaceFile, service: &str) -> bool {
    hive_config.bots.iter().any(|bot| {
        bot.watch.iter().any(|s| s == service) || bot.services.iter().any(|s| s == service)
    })
}

fn hydrate_legacy_service_watchers(config: &mut WorkspaceConfig, hive_config: &HiveWorkspaceFile) {
    if config.watchers.sentry.is_none() && hive_workspace_references_service(hive_config, "sentry")
    {
        if let Some(sentry) = load_legacy_sentry_credentials(&config.root) {
            config.watchers.sentry = Some(SentryWatcherConfig {
                org: sentry.org,
                project: sentry.project,
                token: sentry.token,
                interval_secs: default_watcher_interval(),
                active_hours: None,
            });
        }
    }
}

fn load_legacy_sentry_credentials(root: &Path) -> Option<LegacySentryServiceConfig> {
    let services_path = root.join(".apiari/services.toml");
    if let Ok(contents) = std::fs::read_to_string(&services_path)
        && let Ok(services) = toml::from_str::<LegacyServicesFile>(&contents)
        && let Some(sentry) = services.sentry
    {
        return Some(sentry);
    }

    let hive_workspace_path = root.join(".hive/workspace.yaml");
    if let Ok(contents) = std::fs::read_to_string(&hive_workspace_path)
        && let Ok(workspace) = serde_yaml::from_str::<LegacyHiveWorkspaceYaml>(&contents)
        && let Some(buzz) = workspace.buzz
        && let Some(sentry) = buzz.sentry
    {
        return Some(sentry);
    }

    None
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

/// Find the workspace whose root most specifically (longest prefix) contains
/// `cwd`. Returns `None` if no workspace root is a prefix of `cwd`.
///
/// Using longest-prefix matching ensures nested workspace roots resolve to the
/// most specific workspace, which is the correct and consistent behavior.
pub fn workspace_for_cwd<'a>(
    workspaces: &'a [Workspace],
    cwd: &std::path::Path,
) -> Option<&'a Workspace> {
    workspaces
        .iter()
        .filter(|ws| cwd.starts_with(&ws.config.root))
        .max_by_key(|ws| ws.config.root.as_os_str().len())
}

/// Maximum directory depth for recursive repo discovery.
const MAX_DISCOVER_DEPTH: u32 = 4;

/// Auto-discover git repos under `root`, recursively up to [`MAX_DISCOVER_DEPTH`] levels.
///
/// Skips hidden dirs, `target/`, and `node_modules/`. For each dir containing `.git/`,
/// tries to extract a GitHub `org/repo` slug from the origin remote; falls back to the
/// directory name. Does not recurse into directories that are themselves git repos.
pub fn discover_repos(root: &Path) -> Vec<String> {
    let mut repos = Vec::new();
    discover_repos_recursive(root, 0, &mut repos);
    repos.sort();
    repos.dedup();
    repos
}

fn discover_repos_recursive(dir: &Path, depth: u32, repos: &mut Vec<String>) {
    if depth >= MAX_DISCOVER_DEPTH {
        return;
    }

    // If a non-root dir is itself a git repo, don't descend into it.
    // (The caller already added this dir as a discovered repo.)
    if depth > 0 && dir.join(".git").exists() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !entry.file_type().is_ok_and(|ft| ft.is_dir())
            || name_str.starts_with('.')
            || name_str == "target"
            || name_str == "node_modules"
        {
            continue;
        }

        let path = entry.path();
        if path.join(".git").exists() {
            let slug = extract_github_slug(&path)
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            repos.push(slug);
            // Don't recurse into git repos
        } else {
            discover_repos_recursive(&path, depth + 1, repos);
        }
    }
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
    let review_queue_names: Vec<String> = config
        .watchers
        .github
        .as_ref()
        .map(|g| g.review_queue.iter().map(|e| e.name.clone()).collect())
        .unwrap_or_default();
    let linear_names: Vec<String> = config
        .watchers
        .linear
        .iter()
        .map(|l| l.name.clone())
        .collect();
    let email_names: Vec<String> = config
        .watchers
        .email
        .iter()
        .map(|e| e.name.clone())
        .collect();
    let notion_names: Vec<String> = config
        .watchers
        .notion
        .iter()
        .map(|n| n.name.clone())
        .collect();
    let script_names: Vec<String> = config
        .watchers
        .script
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let resolved_caps = config.capabilities.resolved(config.authority);
    let has_swarm_runtime = config.resolved_swarm_dir().exists();
    crate::buzz::coordinator::skills::SkillContext {
        workspace_name: workspace_name.to_string(),
        workspace_root: config.root.clone(),
        config_path: workspaces_dir().join(format!("{workspace_name}.toml")),
        repos,
        has_sentry: config.watchers.sentry.is_some(),
        has_swarm_runtime,
        can_dispatch_workers: resolved_caps.dispatch_workers,
        has_review_queue: !review_queue_names.is_empty(),
        review_queue_names,
        has_linear: !linear_names.is_empty(),
        linear_names,
        has_email: !email_names.is_empty(),
        email_names,
        has_notion: !notion_names.is_empty(),
        notion_names,
        has_scripts: !script_names.is_empty(),
        script_names,
        has_telegram: config.telegram.is_some(),
        prompt_preamble: config.coordinator.prompt.clone(),
        default_agent: config.swarm.default_agent.clone(),
        authority: config.authority,
        capabilities: resolved_caps,
    }
}

/// Convert a WorkspaceConfig into a buzz BuzzConfig for watcher/coordinator use.
pub fn to_buzz_config(ws: &WorkspaceConfig) -> crate::buzz::config::BuzzConfig {
    crate::buzz::config::BuzzConfig {
        telegram: ws
            .telegram
            .as_ref()
            .map(|t| crate::buzz::config::TelegramConfig {
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
                    repos: if !g.repos.is_empty() {
                        g.repos.clone()
                    } else {
                        resolve_repos(ws)
                    },
                    watch_labels: vec![],
                    review_queue: g
                        .review_queue
                        .iter()
                        .map(|e| crate::buzz::config::ReviewQueueEntry {
                            name: e.name.clone(),
                            query: e.query.clone(),
                        })
                        .collect(),
                    filters: g.filters.clone(),
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
                    state_path: ws.resolved_swarm_state_path(),
                }),
            email: ws
                .watchers
                .email
                .iter()
                .map(|e| crate::buzz::config::EmailMailboxConfig {
                    name: e.name.clone(),
                    host: e.host.clone(),
                    port: e.port,
                    tls: e.tls,
                    username: e.username.clone(),
                    password: e.password.clone(),
                    folder: e.folder.clone(),
                    filter: e.filter.clone(),
                    include_body: e.include_body,
                    max_fetch: e.max_fetch,
                    interval_secs: e.interval_secs,
                    summarizer: e.summarizer.as_ref().map(|s| {
                        crate::buzz::config::EmailSummarizerConfig {
                            base_url: s.base_url.clone(),
                            model: s.model.clone(),
                        }
                    }),
                })
                .collect(),
            notion: ws
                .watchers
                .notion
                .iter()
                .map(|n| crate::buzz::config::NotionWatcherConfig {
                    name: n.name.clone(),
                    token: n.token.clone(),
                    user_id: n.user_id.clone(),
                    poll_database_ids: n.poll_database_ids.clone(),
                    interval_secs: n.interval_secs,
                })
                .collect(),
            linear: ws
                .watchers
                .linear
                .iter()
                .map(|l| crate::buzz::config::LinearWatcherConfig {
                    name: l.name.clone(),
                    api_key: l.api_key.clone(),
                    poll_interval_secs: l.poll_interval_secs,
                    review_queue: l
                        .review_queue
                        .iter()
                        .map(|e| crate::buzz::config::LinearReviewQueueEntry {
                            name: e.name.clone(),
                            query: e.query.clone(),
                        })
                        .collect(),
                })
                .collect(),
            script: ws
                .watchers
                .script
                .iter()
                .map(|s| crate::buzz::config::ScriptWatcherConfig {
                    name: s.name.clone(),
                    command: s.command.clone(),
                    interval_secs: s.interval_secs,
                    emit_on_change: s.emit_on_change,
                    severity_on_fail: s.severity_on_fail.clone(),
                    timeout_secs: s.timeout_secs,
                })
                .collect(),
        },
        coordinator: crate::buzz::config::CoordinatorConfig {
            model: normalize_model_identifier(&ws.coordinator.provider, &ws.coordinator.model),
            max_turns: ws.coordinator.max_turns,
        },
        morning_brief: ws.morning_brief.as_ref().map(|mb| {
            crate::buzz::config::MorningBriefConfig {
                enabled: mb.enabled,
                time: mb.time.clone(),
                timezone: mb.timezone.clone(),
            }
        }),
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

/// Swarm agent configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Default agent for swarm workers: "claude", "codex", "gemini", or "auto".
    /// When "auto", selects an available agent automatically based on the environment.
    #[serde(default = "default_swarm_agent")]
    pub default_agent: String,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            default_agent: default_swarm_agent(),
        }
    }
}

fn default_swarm_agent() -> String {
    "claude".to_string()
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

fn default_coordinator_name() -> String {
    "Bee".to_string()
}

fn default_model() -> String {
    "sonnet".to_string()
}

fn default_max_turns() -> u32 {
    20
}

fn default_max_session_turns() -> u32 {
    50
}

fn default_provider() -> String {
    "claude".to_string()
}

fn normalize_model_identifier(provider: &str, model: &str) -> String {
    let trimmed = model.trim();

    if provider == "codex" && trimmed == default_model() {
        return String::new();
    }

    if provider == "claude"
        && matches!(
            trimmed,
            "claude-sonnet-4.6" | "claude-sonnet-4-6" | "claude-sonnet-4"
        )
    {
        return default_model();
    }

    trimmed.to_string()
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
        assert!(
            config.config_version.is_none(),
            "missing config_version should default to None"
        );
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
        assert!(
            config.config_version.is_none(),
            "missing config_version should default to None"
        );
        assert!(config.repos.is_empty());
        assert!(config.telegram.is_none());
        assert_eq!(config.coordinator.model, "sonnet");
        assert_eq!(config.coordinator.max_turns, 20);
        assert_eq!(config.coordinator.max_session_turns, 50);
        assert_eq!(config.coordinator.signal_hooks.len(), 3);
        assert_eq!(config.coordinator.signal_hooks[0].source, "swarm");
        assert_eq!(
            config.coordinator.signal_hooks[1].source,
            "github_bot_review"
        );
        assert_eq!(config.coordinator.signal_hooks[2].source, "github");
    }

    #[test]
    fn test_parse_hive_workspace_format() {
        let toml_str = r##"
[workspace]
root = "/Users/josh/Developer/apiari"
name = "apiari"
default_agent = "codex"

[[bots]]
name = "Claude"
provider = "claude"
model = "claude-sonnet-4.6"

[[bots]]
name = "Codex"
provider = "codex"
"##;

        let config = toml::from_str::<HiveWorkspaceFile>(toml_str).unwrap();
        let config: WorkspaceConfig = config.into();

        assert_eq!(config.root, PathBuf::from("/Users/josh/Developer/apiari"));
        assert_eq!(config.swarm.default_agent, "codex");
        let bees = config.resolved_bees();
        assert_eq!(bees.len(), 3);
        assert_eq!(bees[0].name, "Bee");
        assert_eq!(bees[0].provider, "claude");
        assert_eq!(bees[1].name, "Claude");
        assert_eq!(bees[1].provider, "claude");
        assert_eq!(bees[1].model, "sonnet");
        assert_eq!(bees[2].name, "Codex");
        assert_eq!(bees[2].provider, "codex");
    }

    #[test]
    fn test_resolved_bees_normalizes_legacy_claude_model_aliases() {
        let toml_str = r#"
root = "/Users/josh/Developer/apiari"

[coordinator]
provider = "claude"
model = "claude-sonnet-4.6"
"#;

        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bees = config.resolved_bees();
        assert_eq!(bees.len(), 1);
        assert_eq!(bees[0].provider, "claude");
        assert_eq!(bees[0].model, "sonnet");
    }

    #[test]
    fn test_load_workspace_preserves_hive_bot_behavior_fields() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mgm");
        std::fs::create_dir_all(root.join(".apiari/bots")).unwrap();
        std::fs::write(
            root.join(".apiari/bots/triage.md"),
            "# Triage\n\nFocus on new issues and decide what to do.",
        )
        .unwrap();

        let config_path = temp.path().join("mgm.toml");
        std::fs::write(
            &config_path,
            format!(
                r##"
[workspace]
root = "{}"

[[bots]]
name = "Triage"
role = "Detect, triage, and fix Sentry issues"
color = "#f59e0b"
prompt_file = ".apiari/bots/triage.md"
watch = ["sentry"]
schedule = "*/30 * * * *"
proactive_prompt = "Check for new issues."
"##,
                root.display()
            ),
        )
        .unwrap();

        let config = load_workspace(&config_path).unwrap();
        let bees = config.resolved_bees();
        let triage = bees.iter().find(|bee| bee.name == "Triage").unwrap();
        assert_eq!(triage.heartbeat.as_deref(), Some("30m"));
        assert_eq!(
            triage.heartbeat_prompt.as_deref(),
            Some("Check for new issues.")
        );
        assert_eq!(triage.signal_hooks.len(), 1);
        assert_eq!(triage.signal_hooks[0].source, "sentry");
        let prompt = triage.prompt.as_deref().unwrap();
        assert!(prompt.contains("Role: Detect, triage, and fix Sentry issues"));
        assert!(prompt.contains("Color: #f59e0b"));
        assert!(prompt.contains("Focus on new issues and decide what to do."));
    }

    #[test]
    fn test_max_session_turns_explicit() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
max_session_turns = 100
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.max_session_turns, 100);
    }

    #[test]
    fn test_max_session_turns_zero_disables() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
max_session_turns = 0
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.max_session_turns, 0);
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
            config_version: None,
            root: "/tmp/nonexistent".into(),
            repos: vec!["Org/Repo".to_string()],
            authority: WorkspaceAuthority::default(),
            capabilities: WorkspaceCapabilities::default(),
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            bees: None,
            watchers: WatchersConfig::default(),
            swarm: SwarmConfig::default(),
            orchestrator: Default::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
            shells: ShellsConfig::default(),
            schedule: None,
            activity: ActivityConfig::default(),
        };
        assert_eq!(resolve_repos(&config), vec!["Org/Repo"]);
    }

    #[test]
    fn test_resolve_repos_empty_discovers() {
        // With a non-existent root, discover_repos returns empty
        let config = WorkspaceConfig {
            config_version: None,
            root: "/tmp/nonexistent-dir-12345".into(),
            repos: vec![],
            authority: WorkspaceAuthority::default(),
            capabilities: WorkspaceCapabilities::default(),
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            bees: None,
            watchers: WatchersConfig::default(),
            swarm: SwarmConfig::default(),
            orchestrator: Default::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
            shells: ShellsConfig::default(),
            schedule: None,
            activity: ActivityConfig::default(),
        };
        assert!(resolve_repos(&config).is_empty());
    }

    #[test]
    fn test_to_buzz_config() {
        let ws = WorkspaceConfig {
            config_version: None,
            root: "/tmp".into(),
            repos: vec![],
            authority: WorkspaceAuthority::default(),
            capabilities: WorkspaceCapabilities::default(),
            telegram: Some(TelegramConfig {
                bot_token: "tok".into(),
                chat_id: 123,
                topic_id: Some(42),
                allowed_user_ids: vec![],
            }),
            coordinator: CoordinatorConfig::default(),
            bees: None,
            watchers: WatchersConfig::default(),
            swarm: SwarmConfig::default(),
            orchestrator: Default::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
            shells: ShellsConfig::default(),
            schedule: None,
            activity: ActivityConfig::default(),
        };

        let buzz = to_buzz_config(&ws);
        assert!(buzz.telegram.is_some());
        assert_eq!(buzz.telegram.unwrap().chat_id, 123);
        assert_eq!(buzz.coordinator.model, "sonnet");
    }

    #[test]
    fn test_tcp_config_defaults_to_none() {
        let toml_str = r#"
            root = "/tmp/test"
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert!(config.daemon_tcp_port.is_none());
        assert!(config.daemon_tcp_bind.is_none());
        assert!(config.daemon_host.is_none());
        assert!(config.daemon_port.is_none());
    }

    #[test]
    fn test_tcp_config_daemon_tcp_port() {
        let toml_str = r#"
            root = "/tmp/test"
            daemon_tcp_port = 7474
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon_tcp_port, Some(7474));
        // bind defaults to None (resolved to 127.0.0.1 at runtime)
        assert!(config.daemon_tcp_bind.is_none());
        assert!(config.daemon_host.is_none());
        assert!(config.daemon_port.is_none());
    }

    #[test]
    fn test_tcp_config_custom_bind_address() {
        let toml_str = r#"
            root = "/tmp/test"
            daemon_tcp_port = 7474
            daemon_tcp_bind = "0.0.0.0"
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon_tcp_port, Some(7474));
        assert_eq!(config.daemon_tcp_bind.as_deref(), Some("0.0.0.0"));
    }

    #[test]
    fn test_tcp_config_remote_connection() {
        let toml_str = r#"
            root = "/tmp/test"
            daemon_host = "100.64.0.1"
            daemon_port = 7474
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert!(config.daemon_tcp_port.is_none());
        assert_eq!(config.daemon_host.as_deref(), Some("100.64.0.1"));
        assert_eq!(config.daemon_port, Some(7474));
    }

    #[test]
    fn test_tcp_config_full() {
        let toml_str = r#"
            root = "/tmp/test"
            daemon_tcp_port = 7474
            daemon_host = "myserver.ts.net"
            daemon_port = 7474
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon_tcp_port, Some(7474));
        assert_eq!(config.daemon_host.as_deref(), Some("myserver.ts.net"));
        assert_eq!(config.daemon_port, Some(7474));
    }

    #[test]
    fn test_load_workspace_hive_sentry_watcher_from_services_toml() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mgm");
        std::fs::create_dir_all(root.join(".apiari")).unwrap();
        std::fs::write(
            root.join(".apiari/services.toml"),
            r#"
[sentry]
org = "josh-holtz"
project = "mostly-good-metrics"
token = "sntryu_test"
"#,
        )
        .unwrap();

        let config_path = temp.path().join("mgm.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
[workspace]
root = "{}"

[[bots]]
name = "Triage"
watch = ["sentry"]
services = ["sentry"]
"#,
                root.display()
            ),
        )
        .unwrap();

        let config = load_workspace(&config_path).unwrap();
        let sentry = config.watchers.sentry.expect("expected sentry watcher");
        assert_eq!(sentry.org, "josh-holtz");
        assert_eq!(sentry.project, "mostly-good-metrics");
        assert_eq!(sentry.token, "sntryu_test");
    }

    #[test]
    fn test_load_workspace_hive_sentry_watcher_from_legacy_hive_workspace_yaml() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mgm");
        std::fs::create_dir_all(root.join(".hive")).unwrap();
        std::fs::write(
            root.join(".hive/workspace.yaml"),
            r#"
name: mgm
repos:
  - backend
buzz:
  sentry:
    token: "sntryu_legacy"
    org: "josh-holtz"
    project: "mostly-good-metrics"
"#,
        )
        .unwrap();

        let config_path = temp.path().join("mgm.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
[workspace]
root = "{}"

[[bots]]
name = "Triage"
watch = ["sentry"]
"#,
                root.display()
            ),
        )
        .unwrap();

        let config = load_workspace(&config_path).unwrap();
        let sentry = config.watchers.sentry.expect("expected sentry watcher");
        assert_eq!(sentry.org, "josh-holtz");
        assert_eq!(sentry.project, "mostly-good-metrics");
        assert_eq!(sentry.token, "sntryu_legacy");
    }

    #[test]
    fn test_tcp_remote_target_detection() {
        // Simulates the TUI logic for choosing Unix vs TCP
        let config_local: WorkspaceConfig = toml::from_str(r#"root = "/tmp""#).unwrap();
        let config_remote: WorkspaceConfig = toml::from_str(
            r#"
            root = "/tmp"
            daemon_host = "10.0.0.1"
            daemon_port = 7474
            "#,
        )
        .unwrap();

        // Local: no remote target
        let target = config_local
            .daemon_host
            .as_ref()
            .zip(config_local.daemon_port);
        assert!(target.is_none());

        // Remote: has target
        let target = config_remote
            .daemon_host
            .as_ref()
            .zip(config_remote.daemon_port);
        assert!(target.is_some());
        let (host, port) = target.unwrap();
        assert_eq!(host, "10.0.0.1");
        assert_eq!(port, 7474);
    }

    #[test]
    fn test_tcp_config_host_without_port_not_remote() {
        let config: WorkspaceConfig = toml::from_str(
            r#"
            root = "/tmp"
            daemon_host = "10.0.0.1"
            "#,
        )
        .unwrap();
        // Both host AND port needed for remote
        let target = config.daemon_host.as_ref().zip(config.daemon_port);
        assert!(target.is_none());
    }

    #[test]
    fn test_daemon_endpoints_array_parsing() {
        let toml_str = r#"
            root = "/tmp/test"

            [[daemon_endpoints]]
            host = "localhost"
            port = 7474

            [[daemon_endpoints]]
            host = "100.64.0.1"
            port = 7474

            [[daemon_endpoints]]
            host = "192.168.1.50"
            port = 7474
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon_endpoints.len(), 3);
        assert_eq!(config.daemon_endpoints[0].host, "localhost");
        assert_eq!(config.daemon_endpoints[0].port, 7474);
        assert_eq!(config.daemon_endpoints[1].host, "100.64.0.1");
        assert_eq!(config.daemon_endpoints[2].host, "192.168.1.50");
    }

    #[test]
    fn test_daemon_endpoints_default_port() {
        let toml_str = r#"
            root = "/tmp/test"

            [[daemon_endpoints]]
            host = "localhost"
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon_endpoints.len(), 1);
        assert_eq!(config.daemon_endpoints[0].port, 7474);
    }

    #[test]
    fn test_resolved_endpoints_from_array() {
        let toml_str = r#"
            root = "/tmp/test"

            [[daemon_endpoints]]
            host = "localhost"
            port = 7474

            [[daemon_endpoints]]
            host = "100.64.0.1"
            port = 7474
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let eps = config.resolved_daemon_endpoints();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].host, "localhost");
        assert_eq!(eps[1].host, "100.64.0.1");
    }

    #[test]
    fn test_resolved_endpoints_backward_compat() {
        let toml_str = r#"
            root = "/tmp/test"
            daemon_host = "10.0.0.1"
            daemon_port = 7474
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert!(config.daemon_endpoints.is_empty());
        let eps = config.resolved_daemon_endpoints();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].host, "10.0.0.1");
        assert_eq!(eps[0].port, 7474);
    }

    #[test]
    fn test_resolved_endpoints_array_takes_precedence() {
        let toml_str = r#"
            root = "/tmp/test"
            daemon_host = "10.0.0.1"
            daemon_port = 7474

            [[daemon_endpoints]]
            host = "localhost"
            port = 8080
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let eps = config.resolved_daemon_endpoints();
        // daemon_endpoints takes precedence over legacy fields
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].host, "localhost");
        assert_eq!(eps[0].port, 8080);
    }

    #[test]
    fn test_resolved_endpoints_empty_when_no_remote() {
        let config: WorkspaceConfig = toml::from_str(r#"root = "/tmp""#).unwrap();
        assert!(config.resolved_daemon_endpoints().is_empty());
    }

    #[test]
    fn test_resolved_endpoints_host_without_port_empty() {
        let toml_str = r#"
            root = "/tmp"
            daemon_host = "10.0.0.1"
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        // Legacy requires both host AND port
        assert!(config.resolved_daemon_endpoints().is_empty());
    }

    #[test]
    fn test_resolved_swarm_state_path_defaults_to_workspace_root() {
        let config: WorkspaceConfig = toml::from_str(r#"root = "/tmp/ws""#).unwrap();
        assert_eq!(
            config.resolved_swarm_state_path(),
            PathBuf::from("/tmp/ws/.swarm/state.json")
        );
        assert_eq!(config.resolved_swarm_dir(), PathBuf::from("/tmp/ws/.swarm"));
    }

    #[test]
    fn test_resolved_swarm_state_path_resolves_relative_override() {
        let toml_str = r#"
            root = "/tmp/ws"

            [watchers.swarm]
            state_path = ".swarm/custom-state.json"
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.resolved_swarm_state_path(),
            PathBuf::from("/tmp/ws/.swarm/custom-state.json")
        );
        assert_eq!(config.resolved_swarm_dir(), PathBuf::from("/tmp/ws/.swarm"));
    }

    #[test]
    fn test_resolved_swarm_state_path_keeps_absolute_override() {
        let toml_str = r#"
            root = "/tmp/ws"

            [watchers.swarm]
            state_path = "/var/tmp/swarm/state.json"
        "#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.resolved_swarm_state_path(),
            PathBuf::from("/var/tmp/swarm/state.json")
        );
        assert_eq!(config.resolved_swarm_dir(), PathBuf::from("/var/tmp/swarm"));
    }

    /// Helper to build a minimal WorkspaceConfig with a GitHub watcher for repo resolution tests.
    fn ws_with_github(ws_repos: Vec<String>, gh_repos: Vec<String>) -> WorkspaceConfig {
        WorkspaceConfig {
            config_version: None,
            root: "/nonexistent".into(),
            repos: ws_repos,
            authority: WorkspaceAuthority::default(),
            capabilities: WorkspaceCapabilities::default(),
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            bees: None,
            watchers: WatchersConfig {
                github: Some(GithubWatcherConfig {
                    repos: gh_repos,
                    interval_secs: default_watcher_interval(),
                    review_queue: vec![],
                    filters: HashMap::new(),
                    active_hours: None,
                }),
                ..Default::default()
            },
            swarm: SwarmConfig::default(),
            orchestrator: Default::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
            shells: ShellsConfig::default(),
            schedule: None,
            activity: ActivityConfig::default(),
        }
    }

    #[test]
    fn test_buzz_github_repos_watcher_override() {
        let ws = ws_with_github(vec!["Org/Workspace".into()], vec!["Org/Override".into()]);
        let gh = to_buzz_config(&ws).watchers.github.unwrap();
        assert_eq!(gh.repos, vec!["Org/Override"]);
    }

    #[test]
    fn test_buzz_github_repos_workspace_fallback() {
        let ws = ws_with_github(vec!["Org/Workspace".into()], vec![]);
        let gh = to_buzz_config(&ws).watchers.github.unwrap();
        assert_eq!(gh.repos, vec!["Org/Workspace"]);
    }

    #[test]
    fn test_discover_repos_recursive() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Depth-1 repo: root/repo_a/.git
        fs::create_dir_all(root.join("repo_a/.git")).unwrap();

        // Depth-2 repo: root/projects/repo_b/.git
        fs::create_dir_all(root.join("projects/repo_b/.git")).unwrap();

        // Non-git dir (traversed but not added as a repo): root/projects/not_a_repo/
        fs::create_dir_all(root.join("projects/not_a_repo")).unwrap();

        // Hidden dir should be skipped: root/.hidden/repo_c/.git
        fs::create_dir_all(root.join(".hidden/repo_c/.git")).unwrap();

        // node_modules should be skipped: root/node_modules/pkg/.git
        fs::create_dir_all(root.join("node_modules/pkg/.git")).unwrap();

        let repos = discover_repos(root);

        // Should find repo_a and repo_b (no git remote, so falls back to dir name)
        assert!(
            repos.contains(&"repo_a".to_string()),
            "missing repo_a: {repos:?}"
        );
        assert!(
            repos.contains(&"repo_b".to_string()),
            "missing repo_b: {repos:?}"
        );
        assert_eq!(repos.len(), 2, "unexpected repos: {repos:?}");
    }

    #[test]
    fn test_discover_repos_does_not_recurse_into_git_repos() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // A git repo with a nested git repo inside it
        fs::create_dir_all(root.join("outer/.git")).unwrap();
        fs::create_dir_all(root.join("outer/inner/.git")).unwrap();

        let repos = discover_repos(root);

        // Should only find "outer", not "inner"
        assert_eq!(repos, vec!["outer".to_string()]);
    }

    #[test]
    fn test_discover_repos_root_is_git_repo() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Root itself is a git repo (e.g. monorepo workspace root)
        fs::create_dir_all(root.join(".git")).unwrap();

        // Child repo inside the root
        fs::create_dir_all(root.join("child_repo/.git")).unwrap();

        let repos = discover_repos(root);

        // Should still find child repos even when root has .git
        assert_eq!(repos, vec!["child_repo".to_string()]);
    }

    #[test]
    fn test_discover_repos_max_depth_boundary() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Repo at exactly MAX_DISCOVER_DEPTH (4 levels of dirs before .git):
        // root/a/b/c/d/repo_at_max/.git — depth counted as: a=0, b=1, c=2, d=3 → repo found at scan depth 3
        // That's within the limit (depth < 4).
        fs::create_dir_all(root.join("a/b/c/repo_at_limit/.git")).unwrap();

        // Repo beyond MAX_DISCOVER_DEPTH:
        // root/a/b/c/d/repo_too_deep/.git — would require depth=4 scan, which is >= MAX_DISCOVER_DEPTH
        fs::create_dir_all(root.join("a/b/c/d/repo_too_deep/.git")).unwrap();

        let repos = discover_repos(root);

        assert!(
            repos.contains(&"repo_at_limit".to_string()),
            "should find repo at depth 3: {repos:?}"
        );
        assert!(
            !repos.contains(&"repo_too_deep".to_string()),
            "should NOT find repo beyond max depth: {repos:?}"
        );
    }

    #[test]
    fn test_buzz_github_repos_discovery_fallback() {
        let ws = ws_with_github(vec![], vec![]);
        let gh = to_buzz_config(&ws).watchers.github.unwrap();
        // Nonexistent root means discover_repos returns empty
        assert!(gh.repos.is_empty());
    }

    #[test]
    fn test_buzz_swarm_state_path_is_resolved_against_workspace_root() {
        let config: WorkspaceConfig = toml::from_str(
            r#"
            root = "/tmp/ws"

            [watchers.swarm]
            state_path = ".swarm/custom-state.json"
            interval_secs = 15
            "#,
        )
        .unwrap();

        let swarm = to_buzz_config(&config).watchers.swarm.unwrap();
        assert_eq!(
            swarm.state_path,
            PathBuf::from("/tmp/ws/.swarm/custom-state.json")
        );
    }

    #[test]
    fn test_signal_hooks_default() {
        let toml_str = r#"root = "/tmp/test""#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.signal_hooks.len(), 3);
        assert_eq!(config.coordinator.signal_hooks[0].source, "swarm");
        assert!(config.coordinator.signal_hooks[0].action.is_some());
        assert_eq!(config.coordinator.signal_hooks[0].ttl_secs, 300);
        assert_eq!(
            config.coordinator.signal_hooks[1].source,
            "github_bot_review"
        );
        assert!(config.coordinator.signal_hooks[1].action.is_some());
        assert_eq!(config.coordinator.signal_hooks[1].ttl_secs, 300);
        assert_eq!(config.coordinator.signal_hooks[2].source, "github");
        assert!(config.coordinator.signal_hooks[2].action.is_some());
    }

    #[test]
    fn test_signal_hooks_explicit() {
        let toml_str = r#"
root = "/tmp/test"

[[coordinator.signal_hooks]]
source = "swarm"
ttl_secs = 60

[[coordinator.signal_hooks]]
source = "github"
prompt = "CI failed: {events}"
action = "Dispatch a worker to fix the CI failure."
ttl_secs = 300
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.signal_hooks.len(), 2);
        assert_eq!(config.coordinator.signal_hooks[0].source, "swarm");
        assert_eq!(config.coordinator.signal_hooks[0].ttl_secs, 60);
        // No action specified → defaults to None
        assert!(config.coordinator.signal_hooks[0].action.is_none());
        assert_eq!(config.coordinator.signal_hooks[1].source, "github");
        assert_eq!(
            config.coordinator.signal_hooks[1].prompt,
            "CI failed: {events}"
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].action.as_deref(),
            Some("Dispatch a worker to fix the CI failure.")
        );
        assert_eq!(config.coordinator.signal_hooks[1].ttl_secs, 300);
    }

    #[test]
    fn test_signal_hook_action_string() {
        let toml_str = r#"
root = "/tmp/test"

[[coordinator.signal_hooks]]
source = "a"

[[coordinator.signal_hooks]]
source = "b"
action = "Find the worker and forward the review."
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.signal_hooks.len(), 2);
        // No action → None (notify only)
        assert!(config.coordinator.signal_hooks[0].action.is_none());
        // With action → Some(string)
        assert_eq!(
            config.coordinator.signal_hooks[1].action.as_deref(),
            Some("Find the worker and forward the review.")
        );
    }

    #[test]
    fn test_signal_hooks_empty_array() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
signal_hooks = []
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert!(config.coordinator.signal_hooks.is_empty());
    }

    #[test]
    fn test_coordinator_config_provider_default() {
        let toml_str = r#"root = "/tmp/test""#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.provider, "claude");
    }

    #[test]
    fn test_coordinator_config_provider_codex() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
provider = "codex"
model = "o4-mini"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.provider, "codex");
        assert_eq!(config.coordinator.model, "o4-mini");
    }

    #[test]
    fn test_coordinator_config_provider_gemini() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
provider = "gemini"
model = "gemini-2.0-flash"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.provider, "gemini");
    }

    // -- resolved_bees tests --

    #[test]
    fn test_resolved_bees_single_coordinator() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
name = "Bee"
model = "sonnet"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bees = config.resolved_bees();
        assert_eq!(bees.len(), 1);
        assert_eq!(bees[0].name, "Bee");
        assert_eq!(bees[0].model, "sonnet");
        assert_eq!(bees[0].provider, "claude");
    }

    #[test]
    fn test_resolved_bees_multi_bee() {
        let toml_str = r#"
root = "/tmp/test"

[[bees]]
name = "CodeBee"
provider = "claude"
model = "sonnet"

[[bees]]
name = "CustomerBee"
provider = "gemini"
model = "gemini-2.0-flash"
topic_id = 456
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bees = config.resolved_bees();
        assert_eq!(bees.len(), 2);
        assert_eq!(bees[0].name, "CodeBee");
        assert_eq!(bees[0].provider, "claude");
        assert_eq!(bees[1].name, "CustomerBee");
        assert_eq!(bees[1].provider, "gemini");
        assert_eq!(bees[1].topic_id, Some(456));
    }

    #[test]
    fn test_resolved_bees_overrides_coordinator() {
        let toml_str = r#"
root = "/tmp/test"
[coordinator]
name = "Ignored"
model = "opus"

[[bees]]
name = "OnlyBee"
model = "sonnet"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bees = config.resolved_bees();
        assert_eq!(bees.len(), 1);
        assert_eq!(bees[0].name, "OnlyBee");
        assert_eq!(bees[0].model, "sonnet");
    }

    #[test]
    fn test_resolved_bees_inherits_telegram_topic() {
        let toml_str = r#"
root = "/tmp/test"
[telegram]
bot_token = "test"
chat_id = 123
topic_id = 789
[coordinator]
name = "Bee"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bees = config.resolved_bees();
        assert_eq!(bees[0].topic_id, Some(789));
    }

    // -- MergePrsPolicy parsing tests --

    fn parse_merge_prs(value: &str) -> Result<MergePrsPolicy, toml::de::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            merge_prs: MergePrsPolicy,
        }
        let s = format!("merge_prs = {value}");
        toml::from_str::<Wrapper>(&s).map(|w| w.merge_prs)
    }

    #[test]
    fn test_merge_prs_false_is_never() {
        assert_eq!(parse_merge_prs("false").unwrap(), MergePrsPolicy::Never);
    }

    #[test]
    fn test_merge_prs_true_is_on_command() {
        assert_eq!(parse_merge_prs("true").unwrap(), MergePrsPolicy::OnCommand);
    }

    #[test]
    fn test_merge_prs_string_never() {
        assert_eq!(parse_merge_prs("\"never\"").unwrap(), MergePrsPolicy::Never);
    }

    #[test]
    fn test_merge_prs_string_on_command() {
        assert_eq!(
            parse_merge_prs("\"on_command\"").unwrap(),
            MergePrsPolicy::OnCommand
        );
    }

    #[test]
    fn test_merge_prs_string_autonomous() {
        assert_eq!(
            parse_merge_prs("\"autonomous\"").unwrap(),
            MergePrsPolicy::Autonomous
        );
    }

    #[test]
    fn test_merge_prs_empty_branch_list_is_never() {
        assert_eq!(parse_merge_prs("[]").unwrap(), MergePrsPolicy::Never);
    }

    #[test]
    fn test_merge_prs_nonempty_branch_list_is_on_command() {
        assert_eq!(
            parse_merge_prs("[\"main\"]").unwrap(),
            MergePrsPolicy::OnCommand
        );
    }

    #[test]
    fn test_merge_prs_unknown_string_is_error() {
        assert!(
            parse_merge_prs("\"typo\"").is_err(),
            "unknown string should fail to parse"
        );
    }

    #[test]
    fn test_merge_prs_default_is_never() {
        let config: WorkspaceConfig = toml::from_str("root = \"/tmp\"").unwrap();
        assert_eq!(config.capabilities.merge_prs, MergePrsPolicy::Never);
    }

    // ── Heartbeat duration parsing ──

    fn default_bee() -> BeeConfig {
        BeeConfig {
            name: "TestBee".into(),
            role: None,
            color: None,
            execution_policy: BeeExecutionPolicy::default(),
            provider: "claude".into(),
            model: "sonnet".into(),
            max_turns: 20,
            prompt: None,
            max_session_turns: 50,
            signal_hooks: vec![],
            topic_id: None,
            heartbeat: None,
            heartbeat_prompt: None,
        }
    }

    #[test]
    fn test_heartbeat_duration_minutes() {
        let bee = BeeConfig {
            heartbeat: Some("30m".into()),
            ..default_bee()
        };
        assert_eq!(
            bee.heartbeat_duration(),
            Some(std::time::Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_heartbeat_duration_hours() {
        let bee = BeeConfig {
            heartbeat: Some("4h".into()),
            ..default_bee()
        };
        assert_eq!(
            bee.heartbeat_duration(),
            Some(std::time::Duration::from_secs(4 * 3600))
        );
    }

    #[test]
    fn test_heartbeat_duration_seconds() {
        let bee = BeeConfig {
            heartbeat: Some("30s".into()),
            ..default_bee()
        };
        assert_eq!(
            bee.heartbeat_duration(),
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn test_heartbeat_duration_none() {
        let bee = BeeConfig {
            heartbeat: None,
            ..default_bee()
        };
        assert_eq!(bee.heartbeat_duration(), None);
    }

    #[test]
    fn test_heartbeat_duration_invalid() {
        let bee = BeeConfig {
            heartbeat: Some("abc".into()),
            ..default_bee()
        };
        assert_eq!(bee.heartbeat_duration(), None);
    }

    #[test]
    fn test_bee_execution_policy_respects_workspace_observe_ceiling() {
        assert_eq!(
            BeeExecutionPolicy::Autonomous.resolved(WorkspaceAuthority::Observe),
            BeeExecutionPolicy::Observe
        );
        assert_eq!(
            BeeExecutionPolicy::DispatchOnly.resolved(WorkspaceAuthority::Observe),
            BeeExecutionPolicy::Observe
        );
        assert_eq!(
            BeeExecutionPolicy::DispatchOnly.resolved(WorkspaceAuthority::Autonomous),
            BeeExecutionPolicy::DispatchOnly
        );
    }
}

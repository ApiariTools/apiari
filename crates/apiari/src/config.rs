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
            model: default_model(),
            max_turns: default_max_turns(),
            prompt: None,
            max_session_turns: default_max_session_turns(),
            signal_hooks: default_signal_hooks(),
        }
    }
}

/// Configuration for a signal hook — triggers coordinator follow-through when
/// signals from the specified source arrive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalHookConfig {
    /// Signal source to match (e.g. "swarm", "github_bot_review", "github_ci_failure").
    /// Matches exactly, or as a prefix with `_` separator (e.g. "github" matches
    /// "github_ci_failure", "github_bot_review", etc.).
    /// The first matching hook wins when multiple hooks could match a signal.
    pub source: String,
    /// Prompt template sent to coordinator. Supports {source} and {events} placeholders.
    /// Empty string = use default formatting.
    #[serde(default)]
    pub prompt: String,
    /// Action the coordinator should take when this hook fires.
    /// Default: `notify` (just send a message).
    #[serde(default)]
    pub action: SignalHookAction,
    /// Max seconds to wait in queue before dropping. Default: 120.
    #[serde(default = "default_hook_ttl")]
    pub ttl_secs: u64,
}

/// Action type for signal hooks — determines what the coordinator should DO
/// when signals arrive, beyond just narrating what happened.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalHookAction {
    /// Just notify via Telegram/TUI (current default behavior).
    #[default]
    Notify,
    /// Identify the failing worker/PR and dispatch a fix worker or send
    /// the error to an existing worker.
    AutoFix,
    /// Find the relevant swarm worker for the PR and forward the
    /// review/comments to it.
    ForwardToWorker,
    /// Assess the situation and decide whether to act or just notify.
    Triage,
}

fn default_hook_ttl() -> u64 {
    120
}

fn default_signal_hooks() -> Vec<SignalHookConfig> {
    vec![
        SignalHookConfig {
            source: "swarm".into(),
            prompt: String::new(),
            action: SignalHookAction::Triage,
            ttl_secs: default_hook_ttl(),
        },
        SignalHookConfig {
            source: "github_bot_review".into(),
            prompt: "Bot code review: {events}".into(),
            action: SignalHookAction::ForwardToWorker,
            ttl_secs: 300,
        },
        SignalHookConfig {
            source: "github_ci_failure".into(),
            prompt: "CI failed: {events}".into(),
            action: SignalHookAction::AutoFix,
            ttl_secs: 300,
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
}

/// A single review queue query for Linear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearReviewQueueEntry {
    pub name: String,
    pub query: String,
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
}

/// Swarm watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmWatcherConfig {
    pub state_path: PathBuf,
    #[serde(default = "default_swarm_interval")]
    pub interval_secs: u64,
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
    crate::buzz::coordinator::skills::SkillContext {
        workspace_name: workspace_name.to_string(),
        workspace_root: config.root.clone(),
        config_path: workspaces_dir().join(format!("{workspace_name}.toml")),
        repos,
        has_sentry: config.watchers.sentry.is_some(),
        has_swarm: config.watchers.swarm.is_some(),
        has_review_queue: !review_queue_names.is_empty(),
        review_queue_names,
        has_linear: !linear_names.is_empty(),
        linear_names,
        has_email: !email_names.is_empty(),
        email_names,
        has_notion: !notion_names.is_empty(),
        notion_names,
        has_telegram: config.telegram.is_some(),
        prompt_preamble: config.coordinator.prompt.clone(),
        default_agent: config.swarm.default_agent.clone(),
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
        },
        coordinator: crate::buzz::config::CoordinatorConfig {
            model: ws.coordinator.model.clone(),
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

/// Swarm agent configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Default agent for swarm workers: "claude", "codex", or "auto".
    /// When "auto", prefers claude if both binaries are available.
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

/// Convert pipeline config rules into buzz pipeline rules.
pub fn to_pipeline_rules(
    config: &PipelineConfig,
) -> Vec<crate::buzz::pipeline::rule::PipelineRule> {
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

fn default_max_session_turns() -> u32 {
    50
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
        assert_eq!(config.coordinator.max_session_turns, 50);
        assert_eq!(config.coordinator.signal_hooks.len(), 3);
        assert_eq!(config.coordinator.signal_hooks[0].source, "swarm");
        assert_eq!(
            config.coordinator.signal_hooks[1].source,
            "github_bot_review"
        );
        assert_eq!(
            config.coordinator.signal_hooks[2].source,
            "github_ci_failure"
        );
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
            root: "/tmp/nonexistent".into(),
            repos: vec!["Org/Repo".to_string()],
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            watchers: WatchersConfig::default(),
            swarm: SwarmConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
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
            swarm: SwarmConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
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
            swarm: SwarmConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
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

    /// Helper to build a minimal WorkspaceConfig with a GitHub watcher for repo resolution tests.
    fn ws_with_github(ws_repos: Vec<String>, gh_repos: Vec<String>) -> WorkspaceConfig {
        WorkspaceConfig {
            root: "/nonexistent".into(),
            repos: ws_repos,
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            watchers: WatchersConfig {
                github: Some(GithubWatcherConfig {
                    repos: gh_repos,
                    interval_secs: default_watcher_interval(),
                    review_queue: vec![],
                }),
                ..Default::default()
            },
            swarm: SwarmConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
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
    fn test_signal_hooks_default() {
        let toml_str = r#"root = "/tmp/test""#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.signal_hooks.len(), 3);
        assert_eq!(config.coordinator.signal_hooks[0].source, "swarm");
        assert!(config.coordinator.signal_hooks[0].prompt.is_empty());
        assert_eq!(
            config.coordinator.signal_hooks[0].action,
            SignalHookAction::Triage
        );
        assert_eq!(config.coordinator.signal_hooks[0].ttl_secs, 120);
        assert_eq!(
            config.coordinator.signal_hooks[1].source,
            "github_bot_review"
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].prompt,
            "Bot code review: {events}"
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].action,
            SignalHookAction::ForwardToWorker
        );
        assert_eq!(config.coordinator.signal_hooks[1].ttl_secs, 300);
        assert_eq!(
            config.coordinator.signal_hooks[2].source,
            "github_ci_failure"
        );
        assert_eq!(
            config.coordinator.signal_hooks[2].action,
            SignalHookAction::AutoFix
        );
    }

    #[test]
    fn test_signal_hooks_explicit() {
        let toml_str = r#"
root = "/tmp/test"

[[coordinator.signal_hooks]]
source = "swarm"
ttl_secs = 60

[[coordinator.signal_hooks]]
source = "github_ci_failure"
prompt = "CI failed: {title}"
action = "auto_fix"
ttl_secs = 300
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.signal_hooks.len(), 2);
        assert_eq!(config.coordinator.signal_hooks[0].source, "swarm");
        assert_eq!(config.coordinator.signal_hooks[0].ttl_secs, 60);
        // No action specified → defaults to Notify
        assert_eq!(
            config.coordinator.signal_hooks[0].action,
            SignalHookAction::Notify
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].source,
            "github_ci_failure"
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].prompt,
            "CI failed: {title}"
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].action,
            SignalHookAction::AutoFix
        );
        assert_eq!(config.coordinator.signal_hooks[1].ttl_secs, 300);
    }

    #[test]
    fn test_signal_hook_action_all_variants() {
        let toml_str = r#"
root = "/tmp/test"

[[coordinator.signal_hooks]]
source = "a"
action = "notify"

[[coordinator.signal_hooks]]
source = "b"
action = "auto_fix"

[[coordinator.signal_hooks]]
source = "c"
action = "forward_to_worker"

[[coordinator.signal_hooks]]
source = "d"
action = "triage"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.coordinator.signal_hooks.len(), 4);
        assert_eq!(
            config.coordinator.signal_hooks[0].action,
            SignalHookAction::Notify
        );
        assert_eq!(
            config.coordinator.signal_hooks[1].action,
            SignalHookAction::AutoFix
        );
        assert_eq!(
            config.coordinator.signal_hooks[2].action,
            SignalHookAction::ForwardToWorker
        );
        assert_eq!(
            config.coordinator.signal_hooks[3].action,
            SignalHookAction::Triage
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
}

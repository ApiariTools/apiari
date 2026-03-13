//! Daemon mode — persistent Telegram bot + buzz auto-triage.
//!
//! The daemon runs a `tokio::select!` loop over multiple sources:
//! 1. Telegram messages (via mpsc channel from background long-poll)
//! 2. Buzz signals (inline watchers when enabled, else JSONL file poll)
//! 3. Swarm agent completion notifications (when enabled)
//! 4. Shutdown signals (SIGTERM/SIGINT)

pub mod config;
pub mod markdown;
pub mod origin_map;
pub mod session_store;
pub mod swarm_watcher;
pub mod tui_socket;

use crate::buzz::config::BuzzConfig;
use crate::buzz::output::{self, OutputMode};
use crate::buzz::reminder::{self, Reminder};
use crate::buzz::signal::{deduplicate, prioritize};
use crate::buzz::watcher::{Watcher, create_watchers, save_cursors};
use crate::channel::telegram::{self, TelegramChannel};
use crate::channel::{Channel, ChannelEvent, OutboundMessage};
use crate::coordinator::Coordinator;
use crate::quest::{QuestStore, TaskOrigin, default_store_path};
use crate::reminder::{self as user_reminder, ReminderStore};
use crate::signal::{Severity, Signal};
use crate::workspace::{self, load_workspace};
use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use apiari_common::ipc::JsonlReader;
use color_eyre::eyre::{Result, WrapErr};
use config::DaemonConfig;
use origin_map::{OriginEntry, OriginMap, route_notification};
use session_store::SessionStore;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tui_socket::{TuiClientRequest, TuiResponse, TuiSocketServer};

// ---------------------------------------------------------------------------
// Global config directory + PID file helpers
// ---------------------------------------------------------------------------

fn global_hive_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".config/hive")
}

pub(crate) fn pid_path() -> PathBuf {
    global_hive_dir().join("daemon.pid")
}

fn write_pid() -> Result<()> {
    let dir = global_hive_dir();
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("failed to create {}", dir.display()))?;
    let path = pid_path();
    std::fs::write(&path, std::process::id().to_string())
        .wrap_err_with(|| format!("failed to write PID file {}", path.display()))
}

pub(crate) fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn remove_pid() {
    let _ = std::fs::remove_file(pid_path());
}

pub(crate) fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ---------------------------------------------------------------------------
// Public API: start / stop
// ---------------------------------------------------------------------------

fn log_path() -> PathBuf {
    global_hive_dir().join("daemon.log")
}

/// Start the daemon.
///
/// By default, spawns a background child process with output redirected to
/// `~/.config/hive/daemon.log` and returns immediately. With `foreground: true`,
/// runs the event loop inline (blocking).
///
/// The daemon discovers workspaces from the global registry
/// (`~/.config/hive/workspaces.toml`) — no workspace root needed.
pub async fn start(foreground: bool) -> Result<()> {
    // Check for stale PID file.
    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            color_eyre::eyre::bail!("daemon already running (PID {pid})");
        }
        eprintln!("[daemon] Removing stale PID file (PID {pid} is not running)");
        remove_pid();
    }

    if !foreground {
        return spawn_background();
    }

    // Init file logging FIRST, before writing PID or any tracing calls.
    // _log_guard must stay alive — dropping it loses buffered log lines.
    let _log_guard = crate::logging::init_file(&log_path());

    // Foreground mode — write PID and run inline.
    write_pid()?;
    let pid = std::process::id();
    tracing::info!(pid, "Daemon started");

    // Discover workspaces from global registry.
    let workspace_configs = discover_daemon_workspaces()?;
    tracing::info!(count = workspace_configs.len(), "Discovered workspaces");
    for (name, path, _config) in &workspace_configs {
        tracing::debug!(name, path = %path.display(), "Workspace");
    }

    let mut runner = DaemonRunner::new(workspace_configs)?;
    let restart = runner.run().await?;

    // Clean up PID file before exit or restart.
    remove_pid();
    tracing::info!("PID file removed");

    if restart {
        // Spawn a new daemon process (new PID, fresh logs).
        tracing::info!("Spawning new daemon process");
        spawn_background()?;
    }

    Ok(())
}

/// Spawn `hive daemon start --foreground` as a detached background process.
/// The foreground process initializes its own file logger via `logging::init_file`.
fn spawn_background() -> Result<()> {
    let exe = std::env::current_exe().wrap_err("failed to find hive executable")?;
    let log = log_path();

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "start", "--foreground"]);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.stdin(std::process::Stdio::null());

    // Detach from parent process group so it survives our exit.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd.spawn().wrap_err("failed to spawn daemon process")?;
    let pid = child.id();

    println!("daemon started (PID {pid})");
    println!("logs: {}", log.display());

    Ok(())
}

/// Stop the running daemon by reading its PID file and sending SIGTERM.
pub fn stop() -> Result<()> {
    let pid = match read_pid() {
        Some(pid) => pid,
        None => {
            tracing::warn!("Daemon is not running (no PID file)");
            return Ok(());
        }
    };

    if !is_process_alive(pid) {
        tracing::warn!(pid, "Daemon is not running (stale PID), removing PID file");
        remove_pid();
        return Ok(());
    }

    // Send SIGTERM.
    let _ = std::process::Command::new("kill")
        .args([&pid.to_string()])
        .status();

    // Wait up to 5 seconds for the process to exit.
    for _ in 0..50 {
        if !is_process_alive(pid) {
            remove_pid();
            tracing::info!(pid, "Daemon stopped");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Still alive — force kill.
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    remove_pid();
    tracing::warn!(pid, "Daemon killed (force)");

    Ok(())
}

/// Discover workspaces with `.hive/daemon.toml` from the global registry.
fn discover_daemon_workspaces() -> Result<Vec<(String, PathBuf, DaemonConfig)>> {
    let entries = workspace::load_registry();
    let mut result = Vec::new();
    for entry in entries {
        if let Ok(config) = DaemonConfig::load(&entry.path) {
            result.push((entry.name, entry.path, config));
        }
    }
    if result.is_empty() {
        color_eyre::eyre::bail!(
            "No workspaces with .hive/daemon.toml found.\n\
             Run `hive init` in a workspace, then create .hive/daemon.toml."
        );
    }
    Ok(result)
}

/// A pending origin awaiting association with a spawned worktree.
struct PendingOrigin {
    origin: TaskOrigin,
    created_at: std::time::Instant,
}

/// Fully self-contained workspace managed by the global daemon.
struct WorkspaceSlot {
    name: String,
    workspace_root: PathBuf,
    config: DaemonConfig,
    coordinator_prompt: String,
    sessions: SessionStore,
    buzz_reader: JsonlReader<Signal>,
    buzz_slot: Option<BuzzSlot>,
    swarm_watcher: Option<swarm_watcher::SwarmWatcher>,
    origin_map: OriginMap,
    pending_origins: Vec<PendingOrigin>,
    reminder_store: ReminderStore,
    bot_index: usize, // index into DaemonRunner::bots
}

/// Per-workspace buzz state — watchers, reminders, output, and sweep timer.
///
/// Each workspace with a `buzz:` section in its `workspace.yaml` gets its own
/// `BuzzSlot`. The daemon polls all slots on the buzz timer and writes signals
/// to each workspace's own `.buzz/signals.jsonl`.
struct BuzzSlot {
    name: String,
    workspace_root: PathBuf,
    watchers: Vec<Box<dyn Watcher>>,
    reminders: Vec<Reminder>,
    output: OutputMode,
    sweep_cron_expr: Option<String>,
    next_sweep_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl BuzzSlot {
    /// Try to create a BuzzSlot for a workspace.
    ///
    /// - For the home workspace, `daemon_buzz_config` provides the legacy `config_path`
    ///   fallback from `daemon.toml [buzz]`.
    /// - For registry workspaces, only the `workspace.yaml buzz:` section is used.
    ///
    /// Returns `None` if no buzz sources are configured.
    fn try_new(
        name: &str,
        workspace_root: &Path,
        daemon_buzz_config: Option<&config::BuzzDaemonConfig>,
    ) -> Option<Self> {
        let ws = match load_workspace(workspace_root) {
            Ok(ws) => ws,
            Err(e) => {
                tracing::error!(name, error = %e, "BuzzSlot failed to load workspace");
                return None;
            }
        };

        // Resolve buzz config: workspace.yaml buzz section first,
        // then legacy config_path fallback (home workspace only).
        let buzz_config = if let Some(buzz) = &ws.buzz {
            tracing::info!(name, "BuzzSlot loaded buzz config from workspace.yaml");
            buzz.clone()
        } else if let Some(daemon_cfg) = daemon_buzz_config {
            // Legacy fallback: load from config_path in daemon.toml.
            let config_path = if daemon_cfg.config_path.is_absolute() {
                daemon_cfg.config_path.clone()
            } else {
                workspace_root.join(&daemon_cfg.config_path)
            };
            match BuzzConfig::load(&config_path) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::error!(name, error = %e, "BuzzSlot failed to load legacy config");
                    return None;
                }
            }
        } else {
            return None;
        };

        let watchers = create_watchers(&buzz_config, workspace_root);
        if watchers.is_empty() && buzz_config.reminders.is_empty() {
            tracing::info!(name, "BuzzSlot has no watchers or reminders — skipping");
            return None;
        }

        let reminders: Vec<Reminder> = buzz_config
            .reminders
            .iter()
            .map(Reminder::from_config)
            .collect();

        // Always write to JSONL so the dashboard can read signals.
        let output = OutputMode::File(
            buzz_config
                .output
                .path
                .clone()
                .unwrap_or_else(|| workspace_root.join(".buzz/signals.jsonl")),
        );

        // Compute sweep schedule from watchers.
        let (next_sweep_at, sweep_cron_expr) = DaemonRunner::compute_sweep_schedule(&watchers);

        tracing::info!(
            name,
            watchers = watchers.len(),
            reminders = reminders.len(),
            sweep = next_sweep_at.is_some(),
            "BuzzSlot initialized",
        );

        Some(Self {
            name: name.to_owned(),
            workspace_root: workspace_root.to_path_buf(),
            watchers,
            reminders,
            output,
            sweep_cron_expr,
            next_sweep_at,
        })
    }
}

/// One Telegram bot instance, possibly shared by multiple workspaces.
struct BotSlot {
    channel: Arc<TelegramChannel>,
    #[allow(dead_code)]
    workspace_indices: Vec<usize>,
}

/// Max number of button label mappings to keep in memory.
const BUTTON_LABEL_CACHE_MAX: usize = 500;

/// Extract a trailing ` ```buttons ... ``` ` fenced code block from coordinator text.
///
/// Returns the cleaned text (with the buttons block stripped) and parsed button rows.
/// Each row is a `Vec<InlineButton>`. If no buttons block is found or JSON is invalid,
/// returns the original text and empty buttons.
fn extract_buttons(text: &str) -> (String, Vec<Vec<crate::channel::InlineButton>>) {
    // Look for the last occurrence of ```buttons ... ```
    let Some(block_start) = text.rfind("```buttons") else {
        return (text.to_owned(), vec![]);
    };

    let after_fence = block_start + "```buttons".len();
    // Find the closing ``` after the opening fence
    let Some(relative_end) = text[after_fence..].find("```") else {
        return (text.to_owned(), vec![]);
    };
    let block_end = after_fence + relative_end + 3; // include closing ```

    let json_str = text[after_fence..after_fence + relative_end].trim();

    // Parse as JSON array of [label, callback_data] pairs.
    // Supports both flat array (single row) and nested array (multiple rows).
    let parsed: Vec<Vec<crate::channel::InlineButton>> =
        match serde_json::from_str::<Vec<Vec<String>>>(json_str) {
            Ok(rows) => rows
                .into_iter()
                .map(|row| {
                    row.chunks(2)
                        .filter_map(|pair| {
                            if pair.len() == 2 {
                                Some(crate::channel::InlineButton {
                                    text: pair[0].clone(),
                                    callback_data: pair[1].clone(),
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .filter(|row: &Vec<crate::channel::InlineButton>| !row.is_empty())
                .collect(),
            Err(_) => return (text.to_owned(), vec![]),
        };

    if parsed.is_empty() {
        return (text.to_owned(), vec![]);
    }

    // Strip the buttons block from the text.
    let cleaned = format!("{}{}", text[..block_start].trim_end(), &text[block_end..]);
    let cleaned = cleaned.trim().to_owned();

    (cleaned, parsed)
}

/// A dispatch request parsed from a coordinator response.
pub(crate) struct DispatchRequest {
    pub(crate) repo: String,
    pub(crate) agent: Option<String>,
    pub(crate) prompt: String,
}

/// Result from a spawned message handler, sent back to the main loop for state mutations.
struct MessageResult {
    ws_idx: usize,
    chat_id: i64,
    outcome: MessageOutcome,
}

/// Outcome of processing a user message.
enum MessageOutcome {
    /// Coordinator responded successfully. Response already sent to Telegram.
    Success {
        session_id: String,
        resume_id: Option<String>,
        buttons: Vec<Vec<crate::channel::InlineButton>>,
    },
    /// Coordinator session failed. Error message already sent to Telegram.
    Error { had_resume: bool },
    /// Pipeline completed (TUI dispatch flow).
    PipelineComplete(crate::pipeline::MultiPipelineResult),
    /// Incremental pipeline progress event.
    PipelineProgress(crate::pipeline::PipelineEvent),
}

/// Extract `` ```dispatch:repo[:agent] `` fenced code blocks from text.
///
/// Returns the cleaned text (blocks stripped) and parsed dispatch requests.
/// Multiple blocks = multiple dispatches. Invalid blocks are left in the text.
pub(crate) fn extract_dispatch_blocks(text: &str) -> (String, Vec<DispatchRequest>) {
    let mut dispatches = Vec::new();
    let mut cleaned = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(fence_start) = rest.find("```dispatch:") {
        // Append text before this block.
        cleaned.push_str(&rest[..fence_start]);

        let after_backticks = &rest[fence_start + 3..]; // skip ```
        // Find end of opening fence line.
        let header_end = after_backticks.find('\n').unwrap_or(after_backticks.len());
        let header = &after_backticks[..header_end]; // "dispatch:repo" or "dispatch:repo:agent"

        // Parse repo and optional agent from header.
        let parts: Vec<&str> = header
            .strip_prefix("dispatch:")
            .unwrap_or("")
            .splitn(2, ':')
            .collect();
        let repo = parts.first().map(|s| s.trim()).unwrap_or("");

        if repo.is_empty() {
            // Malformed — leave in text and skip past "```dispatch:".
            let skip = fence_start + "```dispatch:".len();
            cleaned.push_str(&rest[fence_start..skip]);
            rest = &rest[skip..];
            continue;
        }

        let agent = parts
            .get(1)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());

        // Find closing ``` fence.
        let body_start = header_end + 1; // skip the newline
        let Some(relative_close) = after_backticks[body_start..].find("```") else {
            // Unclosed fence — leave in text, consume everything from fence_start onward.
            cleaned.push_str(&rest[fence_start..]);
            rest = "";
            continue;
        };
        let body_end = body_start + relative_close;
        let prompt = after_backticks[body_start..body_end].trim().to_owned();

        // Skip past the closing ```.
        let block_total_len = 3 + body_end + 3; // opening ``` + content + closing ```
        rest = &rest[fence_start + block_total_len..];

        if !prompt.is_empty() {
            dispatches.push(DispatchRequest {
                repo: repo.to_owned(),
                agent,
                prompt,
            });
        }
    }

    // Append remainder.
    cleaned.push_str(rest);
    let cleaned = cleaned.trim().to_owned();

    (cleaned, dispatches)
}

/// Resolve the `swarm` binary path. Checks `$PATH` first, then `~/.cargo/bin/swarm`.
fn which_swarm() -> Option<PathBuf> {
    // Try $PATH via `which`.
    if let Ok(output) = std::process::Command::new("which").arg("swarm").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    // Fallback: ~/.cargo/bin/swarm
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/swarm");
        if cargo_bin.exists() {
            return Some(cargo_bin);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Dispatch state persistence
// ---------------------------------------------------------------------------

const DISPATCH_STATE_FILE: &str = ".hive/dispatch_state.json";

/// Persist completed dispatch state to disk.
fn save_dispatch_state(workspace_root: &Path, d: &ActiveTuiDispatch) {
    let path = workspace_root.join(DISPATCH_STATE_FILE);
    let response = DaemonRunner::build_dispatch_update_response(d);
    match serde_json::to_string_pretty(&response) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::error!(error = %e, "Failed to save dispatch state");
            } else {
                tracing::debug!(path = %path.display(), "Dispatch state saved");
            }
        }
        Err(e) => tracing::error!(error = %e, "Failed to serialize dispatch state"),
    }
}

/// Load persisted dispatch state from disk (only "ready" status).
fn load_dispatch_state(workspace_root: &Path) -> Option<ActiveTuiDispatch> {
    let path = workspace_root.join(DISPATCH_STATE_FILE);
    let json = std::fs::read_to_string(&path).ok()?;
    let response: TuiResponse = serde_json::from_str(&json).ok()?;
    match response {
        TuiResponse::DispatchUpdate {
            task,
            repos,
            status,
            task_md,
            title,
            per_repo,
            ..
        } if status == "ready" => {
            let repo_stages = per_repo
                .into_iter()
                .map(|r| RepoStageTracker {
                    repo: r.repo,
                    stage: RepoPipelineStage::Done,
                    context_md: r.context_md,
                    plan_md: r.plan_md,
                    file_count: r.file_count,
                    step_count: r.step_count,
                })
                .collect();
            Some(ActiveTuiDispatch {
                task,
                repos,
                status: TuiDispatchStatus::Ready,
                title,
                task_md,
                repo_stages,
            })
        }
        _ => None,
    }
}

/// Delete the persisted dispatch state file.
fn clear_dispatch_state(workspace_root: &Path) {
    let path = workspace_root.join(DISPATCH_STATE_FILE);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        tracing::debug!("Dispatch state cleared");
    }
}

/// Per-repo stage tracking during pipeline execution.
#[derive(Debug, Clone)]
struct RepoStageTracker {
    repo: String,
    stage: RepoPipelineStage,
    context_md: Option<String>,
    plan_md: Option<String>,
    file_count: Option<usize>,
    step_count: Option<usize>,
}

/// Pipeline stage for a single repo.
#[derive(Debug, Clone, PartialEq)]
enum RepoPipelineStage {
    Pending,
    Context,
    Planning,
    Done,
}

impl RepoPipelineStage {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Context => "context",
            Self::Planning => "planning",
            Self::Done => "done",
        }
    }
}

/// Dispatch lifecycle state owned by the daemon (survives TUI restart).
#[derive(Debug, Clone)]
struct ActiveTuiDispatch {
    task: String,
    repos: Vec<String>,
    status: TuiDispatchStatus,
    title: String,
    task_md: String,
    repo_stages: Vec<RepoStageTracker>,
}

/// Status of a daemon-owned dispatch.
#[derive(Debug, Clone, PartialEq)]
enum TuiDispatchStatus {
    Refining,
    Running,
    Ready,
}

/// Main daemon event loop — manages multiple workspaces and bot instances.
struct DaemonRunner {
    workspaces: Vec<WorkspaceSlot>,
    bots: Vec<BotSlot>,
    /// (bot_idx, chat_id, topic_id) → workspace index
    routing_table: HashMap<(usize, i64, Option<i64>), usize>,
    /// Index of the workspace currently being handled (set during event processing).
    active_workspace: Option<usize>,
    /// Resolved path to the `swarm` binary (for dispatch blocks).
    swarm_bin: Option<PathBuf>,
    /// Maps callback_data → button label for resolving tapped buttons.
    /// Bounded to BUTTON_LABEL_CACHE_MAX entries (FIFO eviction).
    button_labels: HashMap<String, String>,
    /// Insertion-order tracking for FIFO eviction of button_labels.
    button_labels_order: VecDeque<String>,
    /// Cancellation token for the main event loop (set at start of `run()`).
    cancel: Option<CancellationToken>,
    /// Set by `/restart` handler before cancelling the loop.
    restart_requested: Arc<AtomicBool>,
    /// Channel for spawned message handlers to send results back to the main loop.
    msg_done_tx: Option<mpsc::UnboundedSender<MessageResult>>,
    /// TUI socket server (one per home workspace).
    tui_socket: Option<TuiSocketServer>,
    /// Receiver for TUI client requests (from socket server).
    tui_rx: Option<mpsc::UnboundedReceiver<TuiClientRequest>>,
    /// Active TUI dispatch state (survives TUI reconnect).
    active_tui_dispatch: Option<ActiveTuiDispatch>,
}

/// Build a coordinator system prompt for a workspace, with daemon-mode addendum.
fn build_coordinator_prompt(workspace_root: &Path, bot_username: Option<&str>) -> Result<String> {
    let workspace = load_workspace(workspace_root)?;
    let store = QuestStore::new(default_store_path(&workspace.root));
    let coord = Coordinator::new(workspace, store);
    let mut prompt = coord.build_system_prompt()?;
    prompt.push_str("\n## Daemon Mode Constraints\n");
    prompt.push_str(
        "You are running in daemon mode, receiving messages from Telegram and the hive TUI.\n",
    );
    if let Some(username) = bot_username {
        prompt.push_str(&format!(
            "Your Telegram bot username is @{username}. When users @mention you in a group chat, that IS a message to you — respond normally.\n"
        ));
    }
    prompt.push_str("Keep responses conversational and concise.\n\n");
    prompt.push_str("### CRITICAL: You Are an Orchestrator, NOT a Coder\n\n");
    prompt.push_str(
        "You are a manager/orchestrator. You do NOT write code, implement features, or fix bugs yourself.\n",
    );
    prompt.push_str(
        "When the user asks you to build, implement, fix, add, refactor, or change code:\n",
    );
    prompt.push_str("1. Briefly acknowledge what they want\n");
    prompt.push_str("2. Dispatch it to a swarm worker using a ```dispatch:repo``` block\n");
    prompt.push_str("3. Do NOT research the codebase, read source files, or plan the implementation yourself\n\n");
    prompt.push_str(
        "Do NOT modify files directly — no Write, Edit, or file-writing shell commands.\n",
    );
    prompt.push_str("Do NOT read source code to understand how to implement something — that's the worker's job.\n");
    prompt.push_str("CRITICAL: You do NOT have a Task tool. Never use Task() to delegate work.\n");
    prompt.push_str("The ONLY way to create workers is via ```dispatch:repo``` code blocks in your response text.\n\n");
    prompt.push_str("You MAY use Bash/Read/Grep ONLY for: checking worker status (`swarm status`), checking git status, viewing PRs (`gh pr list`), or answering questions about project state. NEVER use these tools to research how to implement a task — dispatch it instead.\n");
    prompt.push_str("\n## Interactive Buttons\n\n");
    prompt.push_str("You can attach inline keyboard buttons to any response by ending your message with a buttons block:\n\n");
    prompt.push_str(
        "```buttons\n[[\"Button label\", \"callback:data\"], [\"Another\", \"other:data\"]]\n```\n\n",
    );
    prompt.push_str(
        "Each inner array is [label, callback_data]. Multiple inner arrays = multiple rows.\n",
    );
    prompt.push_str("When the user taps a button, you will receive: \"User tapped: <label>\"\n\n");
    prompt.push_str("Built-in actions (handled automatically, no response needed from you):\n");
    prompt.push_str("- merge_pr:<worktree_id> — merges the PR for that worker\n");
    prompt.push_str("- close_worker:<worktree_id> — closes that swarm worker\n");
    prompt.push_str("- ci_pending:<worktree_id> — tells user CI is still running\n");
    prompt.push_str("- ci_failing_merge:<worktree_id> — merges PR despite failing CI\n\n");
    prompt.push_str("Use buttons to offer clear choices instead of asking open-ended questions.\n");
    prompt.push_str(
        "Example: instead of \"Should I merge?\" write \"Should I merge?\" with [Yes] [No] buttons.\n",
    );
    prompt.push_str("\n## Dispatching Workers\n\n");
    prompt.push_str("To create a swarm worker, end your message with a dispatch block:\n\n");
    prompt.push_str("    ```dispatch:<repo>\n    Your task prompt here.\n    Create a PR when done.\n    ```\n\n");
    prompt.push_str("- One block = one worker. Use multiple blocks for multiple workers.\n");
    prompt.push_str("- `<repo>` is the subdirectory name (e.g. `hive`, `swarm`, `common`).\n");
    prompt.push_str(
        "- Add `:<agent>` to override the default (e.g. `dispatch:hive:claude` for headless).\n",
    );
    prompt.push_str("- You can still check workers with `swarm status` via Bash.\n");
    prompt.push_str("- Do NOT run `swarm create` via Bash — use dispatch blocks instead.\n\n");
    prompt.push_str("\n## Sentry Sweep Context\n\n");
    prompt.push_str(
        "When the user asks about Sentry issues or triage, check `.hive/last_sweep.md` for the latest sweep results. \
         This file is updated after each `/sweep` command and contains issue titles, severity, event counts, and links.\n",
    );
    Ok(prompt)
}

impl DaemonRunner {
    fn new(workspace_configs: Vec<(String, PathBuf, DaemonConfig)>) -> Result<Self> {
        // Phase 1: Deduplicate bots by bot_token.
        let mut token_to_bot_idx: HashMap<String, usize> = HashMap::new();
        let mut bots: Vec<BotSlot> = Vec::new();

        for (_name, _path, config) in &workspace_configs {
            let token = &config.telegram.bot_token;
            if !token_to_bot_idx.contains_key(token) {
                let idx = bots.len();
                bots.push(BotSlot {
                    channel: Arc::new(TelegramChannel::new(token.clone())),
                    workspace_indices: Vec::new(),
                });
                token_to_bot_idx.insert(token.clone(), idx);
            }
        }
        tracing::info!(count = bots.len(), "Telegram bots");

        // Phase 2: Build WorkspaceSlots.
        let mut workspaces: Vec<WorkspaceSlot> = Vec::new();
        let mut routing_table: HashMap<(usize, i64, Option<i64>), usize> = HashMap::new();
        for (name, path, config) in workspace_configs {
            let ws_idx = workspaces.len();
            let bot_idx = token_to_bot_idx[&config.telegram.bot_token];

            // Track which workspaces use which bot.
            bots[bot_idx].workspace_indices.push(ws_idx);

            // Build coordinator prompt.
            let coordinator_prompt =
                match build_coordinator_prompt(&path, config.telegram.bot_username.as_deref()) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!(name, error = %e, "Failed to build coordinator prompt");
                        continue;
                    }
                };

            // Load sessions.
            let mut sessions = SessionStore::load(&path);

            // Init buzz reader.
            let buzz_path = config.resolved_buzz_path(&path);
            let mut buzz_reader = JsonlReader::<Signal>::new(&buzz_path);
            let saved_offset = sessions.buzz_offset();
            if saved_offset > 0 {
                buzz_reader.set_offset(saved_offset);
                tracing::info!(name, saved_offset, "Restored buzz reader offset");
            } else {
                let offset = buzz_reader.skip_to_end().unwrap_or(0);
                sessions.set_buzz_offset(offset);
                tracing::info!(name, offset, "Skipped to end of buzz signals");
            }

            // Init buzz slot if enabled.
            let buzz_slot = if config.buzz.as_ref().is_some_and(|b| b.enabled) {
                if let Some(slot) = BuzzSlot::try_new(&name, &path, config.buzz.as_ref()) {
                    tracing::info!(name, "Buzz slot initialized");
                    Some(slot)
                } else {
                    None
                }
            } else {
                None
            };

            // Init swarm watcher.
            let swarm_watcher_instance =
                if let Some(state_path) = config.resolved_swarm_state_path(&path) {
                    let stall_timeout = config
                        .swarm_watch
                        .as_ref()
                        .map(|sw| sw.stall_timeout_secs)
                        .unwrap_or(300);
                    tracing::info!(
                        name,
                        state_path = %state_path.display(),
                        stall_timeout_secs = stall_timeout,
                        "Swarm watcher enabled",
                    );
                    let mut watcher = swarm_watcher::SwarmWatcher::new(state_path);
                    watcher.set_stall_timeout(stall_timeout);
                    watcher.set_hive_dir(path.join(".hive"));
                    Some(watcher)
                } else {
                    None
                };

            // Load origin map.
            let origin_map = OriginMap::load(&path);
            if !origin_map.entries.is_empty() {
                tracing::info!(
                    name,
                    count = origin_map.entries.len(),
                    "Loaded origin mappings",
                );
            }

            // Load reminders.
            let reminder_store = ReminderStore::load(&path);
            let active_reminders = reminder_store.active().len();
            if active_reminders > 0 {
                tracing::info!(name, count = active_reminders, "Loaded active reminders");
            }

            // Register routing: (bot_idx, alert_chat_id, topic_id) → ws_idx
            routing_table.insert(
                (
                    bot_idx,
                    config.telegram.alert_chat_id,
                    config.telegram.topic_id,
                ),
                ws_idx,
            );
            // Also register without topic for DM fallback (if not already claimed).
            if config.telegram.topic_id.is_some() {
                routing_table
                    .entry((bot_idx, config.telegram.alert_chat_id, None))
                    .or_insert(ws_idx);
            }

            tracing::info!(
                name,
                model = %config.model,
                max_turns = config.max_turns,
                "Workspace configured",
            );

            workspaces.push(WorkspaceSlot {
                name,
                workspace_root: path,
                config,
                coordinator_prompt,
                sessions,
                buzz_reader,
                buzz_slot,
                swarm_watcher: swarm_watcher_instance,
                origin_map,
                pending_origins: Vec::new(),
                reminder_store,
                bot_index: bot_idx,
            });
        }

        // Resolve swarm binary for dispatch blocks.
        let swarm_bin = which_swarm();
        if let Some(ref path) = swarm_bin {
            tracing::info!(path = %path.display(), "Swarm binary found");
        } else {
            tracing::warn!("Swarm binary not found — dispatch blocks will fail");
        }

        // Start TUI socket server on the first workspace (home workspace).
        let (tui_socket, tui_rx) = if let Some(ws) = workspaces.first() {
            match TuiSocketServer::start(&ws.workspace_root) {
                Ok((rx, server)) => (Some(server), Some(rx)),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to start TUI socket");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // Load persisted dispatch state from the home workspace (if any).
        let active_tui_dispatch = workspaces
            .first()
            .and_then(|ws| load_dispatch_state(&ws.workspace_root));
        if active_tui_dispatch.is_some() {
            tracing::info!("Restored dispatch state from disk");
        }

        Ok(Self {
            workspaces,
            bots,
            routing_table,
            active_workspace: None,
            swarm_bin,
            button_labels: HashMap::new(),
            button_labels_order: VecDeque::new(),
            cancel: None,
            restart_requested: Arc::new(AtomicBool::new(false)),
            msg_done_tx: None,
            tui_socket,
            tui_rx,
            active_tui_dispatch,
        })
    }

    // -- Accessors for the active workspace --

    fn active_ws(&self) -> &WorkspaceSlot {
        &self.workspaces[self.active_workspace.expect("no active workspace")]
    }
    fn active_ws_mut(&mut self) -> &mut WorkspaceSlot {
        let idx = self.active_workspace.expect("no active workspace");
        &mut self.workspaces[idx]
    }
    fn active_channel(&self) -> &Arc<TelegramChannel> {
        &self.bots[self.active_ws().bot_index].channel
    }
    fn active_topic_id(&self) -> Option<i64> {
        self.active_ws().config.telegram.topic_id
    }

    /// Resolve which workspace an incoming event should be routed to.
    ///
    /// Returns `Some(workspace_index)` if found, `None` if unregistered.
    fn resolve_workspace(
        &self,
        bot_idx: usize,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> Option<usize> {
        self.routing_table
            .get(&(bot_idx, chat_id, topic_id))
            .copied()
            .or_else(|| {
                // Fall back: try without topic (DM routing).
                topic_id.and_then(|_| self.routing_table.get(&(bot_idx, chat_id, None)).copied())
            })
    }

    async fn run(&mut self) -> Result<bool> {
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());

        // Channel for spawned message handlers to send results back.
        let (msg_done_tx, mut msg_done_rx) = mpsc::unbounded_channel();
        self.msg_done_tx = Some(msg_done_tx);

        // Set up SIGTERM/SIGINT handler.
        let shutdown_cancel = cancel.clone();
        tokio::spawn(async move {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut sigterm =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("failed to install SIGTERM handler");
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = sigterm.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
            tracing::info!("Shutdown signal received");
            shutdown_cancel.cancel();
        });

        // Spawn one polling task per bot, all feeding into a single mpsc channel.
        let (tx, mut rx) = mpsc::channel::<(usize, ChannelEvent)>(64);
        for (bot_idx, bot) in self.bots.iter().enumerate() {
            let channel = bot.channel.clone();
            let poll_cancel = cancel.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let (inner_tx, mut inner_rx) = mpsc::channel(64);
                let fwd = tokio::spawn({
                    let tx = tx.clone();
                    async move {
                        while let Some(ev) = inner_rx.recv().await {
                            if tx.send((bot_idx, ev)).await.is_err() {
                                break;
                            }
                        }
                    }
                });
                channel.run(inner_tx, poll_cancel).await;
                fwd.abort();
            });
        }
        drop(tx); // Drop our copy so rx closes when all bots stop.

        // Compute timer intervals from all workspaces.
        let buzz_interval_secs = self
            .workspaces
            .iter()
            .map(|ws| ws.config.buzz_poll_interval_secs)
            .min()
            .unwrap_or(30);
        let mut buzz_timer =
            tokio::time::interval(std::time::Duration::from_secs(buzz_interval_secs));
        buzz_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        buzz_timer.tick().await; // skip first tick

        let swarm_interval_secs = self
            .workspaces
            .iter()
            .filter_map(|ws| {
                ws.config
                    .swarm_watch
                    .as_ref()
                    .filter(|sw| sw.enabled)
                    .map(|sw| sw.poll_interval_secs)
            })
            .min();
        let has_swarm = swarm_interval_secs.is_some();
        let mut swarm_timer = tokio::time::interval(std::time::Duration::from_secs(
            swarm_interval_secs.unwrap_or(3600),
        ));
        swarm_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        swarm_timer.tick().await; // skip first tick

        let mut reminder_timer = tokio::time::interval(std::time::Duration::from_secs(60));
        reminder_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        reminder_timer.tick().await; // skip first tick

        // Sweep timer — computed from sentry sweep cron schedule across all workspaces.
        let mut sweep_sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = self
            .sweep_instant()
            .map(|i| Box::pin(tokio::time::sleep_until(i)));

        // Take the TUI socket receiver out of self so it can be used in select!.
        let mut tui_rx = self.tui_rx.take();
        let has_tui = tui_rx.is_some();

        tracing::info!("Ready. Listening for Telegram messages and buzz signals.");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Shutting down...");
                    break;
                }

                event = rx.recv() => {
                    match event {
                        Some((bot_idx, event)) => {
                            if let Err(e) = self.handle_channel_event(bot_idx, event).await {
                                tracing::error!(error = %e, "Error handling event");
                            }
                        }
                        None => {
                            tracing::info!("All channels closed, shutting down.");
                            break;
                        }
                    }
                }

                Some(result) = msg_done_rx.recv() => {
                    self.handle_message_result(result).await;
                }

                Some(tui_req) = async { tui_rx.as_mut()?.recv().await }, if has_tui => {
                    self.handle_tui_request(tui_req).await;
                }

                _ = buzz_timer.tick() => {
                    if let Err(e) = self.poll_buzz().await {
                        tracing::error!(error = %e, "Buzz poll error");
                    }
                }

                _ = swarm_timer.tick(), if has_swarm => {
                    if let Err(e) = self.poll_swarm().await {
                        tracing::error!(error = %e, "Swarm poll error");
                    }
                }

                _ = reminder_timer.tick() => {
                    if let Err(e) = self.poll_reminders().await {
                        tracing::error!(error = %e, "Reminder poll error");
                    }
                }

                _ = async { sweep_sleep.as_mut().unwrap().as_mut().await }, if sweep_sleep.is_some() => {
                    if let Err(e) = self.poll_sweep().await {
                        tracing::error!(error = %e, "Sweep error");
                    }
                    self.advance_sweep_timer();
                    sweep_sleep = self.sweep_instant().map(|i| Box::pin(tokio::time::sleep_until(i)));
                }
            }
        }

        // Persist state before exiting — iterate all workspaces.
        for ws in &self.workspaces {
            if let Err(e) = ws.sessions.save() {
                tracing::error!(name = %ws.name, error = %e, "Failed to save sessions");
            }
            if let Err(e) = ws.reminder_store.save() {
                tracing::error!(name = %ws.name, error = %e, "Failed to save reminders");
            }
            if let Err(e) = ws.origin_map.save(&ws.workspace_root) {
                tracing::error!(name = %ws.name, error = %e, "Failed to save origin map");
            }
            // Save buzz watcher cursors.
            if let Some(ref slot) = ws.buzz_slot {
                save_cursors(&slot.watchers, &slot.workspace_root);
            }
        }

        let restart = self.restart_requested.load(Ordering::Relaxed);
        if restart {
            tracing::info!("State saved. Restarting...");
        } else {
            tracing::info!("State saved. Goodbye.");
        }
        Ok(restart)
    }

    /// Handle a message or command from a channel.
    async fn handle_channel_event(&mut self, bot_idx: usize, event: ChannelEvent) -> Result<()> {
        // Callback queries don't have message_id — handle separately.
        if let ChannelEvent::CallbackQuery {
            chat_id,
            user_name,
            data,
            callback_query_id,
            topic_id,
        } = event
        {
            // Resolve workspace for routing.
            let ws_idx = match self.resolve_workspace(bot_idx, chat_id, topic_id) {
                Some(idx) => idx,
                None => {
                    self.bots[bot_idx]
                        .channel
                        .answer_callback(&callback_query_id)
                        .await;
                    return Ok(());
                }
            };

            // Check chat auth.
            if !self.workspaces[ws_idx].config.is_chat_allowed(chat_id) {
                self.bots[bot_idx]
                    .channel
                    .answer_callback(&callback_query_id)
                    .await;
                return Ok(());
            }

            // Check user auth.
            // (user_id not available on callback queries — skip user check)

            tracing::debug!(user_name, data, "Callback received");
            self.bots[bot_idx]
                .channel
                .answer_callback(&callback_query_id)
                .await;
            self.active_workspace = Some(ws_idx);
            let result = self.handle_callback(chat_id, &data).await;
            self.active_workspace = None;
            return result;
        }

        let (chat_id, message_id, user_id, topic_id) = match &event {
            ChannelEvent::Command {
                chat_id,
                message_id,
                user_id,
                topic_id,
                ..
            }
            | ChannelEvent::Message {
                chat_id,
                message_id,
                user_id,
                topic_id,
                ..
            } => (*chat_id, *message_id, *user_id, *topic_id),
            ChannelEvent::CallbackQuery { .. } => unreachable!(),
        };

        // Resolve workspace from routing table.
        let ws_idx = match self.resolve_workspace(bot_idx, chat_id, topic_id) {
            Some(idx) => idx,
            None => {
                tracing::debug!(chat_id, ?topic_id, "Ignoring message from unroutable chat");
                return Ok(());
            }
        };

        // Check chat auth.
        if !self.workspaces[ws_idx].config.is_chat_allowed(chat_id) {
            tracing::debug!(chat_id, "Ignoring message from disallowed chat");
            return Ok(());
        }

        // Check user auth.
        if !self.workspaces[ws_idx].config.is_user_allowed(user_id) {
            tracing::debug!(user_id, "Ignoring message from unauthorized user");
            return Ok(());
        }

        self.active_workspace = Some(ws_idx);

        // React immediately so the user knows we received their message.
        self.active_channel()
            .send_reaction(chat_id, message_id, "👀")
            .await;

        // Touch the telegram channel so presence routing knows telegram is active.
        let _ = crate::presence::touch_channel(&self.active_ws().workspace_root, "telegram");

        let result = match event {
            ChannelEvent::Command {
                chat_id,
                user_name,
                command,
                args,
                ..
            } => {
                tracing::info!(user_name, command, args, "Command received");
                self.handle_command(chat_id, &command, &args).await
            }
            ChannelEvent::Message {
                chat_id,
                user_id,
                user_name,
                text,
                ..
            } => {
                tracing::info!(
                    user_name,
                    chat_id,
                    ?topic_id,
                    text = %truncate(&text, 80),
                    "Message received",
                );
                self.handle_message(chat_id, user_id, &user_name, &text)
                    .await
            }
            ChannelEvent::CallbackQuery { .. } => unreachable!(),
        };

        // Clear workspace context after handling.
        self.active_workspace = None;
        result
    }

    /// Handle an inline keyboard button press.
    async fn handle_callback(&mut self, chat_id: i64, data: &str) -> Result<()> {
        if let Some(worktree_id) = data.strip_prefix("merge_pr:") {
            self.handle_merge_pr(chat_id, worktree_id).await
        } else if let Some(_worktree_id) = data.strip_prefix("ci_pending:") {
            self.send(chat_id, "CI is still running, wait for it to finish.")
                .await
        } else if let Some(worktree_id) = data.strip_prefix("ci_failing_merge:") {
            self.handle_merge_pr(chat_id, worktree_id).await
        } else if let Some(worktree_id) = data.strip_prefix("close_worker:") {
            self.handle_close_worker(chat_id, worktree_id).await
        } else {
            // Route unknown callback back to coordinator as a message.
            let label = self
                .button_labels
                .get(data)
                .cloned()
                .unwrap_or_else(|| data.to_owned());
            let synthetic_message = format!("User tapped: \"{label}\"");
            tracing::debug!(synthetic_message, "Routing callback to coordinator");
            self.handle_message(chat_id, 0, "button_tap", &synthetic_message)
                .await
        }
    }

    /// Merge a PR for the given worktree by looking up its PR URL from swarm state.
    async fn handle_merge_pr(&self, chat_id: i64, worktree_id: &str) -> Result<()> {
        let pr_url = match read_pr_url_from_swarm(&self.active_ws().workspace_root, worktree_id) {
            Some(url) => url,
            None => {
                return self
                    .send(chat_id, &format!("No PR URL found for `{worktree_id}`."))
                    .await;
            }
        };

        self.send(chat_id, &format!("Merging PR for `{worktree_id}`..."))
            .await?;

        let output = tokio::process::Command::new("gh")
            .args(["pr", "merge", &pr_url, "--squash", "--delete-branch"])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                self.send(chat_id, &format!("✅ PR merged for `{worktree_id}`."))
                    .await
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let detail = if !stderr.is_empty() {
                    stderr.to_string()
                } else {
                    stdout.to_string()
                };
                self.send(
                    chat_id,
                    &format!("❌ Failed to merge PR for `{worktree_id}`:\n```\n{detail}\n```"),
                )
                .await
            }
            Err(e) => {
                self.send(
                    chat_id,
                    &format!("❌ Failed to run `gh pr merge` for `{worktree_id}`: {e}"),
                )
                .await
            }
        }
    }

    /// Close a swarm worker by running `swarm close`.
    async fn handle_close_worker(&self, chat_id: i64, worktree_id: &str) -> Result<()> {
        self.send(chat_id, &format!("Closing worker `{worktree_id}`..."))
            .await?;

        let output = tokio::process::Command::new("swarm")
            .args([
                "--dir",
                &self.active_ws().workspace_root.display().to_string(),
                "close",
                worktree_id,
            ])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                self.send(chat_id, &format!("✅ Worker `{worktree_id}` closed."))
                    .await
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let detail = if !stderr.is_empty() {
                    stderr.to_string()
                } else {
                    stdout.to_string()
                };
                self.send(
                    chat_id,
                    &format!("❌ Failed to close `{worktree_id}`:\n```\n{detail}\n```"),
                )
                .await
            }
            Err(e) => {
                self.send(
                    chat_id,
                    &format!("❌ Failed to run `swarm close` for `{worktree_id}`: {e}"),
                )
                .await
            }
        }
    }

    /// Handle a slash command.
    async fn handle_command(&mut self, chat_id: i64, command: &str, args: &str) -> Result<()> {
        match command {
            "start" => {
                self.send(
                    chat_id,
                    "Hive daemon is running. Send me a message to chat with the coordinator.",
                )
                .await
            }
            "reset" => {
                let had_session = self.active_ws_mut().sessions.reset_session(chat_id);
                self.active_ws_mut().sessions.save()?;
                if had_session {
                    self.send(
                        chat_id,
                        "Session reset. Next message starts a fresh conversation.",
                    )
                    .await
                } else {
                    self.send(chat_id, "No active session to reset.").await
                }
            }
            "history" => {
                let history = self.active_ws().sessions.history(chat_id);
                if history.is_empty() {
                    return self.send(chat_id, "No previous sessions.").await;
                }
                let mut text = String::from("Previous sessions:\n");
                for (i, s) in history.iter().enumerate().rev().take(10) {
                    text.push_str(&format!(
                        "\n{}. `{}` — {} turns ({})",
                        i + 1,
                        &s.session_id[..8.min(s.session_id.len())],
                        s.turn_count,
                        s.ended_at.format("%Y-%m-%d %H:%M"),
                    ));
                }
                self.send(chat_id, &text).await
            }
            "resume" => {
                if args.is_empty() {
                    return self
                        .send(chat_id, "Usage: /resume <session-id-prefix>")
                        .await;
                }
                match self.active_ws().sessions.find_archived(chat_id, args) {
                    Some(session_id) => {
                        self.active_ws_mut()
                            .sessions
                            .resume_session(chat_id, &session_id);
                        self.active_ws_mut().sessions.save()?;
                        let short = &session_id[..8.min(session_id.len())];
                        self.send(
                            chat_id,
                            &format!("Resumed session `{short}`. Send a message to continue."),
                        )
                        .await
                    }
                    None => {
                        self.send(chat_id, "No unique session found matching that prefix.")
                            .await
                    }
                }
            }
            "status" => {
                let ws = self.active_ws();
                let mut text =
                    format!("Workspace: {} ({})\n", ws.name, ws.workspace_root.display());
                if let Some(session) = ws.sessions.get_active(chat_id) {
                    text.push_str(&format!(
                        "Active session: `{}`\nTurns: {}\nStarted: {}",
                        &session.session_id[..8.min(session.session_id.len())],
                        session.turn_count,
                        session.created_at.format("%Y-%m-%d %H:%M"),
                    ));
                } else {
                    text.push_str("No active session.");
                }
                self.send(chat_id, &text).await
            }
            "remind" => self.handle_remind_command(chat_id, args).await,
            "reminders" => self.handle_reminders_command(chat_id, args).await,
            "sweep" => self.handle_sweep_command(chat_id).await,
            "help" => {
                let mut text = String::from(
                    "Commands:\n\
                     /reset — Archive current session, start fresh\n\
                     /history — List previous sessions\n\
                     /resume <id> — Resume an archived session\n\
                     /status — Show workspace and session info\n\
                     /remind <dur> <msg> — Schedule a reminder\n\
                     /remind --cron \"expr\" <msg> — Cron reminder\n\
                     /reminders — List pending reminders\n\
                     /reminders cancel <id> — Cancel a reminder\n\
                     /sweep — Run a Sentry sweep now\n\
                     /help — Show this message",
                );
                // Append custom commands.
                for (name, cmd) in &self.active_ws().config.commands {
                    let desc = cmd.description.as_deref().unwrap_or(cmd.run.as_str());
                    text.push_str(&format!("\n/{name} — {desc}"));
                }
                text.push_str("\n\nSend any other message to chat with the coordinator.");
                self.send(chat_id, &text).await
            }
            _ if self.active_ws().config.commands.contains_key(command) => {
                self.run_custom_command(chat_id, command).await
            }
            _ => {
                self.send(
                    chat_id,
                    &format!("Unknown command: /{command}\nSend /help for available commands."),
                )
                .await
            }
        }
    }

    /// Run a user-defined custom command from `[commands]` config.
    async fn run_custom_command(&mut self, chat_id: i64, command: &str) -> Result<()> {
        let cmd = self
            .active_ws()
            .config
            .commands
            .get(command)
            .cloned()
            .unwrap();
        self.send(chat_id, &format!("Running /{command}..."))
            .await?;

        let output = tokio::process::Command::new("sh")
            .args(["-c", &cmd.run])
            .current_dir(&self.active_ws().workspace_root)
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut reply = String::new();
                if !stdout.is_empty() {
                    reply.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !reply.is_empty() {
                        reply.push('\n');
                    }
                    reply.push_str(&stderr);
                }
                if out.status.success() {
                    let msg = if reply.is_empty() {
                        format!("/{command} completed.")
                    } else {
                        // Truncate long output for Telegram.
                        let truncated = if reply.len() > 3500 {
                            format!("{}...\n(truncated)", &reply[..3500])
                        } else {
                            reply
                        };
                        format!("/{command} completed:\n```\n{truncated}\n```")
                    };
                    self.send(chat_id, &msg).await?;
                } else {
                    let code = out.status.code().unwrap_or(-1);
                    let msg = if reply.is_empty() {
                        format!("/{command} failed (exit {code}).")
                    } else {
                        let truncated = if reply.len() > 3500 {
                            format!("{}...\n(truncated)", &reply[..3500])
                        } else {
                            reply
                        };
                        format!("/{command} failed (exit {code}):\n```\n{truncated}\n```")
                    };
                    self.send(chat_id, &msg).await?;
                    return Ok(());
                }
            }
            Err(e) => {
                self.send(chat_id, &format!("/{command} error: {e}"))
                    .await?;
                return Ok(());
            }
        }

        // Handle post-action.
        if cmd.then_action.as_deref() == Some("restart") {
            self.send(chat_id, "Restarting daemon...").await?;
            self.restart_requested.store(true, Ordering::Relaxed);
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
        }

        Ok(())
    }

    /// Handle a regular text message — spawn coordinator work in the background.
    ///
    /// Phase A (sync, in main loop): push pending origin, read resume_id, clone
    /// state, spawn the background task. Returns immediately so the main loop
    /// stays responsive to other events.
    ///
    /// Phase B (spawned task): typing indicator, coordinator turn, send response,
    /// run dispatches, send `MessageResult` back to main loop for state mutations.
    async fn handle_message(
        &mut self,
        chat_id: i64,
        user_id: i64,
        user_name: &str,
        text: &str,
    ) -> Result<()> {
        // -- Phase A: brief sync work in the main loop --

        // Push a pending origin so that if the coordinator spawns a swarm agent,
        // we can associate it with this chat for notification routing.
        self.active_ws_mut().pending_origins.push(PendingOrigin {
            origin: TaskOrigin {
                channel: "telegram".into(),
                chat_id: Some(chat_id),
                user_name: Some(user_name.to_owned()),
                user_id: Some(user_id),
            },
            created_at: std::time::Instant::now(),
        });

        // Check if we have an active session to resume, or start a new one.
        let resume_id = self
            .active_ws()
            .sessions
            .get_active(chat_id)
            .map(|s| s.session_id.clone());

        // Clone everything the spawned task needs.
        let ws_idx = self.active_workspace.expect("no active workspace");
        let channel = self.active_channel().clone();
        let topic_id = self.active_ws().config.telegram.topic_id;
        let coordinator_prompt = self.active_ws().coordinator_prompt.clone();
        let model = self.active_ws().config.model.clone();
        let max_turns = self.active_ws().config.max_turns;
        let workspace_root = self.active_ws().workspace_root.clone();
        let swarm_bin = self.swarm_bin.clone();
        let msg_done_tx = self
            .msg_done_tx
            .as_ref()
            .expect("msg_done_tx not initialized")
            .clone();
        let text = text.to_owned();

        // -- Phase B: spawn the long-running coordinator work --
        tokio::spawn(async move {
            // Start typing indicator loop (expires after 5s, so we resend every 4s).
            let typing_cancel = CancellationToken::new();
            {
                let typing_token = typing_cancel.clone();
                let typing_channel = channel.clone();
                tokio::spawn(async move {
                    loop {
                        typing_channel.send_typing(chat_id, topic_id).await;
                        tokio::select! {
                            _ = typing_token.cancelled() => break,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {}
                        }
                    }
                });
            }

            // Build a callback that sends intermediate text blocks to Telegram.
            let send_channel = channel.clone();
            let send_fn = move |text: String| {
                let channel = send_channel.clone();
                async move {
                    for chunk in split_message(&text, 4000) {
                        channel
                            .send_message(&OutboundMessage {
                                chat_id,
                                text: chunk,
                                buttons: vec![],
                                topic_id,
                            })
                            .await?;
                    }
                    Ok(())
                }
            };

            // Run the coordinator turn (the 10-30s operation — now non-blocking!).
            let result = run_coordinator_turn_standalone(
                &coordinator_prompt,
                &model,
                max_turns,
                &workspace_root,
                &text,
                resume_id.as_deref(),
                send_fn,
            )
            .await;

            // Stop typing indicator.
            typing_cancel.cancel();

            let outcome = match result {
                Ok((final_text, session_id, turn_dispatches)) => {
                    // Extract buttons and dispatch blocks from the response.
                    let (clean_response, buttons) = extract_buttons(&final_text);
                    let (clean_response, final_dispatches) =
                        extract_dispatch_blocks(&clean_response);
                    let mut dispatches = turn_dispatches;
                    dispatches.extend(final_dispatches);

                    // Send the final response to Telegram.
                    if !clean_response.is_empty() {
                        tracing::debug!(
                            chars = clean_response.len(),
                            text = %truncate(&clean_response, 80),
                            "Responding",
                        );
                        if buttons.is_empty() {
                            let _ = send_to_telegram(&channel, chat_id, &clean_response, topic_id)
                                .await;
                        } else {
                            let _ = send_with_buttons_to_telegram(
                                &channel,
                                chat_id,
                                &clean_response,
                                buttons.clone(),
                                topic_id,
                            )
                            .await;
                        }
                    }

                    // Process dispatch blocks after sending the text response.
                    if !dispatches.is_empty() {
                        tracing::info!(count = dispatches.len(), "Processing dispatch blocks");
                        run_dispatches_standalone(
                            chat_id,
                            dispatches,
                            swarm_bin,
                            workspace_root,
                            channel.clone(),
                            topic_id,
                        )
                        .await;
                    }

                    MessageOutcome::Success {
                        session_id,
                        resume_id,
                        buttons,
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Claude session error");
                    let error_msg = format!(
                        "⚠️ Error: {}\n\nSession has been reset. Send another message to reconnect.",
                        truncate(&e.to_string(), 200),
                    );
                    let _ = send_to_telegram(&channel, chat_id, &error_msg, topic_id).await;
                    MessageOutcome::Error {
                        had_resume: resume_id.is_some(),
                    }
                }
            };

            // Send result back to the main loop for state mutations.
            let _ = msg_done_tx.send(MessageResult {
                ws_idx,
                chat_id,
                outcome,
            });
        });

        Ok(())
    }

    /// Process the result of a spawned message handler — brief state mutations only.
    async fn handle_message_result(&mut self, result: MessageResult) {
        let MessageResult {
            ws_idx,
            chat_id,
            outcome,
        } = result;

        // Guard against stale workspace index (workspaces are fixed at startup,
        // but be defensive).
        if ws_idx >= self.workspaces.len() {
            tracing::warn!(ws_idx, "Stale message result, ignoring");
            return;
        }

        match outcome {
            MessageOutcome::Success {
                session_id,
                resume_id,
                buttons,
            } => {
                // Record or update the session.
                if resume_id.is_none() || resume_id.as_deref() != Some(&session_id) {
                    self.workspaces[ws_idx]
                        .sessions
                        .start_session(chat_id, session_id);
                }

                let threshold = self.workspaces[ws_idx].config.nudge_turn_threshold;
                let should_nudge = self.workspaces[ws_idx]
                    .sessions
                    .record_turn(chat_id, threshold);
                if let Err(e) = self.workspaces[ws_idx].sessions.save() {
                    tracing::error!(error = %e, "Failed to save sessions");
                }

                // Send nudge hint if threshold crossed.
                if should_nudge {
                    self.active_workspace = Some(ws_idx);
                    let _ = self
                        .send(
                            chat_id,
                            "_This session has many turns. Consider /reset for better performance._",
                        )
                        .await;
                    self.active_workspace = None;
                }

                // Cache button labels for callback resolution.
                if !buttons.is_empty() {
                    self.cache_button_labels(&buttons);
                }
            }
            MessageOutcome::Error { had_resume } => {
                // If this was a TUI dispatch, mark it Ready with error.
                if chat_id == Self::TUI_CHAT_ID
                    && let Some(ref mut d) = self.active_tui_dispatch
                    && d.status != TuiDispatchStatus::Ready
                {
                    d.status = TuiDispatchStatus::Ready;
                    self.broadcast_dispatch_update();
                }

                // If session died, remove it so next message auto-reconnects.
                if had_resume {
                    self.workspaces[ws_idx].sessions.remove_active(chat_id);
                    if let Err(e) = self.workspaces[ws_idx].sessions.save() {
                        tracing::error!(error = %e, "Failed to save sessions");
                    }
                }
            }
            MessageOutcome::PipelineProgress(event) => {
                use crate::pipeline::PipelineEvent;
                if let Some(ref mut d) = self.active_tui_dispatch {
                    match event {
                        PipelineEvent::RefineComplete { title } => {
                            d.title = title;
                            d.status = TuiDispatchStatus::Running;
                        }
                        PipelineEvent::RepoContextStarted { repo } => {
                            if let Some(rs) = d.repo_stages.iter_mut().find(|r| r.repo == repo) {
                                rs.stage = RepoPipelineStage::Context;
                            }
                        }
                        PipelineEvent::RepoContextComplete { repo, file_count } => {
                            if let Some(rs) = d.repo_stages.iter_mut().find(|r| r.repo == repo) {
                                rs.file_count = Some(file_count);
                            }
                        }
                        PipelineEvent::RepoPlanStarted { repo } => {
                            if let Some(rs) = d.repo_stages.iter_mut().find(|r| r.repo == repo) {
                                rs.stage = RepoPipelineStage::Planning;
                            }
                        }
                        PipelineEvent::RepoPlanComplete { repo, step_count } => {
                            if let Some(rs) = d.repo_stages.iter_mut().find(|r| r.repo == repo) {
                                rs.stage = RepoPipelineStage::Done;
                                rs.step_count = Some(step_count);
                            }
                        }
                    }
                    self.broadcast_dispatch_update();
                }
            }
            MessageOutcome::PipelineComplete(pipeline_result) => {
                // Populate remaining fields from the final result and mark Ready.
                if let Some(ref mut d) = self.active_tui_dispatch {
                    d.task_md = pipeline_result.task_md.clone();
                    if d.title.is_empty() {
                        d.title = pipeline_result.title.clone();
                    }
                    // Fill context_md/plan_md and ensure all repos are Done.
                    for pr in &pipeline_result.per_repo {
                        if let Some(rs) = d.repo_stages.iter_mut().find(|r| r.repo == pr.repo) {
                            rs.context_md = pr.context_md.clone();
                            rs.plan_md = pr.plan_md.clone();
                            rs.stage = RepoPipelineStage::Done;
                        }
                    }
                    d.status = TuiDispatchStatus::Ready;
                }
                // Broadcast + persist outside the mutable borrow.
                self.broadcast_dispatch_update();
                if let Some(ref d) = self.active_tui_dispatch
                    && let Some(ws) = self.workspaces.first()
                {
                    save_dispatch_state(&ws.workspace_root, d);
                }
            }
        }
    }

    // -- TUI socket handling --

    /// Sentinel chat_id used for TUI socket messages (not a real Telegram chat).
    const TUI_CHAT_ID: i64 = i64::MAX;

    /// Handle a request from a connected TUI client.
    async fn handle_tui_request(&mut self, req: TuiClientRequest) {
        let TuiClientRequest { request, responder } = req;

        match request {
            tui_socket::TuiRequest::Message { text } => {
                self.handle_tui_message(&text, responder).await;
            }
            tui_socket::TuiRequest::Dispatch { repo, description } => {
                self.handle_tui_dispatch(&repo, &description, responder)
                    .await;
            }
            tui_socket::TuiRequest::SubmitDispatch { repos, task } => {
                self.handle_tui_submit_dispatch(repos, task, responder)
                    .await;
            }
            tui_socket::TuiRequest::ConfirmDispatch => {
                self.handle_tui_confirm_dispatch(responder).await;
            }
            tui_socket::TuiRequest::CancelDispatch => {
                self.handle_tui_cancel_dispatch();
            }
            tui_socket::TuiRequest::GetDispatchState => {
                self.handle_tui_get_dispatch_state(responder);
            }
        }
    }

    /// Handle a TUI message by routing it through the same coordinator pipeline.
    async fn handle_tui_message(&mut self, text: &str, responder: tui_socket::ResponseSender) {
        // Route to the home workspace (index 0).
        let ws_idx = 0;
        if ws_idx >= self.workspaces.len() {
            let _ = responder.send(TuiResponse::Error {
                text: "no workspace available".into(),
            });
            return;
        }

        self.active_workspace = Some(ws_idx);
        let chat_id = Self::TUI_CHAT_ID;

        tracing::info!(message = %truncate(text, 80), "TUI message");

        // Check if we have an active session to resume.
        let resume_id = self.workspaces[ws_idx]
            .sessions
            .get_active(chat_id)
            .map(|s| s.session_id.clone());

        // Clone everything for the spawned task.
        let coordinator_prompt = self.workspaces[ws_idx].coordinator_prompt.clone();
        let model = self.workspaces[ws_idx].config.model.clone();
        let max_turns = self.workspaces[ws_idx].config.max_turns;
        let workspace_root = self.workspaces[ws_idx].workspace_root.clone();
        let msg_done_tx = self
            .msg_done_tx
            .as_ref()
            .expect("msg_done_tx not initialized")
            .clone();
        let text = text.to_owned();

        // Spawn the coordinator work in the background.
        tokio::spawn(async move {
            // Build a callback that streams tokens to the TUI via socket.
            let send_responder = responder.clone();
            let send_fn = move |text: String| {
                let resp = send_responder.clone();
                async move {
                    let _ = resp.send(TuiResponse::Token { text });
                    Ok(())
                }
            };

            // Run the coordinator turn.
            let result = run_coordinator_turn_standalone(
                &coordinator_prompt,
                &model,
                max_turns,
                &workspace_root,
                &text,
                resume_id.as_deref(),
                send_fn,
            )
            .await;

            let outcome = match result {
                Ok((final_text, session_id, _dispatches)) => {
                    // Send the final text as a token.
                    if !final_text.is_empty() {
                        let _ = responder.send(TuiResponse::Token { text: final_text });
                    }
                    let _ = responder.send(TuiResponse::Done);

                    MessageOutcome::Success {
                        session_id,
                        resume_id,
                        buttons: vec![],
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "TUI coordinator error");
                    let _ = responder.send(TuiResponse::Error {
                        text: format!("{e}"),
                    });
                    MessageOutcome::Error {
                        had_resume: resume_id.is_some(),
                    }
                }
            };

            // Send result back to the main loop for state mutations.
            let _ = msg_done_tx.send(MessageResult {
                ws_idx,
                chat_id: Self::TUI_CHAT_ID,
                outcome,
            });
        });

        self.active_workspace = None;
    }

    /// Handle a TUI dispatch request by running `swarm create`.
    async fn handle_tui_dispatch(
        &self,
        repo: &str,
        description: &str,
        responder: tui_socket::ResponseSender,
    ) {
        let ws_idx = 0;
        if ws_idx >= self.workspaces.len() {
            let _ = responder.send(TuiResponse::Error {
                text: "no workspace available".into(),
            });
            return;
        }

        let Some(swarm_bin) = self.swarm_bin.clone() else {
            let _ = responder.send(TuiResponse::Error {
                text: "swarm binary not found".into(),
            });
            return;
        };

        let workspace_root = self.workspaces[ws_idx].workspace_root.clone();
        let repo = repo.to_owned();
        let description = description.to_owned();

        tokio::spawn(async move {
            let prompt_file =
                std::env::temp_dir().join(format!("dispatch-{}.txt", uuid::Uuid::new_v4()));
            if let Err(e) = std::fs::write(&prompt_file, &description) {
                let _ = responder.send(TuiResponse::Error {
                    text: format!("failed to write prompt file: {e}"),
                });
                return;
            }

            let _ = responder.send(TuiResponse::Token {
                text: format!("Dispatching task to `{repo}`..."),
            });

            let ws_str = workspace_root.to_string_lossy().to_string();
            let pf_str = prompt_file.to_string_lossy().to_string();
            match tokio::process::Command::new(&swarm_bin)
                .args([
                    "--dir",
                    &ws_str,
                    "create",
                    "--repo",
                    &repo,
                    "--prompt-file",
                    &pf_str,
                ])
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let _ = responder.send(TuiResponse::Token {
                        text: format!("Worker created for `{repo}`: {}", stdout.trim()),
                    });
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let _ = responder.send(TuiResponse::Error {
                        text: format!("dispatch failed: {}", stderr.trim()),
                    });
                }
                Err(e) => {
                    let _ = responder.send(TuiResponse::Error {
                        text: format!("failed to run swarm: {e}"),
                    });
                }
            }

            let _ = responder.send(TuiResponse::Done);
            let _ = std::fs::remove_file(&prompt_file);
        });
    }

    /// Handle a TUI submit-dispatch request: run the pipeline (refine → context → plan).
    async fn handle_tui_submit_dispatch(
        &mut self,
        repos: Vec<String>,
        task: String,
        _responder: tui_socket::ResponseSender,
    ) {
        let ws_idx = 0;
        if ws_idx >= self.workspaces.len() {
            let _ = _responder.send(TuiResponse::Error {
                text: "no workspace available".into(),
            });
            return;
        }

        // Set daemon-owned dispatch state with per-repo trackers.
        let repo_stages: Vec<RepoStageTracker> = repos
            .iter()
            .map(|r| RepoStageTracker {
                repo: r.clone(),
                stage: RepoPipelineStage::Pending,
                context_md: None,
                plan_md: None,
                file_count: None,
                step_count: None,
            })
            .collect();

        self.active_tui_dispatch = Some(ActiveTuiDispatch {
            task: task.clone(),
            repos: repos.clone(),
            status: TuiDispatchStatus::Refining,
            title: String::new(),
            task_md: String::new(),
            repo_stages,
        });
        self.broadcast_dispatch_update();

        let workspace_root = self.workspaces[ws_idx].workspace_root.clone();
        let conventions = load_workspace(&workspace_root)
            .ok()
            .and_then(|w| w.conventions);
        let msg_done_tx = self
            .msg_done_tx
            .as_ref()
            .expect("msg_done_tx not initialized")
            .clone();

        // Create progress channel for incremental updates.
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::pipeline::PipelineEvent>();

        // Spawn forwarding task: reads progress events and sends them as MessageResult.
        let fwd_tx = msg_done_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = progress_rx.recv().await {
                let _ = fwd_tx.send(MessageResult {
                    ws_idx,
                    chat_id: DaemonRunner::TUI_CHAT_ID,
                    outcome: MessageOutcome::PipelineProgress(event),
                });
            }
        });

        tokio::spawn(async move {
            let repo_refs: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
            let result =
                crate::pipeline::run_pipeline_multi(crate::pipeline::MultiPipelineOptions {
                    raw_prompt: &task,
                    workspace_root: &workspace_root,
                    conventions: conventions.as_deref(),
                    repos: repo_refs,
                    profile: "default",
                    until: None,
                    dry_run: true,
                    on_progress: Some(progress_tx),
                })
                .await;

            let outcome = match result {
                Ok(pipeline_result) => {
                    tracing::info!(repos = pipeline_result.per_repo.len(), "Pipeline complete");
                    MessageOutcome::PipelineComplete(pipeline_result)
                }
                Err(e) => {
                    tracing::error!(error = %e, "Pipeline error");
                    let _ = _responder.send(TuiResponse::Error {
                        text: format!("{e}"),
                    });
                    MessageOutcome::Error { had_resume: false }
                }
            };

            let _ = msg_done_tx.send(MessageResult {
                ws_idx,
                chat_id: DaemonRunner::TUI_CHAT_ID,
                outcome,
            });
        });

        self.active_workspace = None;
    }

    /// Handle TUI confirm-dispatch: dispatch each repo using Worker with pipeline artifacts.
    async fn handle_tui_confirm_dispatch(&mut self, responder: tui_socket::ResponseSender) {
        let dispatch = match self.active_tui_dispatch.take() {
            Some(d) if d.status == TuiDispatchStatus::Ready => d,
            Some(d) => {
                let msg = if d.status != TuiDispatchStatus::Ready {
                    "dispatch still in progress"
                } else {
                    "no pipeline result to confirm"
                };
                self.active_tui_dispatch = Some(d);
                let _ = responder.send(TuiResponse::Error { text: msg.into() });
                return;
            }
            None => {
                let _ = responder.send(TuiResponse::Error {
                    text: "no active dispatch".into(),
                });
                return;
            }
        };

        // Check that we have at least one repo with context/plan data.
        let has_results = dispatch
            .repo_stages
            .iter()
            .any(|rs| rs.context_md.is_some() || rs.plan_md.is_some());
        if !has_results {
            let _ = responder.send(TuiResponse::Error {
                text: "no pipeline results to confirm".into(),
            });
            self.active_tui_dispatch = Some(dispatch);
            return;
        }

        let ws_idx = 0;
        let workspace_root = self.workspaces[ws_idx].workspace_root.clone();
        let count = dispatch.repo_stages.len();

        // Clear persisted dispatch state.
        clear_dispatch_state(&workspace_root);

        // Broadcast cleared state.
        self.broadcast_dispatch_cleared();

        let _ = responder.send(TuiResponse::Token {
            text: format!("Dispatching {count} worker(s)..."),
        });

        let title = dispatch.title.clone();
        let task_md = dispatch.task_md.clone();

        // Spawn each dispatch as a Worker::dispatch_pipeline_task.
        for rs in dispatch.repo_stages {
            let root = workspace_root.clone();
            let resp = responder.clone();
            let title = title.clone();
            let task_md = task_md.clone();

            tokio::spawn(async move {
                let prompt =
                    crate::pipeline::dispatch::build_dispatch_prompt(&title, Some(&rs.repo));
                let worker = crate::worker::Worker::new();
                match worker
                    .dispatch_pipeline_task(
                        &root,
                        &prompt,
                        Some(&rs.repo),
                        "default",
                        &task_md,
                        rs.context_md.as_deref(),
                        rs.plan_md.as_deref(),
                    )
                    .await
                {
                    Ok(worktree_id) => {
                        let _ = resp.send(TuiResponse::Token {
                            text: format!("Worker created for `{}`: {worktree_id}", rs.repo),
                        });
                    }
                    Err(e) => {
                        let _ = resp.send(TuiResponse::Error {
                            text: format!("dispatch `{}` failed: {e}", rs.repo),
                        });
                    }
                }
            });
        }
    }

    /// Handle TUI cancel-dispatch: clear the active dispatch.
    fn handle_tui_cancel_dispatch(&mut self) {
        self.active_tui_dispatch = None;
        if let Some(ws) = self.workspaces.first() {
            clear_dispatch_state(&ws.workspace_root);
        }
        self.broadcast_dispatch_cleared();
    }

    /// Build a DispatchUpdate response from the current active_tui_dispatch.
    fn build_dispatch_update_response(d: &ActiveTuiDispatch) -> TuiResponse {
        let per_repo: Vec<tui_socket::RepoDispatchInfo> = d
            .repo_stages
            .iter()
            .map(|rs| tui_socket::RepoDispatchInfo {
                repo: rs.repo.clone(),
                stage: rs.stage.as_str().to_string(),
                context_md: rs.context_md.clone(),
                plan_md: rs.plan_md.clone(),
                file_count: rs.file_count,
                step_count: rs.step_count,
            })
            .collect();
        TuiResponse::DispatchUpdate {
            task: d.task.clone(),
            repos: d.repos.clone(),
            status: match d.status {
                TuiDispatchStatus::Refining => "refining".into(),
                TuiDispatchStatus::Running => "running".into(),
                TuiDispatchStatus::Ready => "ready".into(),
            },
            streaming_text: String::new(),
            task_md: d.task_md.clone(),
            title: d.title.clone(),
            per_repo,
        }
    }

    /// Handle TUI get-dispatch-state: reply with current dispatch state.
    fn handle_tui_get_dispatch_state(&self, responder: tui_socket::ResponseSender) {
        if let Some(ref d) = self.active_tui_dispatch {
            let _ = responder.send(Self::build_dispatch_update_response(d));
        } else {
            let _ = responder.send(TuiResponse::DispatchCleared);
        }
    }

    /// Broadcast the current dispatch state to all TUI clients via the notify channel.
    fn broadcast_dispatch_update(&self) {
        if let Some(ref d) = self.active_tui_dispatch {
            self.tui_push_response(Self::build_dispatch_update_response(d));
        }
    }

    /// Broadcast dispatch-cleared to all TUI clients.
    fn broadcast_dispatch_cleared(&self) {
        self.tui_push_response(TuiResponse::DispatchCleared);
    }

    /// Push a response to all connected TUI clients via the broadcast channel.
    fn tui_push_response(&self, response: TuiResponse) {
        if let Some(ref socket) = self.tui_socket {
            socket.push_response(response);
        }
    }

    /// Check if TUI socket has connected clients.
    fn tui_has_clients(&self) -> bool {
        self.tui_socket.as_ref().is_some_and(|s| s.has_clients())
    }

    /// Push a notification to TUI socket clients.
    fn tui_push_notification(&self, event: crate::ui::inbox::UiEvent) {
        if let Some(ref socket) = self.tui_socket {
            socket.push_notification(event);
        }
    }

    /// Poll buzz signals and auto-triage critical/warning ones.
    ///
    /// Iterates all workspaces. For each: if it has a buzz_slot, polls inline
    /// watchers; otherwise reads new signals from the JSONL file.
    async fn poll_buzz(&mut self) -> Result<()> {
        // Collect (ws_idx, signal, topic) tuples across all workspaces.
        struct TaggedSignal {
            ws_idx: usize,
            signal: Signal,
        }
        let mut tagged: Vec<TaggedSignal> = Vec::new();

        for idx in 0..self.workspaces.len() {
            let ws = &mut self.workspaces[idx];

            if let Some(ref mut slot) = ws.buzz_slot {
                // Inline watchers — poll directly.
                let mut slot_signals = Vec::new();
                for watcher in slot.watchers.iter_mut() {
                    match watcher.poll().await {
                        Ok(signals) => {
                            if !signals.is_empty() {
                                tracing::debug!(
                                    name = %slot.name,
                                    watcher = %watcher.name(),
                                    count = signals.len(),
                                    "Watcher returned signals",
                                );
                            }
                            slot_signals.extend(signals);
                        }
                        Err(e) => {
                            tracing::error!(name = %slot.name, watcher = %watcher.name(), error = %e, "Watcher poll error");
                        }
                    }
                }

                let reminder_signals = reminder::check_reminders(&mut slot.reminders);
                if !reminder_signals.is_empty() {
                    tracing::info!(
                        name = %slot.name,
                        count = reminder_signals.len(),
                        "Buzz reminders fired",
                    );
                    slot_signals.extend(reminder_signals);
                }

                save_cursors(&slot.watchers, &slot.workspace_root);

                if let Err(e) = output::emit(&slot_signals, &slot.output) {
                    tracing::error!(name = %slot.name, error = %e, "Failed to write buzz signals");
                }

                let mut deduped = deduplicate(&slot_signals);
                prioritize(&mut deduped);
                tagged.extend(deduped.into_iter().map(|s| TaggedSignal {
                    ws_idx: idx,
                    signal: s,
                }));
            } else {
                // JSONL fallback.
                let signals = ws.buzz_reader.poll().unwrap_or_default();
                ws.sessions.set_buzz_offset(ws.buzz_reader.offset());
                tagged.extend(signals.into_iter().map(|s| TaggedSignal {
                    ws_idx: idx,
                    signal: s,
                }));
            }
        }

        // Filter for important signals.
        let important: Vec<&TaggedSignal> = tagged
            .iter()
            .filter(|t| matches!(t.signal.severity, Severity::Critical | Severity::Warning))
            .collect();

        if important.is_empty() {
            return Ok(());
        }

        tracing::info!(count = important.len(), "New buzz signals to triage");

        for t in important {
            self.active_workspace = Some(t.ws_idx);
            let alert_chat_id = self.active_ws().config.telegram.alert_chat_id;
            let assessment = self.triage_signal(&t.signal).await;
            let alert = format_triage_alert(&t.signal, &assessment);
            self.send(alert_chat_id, &alert).await?;
        }
        self.active_workspace = None;

        Ok(())
    }

    /// Poll user-facing reminders across all workspaces and send fired ones to Telegram.
    async fn poll_reminders(&mut self) -> Result<()> {
        for idx in 0..self.workspaces.len() {
            // Reload from disk to pick up CLI-created reminders.
            self.workspaces[idx].reminder_store =
                ReminderStore::load(&self.workspaces[idx].workspace_root);

            let fired = self.workspaces[idx].reminder_store.check_and_advance();
            if fired.is_empty() {
                continue;
            }

            tracing::info!(
                name = %self.workspaces[idx].name,
                count = fired.len(),
                "User reminders fired",
            );

            self.active_workspace = Some(idx);
            for r in &fired {
                let alert_chat_id = self.active_ws().config.telegram.alert_chat_id;
                let chat_id = r.chat_id.unwrap_or(alert_chat_id);
                let text = format!("🔔 Reminder: {}", r.message);
                self.send(chat_id, &text).await?;
                tracing::info!(
                    message = %r.message,
                    chat_id,
                    "Reminder fired",
                );
            }

            self.workspaces[idx].reminder_store.save()?;
        }
        self.active_workspace = None;
        Ok(())
    }

    /// Compute the sweep schedule from inline watchers' sentry config.
    fn compute_sweep_schedule(
        watchers: &[Box<dyn Watcher>],
    ) -> (Option<chrono::DateTime<chrono::Utc>>, Option<String>) {
        use crate::buzz::watcher::sentry::SentryWatcher;

        for watcher in watchers {
            if let Some(sw) = watcher.as_any().downcast_ref::<SentryWatcher>()
                && let Some(schedule) = sw.sweep_schedule()
                && let Ok(cron) = schedule.parse::<croner::Cron>()
            {
                let now = chrono::Utc::now();
                if let Ok(next) = cron.find_next_occurrence(&now, false) {
                    return (Some(next), Some(schedule.to_string()));
                }
            }
        }
        (None, None)
    }

    /// Convert the earliest `next_sweep_at` across all workspace buzz slots to a `tokio::time::Instant`.
    fn sweep_instant(&self) -> Option<tokio::time::Instant> {
        let next = self
            .workspaces
            .iter()
            .filter_map(|ws| ws.buzz_slot.as_ref())
            .filter_map(|s| s.next_sweep_at)
            .min()?;
        let now_chrono = chrono::Utc::now();
        let duration = (next - now_chrono).to_std().ok()?;
        Some(tokio::time::Instant::now() + duration)
    }

    /// Advance sweep timers for workspace buzz slots where `next_sweep_at <= now`.
    fn advance_sweep_timer(&mut self) {
        let now = chrono::Utc::now();
        for ws in &mut self.workspaces {
            if let Some(ref mut slot) = ws.buzz_slot
                && let Some(next) = slot.next_sweep_at
                && next <= now
                && let Some(ref expr) = slot.sweep_cron_expr
                && let Ok(cron) = expr.parse::<croner::Cron>()
            {
                slot.next_sweep_at = cron.find_next_occurrence(&now, false).ok();
                if let Some(ref next) = slot.next_sweep_at {
                    tracing::debug!(name = %slot.name, %next, "Next sweep scheduled");
                }
            }
        }
    }

    /// Run a sweep across all due workspace buzz slots and send a per-workspace summary digest.
    /// Returns the number of signals found.
    async fn poll_sweep(&mut self) -> Result<usize> {
        let now = chrono::Utc::now();

        struct SweepResult {
            ws_idx: usize,
            message: String,
        }
        let mut results: Vec<SweepResult> = Vec::new();
        let mut total_count = 0usize;

        for idx in 0..self.workspaces.len() {
            let ws = &mut self.workspaces[idx];
            let Some(ref mut slot) = ws.buzz_slot else {
                continue;
            };

            // Only sweep slots that are due.
            if let Some(next) = slot.next_sweep_at {
                if next > now {
                    continue;
                }
            } else {
                continue;
            }

            let mut slot_signals = Vec::new();
            for watcher in slot.watchers.iter_mut() {
                if !watcher.has_sweep() {
                    continue;
                }
                match watcher.sweep().await {
                    Ok(signals) => {
                        if !signals.is_empty() {
                            tracing::info!(
                                name = %slot.name,
                                watcher = %watcher.name(),
                                count = signals.len(),
                                "Sweep returned signals",
                            );
                        }
                        slot_signals.extend(signals);
                    }
                    Err(e) => {
                        tracing::error!(
                            name = %slot.name,
                            watcher = %watcher.name(),
                            error = %e,
                            "Sweep error",
                        );
                    }
                }
            }

            save_cursors(&slot.watchers, &slot.workspace_root);

            if slot_signals.is_empty() {
                tracing::debug!(name = %slot.name, "Sweep complete — no issues to re-triage");
                continue;
            }

            let message = build_sweep_summary(&slot.name, &slot_signals);
            tracing::info!(name = %slot.name, "Sweep digest ready");
            write_last_sweep(&slot.workspace_root, &message);

            total_count += slot_signals.len();
            results.push(SweepResult {
                ws_idx: idx,
                message,
            });
        }

        // Phase 2: Send all digests to Telegram.
        for r in &results {
            self.active_workspace = Some(r.ws_idx);
            let alert_chat_id = self.active_ws().config.telegram.alert_chat_id;
            self.send(alert_chat_id, &r.message).await?;
        }
        self.active_workspace = None;

        if results.is_empty() {
            tracing::info!("Sweep complete — no issues to re-triage across all workspaces");
        }

        Ok(total_count)
    }

    /// Handle `/sweep` Telegram command — run a sweep on demand for the active workspace.
    async fn handle_sweep_command(&mut self, chat_id: i64) -> Result<()> {
        let has_buzz = self.workspaces.iter().any(|ws| ws.buzz_slot.is_some());

        if !has_buzz {
            return self
                .send(
                    chat_id,
                    "Inline buzz watchers are not enabled — sweep unavailable.",
                )
                .await;
        }

        let has_sweep = self.workspaces.iter().any(|ws| {
            ws.buzz_slot
                .as_ref()
                .is_some_and(|s| s.watchers.iter().any(|w| w.has_sweep()))
        });

        if !has_sweep {
            return self
                .send(
                    chat_id,
                    "No watchers support sweep (is sentry.sweep configured?).",
                )
                .await;
        }

        self.send(chat_id, "Running sweep...").await?;

        // Manual sweep: clear seen issues so everything is re-evaluated as fresh,
        // then force all sweep-capable slots due.
        let past = chrono::Utc::now() - chrono::Duration::seconds(1);
        for ws in &mut self.workspaces {
            if let Some(ref mut slot) = ws.buzz_slot
                && slot.next_sweep_at.is_some()
            {
                slot.next_sweep_at = Some(past);
                for watcher in &mut slot.watchers {
                    if let Some(sw) = watcher
                        .as_any_mut()
                        .downcast_mut::<crate::buzz::watcher::sentry::SentryWatcher>()
                    {
                        sw.clear_seen_issues();
                    }
                }
            }
        }

        // Save workspace context — poll_sweep changes active_workspace internally.
        let saved_ws = self.active_workspace;
        match self.poll_sweep().await {
            Ok(0) => {
                self.active_workspace = saved_ws;
                self.send(chat_id, "Sweep complete — nothing to re-triage.")
                    .await?;
            }
            Ok(_count) => {
                // Results already sent to Telegram per-workspace by poll_sweep,
                // and written to .hive/last_sweep.md for coordinator context.
                self.active_workspace = saved_ws;
            }
            Err(e) => {
                self.active_workspace = saved_ws;
                self.send(chat_id, &format!("Sweep failed: {e}")).await?;
            }
        }

        // Advance timers after forced sweep.
        self.advance_sweep_timer();

        Ok(())
    }

    /// Handle `/remind` Telegram command.
    async fn handle_remind_command(&mut self, chat_id: i64, args: &str) -> Result<()> {
        if args.is_empty() {
            return self
                .send(
                    chat_id,
                    "Usage:\n\
                     /remind 30m Check PR status\n\
                     /remind --cron \"0 9 * * *\" Daily standup",
                )
                .await;
        }

        let result = if let Some(rest) = args.strip_prefix("--cron ") {
            match user_reminder::parse_cron_args(rest) {
                Ok((cron_expr, message)) => {
                    user_reminder::create_cron(&cron_expr, &message, Some(chat_id))
                }
                Err(e) => Err(e),
            }
        } else {
            match args.split_once(' ') {
                Some((duration_str, message)) => {
                    user_reminder::create_oneshot(duration_str, message.trim(), Some(chat_id))
                }
                None => Err("Usage: /remind <duration> <message>".into()),
            }
        };

        match result {
            Ok(r) => {
                let fire_str = r
                    .fire_at
                    .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "unknown".into());
                let short_id = r.id[..8.min(r.id.len())].to_owned();
                let msg = r.message.clone();

                // Reload, add, save.
                let ws = self.active_ws_mut();
                ws.reminder_store = ReminderStore::load(&ws.workspace_root);
                ws.reminder_store.add(r);
                ws.reminder_store.save()?;

                tracing::info!(short_id, chat_id, "Created reminder from Telegram");
                self.send(
                    chat_id,
                    &format!(
                        "Reminder set (ID: `{short_id}`)\n\
                         Next fire: {fire_str}\n\
                         Message: {msg}"
                    ),
                )
                .await
            }
            Err(e) => self.send(chat_id, &format!("Error: {e}")).await,
        }
    }

    /// Handle `/reminders` Telegram command.
    async fn handle_reminders_command(&mut self, chat_id: i64, args: &str) -> Result<()> {
        {
            let ws = self.active_ws_mut();
            ws.reminder_store = ReminderStore::load(&ws.workspace_root);
        }

        if let Some(rest) = args.strip_prefix("cancel") {
            let id_prefix = rest.trim();
            if id_prefix.is_empty() {
                return self.send(chat_id, "Usage: /reminders cancel <id>").await;
            }
            match self.active_ws_mut().reminder_store.cancel(id_prefix) {
                Ok(cancelled_id) => {
                    self.active_ws_mut().reminder_store.save()?;
                    let short = &cancelled_id[..8.min(cancelled_id.len())];
                    tracing::info!(
                        reminder_id = short,
                        chat_id,
                        "Cancelled reminder from Telegram"
                    );
                    self.send(chat_id, &format!("Cancelled reminder `{short}`."))
                        .await
                }
                Err(user_reminder::CancelError::NotFound) => {
                    self.send(
                        chat_id,
                        &format!("No reminder found matching `{id_prefix}`."),
                    )
                    .await
                }
                Err(user_reminder::CancelError::Ambiguous(ids)) => {
                    let list = ids
                        .iter()
                        .map(|id| format!("  `{}`", &id[..8.min(id.len())]))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.send(
                        chat_id,
                        &format!("Multiple reminders match `{id_prefix}`:\n{list}"),
                    )
                    .await
                }
            }
        } else {
            let active = self.active_ws().reminder_store.active();
            let text = user_reminder::format_reminder_list(&active);
            self.send(chat_id, &text).await
        }
    }

    /// Check if the hive UI is the active channel (for notification routing).
    fn should_use_ui(&self) -> bool {
        crate::presence::active_channel(&self.active_ws().workspace_root) == "ui"
    }

    /// Poll swarm state for agent completion notifications across all workspaces.
    ///
    /// To avoid Telegram rate-limiting (~1 msg/sec per chat), notifications are
    /// sent with a 500ms delay between consecutive messages. When 3 or more
    /// notifications land in a single poll cycle for the same chat, they are
    /// batched into a single summary message instead of sent individually.
    async fn poll_swarm(&mut self) -> Result<()> {
        for idx in 0..self.workspaces.len() {
            let ws = &mut self.workspaces[idx];
            let watcher = match ws.swarm_watcher.as_mut() {
                Some(w) => w,
                None => continue,
            };

            let notifications = watcher.poll();
            if notifications.is_empty() {
                continue;
            }

            // Expire stale pending origins (older than 60 seconds).
            ws.pending_origins
                .retain(|p| p.created_at.elapsed().as_secs() < 60);

            let alert_chat_id = ws.config.telegram.alert_chat_id;
            let mut origin_map_changed = false;

            // Phase 1: Process origin map and collect routed (chat_id, notification) pairs.
            let mut routed: Vec<(i64, &swarm_watcher::SwarmNotification)> = Vec::new();

            for notification in &notifications {
                let wt_id = notification.worktree_id();

                if let swarm_watcher::SwarmNotification::AgentSpawned {
                    worktree_id,
                    branch,
                    ..
                } = notification
                    && let Some(pending) = ws.pending_origins.pop()
                {
                    ws.origin_map.insert(
                        worktree_id.clone(),
                        OriginEntry {
                            origin: pending.origin,
                            quest_id: None,
                            task_id: None,
                            branch: Some(branch.clone()),
                            created_at: chrono::Utc::now(),
                        },
                    );
                    origin_map_changed = true;
                }

                let target_chat_id = route_notification(&ws.origin_map, wt_id, alert_chat_id);
                routed.push((target_chat_id, notification));

                match notification {
                    swarm_watcher::SwarmNotification::AgentCompleted { worktree_id, .. }
                    | swarm_watcher::SwarmNotification::AgentClosed { worktree_id, .. } => {
                        if ws.origin_map.remove(worktree_id).is_some() {
                            origin_map_changed = true;
                        }
                    }
                    swarm_watcher::SwarmNotification::PrOpened { .. }
                    | swarm_watcher::SwarmNotification::AgentSpawned { .. }
                    | swarm_watcher::SwarmNotification::AgentStalled { .. }
                    | swarm_watcher::SwarmNotification::AgentWaiting { .. } => {}
                }
            }

            // Phase 2: Group by chat_id, preserving insertion order.
            let mut by_chat: Vec<(i64, Vec<&swarm_watcher::SwarmNotification>)> = Vec::new();
            for (chat_id, notification) in &routed {
                if let Some((_, group)) = by_chat.iter_mut().find(|(id, _)| id == chat_id) {
                    group.push(notification);
                } else {
                    by_chat.push((*chat_id, vec![notification]));
                }
            }

            // Set active workspace for send() routing.
            self.active_workspace = Some(idx);

            // Phase 3: Send — use routing::decide() for each notification.
            let ui_active = self.should_use_ui();
            let mut telegram_by_chat: Vec<(i64, Vec<&swarm_watcher::SwarmNotification>)> =
                Vec::new();

            for (chat_id, group) in &by_chat {
                let mut telegram_group: Vec<&swarm_watcher::SwarmNotification> = Vec::new();
                for n in group {
                    let urgent = matches!(n, swarm_watcher::SwarmNotification::AgentStalled { .. });
                    let decision = crate::routing::decide(ui_active, urgent);

                    if matches!(
                        decision,
                        crate::routing::RoutingDecision::UiOnly
                            | crate::routing::RoutingDecision::Both
                    ) && let Some(ui_event) = notification_to_ui_event(n)
                    {
                        // Push via socket if TUI clients are connected,
                        // otherwise fall back to file-based inbox.
                        if self.tui_has_clients() {
                            self.tui_push_notification(ui_event);
                        } else {
                            let _ = crate::ui::inbox::push_event(
                                &self.workspaces[idx].workspace_root,
                                &ui_event,
                            );
                        }
                    }

                    if matches!(
                        decision,
                        crate::routing::RoutingDecision::TelegramOnly
                            | crate::routing::RoutingDecision::Both
                    ) {
                        telegram_group.push(n);
                    }
                }
                if !telegram_group.is_empty() {
                    telegram_by_chat.push((*chat_id, telegram_group));
                }
            }

            // Send the Telegram portion.
            let mut first_send = true;
            for (chat_id, group) in &telegram_by_chat {
                if group.len() >= 3 {
                    let text = format_swarm_batch(group);
                    if !first_send {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    if let Err(e) = self.send(*chat_id, &text).await {
                        tracing::error!(chat_id, error = %e, "Failed to send to chat — retrying on alert_chat_id");
                        if *chat_id != alert_chat_id {
                            let _ = self.send(alert_chat_id, &text).await;
                        }
                    }
                    first_send = false;
                } else {
                    for n in group {
                        if !first_send {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                        let ci_status =
                            if let swarm_watcher::SwarmNotification::PrOpened { pr_url, .. } = n {
                                let ci = swarm_watcher::fetch_ci_status(pr_url).await;
                                if let Some(ref status) = ci {
                                    tracing::debug!(
                                        worktree_id = %n.worktree_id(),
                                        status = %status.status_line(),
                                        "CI status",
                                    );
                                }
                                ci
                            } else {
                                None
                            };
                        let text = n.format_telegram_with_ci(ci_status.as_ref());
                        let buttons = n.inline_buttons_with_ci(ci_status.as_ref());
                        let result = if buttons.is_empty() {
                            self.send(*chat_id, &text).await
                        } else {
                            self.send_with_buttons(*chat_id, &text, buttons).await
                        };
                        if let Err(e) = result {
                            tracing::error!(chat_id, error = %e, "Failed to send to chat — retrying on alert_chat_id");
                            if *chat_id != alert_chat_id {
                                let _ = self.send(alert_chat_id, &text).await;
                            }
                        }
                        first_send = false;
                    }
                }
            }

            // Phase 4: Auto-triage eligible notifications.
            if self.workspaces[idx]
                .config
                .swarm_watch
                .as_ref()
                .is_some_and(|sw| sw.auto_triage)
            {
                let mut auto_triaged: HashSet<String> = HashSet::new();
                for (chat_id, notification) in &routed {
                    let wt_id = notification.worktree_id().to_owned();
                    if auto_triaged.contains(&wt_id) {
                        continue;
                    }
                    let pr_url_for_fetch = match notification {
                        swarm_watcher::SwarmNotification::PrOpened { pr_url, .. } => {
                            Some(pr_url.as_str())
                        }
                        swarm_watcher::SwarmNotification::AgentWaiting {
                            pr_url: Some(url),
                            ..
                        } => Some(url.as_str()),
                        _ => None,
                    };
                    let pr_details = pr_url_for_fetch.and_then(fetch_pr_details);
                    if let Some(prompt) = auto_triage_prompt(notification, pr_details.as_deref()) {
                        auto_triaged.insert(wt_id);
                        let chat_id = *chat_id;
                        let channel = self.active_channel().clone();
                        let notification_topic_id = self.workspaces[idx].config.telegram.topic_id;
                        let coordinator_prompt = self.workspaces[idx].coordinator_prompt.clone();
                        let model = self.workspaces[idx].config.model.clone();
                        let workspace_root = self.workspaces[idx].workspace_root.clone();
                        let worktree_id = notification.worktree_id().to_owned();
                        let notification_kind = match notification {
                            swarm_watcher::SwarmNotification::PrOpened { .. } => "PrOpened",
                            swarm_watcher::SwarmNotification::AgentWaiting { .. } => "AgentWaiting",
                            _ => "notification",
                        };
                        tokio::spawn(async move {
                            tracing::info!(
                                notification_kind,
                                worktree_id,
                                "Auto-triaging notification",
                            );

                            let system_prompt = format!(
                                "{coordinator_prompt}\n\n\
                             ## Auto-Triage Mode\n\
                             You are doing an automated background check triggered by a swarm event.\n\
                             Use your tools to inspect the situation (check PR status with `gh pr view`, read relevant files, etc).\n\
                             Then respond in this exact format:\n\
                             1. One sentence stating what you found (e.g. \"PR #27 has 88 additions, all tests passing, clean diff fixing X\")\n\
                             2. One clear yes/no question (e.g. \"Want me to merge and close the worker?\")\n\
                             Do NOT say \"I will\" or \"let me\" — you are writing a summary, not starting a task.",
                            );

                            let opts = SessionOptions {
                                system_prompt: Some(system_prompt),
                                model: Some(model),
                                max_turns: Some(7),
                                no_session_persistence: true,
                                working_dir: Some(workspace_root),
                                allowed_tools: vec![
                                    "Bash".into(),
                                    "Read".into(),
                                    "Glob".into(),
                                    "Grep".into(),
                                ],
                                ..Default::default()
                            };

                            let client = ClaudeClient::new();
                            let session = match client.spawn(opts).await {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to spawn auto-triage session");
                                    return;
                                }
                            };

                            let response = match run_ephemeral_session(session, &prompt).await {
                                Ok(text) if !text.is_empty() => text,
                                Ok(_) => {
                                    tracing::warn!("Auto-triage session produced no output");
                                    return;
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Auto-triage session error");
                                    return;
                                }
                            };

                            tracing::debug!(
                                chars = response.len(),
                                text = %truncate(&response, 80),
                                "Auto-triage suggestion",
                            );

                            let header = match notification_kind {
                                "PrOpened" => {
                                    format!("🤖 *Auto\\-triage* — `{worktree_id}` opened a PR")
                                }
                                "AgentWaiting" => {
                                    format!("🤖 *Auto\\-triage* — `{worktree_id}` is waiting")
                                }
                                _ => format!("🤖 *Auto\\-triage* — `{worktree_id}`"),
                            };
                            let (clean_response, buttons) = extract_buttons(&response);
                            let body = markdown::sanitize_for_telegram(&clean_response);
                            let text = format!("{header}\n\n{body}");
                            let chunks = split_message(&text, 4000);
                            let last_idx = chunks.len().saturating_sub(1);
                            for (i, chunk) in chunks.into_iter().enumerate() {
                                if let Err(e) = channel
                                    .send_message(&OutboundMessage {
                                        chat_id,
                                        text: chunk,
                                        topic_id: notification_topic_id,
                                        buttons: if i == last_idx {
                                            buttons.clone()
                                        } else {
                                            vec![]
                                        },
                                    })
                                    .await
                                {
                                    tracing::error!(error = %e, "Failed to send auto-triage result");
                                }
                            }
                        });
                    }
                }
            }

            // Persist origin map if it changed.
            if origin_map_changed
                && let Err(e) = self.workspaces[idx]
                    .origin_map
                    .save(&self.workspaces[idx].workspace_root)
            {
                tracing::error!(error = %e, "Failed to save origin map");
            }
        } // end per-workspace loop

        self.active_workspace = None;
        Ok(())
    }

    /// Send a signal to an ephemeral Claude session for triage assessment.
    async fn triage_signal(&self, signal: &Signal) -> String {
        let prompt = format!(
            "You are a triage assistant. Assess this signal and recommend an action.\n\
             Keep your response under 3 sentences.\n\n\
             Source: {}\n\
             Severity: {}\n\
             Title: {}\n\
             Body: {}\n\
             {}",
            signal.source,
            signal.severity,
            signal.title,
            signal.body,
            signal
                .url
                .as_deref()
                .map(|u| format!("URL: {u}"))
                .unwrap_or_default(),
        );

        let opts = SessionOptions {
            system_prompt: Some("You are a concise triage assistant. Assess signals and recommend actions in 1-3 sentences.".into()),
            model: Some(self.active_ws().config.model.clone()),
            max_turns: Some(7),
            no_session_persistence: true,
            working_dir: Some(self.active_ws().workspace_root.clone()),
            ..Default::default()
        };

        let client = ClaudeClient::new();
        let session = match client.spawn(opts).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Failed to spawn triage session");
                return "[Triage session could not be started]".to_owned();
            }
        };

        match run_ephemeral_session(session, &prompt).await {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => "[Triage session produced no output]".to_owned(),
            Err(e) => {
                tracing::error!(error = %e, "Triage session error");
                "[Triage session produced no output]".to_owned()
            }
        }
    }

    /// Cache button labels for later lookup when callbacks arrive.
    fn cache_button_labels(&mut self, buttons: &[Vec<crate::channel::InlineButton>]) {
        for row in buttons {
            for btn in row {
                // Evict oldest entries if at capacity.
                while self.button_labels.len() >= BUTTON_LABEL_CACHE_MAX {
                    if let Some(old_key) = self.button_labels_order.pop_front() {
                        self.button_labels.remove(&old_key);
                    } else {
                        break;
                    }
                }
                self.button_labels
                    .insert(btn.callback_data.clone(), btn.text.clone());
                self.button_labels_order
                    .push_back(btn.callback_data.clone());
            }
        }
    }

    /// Send a message through the Telegram channel, splitting if too long.
    async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
        let text = markdown::sanitize_for_telegram(text);
        for chunk in split_message(&text, 4000) {
            self.active_channel()
                .send_message(&OutboundMessage {
                    chat_id,
                    text: chunk,
                    buttons: vec![],
                    topic_id: self.active_topic_id(),
                })
                .await?;
        }
        Ok(())
    }

    /// Send a message with inline keyboard buttons.
    async fn send_with_buttons(
        &self,
        chat_id: i64,
        text: &str,
        buttons: Vec<Vec<crate::channel::InlineButton>>,
    ) -> Result<()> {
        let text = markdown::sanitize_for_telegram(text);
        let chunks = split_message(&text, 4000);
        let last_idx = chunks.len().saturating_sub(1);
        for (i, chunk) in chunks.into_iter().enumerate() {
            self.active_channel()
                .send_message(&OutboundMessage {
                    chat_id,
                    text: chunk,
                    buttons: if i == last_idx {
                        buttons.clone()
                    } else {
                        vec![]
                    },
                    topic_id: self.active_topic_id(),
                })
                .await?;
        }
        Ok(())
    }
}

/// Send a message through a Telegram channel, sanitizing markdown and splitting if too long.
async fn send_to_telegram(
    channel: &TelegramChannel,
    chat_id: i64,
    text: &str,
    topic_id: Option<i64>,
) -> Result<()> {
    let text = markdown::sanitize_for_telegram(text);
    for chunk in split_message(&text, 4000) {
        channel
            .send_message(&OutboundMessage {
                chat_id,
                text: chunk,
                buttons: vec![],
                topic_id,
            })
            .await?;
    }
    Ok(())
}

/// Send a message with inline keyboard buttons through a Telegram channel.
async fn send_with_buttons_to_telegram(
    channel: &TelegramChannel,
    chat_id: i64,
    text: &str,
    buttons: Vec<Vec<crate::channel::InlineButton>>,
    topic_id: Option<i64>,
) -> Result<()> {
    let text = markdown::sanitize_for_telegram(text);
    let chunks = split_message(&text, 4000);
    let last_idx = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.into_iter().enumerate() {
        channel
            .send_message(&OutboundMessage {
                chat_id,
                text: chunk,
                buttons: if i == last_idx {
                    buttons.clone()
                } else {
                    vec![]
                },
                topic_id,
            })
            .await?;
    }
    Ok(())
}

/// Standalone coordinator turn: send message, stream intermediate text, return result.
///
/// This is the free-function version of `DaemonRunner::run_coordinator_turn`, taking
/// owned parameters so it can be called from a spawned task without borrowing `&self`.
async fn run_coordinator_turn_standalone<F, Fut>(
    coordinator_prompt: &str,
    model: &str,
    max_turns: u32,
    workspace_root: &Path,
    text: &str,
    resume_id: Option<&str>,
    send_fn: F,
) -> Result<(String, String, Vec<DispatchRequest>)>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let opts = SessionOptions {
        system_prompt: if resume_id.is_none() {
            Some(coordinator_prompt.to_owned())
        } else {
            None
        },
        model: Some(model.to_owned()),
        resume: resume_id.map(|s| s.to_owned()),
        max_turns: Some(max_turns.into()),
        working_dir: Some(workspace_root.to_path_buf()),
        allowed_tools: vec![
            "Bash".into(),
            "Read".into(),
            "Glob".into(),
            "Grep".into(),
            "WebSearch".into(),
            "WebFetch".into(),
        ],
        disallowed_tools: vec![
            "Task".into(),
            "Write".into(),
            "Edit".into(),
            "NotebookEdit".into(),
        ],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to spawn Claude session: {e}"))?;

    session
        .send_message(text)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to send message: {e}"))?;

    let mut session_id = String::new();
    let mut final_text = String::new();
    let mut sent_any = false;
    let mut all_dispatches: Vec<DispatchRequest> = Vec::new();

    loop {
        let event = session
            .next_event()
            .await
            .map_err(|e| color_eyre::eyre::eyre!("error reading event: {e}"))?;

        match event {
            Some(Event::Assistant { message, .. }) => {
                if let Some(sid) = &message.session_id {
                    session_id.clone_from(sid);
                }

                let is_end_turn = message.message.stop_reason.as_deref() == Some("end_turn");

                let mut block_text = String::new();
                for block in &message.message.content {
                    match block {
                        ContentBlock::Text { text } => block_text.push_str(text),
                        ContentBlock::ToolUse { name, input, .. } => {
                            tracing::debug!(
                                tool = %name,
                                input = %truncate(&input.to_string(), 120),
                                "Coordinator tool call",
                            );
                        }
                        _ => {}
                    }
                }

                if !block_text.is_empty() {
                    let (clean_text, dispatches) = extract_dispatch_blocks(&block_text);
                    all_dispatches.extend(dispatches);

                    if is_end_turn {
                        final_text = clean_text;
                    } else if !clean_text.is_empty() {
                        tracing::debug!(
                            chars = clean_text.len(),
                            text = %truncate(&clean_text, 80),
                            "Streaming intermediate",
                        );
                        if let Err(e) = send_fn(clean_text).await {
                            tracing::error!(error = %e, "Failed to send intermediate text");
                        } else {
                            sent_any = true;
                        }
                    }
                }

                if is_end_turn {
                    break;
                }
            }
            Some(Event::Result(result)) => {
                session_id = result.session_id;
                if let Some(text) = &result.result
                    && final_text.is_empty()
                    && !sent_any
                {
                    final_text = text.clone();
                }
                break;
            }
            Some(Event::RateLimit(rl)) => {
                if let Some(info) = &rl.rate_limit_info {
                    tracing::warn!(info = %info, "Rate limit status");
                }
            }
            Some(Event::System(sys)) => {
                let sid = sys
                    .data
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let model = sys
                    .data
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                tracing::debug!(session_id = sid, model, "Coordinator session");
                if session_id.is_empty() {
                    session_id = sid.to_owned();
                }
            }
            Some(Event::User(_)) => {}
            Some(Event::Stream { .. }) => {}
            None => break,
        }
    }

    session.close_stdin();

    if final_text.is_empty() && !sent_any {
        final_text = "[No response from coordinator]".to_owned();
    }

    Ok((final_text, session_id, all_dispatches))
}

/// Execute dispatch requests by running `swarm create` for each (standalone version).
async fn run_dispatches_standalone(
    chat_id: i64,
    dispatches: Vec<DispatchRequest>,
    swarm_bin: Option<PathBuf>,
    workspace_root: PathBuf,
    channel: Arc<TelegramChannel>,
    topic_id: Option<i64>,
) {
    let Some(swarm_bin) = swarm_bin else {
        if let Err(e) = send_to_telegram(
            &channel,
            chat_id,
            "❌ Cannot dispatch: `swarm` binary not found on this system.",
            topic_id,
        )
        .await
        {
            tracing::error!(error = %e, "Failed to send dispatch error");
        }
        return;
    };

    for dispatch in dispatches {
        let prompt_file =
            std::env::temp_dir().join(format!("dispatch-{}.txt", uuid::Uuid::new_v4()));
        if let Err(e) = std::fs::write(&prompt_file, &dispatch.prompt) {
            tracing::error!(error = %e, "Failed to write dispatch prompt file");
            let _ = send_to_telegram(
                &channel,
                chat_id,
                &format!(
                    "❌ Dispatch to `{}` failed: could not write prompt file",
                    dispatch.repo
                ),
                topic_id,
            )
            .await;
            continue;
        }

        let mut args = vec![
            "--dir".to_owned(),
            workspace_root.display().to_string(),
            "create".to_owned(),
            "--repo".to_owned(),
            dispatch.repo.clone(),
            "--prompt-file".to_owned(),
            prompt_file.display().to_string(),
        ];
        if let Some(ref agent) = dispatch.agent {
            args.push("--agent".to_owned());
            args.push(agent.clone());
        }

        tracing::info!(
            repo = %dispatch.repo,
            agent = %dispatch.agent.as_deref().unwrap_or("default"),
            prompt = %truncate(&dispatch.prompt, 80),
            "Dispatching worker",
        );

        let output = tokio::process::Command::new(&swarm_bin)
            .args(&args)
            .output()
            .await;

        let _ = std::fs::remove_file(&prompt_file);

        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let worktree_id = stdout
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Created worktree ")
                            .or_else(|| line.strip_prefix("created worktree "))
                    })
                    .unwrap_or(stdout.trim());
                let msg = if worktree_id.is_empty() {
                    format!("🚀 Dispatched worker for `{}`", dispatch.repo)
                } else {
                    format!("🚀 Dispatched `{worktree_id}`")
                };
                if let Err(e) = send_to_telegram(&channel, chat_id, &msg, topic_id).await {
                    tracing::error!(error = %e, "Failed to send dispatch confirmation");
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let detail = if !stderr.is_empty() {
                    stderr.to_string()
                } else {
                    stdout.to_string()
                };
                tracing::error!(
                    repo = %dispatch.repo,
                    detail = %truncate(&detail, 200),
                    "Dispatch failed",
                );
                let _ = send_to_telegram(
                    &channel,
                    chat_id,
                    &format!(
                        "❌ Dispatch to `{}` failed:\n```\n{}\n```",
                        dispatch.repo,
                        truncate(&detail, 500)
                    ),
                    topic_id,
                )
                .await;
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to run swarm create");
                let _ = send_to_telegram(
                    &channel,
                    chat_id,
                    &format!("❌ Dispatch to `{}` failed: {}", dispatch.repo, e),
                    topic_id,
                )
                .await;
            }
        }
    }
}

/// Run an ephemeral Claude session: send one message, collect response, return.
async fn run_ephemeral_session(
    mut session: apiari_claude_sdk::Session,
    prompt: &str,
) -> Result<String> {
    // Send message immediately — don't wait for system event (same deadlock issue).
    session
        .send_message(prompt)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    session.close_stdin();

    let mut text = String::new();
    loop {
        let event = session
            .next_event()
            .await
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        match event {
            Some(Event::Assistant { message, .. }) => {
                for block in &message.message.content {
                    match block {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::ToolUse { name, input, .. } => {
                            tracing::debug!(
                                tool = %name,
                                input = %truncate(&input.to_string(), 120),
                                "Ephemeral session tool call",
                            );
                        }
                        _ => {}
                    }
                }
                if message.message.stop_reason.as_deref() == Some("end_turn") {
                    break;
                }
            }
            Some(Event::Result(r)) => {
                if let Some(result_text) = &r.result
                    && text.is_empty()
                {
                    text = result_text.clone();
                }
                break;
            }
            Some(Event::RateLimit(rl)) => {
                if let Some(info) = &rl.rate_limit_info {
                    tracing::warn!(info = %info, "Ephemeral session rate limit");
                }
            }
            Some(Event::System(sys)) => {
                let session_id = sys
                    .data
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let model = sys
                    .data
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                tracing::debug!(session_id, model, "Ephemeral session started");
            }
            Some(Event::User(_)) => {
                // Echo of our own prompt — nothing to do.
            }
            Some(Event::Stream { .. }) => {
                // Partial streaming events — not used in ephemeral mode.
            }
            None => break,
        }
    }

    Ok(text)
}

/// Build an auto-triage prompt for a swarm notification, if applicable.
///
/// Returns `Some(prompt)` for notification types that benefit from a coordinator
/// follow-up suggestion (PrOpened, AgentWaiting), `None` for others.
///
/// When `pr_details` is `Some`, the pre-fetched PR JSON is embedded directly in
/// the prompt so the coordinator doesn't need to run `gh pr view` itself (which
/// can accidentally pick up the wrong PR).
fn auto_triage_prompt(
    notification: &swarm_watcher::SwarmNotification,
    pr_details: Option<&str>,
) -> Option<String> {
    match notification {
        swarm_watcher::SwarmNotification::PrOpened {
            worktree_id,
            pr_url,
            pr_title,
            ..
        } => {
            let title = pr_title.as_deref().unwrap_or("untitled");
            if let Some(details) = pr_details {
                Some(format!(
                    "Worker {worktree_id} opened PR: \"{title}\" ({pr_url}).\n\n\
                     PR snapshot:\n{details}\n\n\
                     Based on the above, give a brief assessment: is it ready to merge?\n\
                     You may use `gh pr view {pr_url}` for additional detail (diff, CI status) if needed."
                ))
            } else {
                Some(format!(
                    "Worker {worktree_id} opened PR: \"{title}\" ({pr_url}).\n\
                     Check the PR (use `gh pr view {pr_url}`) and give a brief assessment: \
                     is it ready to merge?"
                ))
            }
        }
        swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id,
            pr_url,
            ..
        } => match pr_url {
            Some(url) => {
                if let Some(details) = pr_details {
                    Some(format!(
                        "Worker {worktree_id} is waiting and has an open PR ({url}).\n\n\
                         PR snapshot:\n{details}\n\n\
                         Based on the above, give a brief assessment: is it ready to merge?\n\
                         You may use `gh pr view {url}` for additional detail (diff, CI status) if needed."
                    ))
                } else {
                    Some(format!(
                        "Worker {worktree_id} is waiting and has an open PR ({url}).\n\
                         Check the PR status (use `gh pr view {url}`) and summarize: \
                         is it ready to merge?"
                    ))
                }
            }
            None => Some(format!(
                "Worker {worktree_id} is waiting for input with no open PR.\n\
                 Is this likely done (should be closed) or still in progress (needs a nudge)?"
            )),
        },
        _ => None,
    }
}

/// Pre-fetch PR details using `gh pr view` as a synchronous subprocess.
///
/// Returns the JSON output on success, or `None` if the command fails.
fn fetch_pr_details(pr_url: &str) -> Option<String> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            pr_url,
            "--json",
            "title,body,state,additions,deletions,headRefName",
        ])
        .output()
        .ok()?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if stdout.trim().is_empty() {
            None
        } else {
            Some(stdout)
        }
    } else {
        None
    }
}

/// Read the PR URL for a given worktree from `.swarm/state.json`.
fn read_pr_url_from_swarm(workspace_root: &Path, worktree_id: &str) -> Option<String> {
    let state_path = workspace_root.join(".swarm").join("state.json");
    let content = std::fs::read_to_string(&state_path).ok()?;
    let state: serde_json::Value = serde_json::from_str(&content).ok()?;
    let worktrees = state.get("worktrees")?.as_array()?;
    for wt in worktrees {
        if wt.get("id")?.as_str()? == worktree_id {
            return wt.get("pr")?.get("url")?.as_str().map(String::from);
        }
    }
    None
}

/// Format a batch of swarm notifications into a single summary message.
///
/// Used when 3+ notifications arrive in one poll cycle for the same chat,
/// to avoid spamming Telegram with rapid-fire messages.
/// Convert a `SwarmNotification` to a `UiEvent` for routing to the TUI inbox.
/// Returns `None` for notification types that don't map (e.g. AgentSpawned).
fn notification_to_ui_event(
    n: &swarm_watcher::SwarmNotification,
) -> Option<crate::ui::inbox::UiEvent> {
    match n {
        swarm_watcher::SwarmNotification::PrOpened {
            worktree_id,
            pr_url,
            pr_title,
            ..
        } => Some(crate::ui::inbox::UiEvent::PrOpened {
            worktree_id: worktree_id.clone(),
            pr_url: pr_url.clone(),
            pr_title: pr_title.clone().unwrap_or_default(),
        }),
        swarm_watcher::SwarmNotification::AgentWaiting { worktree_id, .. } => {
            Some(crate::ui::inbox::UiEvent::AgentWaiting {
                worktree_id: worktree_id.clone(),
            })
        }
        swarm_watcher::SwarmNotification::AgentStalled { worktree_id, .. } => {
            Some(crate::ui::inbox::UiEvent::AgentStalled {
                worktree_id: worktree_id.clone(),
            })
        }
        swarm_watcher::SwarmNotification::AgentCompleted { worktree_id, .. } => {
            Some(crate::ui::inbox::UiEvent::AgentCompleted {
                worktree_id: worktree_id.clone(),
            })
        }
        swarm_watcher::SwarmNotification::AgentClosed { worktree_id, .. } => {
            Some(crate::ui::inbox::UiEvent::AgentClosed {
                worktree_id: worktree_id.clone(),
            })
        }
        swarm_watcher::SwarmNotification::AgentSpawned { .. } => None,
    }
}

fn format_swarm_batch(notifications: &[&swarm_watcher::SwarmNotification]) -> String {
    let mut text = format!("🐝 {} swarm updates:", notifications.len());
    for n in notifications {
        text.push_str(&format!("\n• {}", n.summary_line()));
    }
    text
}

/// Build a sweep summary message for a single workspace's signals.
///
/// Groups issues by severity with titles, event counts, and links.
fn build_sweep_summary(workspace_name: &str, signals: &[Signal]) -> String {
    let header = if workspace_name == "home" {
        format!("**Sentry Sweep** — {} issue(s)", signals.len())
    } else {
        format!(
            "**Sentry Sweep** ({workspace_name}) — {} issue(s)",
            signals.len()
        )
    };

    // Group by severity.
    let mut critical: Vec<&Signal> = Vec::new();
    let mut warning: Vec<&Signal> = Vec::new();
    let mut info: Vec<&Signal> = Vec::new();

    for signal in signals {
        match signal.severity {
            Severity::Critical => critical.push(signal),
            Severity::Warning => warning.push(signal),
            Severity::Info => info.push(signal),
        }
    }

    let mut text = header;

    for (emoji, label, group) in [
        ("🔴", "Critical", &critical),
        ("🟡", "Warning", &warning),
        ("🔵", "Info", &info),
    ] {
        if group.is_empty() {
            continue;
        }
        text.push_str(&format!("\n\n{emoji} **{label}** ({})", group.len()));
        for signal in group.iter() {
            // Extract event count from body (format: "culprit\nLevel: X | Events: N").
            let events = signal.body.split("Events: ").nth(1).unwrap_or("?");
            let trigger = if signal.tags.iter().any(|t| t == "spiking") {
                " 📈"
            } else if signal.tags.iter().any(|t| t == "stale") {
                " 🕸"
            } else {
                ""
            };
            let link = signal
                .url
                .as_ref()
                .map(|u| format!(" [↗]({u})"))
                .unwrap_or_default();
            text.push_str(&format!(
                "\n• {} — {events} events{trigger}{link}",
                signal.title
            ));
        }
    }

    text
}

/// Write sweep results to `<workspace_root>/.hive/last_sweep.md` so the
/// coordinator can read them when the user asks follow-up triage questions.
fn write_last_sweep(workspace_root: &Path, message: &str) {
    let path = workspace_root.join(".hive").join("last_sweep.md");
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let content = format!("<!-- Last sweep: {timestamp} -->\n\n{message}\n");
    if let Err(e) = std::fs::write(&path, &content) {
        tracing::error!(path = %path.display(), error = %e, "Failed to write last sweep");
    }
}

/// Format a triage alert for Telegram.
fn format_triage_alert(signal: &Signal, assessment: &str) -> String {
    let severity_emoji = match signal.severity {
        Severity::Critical => "🔴",
        Severity::Warning => "🟡",
        Severity::Info => "🔵",
    };

    let esc_title = telegram::escape_markdown(&signal.title);
    let esc_source = telegram::escape_markdown(&signal.source);
    let esc_assessment = telegram::escape_markdown(assessment);

    let mut alert = format!(
        "{severity_emoji} *{severity}* — {esc_title}\n\
         Source: {esc_source}\n\n\
         {esc_assessment}",
        severity = signal.severity,
    );

    if let Some(url) = &signal.url {
        alert.push_str(&format!("\n\n[View]({url})"));
    }

    alert
}

/// Split text into chunks of at most `limit` characters, preferring to break at newlines.
fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= limit {
            chunks.push(remaining.to_owned());
            break;
        }

        // Look for the last newline within the limit.
        // Find the nearest char boundary at or before `limit` to avoid
        // slicing in the middle of a multi-byte UTF-8 character.
        let safe_limit = floor_char_boundary(remaining, limit);
        let split_at = remaining[..safe_limit]
            .rfind('\n')
            .map(|pos| pos + 1) // include the newline in the current chunk
            .unwrap_or(safe_limit); // no newline found — hard split at limit

        chunks.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }

    chunks
}

/// Truncate a string for display.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..floor_char_boundary(s, max)]
    }
}

/// Find the largest byte index `<= max` that is a valid char boundary.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Dispatch routed swarm notifications through a channel.
///
/// Groups by chat_id, batches 3+ per chat into one summary message, sends
/// individually otherwise. Falls back to `alert_chat_id` on send failure.
#[cfg(test)]
pub(crate) async fn dispatch_notifications(
    channel: &dyn Channel,
    routed: &[(i64, &swarm_watcher::SwarmNotification)],
    alert_chat_id: i64,
) {
    // Group by chat_id, preserving insertion order.
    let mut by_chat: Vec<(i64, Vec<&swarm_watcher::SwarmNotification>)> = Vec::new();
    for &(chat_id, notification) in routed {
        if let Some((_, group)) = by_chat.iter_mut().find(|(id, _)| *id == chat_id) {
            group.push(notification);
        } else {
            by_chat.push((chat_id, vec![notification]));
        }
    }

    let mut first_send = true;
    for (chat_id, group) in &by_chat {
        let texts: Vec<String> = if group.len() >= 3 {
            vec![format_swarm_batch(group)]
        } else {
            group.iter().map(|n| n.format_telegram()).collect()
        };
        for text in &texts {
            if !first_send {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            if let Err(e) = channel
                .send_message(&OutboundMessage {
                    chat_id: *chat_id,
                    text: text.clone(),
                    buttons: vec![],
                    topic_id: None,
                })
                .await
            {
                tracing::error!(chat_id, error = %e, "Failed to send to chat — retrying on alert_chat_id");
                if *chat_id != alert_chat_id {
                    let _ = channel
                        .send_message(&OutboundMessage {
                            chat_id: alert_chat_id,
                            text: text.clone(),
                            buttons: vec![],
                            topic_id: None,
                        })
                        .await;
                }
            }
            first_send = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_swarm_batch_three_notifications() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/task-a".into(),
            summary: Some("propagate waiting status".into()),
        };
        let n2 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "2".into(),
            branch: "swarm/task-b".into(),
            summary: Some("agent waiting notification".into()),
        };
        let n3 = swarm_watcher::SwarmNotification::AgentClosed {
            worktree_id: "3".into(),
            branch: "swarm/task-c".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };

        let batch = format_swarm_batch(&[&n1, &n2, &n3]);
        assert!(batch.starts_with("🐝 3 swarm updates:"));
        assert!(batch.contains("• New agent spawned — task-a (propagate waiting status)"));
        assert!(batch.contains("• New agent spawned — task-b (agent waiting notification)"));
        assert!(batch.contains("• Agent closed — task-c"));
    }

    #[test]
    fn test_format_swarm_batch_mixed_types() {
        let spawned = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/feat-1".into(),
            summary: None,
        };
        let completed = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "2".into(),
            branch: "swarm/feat-2".into(),
            summary: None,
            duration: "10m".into(),
            pr_url: Some("https://github.com/example/pull/7".into()),
        };
        let pr = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "3".into(),
            branch: "swarm/feat-3".into(),
            pr_url: "https://github.com/example/pull/8".into(),
            pr_title: Some("Add feature 3".into()),
            duration: "12m".into(),
        };
        let stalled = swarm_watcher::SwarmNotification::AgentStalled {
            worktree_id: "4".into(),
            branch: "swarm/feat-4".into(),
            stall_kind: swarm_watcher::StallKind::Idle { minutes: 20 },
        };

        let batch = format_swarm_batch(&[&spawned, &completed, &pr, &stalled]);
        assert!(batch.starts_with("🐝 4 swarm updates:"));
        assert!(batch.contains("• New agent spawned — feat-1"));
        assert!(
            batch.contains("• Agent completed — feat-2 (PR: https://github.com/example/pull/7)")
        );
        assert!(batch.contains("• PR opened — feat-3 (https://github.com/example/pull/8)"));
        assert!(batch.contains("• Agent stalled — feat-4 (idle 20m)"));
    }

    // --- Notification dispatch tests (mockall-backed) ---

    use crate::channel::MockChannel;
    use crate::daemon::origin_map::route_notification;

    fn make_notification(wt_id: &str, branch: &str) -> swarm_watcher::SwarmNotification {
        swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: wt_id.into(),
            branch: branch.into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_notification_routed_to_origin_chat() {
        let mut om = origin_map::OriginMap::default();
        om.insert(
            "wt-1".into(),
            origin_map::OriginEntry {
                origin: crate::quest::TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(100),
                    user_name: None,
                    user_id: None,
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: chrono::Utc::now(),
            },
        );

        let alert_chat_id = 999;
        let target = route_notification(&om, "wt-1", alert_chat_id);
        assert_eq!(target, 100, "should route to origin chat");

        let notification = make_notification("wt-1", "swarm/fix-bug");
        let routed = vec![(target, &notification)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, alert_chat_id).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_notification_falls_back_to_alert_chat_when_origin_missing() {
        let om = origin_map::OriginMap::default();
        let alert_chat_id = 999;

        let target = route_notification(&om, "unknown-wt", alert_chat_id);
        assert_eq!(target, 999, "should fall back to alert chat");

        let notification = make_notification("unknown-wt", "swarm/task");
        let routed = vec![(target, &notification)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 999)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, alert_chat_id).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_notification_falls_back_when_send_fails() {
        let notification = make_notification("wt-1", "swarm/fix");
        let routed = vec![(100_i64, &notification)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(1)
            .returning(|_| Err(color_eyre::eyre::eyre!("chat not found")));
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 999)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_batch_sends_single_message_for_three_or_more() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        let n2 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "2".into(),
            branch: "swarm/b".into(),
            summary: None,
        };
        let n3 = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "3".into(),
            branch: "swarm/c".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };

        let routed = vec![(100_i64, &n1), (100_i64, &n2), (100_i64, &n3)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| {
                msg.chat_id == 100 && msg.text.contains("3 swarm updates:")
            })
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_batch_sends_individually_for_fewer_than_three() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        let n2 = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "2".into(),
            branch: "swarm/b".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };

        let routed = vec![(100_i64, &n1), (100_i64, &n2)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(2)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_all_notifications_sent_even_if_one_fails() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        let n2 = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "2".into(),
            branch: "swarm/b".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };

        let routed = vec![(100_i64, &n1), (200_i64, &n2)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(1)
            .returning(|_| Err(color_eyre::eyre::eyre!("error")));
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 999)
            .times(1)
            .returning(|_| Ok(()));
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 200)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    // --- Auto-triage prompt tests ---

    #[test]
    fn test_auto_triage_prompt_pr_opened() {
        let n = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "hive-3".into(),
            branch: "swarm/add-auth".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/22".into(),
            pr_title: Some("Add OAuth support".into()),
            duration: "10m".into(),
        };
        let prompt = auto_triage_prompt(&n, None).unwrap();
        assert!(prompt.contains("hive-3"));
        assert!(prompt.contains("Add OAuth support"));
        assert!(prompt.contains("https://github.com/ApiariTools/hive/pull/22"));
        assert!(prompt.contains("gh pr view"));
        assert!(prompt.contains("ready to merge"));
    }

    #[test]
    fn test_auto_triage_prompt_pr_opened_no_title() {
        let n = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "hive-1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let prompt = auto_triage_prompt(&n, None).unwrap();
        assert!(prompt.contains("untitled"));
    }

    #[test]
    fn test_auto_triage_prompt_agent_waiting_with_pr() {
        let n = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "hive-2".into(),
            branch: "swarm/feat".into(),
            summary: Some("Add feature".into()),
            pr_url: Some("https://github.com/example/pull/5".into()),
        };
        let prompt = auto_triage_prompt(&n, None).unwrap();
        assert!(prompt.contains("hive-2"));
        assert!(prompt.contains("https://github.com/example/pull/5"));
        assert!(prompt.contains("gh pr view"));
        assert!(prompt.contains("ready to merge"));
    }

    #[test]
    fn test_auto_triage_prompt_agent_waiting_no_pr() {
        let n = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "hive-4".into(),
            branch: "swarm/task".into(),
            summary: None,
            pr_url: None,
        };
        let prompt = auto_triage_prompt(&n, None).unwrap();
        assert!(prompt.contains("hive-4"));
        assert!(prompt.contains("waiting for input"));
        assert!(prompt.contains("should be closed"));
        assert!(prompt.contains("needs a nudge"));
    }

    #[test]
    fn test_auto_triage_prompt_returns_none_for_other_types() {
        let spawned = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        assert!(auto_triage_prompt(&spawned, None).is_none());

        let completed = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };
        assert!(auto_triage_prompt(&completed, None).is_none());

        let closed = swarm_watcher::SwarmNotification::AgentClosed {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };
        assert!(auto_triage_prompt(&closed, None).is_none());

        let stalled = swarm_watcher::SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            stall_kind: swarm_watcher::StallKind::Idle { minutes: 10 },
        };
        assert!(auto_triage_prompt(&stalled, None).is_none());
    }

    #[test]
    fn test_auto_triage_dedup_same_worker() {
        // NOTIFICATION DESIGN: When PrOpened and AgentWaiting fire for the same
        // worker in the same poll cycle, only auto-triage once per worktree to
        // avoid duplicate assessments.
        let pr_opened = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "wt-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pr/1".into(),
            pr_title: Some("Feature".into()),
            duration: "5m".into(),
        };
        let waiting = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "wt-1".into(),
            branch: "swarm/feat".into(),
            summary: Some("Feature".into()),
            pr_url: Some("https://example.com/pr/1".into()),
        };

        // Simulate the dedup logic from poll_swarm Phase 4.
        let routed: Vec<(i64, &swarm_watcher::SwarmNotification)> =
            vec![(100, &pr_opened), (100, &waiting)];

        let mut auto_triaged: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut triage_count = 0;

        for (_chat_id, notification) in &routed {
            let wt_id = notification.worktree_id().to_owned();
            if auto_triaged.contains(&wt_id) {
                continue;
            }
            if auto_triage_prompt(notification, None).is_some() {
                auto_triaged.insert(wt_id);
                triage_count += 1;
            }
        }

        assert_eq!(triage_count, 1, "should auto-triage only once per worktree");
    }

    #[test]
    fn test_auto_triage_prompt_pr_opened_with_details() {
        let n = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "hive-5".into(),
            branch: "swarm/fix-login".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/30".into(),
            pr_title: Some("Fix login bug".into()),
            duration: "8m".into(),
        };
        let details = r#"{"title":"Fix login bug","body":"Fixes #42","state":"OPEN","additions":10,"deletions":3,"headRefName":"swarm/fix-login"}"#;
        let prompt = auto_triage_prompt(&n, Some(details)).unwrap();
        assert!(prompt.contains("hive-5"));
        assert!(prompt.contains("Fix login bug"));
        assert!(prompt.contains("PR snapshot:"));
        assert!(prompt.contains(details));
        assert!(prompt.contains("Based on the above"));
        assert!(prompt.contains("ready to merge"));
        // Should still mention gh pr view as a fallback for deeper inspection.
        assert!(prompt.contains("gh pr view"));
    }

    #[test]
    fn test_auto_triage_prompt_agent_waiting_with_pr_details() {
        let n = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "hive-6".into(),
            branch: "swarm/refactor".into(),
            summary: Some("Refactor auth".into()),
            pr_url: Some("https://github.com/example/pull/99".into()),
        };
        let details = r#"{"title":"Refactor auth","state":"OPEN","additions":50,"deletions":20,"headRefName":"swarm/refactor"}"#;
        let prompt = auto_triage_prompt(&n, Some(details)).unwrap();
        assert!(prompt.contains("hive-6"));
        assert!(prompt.contains("PR snapshot:"));
        assert!(prompt.contains(details));
        assert!(prompt.contains("Based on the above"));
        assert!(prompt.contains("ready to merge"));
    }

    // --- extract_buttons tests ---

    #[test]
    fn test_extract_buttons_valid_block() {
        // Each [label, data] pair is one row with one button.
        let input = "Want me to merge this PR?\n\n```buttons\n[[\"✅ Yes, merge\", \"confirm:merge:hive-1\"], [\"❌ No thanks\", \"confirm:no\"]]\n```";
        let (text, buttons) = extract_buttons(input);
        assert_eq!(text, "Want me to merge this PR?");
        assert_eq!(buttons.len(), 2); // two rows, one button each
        assert_eq!(buttons[0][0].text, "✅ Yes, merge");
        assert_eq!(buttons[0][0].callback_data, "confirm:merge:hive-1");
        assert_eq!(buttons[1][0].text, "❌ No thanks");
        assert_eq!(buttons[1][0].callback_data, "confirm:no");
    }

    #[test]
    fn test_extract_buttons_multiple_rows() {
        // Each inner array [label, data] is one row with one button.
        // Three inner arrays = three rows.
        let input = "Choose:\n\n```buttons\n[[\"A\", \"a\"], [\"B\", \"b\"], [\"C\", \"c\"]]\n```";
        let (text, buttons) = extract_buttons(input);
        assert_eq!(text, "Choose:");
        assert_eq!(buttons.len(), 3); // three rows
        assert_eq!(buttons[0][0].text, "A");
        assert_eq!(buttons[1][0].text, "B");
        assert_eq!(buttons[2][0].text, "C");
    }

    #[test]
    fn test_extract_buttons_no_block() {
        let input = "Just a regular message with no buttons.";
        let (text, buttons) = extract_buttons(input);
        assert_eq!(text, input);
        assert!(buttons.is_empty());
    }

    #[test]
    fn test_extract_buttons_malformed_json() {
        let input = "Some text\n\n```buttons\nnot valid json\n```";
        let (text, buttons) = extract_buttons(input);
        assert_eq!(text, input);
        assert!(buttons.is_empty());
    }

    #[test]
    fn test_extract_buttons_unclosed_fence() {
        let input = "Some text\n\n```buttons\n[[\"A\", \"a\"]]";
        let (text, buttons) = extract_buttons(input);
        assert_eq!(text, input);
        assert!(buttons.is_empty());
    }

    #[test]
    fn test_extract_buttons_empty_array() {
        let input = "Some text\n\n```buttons\n[]\n```";
        let (text, buttons) = extract_buttons(input);
        assert_eq!(text, input);
        assert!(buttons.is_empty());
    }

    // --- extract_dispatch_blocks tests ---

    #[test]
    fn test_extract_dispatch_single_block() {
        let input = "On it!\n\n```dispatch:backend\nFix the DB pool.\nCreate a PR.\n```";
        let (text, dispatches) = extract_dispatch_blocks(input);
        assert_eq!(text, "On it!");
        assert_eq!(dispatches.len(), 1);
        assert_eq!(dispatches[0].repo, "backend");
        assert!(dispatches[0].agent.is_none());
        assert_eq!(dispatches[0].prompt, "Fix the DB pool.\nCreate a PR.");
    }

    #[test]
    fn test_extract_dispatch_with_agent() {
        let input = "```dispatch:hive:claude\nDo the thing.\n```";
        let (text, dispatches) = extract_dispatch_blocks(input);
        assert_eq!(text, "");
        assert_eq!(dispatches.len(), 1);
        assert_eq!(dispatches[0].repo, "hive");
        assert_eq!(dispatches[0].agent.as_deref(), Some("claude"));
    }

    #[test]
    fn test_extract_dispatch_multiple_blocks() {
        let input = "Spinning up two workers.\n\n```dispatch:backend\nFix pools.\n```\n\n```dispatch:frontend\nUpdate CSS.\n```";
        let (text, dispatches) = extract_dispatch_blocks(input);
        assert_eq!(text, "Spinning up two workers.");
        assert_eq!(dispatches.len(), 2);
        assert_eq!(dispatches[0].repo, "backend");
        assert_eq!(dispatches[0].prompt, "Fix pools.");
        assert_eq!(dispatches[1].repo, "frontend");
        assert_eq!(dispatches[1].prompt, "Update CSS.");
    }

    #[test]
    fn test_extract_dispatch_no_blocks() {
        let input = "Just a regular message.";
        let (text, dispatches) = extract_dispatch_blocks(input);
        assert_eq!(text, input);
        assert!(dispatches.is_empty());
    }

    #[test]
    fn test_extract_dispatch_unclosed_fence() {
        let input = "Some text\n\n```dispatch:backend\nNo closing fence";
        let (text, dispatches) = extract_dispatch_blocks(input);
        assert_eq!(text, input);
        assert!(dispatches.is_empty());
    }

    #[test]
    fn test_extract_dispatch_empty_prompt() {
        let input = "```dispatch:backend\n\n```";
        let (_text, dispatches) = extract_dispatch_blocks(input);
        // Empty prompt is skipped.
        assert!(dispatches.is_empty());
    }

    #[test]
    fn test_extract_dispatch_coexists_with_buttons() {
        let input = "Here's the plan.\n\n```dispatch:backend\nFix bug.\n```\n\n```buttons\n[[\"OK\", \"ok\"]]\n```";
        // Buttons are NOT stripped by extract_dispatch_blocks — that's extract_buttons' job.
        let (text, dispatches) = extract_dispatch_blocks(input);
        assert_eq!(dispatches.len(), 1);
        assert_eq!(dispatches[0].repo, "backend");
        // The buttons block should remain in the cleaned text.
        assert!(text.contains("```buttons"));
    }

    // -- notification_to_ui_event tests --

    #[test]
    fn test_pr_opened_to_ui_event() {
        let n = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "hive-1".into(),
            branch: "feat/test".into(),
            pr_url: "https://github.com/test/1".into(),
            pr_title: Some("Fix bug".into()),
            duration: "5m".into(),
        };
        let event = notification_to_ui_event(&n).unwrap();
        if let crate::ui::inbox::UiEvent::PrOpened {
            worktree_id,
            pr_url,
            pr_title,
        } = event
        {
            assert_eq!(worktree_id, "hive-1");
            assert_eq!(pr_url, "https://github.com/test/1");
            assert_eq!(pr_title, "Fix bug");
        } else {
            panic!("expected PrOpened");
        }
    }

    #[test]
    fn test_agent_waiting_to_ui_event() {
        let n = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "hive-2".into(),
            branch: "feat/x".into(),
            summary: None,
            pr_url: None,
        };
        let event = notification_to_ui_event(&n).unwrap();
        assert!(matches!(
            event,
            crate::ui::inbox::UiEvent::AgentWaiting { worktree_id } if worktree_id == "hive-2"
        ));
    }

    #[test]
    fn test_agent_stalled_to_ui_event() {
        let n = swarm_watcher::SwarmNotification::AgentStalled {
            worktree_id: "hive-3".into(),
            branch: "feat/y".into(),
            stall_kind: swarm_watcher::StallKind::Idle { minutes: 10 },
        };
        let event = notification_to_ui_event(&n).unwrap();
        assert!(matches!(
            event,
            crate::ui::inbox::UiEvent::AgentStalled { worktree_id } if worktree_id == "hive-3"
        ));
    }

    #[test]
    fn test_agent_completed_to_ui_event() {
        let n = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "hive-4".into(),
            branch: "feat/z".into(),
            summary: None,
            duration: "3m".into(),
            pr_url: None,
        };
        let event = notification_to_ui_event(&n).unwrap();
        assert!(matches!(
            event,
            crate::ui::inbox::UiEvent::AgentCompleted { worktree_id } if worktree_id == "hive-4"
        ));
    }

    #[test]
    fn test_agent_closed_to_ui_event() {
        let n = swarm_watcher::SwarmNotification::AgentClosed {
            worktree_id: "hive-5".into(),
            branch: "feat/w".into(),
            summary: None,
            duration: "2m".into(),
            pr_url: None,
        };
        let event = notification_to_ui_event(&n).unwrap();
        assert!(matches!(
            event,
            crate::ui::inbox::UiEvent::AgentClosed { worktree_id } if worktree_id == "hive-5"
        ));
    }

    #[test]
    fn test_agent_spawned_returns_none() {
        let n = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "hive-6".into(),
            branch: "feat/v".into(),
            summary: None,
        };
        assert!(notification_to_ui_event(&n).is_none());
    }
}

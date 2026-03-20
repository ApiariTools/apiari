//! App state machine for the apiari TUI.

use crate::buzz::coordinator::memory::MemoryStore;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalRecord};
use apiari_tui::conversation::ConversationEntry;
use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use serde::Deserialize;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use apiari_tui::scroll::ScrollState;

use crate::buzz::conversation::ConversationStore;
use crate::config::{self, Workspace};

/// Maximum number of chat history messages to load from a previous session.
const CHAT_HISTORY_LIMIT: usize = 20;

// ── Types ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Dashboard,
    WorkerDetail(usize),
    SignalDetail(usize),
    SignalList,
    ReviewList,
    PrList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Home,
    Workers,
    Signals,
    Reviews,
    Feed,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Confirm,
    Help,
}

/// Where a chat message originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSource {
    Tui,
    Telegram,
    System,
}

#[derive(Debug, Clone)]
pub enum ChatLine {
    User(String, String, Option<MessageSource>), // content, timestamp, source
    Assistant(String, String, Option<MessageSource>), // content, timestamp, source
    System(String),                              // system message
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    CloseWorker(String), // worktree id
    ResolveSignal(i64),  // signal db id
    ApproveReview { repo: String, pr_number: u64 },
    SnoozeSignal(i64), // signal db id
}

/// Snooze duration options for the duration picker.
pub const SNOOZE_OPTIONS: &[&str] = &["1 hour", "4 hours", "Tomorrow 9am", "Next Monday 9am"];

#[derive(Debug, Clone)]
pub struct FlashMessage {
    pub text: String,
    pub expires: Instant,
}

// ── Feed + dashboard types ────────────────────────────────

#[derive(Debug, Clone)]
pub struct FeedItem {
    pub when: DateTime<Utc>,
    pub kind: FeedKind,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum FeedKind {
    Signal,
    Worker,
    Heartbeat, // watcher poll / daemon check-in
}

#[derive(Debug, Clone)]
pub struct WatcherHealth {
    pub name: String,
    pub healthy: bool,        // updated_at within 5 min
    pub last_check_secs: i64, // seconds since last check
}

// ── Background refresh types ─────────────────────────────

/// Info needed by the background refresh task for each workspace.
#[derive(Clone)]
pub(super) struct WorkspaceRefreshInfo {
    pub(super) name: String,
    pub(super) root: std::path::PathBuf,
    pub(super) has_github_watcher: bool,
    pub(super) has_sentry_watcher: bool,
    pub(super) has_swarm_watcher: bool,
}

/// Data returned from background extras refresh (per workspace).
pub(super) struct WorkspaceExtrasData {
    pub(super) sparkline_data: Vec<u64>,
    pub(super) watcher_health: Vec<WatcherHealth>,
    pub(super) thoughts: Vec<(String, String)>,
    /// Feed items from SQLite (signals + watcher heartbeats). Worker items merged by caller.
    pub(super) feed_items: Vec<FeedItem>,
}

/// Messages from background refresh tasks to the TUI event loop.
pub(super) enum AppUpdate {
    Workers(Vec<(String, Vec<WorkerInfo>)>),
    Signals(Vec<(String, Vec<SignalRecord>)>),
    Extras {
        daemon_alive: bool,
        daemon_uptime_secs: Option<u64>,
        per_workspace: Vec<(String, WorkspaceExtrasData)>,
    },
    ChatHistory(Vec<(String, Vec<ChatLine>, Option<String>)>),
    DaemonStatus {
        connected: bool,
        alive: bool,
        remote_host: Option<String>,
    },
    WorkerConversation {
        workspace_name: String,
        worker_id: String,
        entries: Vec<ConversationEntry>,
    },
}

// ── Worker info from state.json ───────────────────────────

#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub id: String,
    pub branch: String,
    pub prompt: String,
    #[allow(dead_code)]
    pub agent_kind: String,
    pub phase: Option<String>,
    pub agent_session_status: Option<String>,
    #[allow(dead_code)]
    pub summary: Option<String>,
    pub created_at: Option<DateTime<Local>>,
    pub pr: Option<PrInfo>,
    pub last_activity: Option<String>,
    /// Parsed conversation from events.jsonl (loaded on demand for detail view).
    pub conversation: Vec<ConversationEntry>,
    /// Per-worker scroll state for conversation view.
    pub conv_scroll: ScrollState,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
}

/// Deserialization types for .swarm/state.json.
#[derive(Debug, Deserialize)]
struct SwarmStateFile {
    #[serde(default)]
    worktrees: Vec<SwarmWorktreeState>,
}

#[derive(Debug, Deserialize)]
struct SwarmWorktreeState {
    id: String,
    branch: String,
    prompt: String,
    #[serde(default)]
    agent_kind: String,
    #[serde(default)]
    created_at: Option<DateTime<Local>>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    pr: Option<SwarmPrInfo>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    agent_session_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SwarmPrInfo {
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

// ── Onboarding (progressive dashboard reveal) ────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStage {
    Chat,      // only chat panel bright, Bee introduces herself
    Workers,   // workers panel brightens
    Heartbeat, // feed panel brightens
    Reviews,   // reviews/signals panels brighten
    Complete,  // all panels bright, onboarding done
}

pub struct OnboardingState {
    pub active: bool,
    pub stage: OnboardingStage,
    pub revealed_panels: std::collections::HashSet<Panel>,
}

impl OnboardingState {
    pub fn new_active() -> Self {
        let mut revealed = std::collections::HashSet::new();
        revealed.insert(Panel::Chat);
        Self {
            active: true,
            stage: OnboardingStage::Chat,
            revealed_panels: revealed,
        }
    }

    pub fn completed() -> Self {
        Self {
            active: false,
            stage: OnboardingStage::Complete,
            revealed_panels: [
                Panel::Home,
                Panel::Workers,
                Panel::Signals,
                Panel::Reviews,
                Panel::Feed,
                Panel::Chat,
            ]
            .into_iter()
            .collect(),
        }
    }

    pub fn is_revealed(&self, panel: Panel) -> bool {
        !self.active || self.revealed_panels.contains(&panel)
    }

    /// Advance to next stage, returning the Bee message for the new stage.
    pub fn advance(&mut self) -> Option<&'static str> {
        match self.stage {
            OnboardingStage::Chat => {
                self.stage = OnboardingStage::Workers;
                self.revealed_panels.insert(Panel::Workers);
                Some(
                    "On your left: Workers. When you dispatch a coding task, \
                     an AI agent spins up in its own git worktree and gets to work. \
                     Each worker shows its status, branch, and PR link.\n\n\
                     Press enter to continue.",
                )
            }
            OnboardingStage::Workers => {
                self.stage = OnboardingStage::Heartbeat;
                self.revealed_panels.insert(Panel::Feed);
                self.revealed_panels.insert(Panel::Home);
                Some(
                    "This is your Heartbeat \u{2014} a live feed of everything happening \
                     in your workspace. Signals from GitHub (CI, PRs, releases), Sentry \
                     errors, Linear issues \u{2014} it all flows here.\n\n\
                     Press enter to continue.",
                )
            }
            OnboardingStage::Heartbeat => {
                self.stage = OnboardingStage::Reviews;
                self.revealed_panels.insert(Panel::Signals);
                self.revealed_panels.insert(Panel::Reviews);
                Some(
                    "Over here: your signal queue and reviews. Open PRs, review requests, \
                     anything that needs your attention surfaces here automatically.\n\n\
                     Press enter to continue.",
                )
            }
            OnboardingStage::Reviews => {
                self.stage = OnboardingStage::Complete;
                self.active = false;
                // Reveal everything
                self.revealed_panels.insert(Panel::Home);
                self.revealed_panels.insert(Panel::Workers);
                self.revealed_panels.insert(Panel::Signals);
                self.revealed_panels.insert(Panel::Reviews);
                self.revealed_panels.insert(Panel::Feed);
                self.revealed_panels.insert(Panel::Chat);
                Some(
                    "You're all set. \u{1f41d} The whole dashboard is yours.\n\n\
                     Ask me anything \u{2014} I know your repos, your workers, and your config. \
                     Try: 'what's the status?' or '/help' to see what I can do.",
                )
            }
            OnboardingStage::Complete => None,
        }
    }

    /// Skip straight to complete.
    pub fn skip_to_complete(&mut self) {
        self.stage = OnboardingStage::Complete;
        self.active = false;
        self.revealed_panels = [
            Panel::Home,
            Panel::Workers,
            Panel::Signals,
            Panel::Reviews,
            Panel::Feed,
            Panel::Chat,
        ]
        .into_iter()
        .collect();
    }
}

// ── Setup mode (first-run onboarding) ─────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStep {
    AskRoot,
    AskName,
    AskProvider,
    AskGithub,
    AskTelegram,
    AskTelegramChatId,
    Done,
}

#[derive(Debug, Clone)]
pub struct SetupState {
    pub step: SetupStep,
    pub workspace_root: std::path::PathBuf,
    pub workspace_name: String,
    pub default_agent: String,
    pub has_github: bool,
    pub telegram_token: Option<String>,
    pub telegram_chat_id: Option<i64>,
    /// True when adding a workspace to an existing setup (simplified flow).
    pub add_workspace: bool,
}

// ── Per-workspace state ───────────────────────────────────

pub struct WorkspaceState {
    pub name: String,
    pub config: config::WorkspaceConfig,
    pub signals: Vec<SignalRecord>,
    pub workers: Vec<WorkerInfo>,
    pub chat_history: Vec<ChatLine>,
    pub input: String,
    /// Cursor byte offset into `input`. Must always be on a char boundary.
    pub cursor_pos: usize,
    pub chat_scroll: ScrollState,
    pub streaming: bool,
    pub coordinator_preview: Option<String>,
    pub has_unread_response: bool,
    /// Coordinator turn count (exchanges completed in current session).
    pub coordinator_turns: u32,
    // State tracking for proactive notifications
    pub(super) prev_worker_phases: std::collections::HashMap<String, String>,
    pub(super) prev_signal_ids: std::collections::HashSet<i64>,
    pub(super) prev_pr_workers: std::collections::HashSet<String>,
    // Dashboard extras
    pub sparkline_data: Vec<u64>,
    pub watcher_health: Vec<WatcherHealth>,
    pub feed: Vec<FeedItem>,
    pub feed_scroll: ScrollState,
    pub thoughts: Vec<(String, String)>, // (category, content)
}

// ── App ───────────────────────────────────────────────────

pub struct App {
    pub workspaces: Vec<WorkspaceState>,
    pub active_tab: usize,
    pub prefix_active: bool,
    pub view: View,
    pub mode: Mode,
    // Dashboard
    pub focused_panel: Panel,
    pub zoomed_panel: Option<Panel>,
    pub worker_selection: usize,
    pub signal_selection: usize,
    pub review_selection: usize,
    pub feed_selection: usize,
    pub chat_focused: bool,
    // Worker detail
    pub worker_input: String,
    pub worker_input_active: bool,
    // Review comment input
    pub review_comment_active: bool,
    pub review_comment_input: String,
    pub review_comment_repo: String,
    pub review_comment_pr: u64,
    // Detail/list views
    pub content_scroll: u16,
    pub signal_list_selection: usize,
    pub review_list_selection: usize,
    pub pr_list_selection: usize,
    // Daemon / extras
    pub daemon_alive: bool,
    pub daemon_connected: bool, // true if TUI is connected to daemon via socket
    pub daemon_remote: bool,    // true if connected via TCP (remote)
    pub remote_host: Option<String>, // which endpoint host we connected to
    pub daemon_uptime_secs: Option<u64>,
    pub last_extras_refresh: Instant,
    // Terminal size (updated each frame)
    pub terminal_width: u16,
    // Activity graph (network-style throughput chart in status bar)
    pub activity_buf: Vec<u8>, // fixed array, each value = bar height 0-7
    // Snooze
    pub snooze_selection: usize,
    // Signal filtering
    pub signals_debug_mode: bool,
    // Onboarding
    pub onboarding: OnboardingState,
    // Setup mode (first-run, no workspace exists yet)
    pub setup: Option<SetupState>,
    // Common
    pub pending_action: Option<PendingAction>,
    pub flash: Option<FlashMessage>,
    pub needs_redraw: bool,
    pub spinner_tick: usize,
    pub last_worker_refresh: Instant,
    pub last_signal_refresh: Instant,
}

impl App {
    /// Create app from discovered workspaces, focusing the given tab.
    /// If `needs_onboarding` is true, the dashboard starts with progressive reveal.
    ///
    /// **No I/O**: initializes with empty state. Background tasks load data
    /// and send updates via the `AppUpdate` channel.
    pub fn new(
        workspaces: Vec<Workspace>,
        focus_workspace: Option<&str>,
        needs_onboarding: bool,
    ) -> Self {
        let ws_states: Vec<WorkspaceState> = workspaces
            .into_iter()
            .map(|ws| WorkspaceState {
                name: ws.name,
                config: ws.config,
                signals: Vec::new(),
                workers: Vec::new(),
                chat_history: Vec::new(),
                input: String::new(),
                cursor_pos: 0,
                chat_scroll: ScrollState::new(),
                streaming: false,
                coordinator_preview: None,
                has_unread_response: false,
                coordinator_turns: 0,
                sparkline_data: vec![0; 24],
                watcher_health: Vec::new(),
                feed: Vec::new(),
                prev_worker_phases: std::collections::HashMap::new(),
                prev_signal_ids: std::collections::HashSet::new(),
                prev_pr_workers: std::collections::HashSet::new(),
                feed_scroll: ScrollState::new(),
                thoughts: Vec::new(),
            })
            .collect();

        let active_tab = if let Some(name) = focus_workspace {
            ws_states.iter().position(|ws| ws.name == name).unwrap_or(0)
        } else {
            0
        };

        let onboarding = if needs_onboarding {
            OnboardingState::new_active()
        } else {
            OnboardingState::completed()
        };

        let focused_panel = if needs_onboarding {
            Panel::Chat
        } else {
            Panel::Workers
        };

        let chat_focused = needs_onboarding;

        let mut app = Self {
            workspaces: ws_states,
            active_tab,
            prefix_active: false,
            view: View::Dashboard,
            mode: Mode::Normal,
            focused_panel,
            zoomed_panel: None,
            worker_selection: 0,
            signal_selection: 0,
            review_selection: 0,
            feed_selection: 0,
            chat_focused,
            worker_input: String::new(),
            worker_input_active: false,
            review_comment_active: false,
            review_comment_input: String::new(),
            review_comment_repo: String::new(),
            review_comment_pr: 0,
            content_scroll: 0,
            signal_list_selection: 0,
            review_list_selection: 0,
            pr_list_selection: 0,
            daemon_alive: false,
            daemon_connected: false,
            daemon_remote: false,
            remote_host: None,
            daemon_uptime_secs: None,
            last_extras_refresh: Instant::now(),
            terminal_width: 80,
            activity_buf: vec![0; 18],
            snooze_selection: 0,
            signals_debug_mode: false,
            onboarding,
            setup: None,
            pending_action: None,
            flash: None,
            needs_redraw: true,
            spinner_tick: 0,
            last_worker_refresh: Instant::now(),
            last_signal_refresh: Instant::now(),
        };

        // Inject first onboarding message into chat
        if needs_onboarding && let Some(ws) = app.workspaces.get_mut(app.active_tab) {
            ws.chat_history.push(ChatLine::Assistant(
                "Hey! I'm Bee \u{2014} your dev workspace coordinator. \
                 I watch your GitHub, manage AI workers, and keep you in the loop.\n\n\
                 Let me show you around. Press enter to continue."
                    .to_string(),
                now_ts(),
                None,
            ));
            ws.chat_scroll.scroll_to_bottom();
        }

        // No I/O here — background tasks handle initial data load.
        app
    }

    /// Create app in setup mode (no workspaces exist yet).
    /// Provides a placeholder workspace for the chat UI while Bee walks
    /// the user through initial configuration.
    pub fn new_setup() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let ws_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_string();

        // Placeholder workspace for chat — fall back to a cwd-rooted config on
        // any parse error so we never panic on first run.
        let config: config::WorkspaceConfig =
            serde_json::from_value(serde_json::json!({"root": cwd.to_string_lossy()}))
                .unwrap_or_else(|_| {
                    serde_json::from_value(serde_json::json!({"root": "."}))
                        .expect("hardcoded fallback config")
                });

        let ws_state = WorkspaceState {
            name: "(setup)".to_string(),
            config,
            signals: Vec::new(),
            workers: Vec::new(),
            chat_history: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            chat_scroll: ScrollState::new(),
            streaming: false,
            coordinator_preview: None,
            has_unread_response: false,
            coordinator_turns: 0,
            prev_worker_phases: std::collections::HashMap::new(),
            prev_signal_ids: std::collections::HashSet::new(),
            prev_pr_workers: std::collections::HashSet::new(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            thoughts: Vec::new(),
        };

        let setup = SetupState {
            step: SetupStep::AskRoot,
            workspace_root: cwd.clone(),
            workspace_name: ws_name,
            default_agent: "claude".to_string(),
            has_github: false,
            telegram_token: None,
            telegram_chat_id: None,
            add_workspace: false,
        };

        let cwd_display = cwd.display().to_string();

        let mut app = Self {
            workspaces: vec![ws_state],
            active_tab: 0,
            prefix_active: false,
            view: View::Dashboard,
            mode: Mode::Normal,
            focused_panel: Panel::Chat,
            zoomed_panel: None,
            worker_selection: 0,
            signal_selection: 0,
            review_selection: 0,
            feed_selection: 0,
            chat_focused: true,
            worker_input: String::new(),
            worker_input_active: false,
            review_comment_active: false,
            review_comment_input: String::new(),
            review_comment_repo: String::new(),
            review_comment_pr: 0,
            content_scroll: 0,
            signal_list_selection: 0,
            review_list_selection: 0,
            pr_list_selection: 0,
            daemon_alive: false,
            daemon_connected: false,
            daemon_remote: false,
            remote_host: None,
            daemon_uptime_secs: None,
            last_extras_refresh: Instant::now(),
            terminal_width: 80,
            activity_buf: vec![0; 18],
            snooze_selection: 0,
            onboarding: OnboardingState::new_active(), // only Chat panel visible
            setup: Some(setup),
            signals_debug_mode: false,
            pending_action: None,
            flash: None,
            needs_redraw: true,
            spinner_tick: 0,
            last_worker_refresh: Instant::now(),
            last_signal_refresh: Instant::now(),
        };

        // Inject first setup message
        if let Some(ws) = app.workspaces.get_mut(0) {
            ws.chat_history.push(ChatLine::Assistant(
                format!(
                    "Hi! I'm Bee \u{2014} your dev workspace coordinator.\n\n\
                     Looks like you haven't set up a workspace yet. Let's fix that!\n\n\
                     What directory would you like to use as your workspace root?\n\
                     (Press Enter for current directory: {cwd_display})"
                ),
                now_ts(),
                None,
            ));
            ws.chat_scroll.scroll_to_bottom();
        }

        app
    }

    /// Enter add-workspace mode on an existing app.
    /// Adds a placeholder "(setup)" workspace tab, switches to it,
    /// and starts the simplified setup flow.
    pub fn enter_add_workspace(&mut self, dir: std::path::PathBuf) {
        let ws_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_string();

        let dir_display = dir.display().to_string();

        let config: config::WorkspaceConfig =
            serde_json::from_value(serde_json::json!({"root": dir.to_string_lossy()}))
                .unwrap_or_else(|_| {
                    serde_json::from_value(serde_json::json!({"root": "."}))
                        .expect("hardcoded fallback config")
                });

        let mut ws_state = WorkspaceState {
            name: "(setup)".to_string(),
            config,
            signals: Vec::new(),
            workers: Vec::new(),
            chat_history: Vec::new(),
            input: String::new(),
            chat_scroll: ScrollState::new(),
            streaming: false,
            coordinator_preview: None,
            has_unread_response: false,
            coordinator_turns: 0,
            prev_worker_phases: std::collections::HashMap::new(),
            prev_signal_ids: std::collections::HashSet::new(),
            prev_pr_workers: std::collections::HashSet::new(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            thoughts: Vec::new(),
        };

        ws_state.chat_history.push(ChatLine::Assistant(
            format!(
                "Setting up a new workspace.\n\n\
                 I'll use `{dir_display}` \u{2014} is that right?\n\
                 (Press Enter to confirm, or type a different path)"
            ),
            now_ts(),
            None,
        ));
        ws_state.chat_scroll.scroll_to_bottom();

        self.workspaces.push(ws_state);
        let new_idx = self.workspaces.len() - 1;
        self.active_tab = new_idx;
        self.view = View::Dashboard;
        self.focused_panel = Panel::Chat;
        self.chat_focused = true;
        self.setup = Some(SetupState {
            step: SetupStep::AskRoot,
            workspace_root: dir,
            workspace_name: ws_name,
            default_agent: "claude".to_string(),
            has_github: false,
            telegram_token: None,
            telegram_chat_id: None,
            add_workspace: true,
        });
        self.needs_redraw = true;
    }

    /// Process user input during setup mode. Returns true when setup is complete.
    pub fn process_setup_input(&mut self, input: &str) -> bool {
        // Compute the response and whether we're done (scope ends mutable borrow on self.setup)
        let (response, done) = {
            let setup = match self.setup.as_mut() {
                Some(s) => s,
                None => return false,
            };

            match setup.step {
                SetupStep::AskRoot => {
                    let input = input.trim();
                    if !input.is_empty() {
                        setup.workspace_root = std::path::PathBuf::from(input);
                    }
                    setup.workspace_name = setup
                        .workspace_root
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("workspace")
                        .to_string();
                    let root = setup.workspace_root.display().to_string();
                    if setup.add_workspace {
                        // Simplified flow: just confirm root then ask name
                        let suggested = setup.workspace_name.clone();
                        setup.step = SetupStep::AskName;
                        (
                            format!(
                                "\u{2713} Workspace root: {root}\n\n\
                                 What should I call this workspace?\n\
                                 (Press Enter for '{suggested}')"
                            ),
                            false,
                        )
                    } else {
                        setup.step = SetupStep::AskProvider;
                        (
                            format!(
                                "\u{2713} Workspace root: {root}\n\n\
                                 Which AI providers do you have access to?\n\
                                 \u{2022} claude \u{2014} Anthropic Claude (recommended)\n\
                                 \u{2022} codex \u{2014} OpenAI Codex\n\
                                 \u{2022} both \u{2014} auto-detect at dispatch time"
                            ),
                            false,
                        )
                    }
                }
                SetupStep::AskName => {
                    let input = input.trim();
                    if !input.is_empty() {
                        setup.workspace_name = input.to_string();
                    }
                    let name = setup.workspace_name.clone();
                    setup.step = SetupStep::Done;
                    (
                        format!(
                            "\u{2713} Workspace name: {name}\n\n\
                             Writing workspace config..."
                        ),
                        true,
                    )
                }
                SetupStep::AskProvider => {
                    let input = input.trim().to_lowercase();
                    setup.default_agent = match input.as_str() {
                        "codex" => "codex".to_string(),
                        "both" => "auto".to_string(),
                        _ => "claude".to_string(),
                    };
                    let agent = setup.default_agent.clone();
                    setup.step = SetupStep::AskGithub;
                    (
                        format!(
                            "\u{2713} Default agent: {agent}\n\n\
                             Do you have a GitHub token configured with the `gh` CLI? (yes / no)"
                        ),
                        false,
                    )
                }
                SetupStep::AskGithub => {
                    let input = input.trim().to_lowercase();
                    setup.has_github = matches!(input.as_str(), "y" | "yes");
                    let gh_status = if setup.has_github {
                        "enabled"
                    } else {
                        "skipped"
                    };
                    setup.step = SetupStep::AskTelegram;
                    (
                        format!(
                            "\u{2713} GitHub watcher: {gh_status}\n\n\
                             Would you like Telegram notifications?\n\
                             Enter your bot token from @BotFather, or type 'skip'."
                        ),
                        false,
                    )
                }
                SetupStep::AskTelegram => {
                    let input = input.trim();
                    if input.is_empty()
                        || input.eq_ignore_ascii_case("skip")
                        || input.eq_ignore_ascii_case("no")
                    {
                        setup.telegram_token = None;
                        setup.step = SetupStep::Done;
                        (
                            "\u{2713} Telegram: skipped\n\nWriting workspace config...".to_string(),
                            true,
                        )
                    } else {
                        setup.telegram_token = Some(input.to_string());
                        setup.step = SetupStep::AskTelegramChatId;
                        (
                            "\u{2713} Bot token saved.\n\n\
                             Now I need your Telegram chat ID.\n\
                             Message @userinfobot on Telegram to get it, then paste it here."
                                .to_string(),
                            false,
                        )
                    }
                }
                SetupStep::AskTelegramChatId => {
                    let input = input.trim();
                    if let Ok(chat_id) = input.parse::<i64>() {
                        setup.telegram_chat_id = Some(chat_id);
                        setup.step = SetupStep::Done;
                        (
                            format!("\u{2713} Chat ID: {chat_id}\n\nWriting workspace config..."),
                            true,
                        )
                    } else {
                        (
                            "That doesn't look like a valid chat ID (should be a number). \
                             Try again:"
                                .to_string(),
                            false,
                        )
                    }
                }
                SetupStep::Done => return false,
            }
        }; // setup borrow released

        // Push Bee's response to chat
        if let Some(ws) = self.current_ws_mut() {
            ws.chat_history
                .push(ChatLine::Assistant(response, now_ts(), None));
            ws.chat_scroll.scroll_to_bottom();
        }
        self.needs_redraw = true;
        done
    }

    /// Write workspace config and transition from setup mode to normal dashboard.
    pub fn complete_setup(&mut self) -> Result<(), String> {
        let setup = self.setup.take().ok_or("not in setup mode")?;
        let is_add = setup.add_workspace;

        // Build and write TOML config
        let toml_content = build_setup_toml(&setup);
        let dir = config::workspaces_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let config_path = find_available_config_path(&dir, &setup.workspace_name);
        std::fs::write(&config_path, &toml_content)
            .map_err(|e| format!("could not write config: {e}"))?;

        // Write .onboarded marker
        if let Err(e) = super::onboarding::mark_onboarded() {
            tracing::warn!("failed to write onboarded marker: {e}");
        }

        // Reload workspaces — the actual file name may differ from
        // setup.workspace_name if a collision was found (e.g. "foo-2.toml").
        let actual_name = config_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&setup.workspace_name)
            .to_string();
        let workspaces = config::discover_workspaces()
            .map_err(|e| format!("could not reload workspaces: {e}"))?;
        let ws = workspaces
            .into_iter()
            .find(|w| w.name == actual_name)
            .ok_or("workspace not found after creation")?;

        // Config display path with ~ shorthand
        let config_display = if let Some(home) = dirs::home_dir() {
            if let Ok(suffix) = config_path.strip_prefix(&home) {
                format!("~/{}", suffix.display())
            } else {
                config_path.display().to_string()
            }
        } else {
            config_path.display().to_string()
        };

        // Build real workspace state
        let mut ws_state = WorkspaceState {
            name: ws.name,
            config: ws.config,
            signals: Vec::new(),
            workers: Vec::new(),
            chat_history: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            chat_scroll: ScrollState::new(),
            streaming: false,
            coordinator_preview: None,
            has_unread_response: false,
            coordinator_turns: 0,
            prev_worker_phases: std::collections::HashMap::new(),
            prev_signal_ids: std::collections::HashSet::new(),
            prev_pr_workers: std::collections::HashSet::new(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            thoughts: Vec::new(),
        };

        // Completion message
        ws_state.chat_history.push(ChatLine::Assistant(
            format!(
                "\u{2713} Workspace '{}' created!\n\n\
                 Config written to {}\n\n\
                 The dashboard is all yours. Ask me anything \u{2014} \
                 try 'what can you do?' or '/help'.",
                setup.workspace_name, config_display
            ),
            now_ts(),
            None,
        ));
        ws_state.chat_scroll.scroll_to_bottom();

        if is_add {
            // Add-workspace: replace the placeholder "(setup)" tab with the real one
            if let Some(idx) = self.workspaces.iter().position(|w| w.name == "(setup)") {
                self.workspaces[idx] = ws_state;
                self.active_tab = idx;
            } else {
                self.workspaces.push(ws_state);
                self.active_tab = self.workspaces.len() - 1;
            }
        } else {
            // First-run setup: preserve chat history from setup conversation
            let chat_history = self
                .workspaces
                .first()
                .map(|w| w.chat_history.clone())
                .unwrap_or_default();
            ws_state.chat_history.splice(0..0, chat_history);

            self.workspaces = vec![ws_state];
            self.active_tab = 0;
        }

        self.onboarding = OnboardingState::completed();
        self.focused_panel = Panel::Workers;
        self.chat_focused = false;
        self.needs_redraw = true;

        Ok(())
    }

    /// Current workspace, if any.
    pub fn current_ws(&self) -> Option<&WorkspaceState> {
        self.workspaces.get(self.active_tab)
    }

    /// Current workspace mutably, if any.
    pub fn current_ws_mut(&mut self) -> Option<&mut WorkspaceState> {
        self.workspaces.get_mut(self.active_tab)
    }

    // ── Tab switching ─────────────────────────────────────

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.workspaces.len() {
            self.active_tab = idx;
            self.content_scroll = 0;
            self.worker_input_active = false;

            // Preserve full-screen view type, clamped to new workspace
            match self.view {
                View::WorkerDetail(_) => {
                    let count = self.workspaces[idx].workers.len();
                    if count > 0 {
                        self.worker_selection = 0;
                        self.enter_worker_detail(0);
                    } else {
                        self.view = View::Dashboard;
                        self.focused_panel = Panel::Workers;
                        self.worker_selection = 0;
                    }
                }
                View::SignalDetail(_) => {
                    let count = self.workspaces[idx].signals.len();
                    if count > 0 {
                        self.signal_selection = 0;
                        self.enter_signal_detail(0);
                    } else {
                        self.view = View::Dashboard;
                        self.focused_panel = Panel::Signals;
                        self.signal_selection = 0;
                    }
                }
                _ => {
                    self.view = View::Dashboard;
                    // Keep focused_panel in sync with zoomed panel
                    self.focused_panel = self.zoomed_panel.unwrap_or(Panel::Workers);
                    self.worker_selection = 0;
                    self.signal_selection = 0;
                }
            }
            self.needs_redraw = true;
        }
    }

    // ── Panel navigation ──────────────────────────────────

    pub fn toggle_zoom(&mut self) {
        if self.zoomed_panel.is_some() {
            self.zoomed_panel = None;
        } else {
            self.zoomed_panel = Some(self.focused_panel);
        }
        self.needs_redraw = true;
    }

    pub fn next_panel(&mut self) {
        self.focused_panel = match self.focused_panel {
            Panel::Home => Panel::Workers,
            Panel::Workers => {
                if self.has_review_queue() {
                    Panel::Reviews
                } else {
                    Panel::Signals
                }
            }
            Panel::Reviews => Panel::Signals,
            Panel::Signals => Panel::Feed,
            Panel::Feed => Panel::Chat,
            Panel::Chat => Panel::Home,
        };
        self.chat_focused = false;
        self.needs_redraw = true;
    }

    pub fn prev_panel(&mut self) {
        self.focused_panel = match self.focused_panel {
            Panel::Home => Panel::Chat,
            Panel::Workers => Panel::Home,
            Panel::Reviews => Panel::Workers,
            Panel::Signals => {
                if self.has_review_queue() {
                    Panel::Reviews
                } else {
                    Panel::Workers
                }
            }
            Panel::Feed => Panel::Signals,
            Panel::Chat => Panel::Feed,
        };
        self.chat_focused = false;
        self.needs_redraw = true;
    }

    pub fn select_next_in_panel(&mut self) {
        match self.focused_panel {
            Panel::Home => {}
            Panel::Workers => {
                let count = self.current_ws().map_or(0, |ws| ws.workers.len());
                if count > 0 && self.worker_selection + 1 < count {
                    self.worker_selection += 1;
                }
            }
            Panel::Signals => {
                // Horizontal carousel: j/down = swipe left (previous card)
                self.signal_selection = self.signal_selection.saturating_sub(1);
            }
            Panel::Reviews => {
                // Horizontal carousel like signals
                self.review_selection = self.review_selection.saturating_sub(1);
            }
            Panel::Feed => {
                let count = self.current_ws().map_or(0, |ws| ws.feed.len());
                if count > 0 && self.feed_selection + 1 < count {
                    self.feed_selection += 1;
                }
            }
            Panel::Chat => {
                self.scroll_chat_down(3);
                return;
            }
        }
        self.needs_redraw = true;
    }

    pub fn select_prev_in_panel(&mut self) {
        match self.focused_panel {
            Panel::Home => {}
            Panel::Workers => {
                self.worker_selection = self.worker_selection.saturating_sub(1);
            }
            Panel::Signals => {
                // Horizontal carousel: k/up = swipe right (next card)
                let count = self.signal_selectable_count();
                if count > 0 && self.signal_selection + 1 < count {
                    self.signal_selection += 1;
                }
            }
            Panel::Reviews => {
                let count = self.review_signal_count();
                if count > 0 && self.review_selection + 1 < count {
                    self.review_selection += 1;
                }
            }
            Panel::Feed => {
                self.feed_selection = self.feed_selection.saturating_sub(1);
            }
            Panel::Chat => {
                self.scroll_chat_up(3);
                return;
            }
        }
        self.needs_redraw = true;
    }

    fn signal_selectable_count(&self) -> usize {
        self.current_ws().map_or(0, |ws| {
            ws.signals
                .iter()
                .filter(|s| s.source != "github_review_queue")
                .filter(|s| self.signals_debug_mode || !is_noise_signal(s))
                .count()
        })
    }

    fn review_signal_count(&self) -> usize {
        self.current_ws().map_or(0, |ws| {
            ws.signals
                .iter()
                .filter(|s| s.source == "github_review_queue")
                .count()
        })
    }

    /// Clamp selections after data refresh.
    pub fn clamp_selections(&mut self) {
        let (worker_count, sig_count, review_count, feed_count) =
            if let Some(ws) = self.current_ws() {
                (
                    ws.workers.len(),
                    ws.signals
                        .iter()
                        .filter(|s| s.source != "github_review_queue")
                        .filter(|s| self.signals_debug_mode || !is_noise_signal(s))
                        .count(),
                    ws.signals
                        .iter()
                        .filter(|s| s.source == "github_review_queue")
                        .count(),
                    ws.feed.len(),
                )
            } else {
                return;
            };

        if worker_count == 0 {
            self.worker_selection = 0;
        } else if self.worker_selection >= worker_count {
            self.worker_selection = worker_count - 1;
        }

        if sig_count == 0 {
            self.signal_selection = 0;
        } else if self.signal_selection >= sig_count {
            self.signal_selection = sig_count - 1;
        }

        if review_count == 0 {
            self.review_selection = 0;
        } else if self.review_selection >= review_count {
            self.review_selection = review_count - 1;
        }

        if feed_count == 0 {
            self.feed_selection = 0;
        } else if self.feed_selection >= feed_count {
            self.feed_selection = feed_count - 1;
        }
    }

    // ── View transitions ──────────────────────────────────

    pub fn enter_worker_detail(&mut self, idx: usize) {
        self.view = View::WorkerDetail(idx);
        self.content_scroll = 0;
        self.worker_input.clear();
        self.worker_input_active = false;
        self.refresh_worker_conversation(idx);
        self.needs_redraw = true;
    }

    pub fn enter_signal_detail(&mut self, idx: usize) {
        self.view = View::SignalDetail(idx);
        self.content_scroll = 0;
        self.needs_redraw = true;
    }

    pub fn enter_signal_list(&mut self) {
        self.view = View::SignalList;
        self.signal_list_selection = 0;
        self.content_scroll = 0;
        self.needs_redraw = true;
    }

    pub fn enter_review_list(&mut self) {
        self.view = View::ReviewList;
        self.review_list_selection = 0;
        self.content_scroll = 0;
        self.needs_redraw = true;
    }

    /// Whether the current workspace has review queue configured.
    pub fn has_review_queue(&self) -> bool {
        self.current_ws().is_some_and(|ws| {
            ws.config
                .watchers
                .github
                .as_ref()
                .is_some_and(|gh| !gh.review_queue.is_empty())
        })
    }

    pub fn back_to_dashboard(&mut self) {
        match self.view {
            View::WorkerDetail(_) | View::PrList => self.focused_panel = Panel::Workers,
            View::SignalDetail(_) | View::SignalList => {
                // Return to whichever signal panel was focused before drill-in
                if self.focused_panel != Panel::Reviews {
                    self.focused_panel = Panel::Signals;
                }
            }
            View::ReviewList => self.focused_panel = Panel::Reviews,
            _ => {
                // Preserve current focused_panel
            }
        }
        self.view = View::Dashboard;
        self.chat_focused = false;
        self.worker_input_active = false;
        self.needs_redraw = true;
    }

    /// Reload conversation entries for a specific worker from its events.jsonl.
    pub fn refresh_worker_conversation(&mut self, idx: usize) {
        if let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            let events_path = ws
                .config
                .root
                .join(".swarm")
                .join("agents")
                .join(&worker.id)
                .join("events.jsonl");
            let new_entries = apiari_tui::events_parser::parse_events(&events_path);
            let had_entries = worker.conversation.len();
            worker.conversation = new_entries;
            // Auto-scroll to bottom when new entries appear
            if worker.conversation.len() > had_entries {
                worker.conv_scroll.scroll_to_bottom();
            }
        }
    }

    /// Drill into the currently selected panel item.
    pub fn drill_in(&mut self) {
        match self.focused_panel {
            Panel::Home => {} // Home has no drill-in
            Panel::Workers => {
                if let Some(ws) = self.current_ws()
                    && self.worker_selection < ws.workers.len()
                {
                    self.enter_worker_detail(self.worker_selection);
                }
            }
            Panel::Signals => {
                let debug = self.signals_debug_mode;
                let orig_idx = self.current_ws().and_then(|ws| {
                    ws.signals
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.source != "github_review_queue")
                        .filter(|(_, s)| debug || !is_noise_signal(s))
                        .map(|(i, _)| i)
                        .nth(self.signal_selection)
                });
                if let Some(idx) = orig_idx {
                    self.enter_signal_detail(idx);
                }
            }
            Panel::Reviews => {
                let orig_idx = self.current_ws().and_then(|ws| {
                    ws.signals
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.source == "github_review_queue")
                        .map(|(i, _)| i)
                        .nth(self.review_selection)
                });
                if let Some(idx) = orig_idx {
                    self.enter_signal_detail(idx);
                }
            }
            Panel::Feed => {
                // Feed items that are signals could drill into signal detail
                if let Some(ws) = self.current_ws()
                    && self.feed_selection < ws.feed.len()
                    && matches!(ws.feed[self.feed_selection].kind, FeedKind::Signal)
                {
                    // Find matching signal by title substring
                    let feed_text = ws.feed[self.feed_selection].text.clone();
                    if let Some(idx) = ws.signals.iter().position(|s| feed_text.contains(&s.title))
                    {
                        self.enter_signal_detail(idx);
                    }
                }
            }
            Panel::Chat => {
                self.chat_focused = true;
                if let Some(ws) = self.current_ws_mut() {
                    ws.has_unread_response = false;
                }
                self.needs_redraw = true;
            }
        }
    }

    pub fn enter_pr_list(&mut self) {
        self.view = View::PrList;
        self.pr_list_selection = 0;
        self.content_scroll = 0;
        self.needs_redraw = true;
    }

    /// Workers that have PRs, with their original indices.
    pub fn workers_with_prs(&self) -> Vec<(usize, &WorkerInfo)> {
        match self.current_ws() {
            Some(ws) => ws
                .workers
                .iter()
                .enumerate()
                .filter(|(_, w)| w.pr.is_some())
                .collect(),
            None => Vec::new(),
        }
    }

    // ── Input editing ─────────────────────────────────────

    pub fn insert_char(&mut self, c: char) {
        if let Some(ws) = self.current_ws_mut() {
            ws.input.insert(ws.cursor_pos, c);
            ws.cursor_pos += c.len_utf8();
            self.needs_redraw = true;
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        if let Some(ws) = self.current_ws_mut() {
            ws.input.insert_str(ws.cursor_pos, s);
            ws.cursor_pos += s.len();
            self.needs_redraw = true;
        }
    }

    pub fn backspace(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            if ws.cursor_pos > 0 {
                // Find the previous char boundary
                let prev = ws.input[..ws.cursor_pos]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                ws.input.remove(prev);
                ws.cursor_pos = prev;
            }
            self.needs_redraw = true;
        }
    }

    pub fn delete_forward(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            if ws.cursor_pos < ws.input.len() {
                ws.input.remove(ws.cursor_pos);
            }
            self.needs_redraw = true;
        }
    }

    pub fn take_input(&mut self) -> String {
        match self.current_ws_mut() {
            Some(ws) => {
                ws.cursor_pos = 0;
                std::mem::take(&mut ws.input)
            }
            None => String::new(),
        }
    }

    pub fn clear_input(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            ws.input.clear();
            ws.cursor_pos = 0;
            self.needs_redraw = true;
        }
    }

    // ── Cursor movement ──────────────────────────────────

    pub fn cursor_left(&mut self) {
        if let Some(ws) = self.current_ws_mut()
            && ws.cursor_pos > 0
        {
            ws.cursor_pos = ws.input[..ws.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.needs_redraw = true;
        }
    }

    pub fn cursor_right(&mut self) {
        if let Some(ws) = self.current_ws_mut()
            && ws.cursor_pos < ws.input.len()
        {
            ws.cursor_pos += ws.input[ws.cursor_pos..].chars().next().unwrap().len_utf8();
            self.needs_redraw = true;
        }
    }

    pub fn cursor_word_left(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            let s = &ws.input[..ws.cursor_pos];
            // Skip trailing whitespace, then skip non-whitespace
            let trimmed = s.trim_end();
            if trimmed.is_empty() {
                ws.cursor_pos = 0;
            } else {
                // Find last whitespace before end of trimmed
                let pos = trimmed
                    .rfind(char::is_whitespace)
                    .map(|i| {
                        // advance past the whitespace char
                        i + trimmed[i..].chars().next().unwrap().len_utf8()
                    })
                    .unwrap_or(0);
                ws.cursor_pos = pos;
            }
            self.needs_redraw = true;
        }
    }

    pub fn cursor_word_right(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            let s = &ws.input[ws.cursor_pos..];
            // Skip non-whitespace, then skip whitespace
            let after_word = s.find(char::is_whitespace).unwrap_or(s.len());
            let rest = &s[after_word..];
            let after_space = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            ws.cursor_pos += after_word + after_space;
            self.needs_redraw = true;
        }
    }

    pub fn cursor_home(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            // Move to start of current line (find last '\n' before cursor)
            ws.cursor_pos = ws.input[..ws.cursor_pos]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            self.needs_redraw = true;
        }
    }

    pub fn cursor_end(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            // Move to end of current line (find next '\n' after cursor)
            ws.cursor_pos = ws.input[ws.cursor_pos..]
                .find('\n')
                .map(|i| ws.cursor_pos + i)
                .unwrap_or(ws.input.len());
            self.needs_redraw = true;
        }
    }

    pub fn cursor_up(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            // Find current line start and column (as char count, not bytes)
            let line_start = ws.input[..ws.cursor_pos]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            if line_start == 0 {
                // Already on first line — move to beginning
                ws.cursor_pos = 0;
                self.needs_redraw = true;
                return;
            }
            let char_col = ws.input[line_start..ws.cursor_pos].chars().count();
            // Find previous line start
            let prev_line_start = ws.input[..line_start - 1]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            let prev_line = &ws.input[prev_line_start..line_start - 1];
            // Map char column back to byte offset on prev line
            let byte_offset: usize = prev_line
                .char_indices()
                .nth(char_col)
                .map(|(i, _)| i)
                .unwrap_or(prev_line.len());
            ws.cursor_pos = prev_line_start + byte_offset;
            self.needs_redraw = true;
        }
    }

    pub fn cursor_down(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            // Find current line start and column (as char count, not bytes)
            let line_start = ws.input[..ws.cursor_pos]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            let char_col = ws.input[line_start..ws.cursor_pos].chars().count();
            // Find next line
            let next_newline = ws.input[ws.cursor_pos..].find('\n');
            match next_newline {
                Some(offset) => {
                    let next_line_start = ws.cursor_pos + offset + 1;
                    let next_line_end = ws.input[next_line_start..]
                        .find('\n')
                        .map(|i| next_line_start + i)
                        .unwrap_or(ws.input.len());
                    let next_line = &ws.input[next_line_start..next_line_end];
                    // Map char column back to byte offset on next line
                    let byte_offset: usize = next_line
                        .char_indices()
                        .nth(char_col)
                        .map(|(i, _)| i)
                        .unwrap_or(next_line.len());
                    ws.cursor_pos = next_line_start + byte_offset;
                }
                None => {
                    // Already on last line — move to end
                    ws.cursor_pos = ws.input.len();
                }
            }
            self.needs_redraw = true;
        }
    }

    // ── Chat ──────────────────────────────────────────────

    pub fn push_user_message(&mut self, text: String) {
        self.push_user_message_with_source(text, None);
    }

    pub fn push_user_message_with_source(&mut self, text: String, source: Option<MessageSource>) {
        let ts = now_ts();
        if let Some(ws) = self.current_ws_mut() {
            ws.chat_history
                .push(ChatLine::User(text.clone(), ts, source));
            ws.streaming = true;
            ws.chat_scroll.scroll_to_bottom();
        }
        // Persist
        if let Some(ws) = self.current_ws() {
            let _ = super::history::save_message(
                &ws.name,
                &super::history::ChatMessage {
                    role: "user".into(),
                    content: text,
                    ts: Utc::now(),
                    source: source.map(|s| match s {
                        MessageSource::Telegram => "telegram".into(),
                        MessageSource::System => "system".into(),
                        MessageSource::Tui => "tui".into(),
                    }),
                },
            );
        }
        self.needs_redraw = true;
    }

    pub fn append_assistant_token(&mut self, token: &str) {
        if let Some(ws) = self.current_ws_mut() {
            if let Some(ChatLine::Assistant(s, _, _)) = ws.chat_history.last_mut() {
                s.push_str(token);
            } else {
                ws.chat_history
                    .push(ChatLine::Assistant(token.to_string(), now_ts(), None));
            }
            ws.chat_scroll.scroll_to_bottom();
            self.needs_redraw = true;
        }
    }

    pub fn finish_assistant_message(&mut self) {
        let is_chat_visible = matches!(self.view, View::Dashboard);
        if let Some(ws) = self.current_ws_mut() {
            ws.streaming = false;
            ws.coordinator_turns += 1;
            if let Some(ChatLine::Assistant(s, _, _)) = ws.chat_history.last() {
                let _ = super::history::save_message(
                    &ws.name,
                    &super::history::ChatMessage {
                        role: "assistant".into(),
                        content: s.clone(),
                        ts: Utc::now(),
                        source: None,
                    },
                );
                ws.coordinator_preview = Some(truncate_preview(s, 120));
                if !is_chat_visible {
                    ws.has_unread_response = true;
                }
            }
            self.needs_redraw = true;
        }
    }

    pub fn push_system_message(&mut self, text: String) {
        if let Some(ws) = self.current_ws_mut() {
            ws.chat_history.push(ChatLine::System(text));
            ws.streaming = false;
            self.needs_redraw = true;
        }
    }

    /// Push an activity event from the daemon (Telegram or TUI-sourced).
    pub fn push_activity(&mut self, workspace: &str, source: &str, kind: &str, text: &str) {
        let ws_name = {
            let ws = match self.workspaces.iter_mut().find(|ws| ws.name == workspace) {
                Some(ws) => ws,
                None => return,
            };

            let msg_source = match source {
                "telegram" => Some(MessageSource::Telegram),
                "tui" => Some(MessageSource::Tui),
                _ => Some(MessageSource::System),
            };

            let ts = now_ts();
            match kind {
                "session_reset" => {
                    // Explicit session reset signal from daemon (clear/compact)
                    ws.coordinator_turns = 0;
                    ws.chat_history
                        .push(ChatLine::Assistant(text.to_string(), ts, msg_source));
                    ws.coordinator_preview = Some(truncate_preview(text, 120));
                }
                "user_message" => {
                    ws.chat_history
                        .push(ChatLine::User(text.to_string(), ts, msg_source));
                }
                "assistant_message" => {
                    ws.chat_history
                        .push(ChatLine::Assistant(text.to_string(), ts, msg_source));
                    ws.coordinator_preview = Some(truncate_preview(text, 120));
                    ws.has_unread_response = true;
                }
                _ => {
                    ws.chat_history.push(ChatLine::System(text.to_string()));
                }
            }
            ws.chat_scroll.scroll_to_bottom();
            ws.name.clone()
        };

        // Persist user/assistant messages from external sources (e.g. Telegram)
        let role = match kind {
            "user_message" => Some("user"),
            "assistant_message" => Some("assistant"),
            _ => None,
        };
        if let Some(role) = role {
            let _ = super::history::save_message(
                &ws_name,
                &super::history::ChatMessage {
                    role: role.into(),
                    content: text.to_string(),
                    ts: chrono::Utc::now(),
                    source: Some(source.to_string()),
                },
            );
        }

        self.needs_redraw = true;
    }

    // ── Scroll ────────────────────────────────────────────

    pub fn scroll_chat_up(&mut self, amount: u16) {
        if let Some(ws) = self.current_ws_mut() {
            ws.chat_scroll.scroll_up(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_chat_down(&mut self, amount: u16) {
        if let Some(ws) = self.current_ws_mut() {
            ws.chat_scroll.scroll_down(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_worker_conv_up(&mut self, amount: u16) {
        if let View::WorkerDetail(idx) = self.view
            && let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            worker.conv_scroll.scroll_up(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_worker_conv_down(&mut self, amount: u16) {
        if let View::WorkerDetail(idx) = self.view
            && let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            worker.conv_scroll.scroll_down(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_content_up(&mut self, amount: u16) {
        self.content_scroll = self.content_scroll.saturating_add(amount);
        self.needs_redraw = true;
    }

    pub fn scroll_content_down(&mut self, amount: u16) {
        self.content_scroll = self.content_scroll.saturating_sub(amount);
        self.needs_redraw = true;
    }

    // ── Flash messages ────────────────────────────────────

    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some(FlashMessage {
            text: text.into(),
            expires: Instant::now() + std::time::Duration::from_secs(3),
        });
        self.needs_redraw = true;
    }

    /// Compute snooze-until timestamp from the selected duration index.
    pub fn compute_snooze_until(selection: usize) -> DateTime<Utc> {
        let now = Utc::now();
        let local_now = Local::now();
        match selection {
            0 => now + chrono::Duration::hours(1),
            1 => now + chrono::Duration::hours(4),
            2 => {
                // Tomorrow 9am local time
                let tomorrow = local_now.date_naive() + chrono::Duration::days(1);
                let nine_am = tomorrow.and_hms_opt(9, 0, 0).unwrap();
                local_now
                    .timezone()
                    .from_local_datetime(&nine_am)
                    .single()
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now + chrono::Duration::hours(12))
            }
            3 => {
                // Next Monday 9am local time
                let today = local_now.date_naive();
                let days_until_monday = (7 - today.weekday().num_days_from_monday()) % 7;
                let days_until_monday = if days_until_monday == 0 {
                    7
                } else {
                    days_until_monday
                };
                let next_monday = today + chrono::Duration::days(days_until_monday as i64);
                let nine_am = next_monday.and_hms_opt(9, 0, 0).unwrap();
                local_now
                    .timezone()
                    .from_local_datetime(&nine_am)
                    .single()
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now + chrono::Duration::hours(24))
            }
            _ => now + chrono::Duration::hours(1),
        }
    }

    pub fn tick_flash(&mut self) {
        if let Some(ref f) = self.flash
            && Instant::now() > f.expires
        {
            self.flash = None;
            self.needs_redraw = true;
        }
    }

    /// Push a new value into the activity graph — shifts everything right,
    /// new data enters from the left, oldest falls off the right edge.
    pub fn push_activity_value(&mut self, val: u8) {
        let len = self.activity_buf.len();
        if len > 1 {
            // Shift right: drop last, insert at front
            self.activity_buf.pop();
            self.activity_buf.insert(0, val);
        } else if len == 1 {
            self.activity_buf[0] = val;
        }
    }

    // ── Data refresh ──────────────────────────────────────

    pub fn refresh_signals(&mut self) {
        let db = config::db_path();
        for ws in &mut self.workspaces {
            if let Ok(store) = SignalStore::open(&db, &ws.name) {
                ws.signals = store.get_open_signals().unwrap_or_default();
            }

            // Detect new signals
            let current_ids: std::collections::HashSet<i64> =
                ws.signals.iter().map(|s| s.id).collect();
            let is_first_load = ws.prev_signal_ids.is_empty() && !ws.signals.is_empty();

            if !is_first_load {
                // Find signals that are new since last refresh
                let new_signals: Vec<&SignalRecord> = ws
                    .signals
                    .iter()
                    .filter(|s| !ws.prev_signal_ids.contains(&s.id))
                    .collect();

                if !new_signals.is_empty() {
                    let count = new_signals.len();
                    // Group by source for concise notification
                    let mut by_source: std::collections::HashMap<&str, usize> =
                        std::collections::HashMap::new();
                    for sig in &new_signals {
                        *by_source.entry(sig.source.as_str()).or_default() += 1;
                    }
                    let summary: Vec<String> = by_source
                        .iter()
                        .map(|(src, n)| format!("{n} {src}"))
                        .collect();
                    let msg = format!(
                        "! {} new signal{}: {}",
                        count,
                        if count > 1 { "s" } else { "" },
                        summary.join(", ")
                    );
                    ws.chat_history.push(ChatLine::System(msg));
                    ws.has_unread_response = true;
                    ws.chat_scroll.scroll_to_bottom();
                }
            }

            ws.prev_signal_ids = current_ids;
        }
        self.last_signal_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Apply worker data from background refresh.
    pub(super) fn apply_worker_update(&mut self, data: Vec<(String, Vec<WorkerInfo>)>) {
        for (name, new_workers) in data {
            if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == name) {
                // Detect state changes and inject chat notifications
                let is_first_load = ws.prev_worker_phases.is_empty() && !new_workers.is_empty();
                for worker in &new_workers {
                    let phase = phase_display(worker).to_string();
                    let prev = ws.prev_worker_phases.get(&worker.id);

                    // Skip first load — don't spam on startup
                    if !is_first_load {
                        if let Some(prev_phase) = prev {
                            if *prev_phase != phase {
                                let msg = match phase.as_str() {
                                    "completed" => format!("\u{2713} {} completed", worker.id),
                                    "waiting" => {
                                        format!("\u{25cb} {} waiting for input", worker.id)
                                    }
                                    "closed" => format!("\u{2500} {} closed", worker.id),
                                    "running" if prev_phase == "waiting" => {
                                        format!("\u{25cf} {} resumed", worker.id)
                                    }
                                    _ => String::new(),
                                };
                                if !msg.is_empty() {
                                    ws.chat_history.push(ChatLine::System(msg));
                                    ws.has_unread_response = true;
                                    ws.chat_scroll.scroll_to_bottom();
                                }
                            }
                        } else {
                            // New worker appeared
                            ws.chat_history
                                .push(ChatLine::System(format!("\u{25cf} {} spawned", worker.id)));
                            ws.has_unread_response = true;
                            ws.chat_scroll.scroll_to_bottom();
                        }

                        // PR opened
                        if let Some(ref pr) = worker.pr
                            && !ws.prev_pr_workers.contains(&worker.id)
                        {
                            ws.chat_history.push(ChatLine::System(format!(
                                "\u{27f3} {} opened PR #{}: {}",
                                worker.id, pr.number, pr.title
                            )));
                            ws.has_unread_response = true;
                            ws.chat_scroll.scroll_to_bottom();
                        }
                    }
                }

                // Update tracking state
                ws.prev_worker_phases = new_workers
                    .iter()
                    .map(|w| (w.id.clone(), phase_display(w).to_string()))
                    .collect();
                ws.prev_pr_workers = new_workers
                    .iter()
                    .filter(|w| w.pr.is_some())
                    .map(|w| w.id.clone())
                    .collect();

                ws.workers = new_workers;
            }
        }
        self.last_worker_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Apply signal data from background refresh.
    pub(super) fn apply_signal_update(&mut self, data: Vec<(String, Vec<SignalRecord>)>) {
        for (name, new_signals) in data {
            if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == name) {
                let current_ids: std::collections::HashSet<i64> =
                    new_signals.iter().map(|s| s.id).collect();
                let is_first_load = ws.prev_signal_ids.is_empty() && !new_signals.is_empty();

                if !is_first_load {
                    let new_sigs: Vec<&SignalRecord> = new_signals
                        .iter()
                        .filter(|s| !ws.prev_signal_ids.contains(&s.id))
                        .collect();

                    if !new_sigs.is_empty() {
                        let count = new_sigs.len();
                        let mut by_source: std::collections::HashMap<&str, usize> =
                            std::collections::HashMap::new();
                        for sig in &new_sigs {
                            *by_source.entry(sig.source.as_str()).or_default() += 1;
                        }
                        let summary: Vec<String> = by_source
                            .iter()
                            .map(|(src, n)| format!("{n} {src}"))
                            .collect();
                        let msg = format!(
                            "! {} new signal{}: {}",
                            count,
                            if count > 1 { "s" } else { "" },
                            summary.join(", ")
                        );
                        ws.chat_history.push(ChatLine::System(msg));
                        ws.has_unread_response = true;
                        ws.chat_scroll.scroll_to_bottom();
                    }
                }

                ws.prev_signal_ids = current_ids;
                ws.signals = new_signals;
            }
        }
        self.last_signal_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Apply extras data from background refresh.
    pub(super) fn apply_extras_update(
        &mut self,
        daemon_alive: bool,
        daemon_uptime_secs: Option<u64>,
        per_workspace: Vec<(String, WorkspaceExtrasData)>,
    ) {
        self.daemon_alive = daemon_alive;
        self.daemon_uptime_secs = daemon_uptime_secs;

        for (name, data) in per_workspace {
            if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == name) {
                ws.sparkline_data = data.sparkline_data;
                ws.watcher_health = data.watcher_health;
                ws.thoughts = data.thoughts;

                // Start with SQLite-sourced feed items, then add in-memory worker items
                let mut feed = data.feed_items;

                let now_utc = Utc::now();
                for worker in &ws.workers {
                    let phase = phase_display(worker);
                    let when = worker
                        .created_at
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or(now_utc);
                    let text = match phase {
                        "running" => format!("{} running", worker.id),
                        "waiting" => format!("{} waiting for input", worker.id),
                        "completed" => format!("{} completed", worker.id),
                        "closed" => format!("{} closed", worker.id),
                        _ => format!("{} {}", worker.id, phase),
                    };
                    feed.push(FeedItem {
                        when,
                        kind: FeedKind::Worker,
                        text,
                    });
                }

                if daemon_alive
                    && ws.signals.is_empty()
                    && ws.workers.iter().all(|w| {
                        let p = phase_display(w);
                        p == "completed" || p == "closed"
                    })
                {
                    feed.push(FeedItem {
                        when: now_utc,
                        kind: FeedKind::Heartbeat,
                        text: "all clear — nothing needs attention".into(),
                    });
                }

                feed.sort_by(|a, b| b.when.cmp(&a.when));
                feed.truncate(20);
                ws.feed = feed;
            }
        }

        self.last_extras_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Apply initial chat history loaded in background.
    pub(super) fn apply_chat_history(
        &mut self,
        data: Vec<(String, Vec<ChatLine>, Option<String>)>,
    ) {
        for (name, history, preview) in data {
            if history.is_empty() {
                continue;
            }
            if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == name) {
                // Only apply if no real user/assistant messages yet — system-only
                // messages (e.g. "Starting daemon…") should not block history load.
                let has_user = ws
                    .chat_history
                    .iter()
                    .any(|m| matches!(m, ChatLine::User(..)));
                // Count only assistant messages from real chat sources (Tui,
                // Telegram, None). Assistant lines injected with
                // MessageSource::System (daemon broadcasts) don't count.
                let real_assistant_count = ws
                    .chat_history
                    .iter()
                    .filter(|m| {
                        matches!(
                            m,
                            ChatLine::Assistant(_, _, src)
                                if !matches!(src, Some(MessageSource::System))
                        )
                    })
                    .count();
                // The single onboarding assistant message doesn't count as real chat.
                let has_real_assistant = real_assistant_count > 0
                    && !(self.onboarding.active && real_assistant_count <= 1);
                if has_user || has_real_assistant {
                    continue;
                }

                // Preserve existing messages (system status + onboarding) to
                // re-append after history.
                let existing: Vec<ChatLine> = ws.chat_history.drain(..).collect();

                // Take only the last N history messages to keep the panel light.
                let skip = history.len().saturating_sub(CHAT_HISTORY_LIMIT);
                let trimmed: Vec<ChatLine> = history.into_iter().skip(skip).collect();

                // Inject history with session dividers.
                ws.chat_history
                    .push(ChatLine::System("─── previous session ───".into()));
                ws.chat_history.extend(trimmed);
                ws.chat_history
                    .push(ChatLine::System("─── new session ───".into()));

                // Re-append existing messages (system statuses, onboarding).
                ws.chat_history.extend(existing);

                ws.coordinator_preview = preview;
                ws.chat_scroll.scroll_to_bottom();
            }
        }
        self.needs_redraw = true;
    }

    /// Build refresh infos for background task from current workspace state.
    pub(super) fn build_refresh_infos(&self) -> Vec<WorkspaceRefreshInfo> {
        self.workspaces
            .iter()
            .map(|ws| WorkspaceRefreshInfo {
                name: ws.name.clone(),
                root: ws.config.root.clone(),
                has_github_watcher: ws.config.watchers.github.is_some(),
                has_sentry_watcher: ws.config.watchers.sentry.is_some(),
                has_swarm_watcher: ws.config.watchers.swarm.is_some(),
            })
            .collect()
    }

    // ── Helpers ───────────────────────────────────────────

    /// Get the selected worker info based on current view/selection.
    pub fn selected_worker(&self) -> Option<&WorkerInfo> {
        match &self.view {
            View::Dashboard => {
                if self.focused_panel == Panel::Workers {
                    self.current_ws()?.workers.get(self.worker_selection)
                } else {
                    None
                }
            }
            View::WorkerDetail(i) => self.current_ws()?.workers.get(*i),
            View::PrList => {
                let prs = self.workers_with_prs();
                prs.get(self.pr_list_selection).map(|(_, w)| *w)
            }
            _ => None,
        }
    }

    /// Get the selected signal based on current view/selection.
    pub fn selected_signal(&self) -> Option<&SignalRecord> {
        match &self.view {
            View::Dashboard => {
                if self.focused_panel == Panel::Signals {
                    self.current_ws()?
                        .signals
                        .iter()
                        .filter(|s| s.source != "github_review_queue")
                        .filter(|s| self.signals_debug_mode || !is_noise_signal(s))
                        .nth(self.signal_selection)
                } else if self.focused_panel == Panel::Reviews {
                    self.current_ws()?
                        .signals
                        .iter()
                        .filter(|s| s.source == "github_review_queue")
                        .nth(self.review_selection)
                } else {
                    None
                }
            }
            View::SignalDetail(i) => self.current_ws()?.signals.get(*i),
            View::SignalList => self.current_ws()?.signals.get(self.signal_list_selection),
            View::ReviewList => self
                .current_ws()?
                .signals
                .iter()
                .filter(|s| s.source.ends_with("_review_queue"))
                .nth(self.review_list_selection),
            _ => None,
        }
    }

    /// Get the URL associated with the current selection (for 'o' to open).
    pub fn selected_url(&self) -> Option<String> {
        if let Some(worker) = self.selected_worker() {
            return worker.pr.as_ref().map(|pr| pr.url.clone());
        }
        if let Some(signal) = self.selected_signal() {
            return signal.url.clone();
        }
        None
    }
}

// ── Free functions ────────────────────────────────────────

/// Return a config path that does not already exist.
///
/// Tries `<dir>/<name>.toml` first, then `<name>-2.toml`, `<name>-3.toml`, etc.
fn find_available_config_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(format!("{name}.toml"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 2..100 {
        let candidate = dir.join(format!("{name}-{n}.toml"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Extremely unlikely: just overwrite the base name.
    dir.join(format!("{name}.toml"))
}

/// Build a workspace TOML config from setup state.
fn build_setup_toml(setup: &SetupState) -> String {
    use toml_edit::{Array, DocumentMut, Item, Table, value};

    let mut doc = DocumentMut::new();

    doc["root"] = value(setup.workspace_root.display().to_string());
    doc["repos"] = Item::Value(toml_edit::Value::Array(Array::new()));

    // [coordinator] + [[coordinator.signal_hooks]]
    let mut coordinator = Table::new();
    coordinator["model"] = value("sonnet");
    coordinator["max_turns"] = value(20i64);

    let mut signal_hooks = toml_edit::ArrayOfTables::new();

    let mut hook_swarm = Table::new();
    hook_swarm["source"] = value("swarm");
    hook_swarm["prompt"] = value("Swarm activity: {events}");
    hook_swarm["action"] = value(
        "Assess the situation. If a worker opened a PR, check if Copilot has reviewed it and if so forward any comments to the worker. If a worker is stuck or failed, investigate and either send a fix or dispatch a new worker.",
    );
    hook_swarm["ttl_secs"] = value(300i64);
    signal_hooks.push(hook_swarm);

    let mut hook_ci = Table::new();
    hook_ci["source"] = value("github");
    hook_ci["prompt"] = value("CI failed: {events}");
    hook_ci["action"] = value(
        "Find the relevant swarm worker for this PR. If a worker exists, send it the CI error details so it can fix them. If no worker exists, dispatch a new one to fix the failure.",
    );
    hook_ci["ttl_secs"] = value(300i64);
    signal_hooks.push(hook_ci);

    let mut hook_review = Table::new();
    hook_review["source"] = value("github_bot_review");
    hook_review["prompt"] = value("Bot review received: {events}");
    hook_review["action"] = value(
        "Find the swarm worker whose branch matches this PR and forward the review comments directly to it so it can address them.",
    );
    hook_review["ttl_secs"] = value(300i64);
    signal_hooks.push(hook_review);

    coordinator.insert("signal_hooks", Item::ArrayOfTables(signal_hooks));
    doc["coordinator"] = Item::Table(coordinator);

    // [swarm]
    let mut swarm = Table::new();
    swarm["default_agent"] = value(setup.default_agent.clone());
    doc["swarm"] = Item::Table(swarm);

    // [telegram] (optional)
    if let Some(ref token) = setup.telegram_token {
        let mut telegram = Table::new();
        telegram["bot_token"] = value(token.clone());
        if let Some(chat_id) = setup.telegram_chat_id {
            telegram["chat_id"] = value(chat_id);
        }
        doc["telegram"] = Item::Table(telegram);
    }

    // [watchers]
    let mut watchers = Table::new();

    if setup.has_github {
        let mut github = Table::new();
        github["interval_secs"] = value(120i64);
        watchers["github"] = Item::Table(github);
    }

    let swarm_state = setup.workspace_root.join(".swarm/state.json");
    let mut swarm_watcher = Table::new();
    swarm_watcher["state_path"] = value(swarm_state.display().to_string());
    swarm_watcher["interval_secs"] = value(15i64);
    watchers["swarm"] = Item::Table(swarm_watcher);

    doc["watchers"] = Item::Table(watchers);

    doc.to_string()
}

/// Legacy fallback: load chat history from JSONL file.
fn load_history_from_jsonl(workspace: &str) -> Vec<ChatLine> {
    let history = super::history::load_history(workspace, 200);
    history
        .into_iter()
        .map(|msg| {
            let ts = msg.ts.format("%H:%M").to_string();
            let source = msg.source.as_deref().map(|s| match s {
                "telegram" => MessageSource::Telegram,
                "system" => MessageSource::System,
                _ => MessageSource::Tui,
            });
            match msg.role.as_str() {
                "user" => ChatLine::User(msg.content, ts, source),
                _ => ChatLine::Assistant(msg.content, ts, source),
            }
        })
        .collect()
}

pub fn now_ts() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

fn load_workers_from_state(state_path: &std::path::Path) -> Vec<WorkerInfo> {
    let data = match std::fs::read_to_string(state_path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let state: SwarmStateFile = match serde_json::from_str(&data) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    state
        .worktrees
        .into_iter()
        .map(|wt| {
            let pr = wt.pr.and_then(|p| {
                let url = p.url?;
                if url.is_empty() {
                    return None;
                }
                Some(PrInfo {
                    number: p.number.unwrap_or(0),
                    title: p.title.unwrap_or_default(),
                    state: p.state.unwrap_or_else(|| "OPEN".into()),
                    url,
                })
            });
            WorkerInfo {
                id: wt.id,
                branch: wt.branch,
                prompt: wt.prompt,
                agent_kind: wt.agent_kind,
                phase: wt.phase,
                agent_session_status: wt.agent_session_status,
                summary: wt.summary,
                created_at: wt.created_at,
                pr,
                last_activity: None, // filled by refresh_workers
                conversation: Vec::new(),
                conv_scroll: ScrollState::new(),
            }
        })
        .collect()
}

/// Read last activity from `.swarm/agents/<id>/events.jsonl`.
/// Falls back to None if the file doesn't exist.
fn load_last_activity(workspace_root: &Path, worktree_id: &str) -> Option<String> {
    let events_path = workspace_root
        .join(".swarm")
        .join("agents")
        .join(worktree_id)
        .join("events.jsonl");

    let file = std::fs::File::open(&events_path).ok()?;
    let metadata = file.metadata().ok()?;
    let size = metadata.len();
    if size == 0 {
        return None;
    }

    let mut reader = std::io::BufReader::new(file);

    // Seek near end to avoid reading entire file
    let skip = size.saturating_sub(4096);
    if skip > 0 {
        reader.seek(SeekFrom::Start(skip)).ok()?;
        // Skip partial line
        let mut partial = String::new();
        reader.read_line(&mut partial).ok()?;
    }

    let mut last_activity = None;
    for line in reader.lines().map_while(Result::ok) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            match val.get("type").and_then(|t| t.as_str()) {
                Some("assistant") => match val.get("subtype").and_then(|s| s.as_str()) {
                    Some("tool_use") => {
                        let tool_name = val
                            .get("tool")
                            .and_then(|t| t.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool");
                        last_activity = Some(format!("last: {tool_name}"));
                    }
                    Some("text") => {
                        last_activity = Some("responded".to_string());
                    }
                    _ => {}
                },
                Some("result") => {
                    let turns = val.get("num_turns").and_then(|t| t.as_u64()).unwrap_or(0);
                    last_activity = Some(format!("done ({turns} turns)"));
                }
                _ => {}
            }
        }
    }

    last_activity
}

/// Truncate a string for preview display (collapse whitespace, limit chars).
fn truncate_preview(s: &str, max_chars: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = collapsed.chars().count();
    if char_count > max_chars {
        let truncated: String = collapsed
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect();
        format!("{truncated}...")
    } else {
        collapsed
    }
}

// ── Blocking I/O for background tasks ─────────────────────

/// Load workers for all workspaces (blocking filesystem reads).
pub(super) fn load_all_workers_blocking(
    infos: &[WorkspaceRefreshInfo],
) -> Vec<(String, Vec<WorkerInfo>)> {
    infos
        .iter()
        .map(|info| {
            let state_path = info.root.join(".swarm/state.json");
            let mut workers = load_workers_from_state(&state_path);
            for worker in &mut workers {
                worker.last_activity = load_last_activity(&info.root, &worker.id);
            }
            (info.name.clone(), workers)
        })
        .collect()
}

/// Load signals for all workspaces (blocking SQLite queries).
pub(super) fn load_all_signals_blocking(
    db_path: &Path,
    names: &[String],
) -> Vec<(String, Vec<SignalRecord>)> {
    names
        .iter()
        .map(|name| {
            let signals = if let Ok(store) = SignalStore::open(db_path, name) {
                store.get_open_signals().unwrap_or_default()
            } else {
                Vec::new()
            };
            (name.clone(), signals)
        })
        .collect()
}

/// Load extras (sparkline, thoughts, watcher health, feed) for all workspaces.
/// Returns (daemon_alive, daemon_uptime_secs, per-workspace extras).
pub(super) fn load_all_extras_blocking(
    db_path: &Path,
    pid_path: &Path,
    infos: &[WorkspaceRefreshInfo],
) -> (bool, Option<u64>, Vec<(String, WorkspaceExtrasData)>) {
    let daemon_alive = crate::daemon::is_daemon_running();
    let daemon_uptime_secs = if daemon_alive {
        std::fs::metadata(pid_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs())
    } else {
        None
    };

    let per_workspace = infos
        .iter()
        .map(|info| {
            let mut data = WorkspaceExtrasData {
                sparkline_data: vec![0; 24],
                watcher_health: Vec::new(),
                thoughts: Vec::new(),
                feed_items: Vec::new(),
            };

            if let Ok(store) = SignalStore::open(db_path, &info.name) {
                // Sparkline
                data.sparkline_data = store
                    .count_signals_by_hour()
                    .unwrap_or_else(|_| vec![0; 24]);

                // Thoughts from MemoryStore
                let mem = MemoryStore::new(store.conn(), &info.name);
                data.thoughts = mem
                    .get_recent(20)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| (e.category.as_str().to_string(), e.content))
                    .collect();

                // Watcher health
                let now = Utc::now();
                let cursor_map: std::collections::HashMap<String, String> = store
                    .get_watcher_cursors()
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                let configured: &[(&str, bool)] = &[
                    ("github", info.has_github_watcher),
                    ("sentry", info.has_sentry_watcher),
                    ("swarm", info.has_swarm_watcher),
                ];
                for &(name, enabled) in configured {
                    if !enabled {
                        continue;
                    }
                    if let Some(updated_at) = cursor_map.get(name) {
                        let dt = chrono::DateTime::parse_from_rfc3339(updated_at)
                            .map(|d| d.with_timezone(&Utc))
                            .unwrap_or(now);
                        let age = now.signed_duration_since(dt);
                        data.watcher_health.push(WatcherHealth {
                            name: name.to_string(),
                            healthy: age.num_minutes() < 5,
                            last_check_secs: age.num_seconds(),
                        });
                    } else {
                        data.watcher_health.push(WatcherHealth {
                            name: name.to_string(),
                            healthy: false,
                            last_check_secs: -1,
                        });
                    }
                }

                // Feed: signal items
                if let Ok(recent) = store.get_recent_signals(30) {
                    let mut seen_ids = std::collections::HashSet::new();
                    let mut seen_titles = std::collections::HashSet::new();
                    for sig in recent {
                        if !seen_ids.insert(sig.external_id.clone()) {
                            continue;
                        }
                        let title_key: String = sig.title.chars().take(50).collect();
                        if !seen_titles.insert(title_key) {
                            continue;
                        }
                        data.feed_items.push(FeedItem {
                            when: sig.updated_at,
                            kind: FeedKind::Signal,
                            text: sig.title.clone(),
                        });
                    }
                }

                // Feed: watcher heartbeats
                if let Ok(cursors) = store.get_watcher_cursors() {
                    for (watcher, updated_at_str) in &cursors {
                        if !is_top_level_watcher(watcher) {
                            continue;
                        }
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(updated_at_str) {
                            let dt_utc = dt.with_timezone(&Utc);
                            data.feed_items.push(FeedItem {
                                when: dt_utc,
                                kind: FeedKind::Heartbeat,
                                text: format!("{watcher} checked"),
                            });
                        }
                    }
                }
            }

            (info.name.clone(), data)
        })
        .collect();

    (daemon_alive, daemon_uptime_secs, per_workspace)
}

/// Load chat history for all workspaces (blocking SQLite/JSONL reads).
pub(super) fn load_chat_history_blocking(
    db_path: &Path,
    workspace_names: &[String],
) -> Vec<(String, Vec<ChatLine>, Option<String>)> {
    workspace_names
        .iter()
        .map(|name| {
            let chat_history: Vec<ChatLine> = if let Ok(store) = SignalStore::open(db_path, name) {
                let conv = ConversationStore::new(store.conn(), name);
                match conv.load_history(CHAT_HISTORY_LIMIT) {
                    Ok(rows) if !rows.is_empty() => rows
                        .into_iter()
                        .map(|row| {
                            let ts = chrono::DateTime::parse_from_rfc3339(&row.created_at)
                                .map(|dt| dt.format("%H:%M").to_string())
                                .unwrap_or_default();
                            let source = row.source.as_deref().map(|s| match s {
                                "telegram" => MessageSource::Telegram,
                                "system" => MessageSource::System,
                                _ => MessageSource::Tui,
                            });
                            match row.role.as_str() {
                                "user" => ChatLine::User(row.content, ts, source),
                                _ => ChatLine::Assistant(row.content, ts, source),
                            }
                        })
                        .collect(),
                    _ => load_history_from_jsonl(name),
                }
            } else {
                load_history_from_jsonl(name)
            };

            let coordinator_preview = chat_history.iter().rev().find_map(|msg| {
                if let ChatLine::Assistant(s, _, _) = msg {
                    Some(truncate_preview(s, 120))
                } else {
                    None
                }
            });

            (name.clone(), chat_history, coordinator_preview)
        })
        .collect()
}

/// Load worker conversation entries from events.jsonl (blocking).
pub(super) fn load_worker_conversation_blocking(
    root: &Path,
    worker_id: &str,
) -> Vec<ConversationEntry> {
    let events_path = root
        .join(".swarm")
        .join("agents")
        .join(worker_id)
        .join("events.jsonl");
    apiari_tui::events_parser::parse_events(&events_path)
}

/// Extract (repo, pr_number) from a review queue signal.
/// Prefers metadata fields; falls back to parsing `external_id` (`rq-{repo}-{number}`).
/// Returns `None` for non-GitHub review signals (e.g. Linear).
pub fn review_signal_target(signal: &SignalRecord) -> Option<(String, u64)> {
    if signal.source != "github_review_queue" {
        return None;
    }

    // Try metadata first
    if let Some(ref meta) = signal.metadata
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(meta)
        && let (Some(repo), Some(pr)) = (
            val.get("repo").and_then(|v| v.as_str()),
            val.get("pr_number").and_then(|v| v.as_u64()),
        )
    {
        return Some((repo.to_string(), pr));
    }

    // Fallback: parse from external_id (format: rq-{owner/repo}-{number})
    let rest = signal.external_id.strip_prefix("rq-")?;
    let last_dash = rest.rfind('-')?;
    let repo = &rest[..last_dash];
    let number: u64 = rest[last_dash + 1..].parse().ok()?;
    Some((repo.to_string(), number))
}

/// Non-actionable signal sources that are hidden by default (shown in debug mode).
const NOISE_SOURCES: &[&str] = &["github_merged_pr", "github_ci_pass"];

/// Returns true if a signal is non-actionable noise (merged PR or CI pass).
pub fn is_noise_signal(signal: &SignalRecord) -> bool {
    NOISE_SOURCES.contains(&signal.source.as_str())
}

/// Severity icon for display.
pub fn severity_icon(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "!!",
        Severity::Error => "!",
        Severity::Warning => "~",
        Severity::Info => "-",
    }
}

/// Phase display string for workers.
pub fn phase_display(worker: &WorkerInfo) -> &str {
    match worker.phase.as_deref() {
        Some("running") => {
            if worker.agent_session_status.as_deref() == Some("waiting") {
                "waiting"
            } else {
                "running"
            }
        }
        Some(p) => p,
        None => "unknown",
    }
}

/// Time elapsed display.
pub fn elapsed_display(created_at: &Option<DateTime<Local>>) -> String {
    match created_at {
        Some(dt) => {
            let elapsed = chrono::Local::now().signed_duration_since(dt);
            if elapsed.num_hours() > 0 {
                format!("{}h{}m", elapsed.num_hours(), elapsed.num_minutes() % 60)
            } else if elapsed.num_minutes() > 0 {
                format!("{}m", elapsed.num_minutes())
            } else {
                "now".to_string()
            }
        }
        None => "?".to_string(),
    }
}

/// Returns true if the watcher name is a known top-level watcher (not a per-repo sub-cursor).
fn is_top_level_watcher(name: &str) -> bool {
    matches!(
        name,
        "github" | "swarm" | "sentry" | "review_queue" | "linear" | "notion" | "email"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::{Severity, SignalRecord, SignalStatus};
    use chrono::Utc;

    fn make_signal(source: &str, external_id: &str, metadata: Option<&str>) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.to_string(),
            external_id: external_id.to_string(),
            title: "Test PR".to_string(),
            body: None,
            severity: Severity::Info,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: metadata.map(String::from),
            snoozed_until: None,
        }
    }

    #[test]
    fn test_review_signal_target_from_metadata() {
        let signal = make_signal(
            "github_review_queue",
            "rq-ApiariTools/apiari-42",
            Some(r#"{"repo":"ApiariTools/apiari","pr_number":42,"author":"user1"}"#),
        );
        let result = review_signal_target(&signal);
        assert_eq!(result, Some(("ApiariTools/apiari".to_string(), 42)));
    }

    #[test]
    fn test_review_signal_target_fallback_from_external_id() {
        let signal = make_signal(
            "github_review_queue",
            "rq-ApiariTools/apiari-42",
            Some(r#"{"repo":"ApiariTools/apiari","author":"user1"}"#), // no pr_number
        );
        let result = review_signal_target(&signal);
        assert_eq!(result, Some(("ApiariTools/apiari".to_string(), 42)));
    }

    #[test]
    fn test_review_signal_target_no_metadata() {
        let signal = make_signal("github_review_queue", "rq-org/repo-99", None);
        let result = review_signal_target(&signal);
        assert_eq!(result, Some(("org/repo".to_string(), 99)));
    }

    #[test]
    fn test_review_signal_target_linear_returns_none() {
        let signal = make_signal(
            "linear_review_queue",
            "linear-123",
            Some(r#"{"repo":"org/repo","pr_number":123}"#),
        );
        let result = review_signal_target(&signal);
        assert_eq!(result, None);
    }

    #[test]
    fn test_review_signal_target_non_review_source_returns_none() {
        let signal = make_signal("sentry", "sentry-42", None);
        let result = review_signal_target(&signal);
        assert_eq!(result, None);
    }

    // ── is_top_level_watcher tests ────────────────────────────

    #[test]
    fn test_top_level_watchers_are_included() {
        for name in [
            "github",
            "swarm",
            "sentry",
            "review_queue",
            "linear",
            "notion",
            "email",
        ] {
            assert!(is_top_level_watcher(name), "{name} should be top-level");
        }
    }

    #[test]
    fn test_sub_cursors_are_excluded() {
        for name in [
            "github_ci_pass:ApiariTools/web",
            "github_merged_pr:ApiariTools/apiari",
            "github_release:ApiariTools/apiari",
            "github_ci_pass:ApiariTools/swarm",
        ] {
            assert!(!is_top_level_watcher(name), "{name} should be excluded");
        }
    }

    #[test]
    fn test_unknown_watcher_is_excluded() {
        assert!(!is_top_level_watcher("some_future_thing"));
    }

    // ── apply_chat_history tests ─────────────────────────────

    fn make_test_workspace(name: &str) -> config::Workspace {
        let config: config::WorkspaceConfig =
            toml::from_str(&format!("root = '/tmp/{name}'")).unwrap();
        config::Workspace {
            name: name.to_string(),
            config,
        }
    }

    fn make_app(names: &[&str], needs_onboarding: bool) -> App {
        let workspaces: Vec<config::Workspace> =
            names.iter().map(|n| make_test_workspace(n)).collect();
        App::new(workspaces, None, needs_onboarding)
    }

    #[test]
    fn test_apply_chat_history_populates_empty_workspace() {
        let mut app = make_app(&["ws1"], false);
        assert!(app.workspaces[0].chat_history.is_empty());

        let history = vec![
            ChatLine::User("hello".into(), "12:00".into(), None),
            ChatLine::Assistant("hi".into(), "12:01".into(), None),
        ];
        app.apply_chat_history(vec![("ws1".into(), history, Some("hi".into()))]);

        // 2 history + 2 dividers = 4
        assert_eq!(app.workspaces[0].chat_history.len(), 4);
        assert!(matches!(
            &app.workspaces[0].chat_history[0],
            ChatLine::System(s) if s.contains("previous session")
        ));
        assert!(matches!(
            &app.workspaces[0].chat_history[3],
            ChatLine::System(s) if s.contains("new session")
        ));
        assert_eq!(app.workspaces[0].coordinator_preview, Some("hi".into()));
    }

    #[test]
    fn test_apply_chat_history_preserves_onboarding_message() {
        let mut app = make_app(&["ws1"], true);
        // Onboarding injects one message
        assert_eq!(app.workspaces[0].chat_history.len(), 1);

        let history = vec![ChatLine::User("old msg".into(), "11:00".into(), None)];
        app.apply_chat_history(vec![("ws1".into(), history, None)]);

        // 1 history + 2 dividers + 1 onboarding = 4
        assert_eq!(app.workspaces[0].chat_history.len(), 4);
        // Last message should be the onboarding assistant message (re-appended)
        assert!(matches!(
            &app.workspaces[0].chat_history[3],
            ChatLine::Assistant(_, _, _)
        ));
    }

    #[test]
    fn test_apply_chat_history_does_not_overwrite_user_chat() {
        let mut app = make_app(&["ws1"], false);
        // Simulate user already typing messages before background load completes
        app.workspaces[0].chat_history.push(ChatLine::User(
            "user typed this".into(),
            "12:00".into(),
            None,
        ));
        app.workspaces[0].chat_history.push(ChatLine::Assistant(
            "bot replied".into(),
            "12:01".into(),
            None,
        ));

        let history = vec![ChatLine::User("old history".into(), "10:00".into(), None)];
        app.apply_chat_history(vec![("ws1".into(), history, None)]);

        // Should NOT overwrite — still has the 2 user messages
        assert_eq!(app.workspaces[0].chat_history.len(), 2);
        if let ChatLine::User(content, _, _) = &app.workspaces[0].chat_history[0] {
            assert_eq!(content, "user typed this");
        } else {
            panic!("expected user message");
        }
    }

    #[test]
    fn test_apply_chat_history_inactive_onboarding_empty_workspace() {
        let mut app = make_app(&["ws1"], false);
        assert!(!app.onboarding.active);
        assert!(app.workspaces[0].chat_history.is_empty());

        let history = vec![ChatLine::Assistant(
            "welcome back".into(),
            "09:00".into(),
            None,
        )];
        app.apply_chat_history(vec![("ws1".into(), history, Some("welcome back".into()))]);

        // 1 history + 2 dividers = 3
        assert_eq!(app.workspaces[0].chat_history.len(), 3);
        assert_eq!(
            app.workspaces[0].coordinator_preview,
            Some("welcome back".into())
        );
    }

    #[test]
    fn test_apply_chat_history_loads_when_only_system_messages_present() {
        let mut app = make_app(&["ws1"], false);
        // Simulate system status messages that arrive before history loads
        app.workspaces[0]
            .chat_history
            .push(ChatLine::System("Starting daemon…".into()));
        app.workspaces[0]
            .chat_history
            .push(ChatLine::System("Connected to daemon ✓".into()));

        let history = vec![
            ChatLine::User("old question".into(), "10:00".into(), None),
            ChatLine::Assistant("old answer".into(), "10:01".into(), None),
        ];
        app.apply_chat_history(vec![("ws1".into(), history, None)]);

        // History should load: 2 dividers + 2 history + 2 re-appended system msgs = 6
        assert_eq!(app.workspaces[0].chat_history.len(), 6);
        assert!(matches!(
            &app.workspaces[0].chat_history[0],
            ChatLine::System(s) if s.contains("previous session")
        ));
        // The original system messages should be re-appended after the new session divider
        assert!(matches!(
            &app.workspaces[0].chat_history[4],
            ChatLine::System(s) if s.contains("Starting daemon")
        ));
    }

    #[test]
    fn test_apply_chat_history_truncates_to_limit() {
        let mut app = make_app(&["ws1"], false);
        // Build 30 history items — should be trimmed to CHAT_HISTORY_LIMIT (20).
        let history: Vec<ChatLine> = (0..30)
            .map(|i| ChatLine::User(format!("msg {i}"), "10:00".into(), None))
            .collect();
        app.apply_chat_history(vec![("ws1".into(), history, None)]);

        // 20 kept + 2 dividers = 22
        assert_eq!(app.workspaces[0].chat_history.len(), CHAT_HISTORY_LIMIT + 2);
        // First real message should be msg 10 (items 0–9 were trimmed).
        if let ChatLine::User(content, _, _) = &app.workspaces[0].chat_history[1] {
            assert_eq!(content, "msg 10");
        } else {
            panic!("expected user message at index 1");
        }
    }

    #[test]
    fn test_apply_chat_history_ignores_system_source_assistant_lines() {
        let mut app = make_app(&["ws1"], false);
        // Simulate a daemon broadcast assistant message with MessageSource::System.
        app.workspaces[0].chat_history.push(ChatLine::Assistant(
            "daemon broadcast".into(),
            "12:00".into(),
            Some(MessageSource::System),
        ));

        let history = vec![ChatLine::User("old msg".into(), "10:00".into(), None)];
        app.apply_chat_history(vec![("ws1".into(), history, None)]);

        // History should load despite the System-source assistant line.
        // 2 dividers + 1 history + 1 re-appended System assistant = 4
        assert_eq!(app.workspaces[0].chat_history.len(), 4);
        assert!(matches!(
            &app.workspaces[0].chat_history[0],
            ChatLine::System(s) if s.contains("previous session")
        ));
    }

    // ── Setup state machine tests ────────────────────────────

    /// Drive the setup state machine and return the finished SetupState + generated TOML.
    fn run_setup(inputs: &[&str]) -> (App, String) {
        let mut app = App::new_setup();
        for input in inputs {
            let done = app.process_setup_input(input);
            if done {
                let setup = app.setup.as_ref().expect("setup should still be present");
                let toml_str = build_setup_toml(setup);
                return (app, toml_str);
            }
        }
        panic!("setup did not complete after all inputs");
    }

    fn assert_valid_config(toml_str: &str) -> crate::config::WorkspaceConfig {
        toml::from_str(toml_str).unwrap_or_else(|e| panic!("invalid TOML config: {e}\n{toml_str}"))
    }

    #[test]
    fn test_setup_claude_no_github_no_telegram() {
        let (_, toml_str) = run_setup(&[
            "",       // accept default root
            "claude", // provider
            "no",     // github
            "skip",   // telegram
        ]);
        let cfg = assert_valid_config(&toml_str);
        assert_eq!(cfg.swarm.default_agent, "claude");
        assert!(cfg.telegram.is_none());
        assert!(cfg.watchers.github.is_none());
    }

    #[test]
    fn test_setup_codex_with_github_no_telegram() {
        let (_, toml_str) = run_setup(&[
            "/tmp/myproject", // custom root
            "codex",          // provider
            "yes",            // github
            "skip",           // telegram
        ]);
        let cfg = assert_valid_config(&toml_str);
        assert_eq!(cfg.root, std::path::PathBuf::from("/tmp/myproject"));
        assert_eq!(cfg.swarm.default_agent, "codex");
        assert!(cfg.watchers.github.is_some());
        assert!(cfg.telegram.is_none());
    }

    #[test]
    fn test_setup_auto_with_telegram() {
        let (_, toml_str) = run_setup(&[
            "",                // default root
            "both",            // auto
            "no",              // github
            "123456:AABBccDD", // telegram token
            "-1001234567890",  // chat id
        ]);
        let cfg = assert_valid_config(&toml_str);
        assert_eq!(cfg.swarm.default_agent, "auto");
        let tg = cfg.telegram.expect("telegram should be set");
        assert_eq!(tg.bot_token, "123456:AABBccDD");
        assert_eq!(tg.chat_id, -1001234567890);
    }

    #[test]
    fn test_setup_invalid_chat_id_retries() {
        let mut app = App::new_setup();
        // AskRoot -> AskProvider
        app.process_setup_input("");
        // AskProvider -> AskGithub
        app.process_setup_input("claude");
        // AskGithub -> AskTelegram
        app.process_setup_input("no");
        // AskTelegram -> AskTelegramChatId
        app.process_setup_input("sometoken");
        // Invalid chat ID — should NOT complete
        let done = app.process_setup_input("not-a-number");
        assert!(!done);
        assert_eq!(
            app.setup.as_ref().unwrap().step,
            SetupStep::AskTelegramChatId
        );
        // Valid chat ID — should complete
        let done = app.process_setup_input("12345");
        assert!(done);
    }

    #[test]
    fn test_setup_toml_special_chars_in_root() {
        // Paths with quotes/backslashes should be safely encoded
        let mut app = App::new_setup();
        app.process_setup_input("/tmp/my \"project\"");
        app.process_setup_input("claude");
        app.process_setup_input("no");
        let done = app.process_setup_input("skip");
        assert!(done);
        let toml_str = build_setup_toml(app.setup.as_ref().unwrap());
        // Must parse without error — toml_edit handles escaping
        assert_valid_config(&toml_str);
    }

    #[test]
    fn test_find_available_config_path_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let path = find_available_config_path(dir.path(), "myws");
        assert_eq!(path, dir.path().join("myws.toml"));
    }

    #[test]
    fn test_find_available_config_path_collision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myws.toml"), "root = '/'").unwrap();
        let path = find_available_config_path(dir.path(), "myws");
        assert_eq!(path, dir.path().join("myws-2.toml"));
    }

    #[test]
    fn test_find_available_config_path_multiple_collisions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myws.toml"), "root = '/'").unwrap();
        std::fs::write(dir.path().join("myws-2.toml"), "root = '/'").unwrap();
        let path = find_available_config_path(dir.path(), "myws");
        assert_eq!(path, dir.path().join("myws-3.toml"));
    }
}

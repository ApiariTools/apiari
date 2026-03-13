//! App state machine for the apiari TUI.

use apiari_tui::conversation::ConversationEntry;
use buzz::coordinator::memory::MemoryStore;
use buzz::signal::store::SignalStore;
use buzz::signal::{Severity, SignalRecord};
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use apiari_tui::scroll::ScrollState;

use crate::config::{self, Workspace};

// ── Constants ─────────────────────────────────────────────

pub const MAX_VISIBLE_SIGNALS: usize = 5;

// ── Types ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Dashboard,
    WorkerDetail(usize),
    SignalDetail(usize),
    SignalList,
    PrList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Home,
    Workers,
    Signals,
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
    User(String, String, Option<MessageSource>),      // content, timestamp, source
    Assistant(String, String, Option<MessageSource>), // content, timestamp, source
    System(String),                                    // system message
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    CloseWorker(String), // worktree id
    ResolveSignal(i64),  // signal db id
}

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
    pub healthy: bool,       // updated_at within 5 min
    pub last_check_secs: i64, // seconds since last check
}

// ── Worker info from state.json ───────────────────────────

#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub id: String,
    pub branch: String,
    pub prompt: String,
    pub agent_kind: String,
    pub phase: Option<String>,
    pub agent_session_status: Option<String>,
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

// ── Per-workspace state ───────────────────────────────────

pub struct WorkspaceState {
    pub name: String,
    pub config: config::WorkspaceConfig,
    pub signals: Vec<SignalRecord>,
    pub workers: Vec<WorkerInfo>,
    pub chat_history: Vec<ChatLine>,
    pub input: String,
    pub chat_scroll: ScrollState,
    pub streaming: bool,
    pub coordinator_preview: Option<String>,
    pub has_unread_response: bool,
    // State tracking for proactive notifications
    prev_worker_phases: std::collections::HashMap<String, String>,
    prev_signal_ids: std::collections::HashSet<i64>,
    prev_pr_workers: std::collections::HashSet<String>,
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
    pub feed_selection: usize,
    pub chat_focused: bool,
    // Worker detail
    pub worker_input: String,
    pub worker_input_active: bool,
    // Detail/list views
    pub content_scroll: u16,
    pub signal_list_selection: usize,
    pub pr_list_selection: usize,
    // Daemon / extras
    pub daemon_alive: bool,
    pub daemon_connected: bool, // true if TUI is connected to daemon via socket
    pub daemon_uptime_secs: Option<u64>,
    pub last_extras_refresh: Instant,
    // Terminal size (updated each frame)
    pub terminal_width: u16,
    // Activity graph (network-style throughput chart in status bar)
    pub activity_buf: Vec<u8>, // fixed array, each value = bar height 0-7
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
    pub fn new(workspaces: Vec<Workspace>, focus_workspace: Option<&str>) -> Self {
        let ws_states: Vec<WorkspaceState> = workspaces
            .into_iter()
            .map(|ws| {
                // Load chat history
                let history = super::history::load_history(&ws.name, 200);
                let chat_history: Vec<ChatLine> = history
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
                    .collect();

                // Extract coordinator preview from last assistant message
                let coordinator_preview = chat_history.iter().rev().find_map(|msg| {
                    if let ChatLine::Assistant(s, _, _) = msg {
                        Some(truncate_preview(s, 120))
                    } else {
                        None
                    }
                });

                WorkspaceState {
                    name: ws.name,
                    config: ws.config,
                    signals: Vec::new(),
                    workers: Vec::new(),
                    chat_history,
                    input: String::new(),
                    chat_scroll: ScrollState::new(),
                    streaming: false,
                    coordinator_preview,
                    has_unread_response: false,
                    sparkline_data: vec![0; 24],
                    watcher_health: Vec::new(),
                    feed: Vec::new(),
                    prev_worker_phases: std::collections::HashMap::new(),
                    prev_signal_ids: std::collections::HashSet::new(),
                    prev_pr_workers: std::collections::HashSet::new(),
                    feed_scroll: ScrollState::new(),
                    thoughts: Vec::new(),
                }
            })
            .collect();

        let active_tab = if let Some(name) = focus_workspace {
            ws_states.iter().position(|ws| ws.name == name).unwrap_or(0)
        } else {
            0
        };

        let mut app = Self {
            workspaces: ws_states,
            active_tab,
            prefix_active: false,
            view: View::Dashboard,
            mode: Mode::Normal,
            focused_panel: Panel::Workers,
            zoomed_panel: None,
            worker_selection: 0,
            signal_selection: 0,
            feed_selection: 0,
            chat_focused: false,
            worker_input: String::new(),
            worker_input_active: false,
            content_scroll: 0,
            signal_list_selection: 0,
            pr_list_selection: 0,
            daemon_alive: false,
            daemon_connected: false,
            daemon_uptime_secs: None,
            last_extras_refresh: Instant::now(),
            terminal_width: 80,
            activity_buf: vec![0; 18],
            pending_action: None,
            flash: None,
            needs_redraw: true,
            spinner_tick: 0,
            last_worker_refresh: Instant::now(),
            last_signal_refresh: Instant::now(),
        };

        app.refresh_workers();
        app.refresh_signals();
        app.refresh_extras();
        app
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
            Panel::Workers => Panel::Signals,
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
            Panel::Signals => Panel::Workers,
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
        self.current_ws().map_or(0, |ws| ws.signals.len())
    }

    /// Clamp selections after data refresh.
    pub fn clamp_selections(&mut self) {
        let (worker_count, sig_count, feed_count) =
            if let Some(ws) = self.current_ws() {
                (ws.workers.len(), ws.signals.len(), ws.feed.len())
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

    pub fn back_to_dashboard(&mut self) {
        match self.view {
            View::WorkerDetail(_) | View::PrList => self.focused_panel = Panel::Workers,
            View::SignalDetail(_) | View::SignalList => self.focused_panel = Panel::Signals,
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
                .join(".swarm/agents")
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
                if let Some(ws) = self.current_ws()
                    && self.signal_selection < ws.signals.len()
                {
                    self.enter_signal_detail(self.signal_selection);
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
            ws.input.push(c);
            self.needs_redraw = true;
        }
    }

    pub fn backspace(&mut self) {
        if let Some(ws) = self.current_ws_mut() {
            ws.input.pop();
            self.needs_redraw = true;
        }
    }

    pub fn take_input(&mut self) -> String {
        match self.current_ws_mut() {
            Some(ws) => std::mem::take(&mut ws.input),
            None => String::new(),
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
    pub fn push_activity(
        &mut self,
        workspace: &str,
        source: &str,
        kind: &str,
        text: &str,
    ) {
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

    pub fn refresh_workers(&mut self) {
        for ws in &mut self.workspaces {
            let state_path = ws.config.root.join(".swarm/state.json");
            ws.workers = load_workers_from_state(&state_path);
            // Enrich with last activity from events
            for worker in &mut ws.workers {
                worker.last_activity = load_last_activity(&ws.config.root, &worker.id);
            }

            // Detect state changes and inject chat notifications
            let is_first_load = ws.prev_worker_phases.is_empty() && !ws.workers.is_empty();
            for worker in &ws.workers {
                let phase = phase_display(worker).to_string();
                let prev = ws.prev_worker_phases.get(&worker.id);

                // Skip first load — don't spam on startup
                if !is_first_load {
                    if let Some(prev_phase) = prev {
                        if *prev_phase != phase {
                            // Phase changed — announce it
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
                        ws.chat_history.push(ChatLine::System(format!(
                            "\u{25cf} {} spawned",
                            worker.id
                        )));
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
            ws.prev_worker_phases = ws
                .workers
                .iter()
                .map(|w| (w.id.clone(), phase_display(w).to_string()))
                .collect();
            ws.prev_pr_workers = ws
                .workers
                .iter()
                .filter(|w| w.pr.is_some())
                .map(|w| w.id.clone())
                .collect();
        }
        self.last_worker_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

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

    /// Refresh sparkline, thoughts, daemon health, and activity feed.
    pub fn refresh_extras(&mut self) {
        let db = config::db_path();
        let pid_path = config::pid_path();

        // Daemon health
        self.daemon_alive = crate::daemon::is_daemon_running();
        self.daemon_uptime_secs = if self.daemon_alive {
            std::fs::metadata(&pid_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
                .map(|d| d.as_secs())
        } else {
            None
        };

        for ws in &mut self.workspaces {
            if let Ok(store) = SignalStore::open(&db, &ws.name) {
                // Sparkline
                ws.sparkline_data = store.count_signals_by_hour().unwrap_or_else(|_| vec![0; 24]);

                // Thoughts from MemoryStore
                let mem = MemoryStore::new(store.conn(), &ws.name);
                ws.thoughts = mem
                    .get_recent(20)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| (e.category.as_str().to_string(), e.content))
                    .collect();

                // Watcher health — merge config (what's configured) with cursors (runtime)
                let now = Utc::now();
                let cursor_map: std::collections::HashMap<String, String> = store
                    .get_watcher_cursors()
                    .unwrap_or_default()
                    .into_iter()
                    .collect();

                // Build list from configured watchers
                let mut watchers: Vec<WatcherHealth> = Vec::new();
                let wc = &ws.config.watchers;
                let configured: &[(&str, bool)] = &[
                    ("github", wc.github.is_some()),
                    ("sentry", wc.sentry.is_some()),
                    ("swarm", wc.swarm.is_some()),
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
                        watchers.push(WatcherHealth {
                            name: name.to_string(),
                            healthy: age.num_minutes() < 5,
                            last_check_secs: age.num_seconds(),
                        });
                    } else {
                        // Configured but never ran
                        watchers.push(WatcherHealth {
                            name: name.to_string(),
                            healthy: false,
                            last_check_secs: -1, // sentinel: never checked
                        });
                    }
                }
                ws.watcher_health = watchers;

                // Build feed: proactive AI activity only (signals + workers, not chat)
                let mut feed: Vec<FeedItem> = Vec::new();
                let now_utc = Utc::now();

                // Recent signals — deduplicated by title prefix (collapse similar errors)
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
                        feed.push(FeedItem {
                            when: sig.updated_at,
                            kind: FeedKind::Signal,
                            text: sig.title.clone(),
                        });
                    }
                }

                // Worker lifecycle events
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

                // Watcher heartbeats — show when each watcher last checked in
                if let Ok(cursors) = store.get_watcher_cursors() {
                    for (watcher, updated_at_str) in &cursors {
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(updated_at_str) {
                            let dt_utc = dt.with_timezone(&Utc);
                            feed.push(FeedItem {
                                when: dt_utc,
                                kind: FeedKind::Heartbeat,
                                text: format!("{watcher} checked"),
                            });
                        }
                    }
                }

                // If daemon is alive but no signals and no workers doing anything, say so
                if self.daemon_alive
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

                // Sort by time descending (most recent first)
                feed.sort_by(|a, b| b.when.cmp(&a.when));
                feed.truncate(20);
                ws.feed = feed;
            }
        }

        self.last_extras_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Check if periodic refreshes are due.
    pub fn maybe_refresh(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_worker_refresh).as_secs() >= 2 {
            self.refresh_workers();
            // Also refresh conversation when viewing a worker detail
            if let View::WorkerDetail(idx) = self.view {
                self.refresh_worker_conversation(idx);
            }
        }
        if now.duration_since(self.last_signal_refresh).as_secs() >= 5 {
            self.refresh_signals();
        }
        if now.duration_since(self.last_extras_refresh).as_secs() >= 10 {
            self.refresh_extras();
        }
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
                    let visible = self.current_ws()?.signals.len().min(MAX_VISIBLE_SIGNALS);
                    if self.signal_selection < visible {
                        self.current_ws()?.signals.get(self.signal_selection)
                    } else {
                        None // "more" item
                    }
                } else {
                    None
                }
            }
            View::SignalDetail(i) => self.current_ws()?.signals.get(*i),
            View::SignalList => self.current_ws()?.signals.get(self.signal_list_selection),
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
        let truncated: String = collapsed.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{truncated}...")
    } else {
        collapsed
    }
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

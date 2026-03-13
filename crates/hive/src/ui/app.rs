//! Application state for the unified hive TUI.

use crate::keeper::discovery::{self, SwarmSession, WorktreeInfo};
use crate::workspace::RegistryEntry;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::history;

/// Pipeline stage for a single repo in dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum RepoStage {
    Pending,
    Context,
    Planning,
    Done,
}

/// Per-repo pipeline artifacts for dispatch review.
#[derive(Debug, Clone)]
pub struct RepoDispatchArtifacts {
    pub repo: String,
    pub stage: RepoStage,
    pub context_md: Option<String>,
    pub plan_md: Option<String>,
    pub file_count: Option<usize>,
    pub step_count: Option<usize>,
}

/// Pending confirmation action.
#[derive(Debug, Clone)]
pub enum PendingAction {
    CloseWorker(String), // worktree_id
}

/// Status of an active dispatch workflow.
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchStatus {
    Refining,
    Running,
    Ready,
}

/// A collapsible section in the dispatch review.
#[derive(Debug, Clone)]
pub struct ReviewSection {
    pub collapsed: bool,
}

/// An active dispatch being reviewed in the sidebar/right panel.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ActiveDispatch {
    pub task: String,
    pub repos: Vec<String>,
    pub status: DispatchStatus,
    pub task_md: String,
    pub title: String,
    pub per_repo: Vec<RepoDispatchArtifacts>,
    /// Scroll offset (lines from top). Auto-set on section nav, manual via arrows.
    pub scroll: u16,
    /// Collapsible sections: index 0 = Task, 1..=N = per_repo[i-1].
    pub sections: Vec<ReviewSection>,
    /// Which section has the cursor for `[/]` navigation.
    pub focused_section: usize,
}

/// A transient flash message shown in the status bar.
#[derive(Debug, Clone)]
pub struct FlashMessage {
    pub text: String,
    pub expires: Instant,
}

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Workers,
    Chat,
}

/// What is selected in the left sidebar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SidebarItem {
    Chat,
    Dispatch,
    Worker(usize), // index into flat_workers()
}

/// Which step of the dispatch flow we're in.
#[derive(Debug, Clone)]
pub enum DispatchPhase {
    /// Selecting repos (checkboxes).
    SelectRepos,
    /// Typing the task prompt.
    EnterPrompt,
}

/// State for the dispatch overlay.
#[derive(Debug, Clone)]
pub struct DispatchState {
    pub phase: DispatchPhase,
    pub repos: Vec<String>,
    pub selected: Vec<bool>,
    pub cursor: usize,
    pub prompt: String,
    pub prompt_cursor: usize,
}

/// PR detail info for the overlay.
#[derive(Debug, Clone)]
pub struct PrDetailInfo {
    pub number: String,
    pub title: String,
    pub state: String,
    pub url: String,
}

/// A line in the chat history.
#[derive(Clone, Debug)]
pub enum ChatLine {
    User(String, String),      // (text, timestamp)
    Assistant(String, String), // (text, timestamp)
    System(String),
}

/// Top-level application state.
pub struct App {
    // Tab bar
    pub workspaces: Vec<RegistryEntry>,
    pub active_tab: usize,
    pub prefix_active: bool,

    // Left panel — sidebar (Chat + workers)
    pub sessions: Vec<SwarmSession>,
    pub sidebar_selection: SidebarItem,

    // Right panel — chat
    pub chat_history: Vec<ChatLine>,
    pub input: String,
    pub input_cursor: usize,
    pub streaming: bool,

    // Panel focus
    pub focus: Panel,

    // Scroll offset for chat (lines from bottom)
    pub chat_scroll: u16,

    // Worker pane capture (for selected worker)
    pub pane_content: String,

    // Workspace root for history/inbox persistence
    pub workspace_root: PathBuf,

    // UI inbox file position for incremental polling
    pub inbox_pos: u64,

    // Sidebar scroll offset (in logical pixels/rows from top).
    // Uses Cell for interior mutability during rendering.
    pub sidebar_scroll: Cell<usize>,

    // Confirmation / flash
    pub pending_action: Option<PendingAction>,
    pub flash_message: Option<FlashMessage>,

    // Daemon connection state
    pub daemon_connected: bool,

    // Dispatch overlay
    pub dispatch: Option<DispatchState>,

    // Active dispatch review (sidebar item + right panel)
    pub active_dispatch: Option<ActiveDispatch>,

    // Spinner animation tick (incremented each 100ms poll cycle)
    pub spinner_tick: usize,

    // Zoom: hide sidebar and show content panel full-width.
    pub zoomed: bool,

    // Help overlay.
    pub show_help: bool,

    // PR detail overlay.
    pub pr_detail: Option<PrDetailInfo>,

    // Periodic refresh
    last_worker_refresh: Instant,
}

impl App {
    pub fn new(workspaces: Vec<RegistryEntry>, active_tab: usize, workspace_root: PathBuf) -> Self {
        // Load persisted chat history.
        let saved = history::load_history(&workspace_root, 50);
        let mut chat_history: Vec<ChatLine> = vec![ChatLine::System(
            "Welcome to hive ui. Type a message to chat with the coordinator.".into(),
        )];
        for msg in &saved {
            let ts = msg
                .ts
                .with_timezone(&chrono::Local)
                .format("%-I:%M %p")
                .to_string();
            match msg.role.as_str() {
                "user" => chat_history.push(ChatLine::User(msg.content.clone(), ts)),
                "assistant" => chat_history.push(ChatLine::Assistant(msg.content.clone(), ts)),
                _ => {}
            }
        }

        // Skip to end of inbox file so we only see new events.
        let inbox_pos = std::fs::metadata(workspace_root.join(".hive/ui_inbox.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);

        Self {
            workspaces,
            active_tab,
            prefix_active: false,
            sessions: Vec::new(),
            sidebar_selection: SidebarItem::Chat,
            chat_history,
            input: String::new(),
            input_cursor: 0,
            streaming: false,
            focus: Panel::Workers,
            chat_scroll: 0,
            pane_content: String::new(),
            workspace_root,
            inbox_pos,
            sidebar_scroll: Cell::new(0),
            pending_action: None,
            flash_message: None,
            daemon_connected: false,
            dispatch: None,
            active_dispatch: None,
            spinner_tick: 0,
            zoomed: false,
            show_help: false,
            pr_detail: None,
            last_worker_refresh: Instant::now(),
        }
    }

    /// The root path of the currently active workspace.
    pub fn active_workspace_root(&self) -> Option<&std::path::Path> {
        self.workspaces
            .get(self.active_tab)
            .map(|e| e.path.as_path())
    }

    /// Periodic tick — refresh workers every 2 seconds + expire flashes.
    pub fn tick(&mut self) {
        if self.last_worker_refresh.elapsed().as_secs() >= 2 {
            self.refresh_workers();
            self.refresh_pane_content();
        }
        // Expire flash messages.
        if let Some(ref flash) = self.flash_message
            && Instant::now() >= flash.expires
        {
            self.flash_message = None;
        }
    }

    /// Refresh the worker list from swarm state.
    ///
    /// Reads `.swarm/state.json` directly from the active workspace root.
    pub fn refresh_workers(&mut self) {
        if let Some(root) = self.active_workspace_root() {
            if let Some(session) = discovery::discover_from_state_file(root) {
                self.sessions = vec![session];
            } else {
                self.sessions.clear();
            }
        } else {
            // No active workspace — fall back to legacy discovery.
            if let Ok(result) = discovery::discover_all() {
                self.sessions = result.sessions;
            }
        }
        self.clamp_sidebar_selection();
        self.last_worker_refresh = Instant::now();
    }

    /// Refresh content for the currently selected worker.
    ///
    /// Shows recent agent events from `.swarm/agents/<id>/events.jsonl`.
    /// Falls back to legacy pane capture if a pane_id is available.
    pub fn refresh_pane_content(&mut self) {
        let SidebarItem::Worker(idx) = self.sidebar_selection else {
            self.pane_content.clear();
            return;
        };

        let workers = self.flat_workers();
        let Some((_, wt)) = workers.get(idx) else {
            self.pane_content.clear();
            return;
        };

        // Try legacy pane capture if pane_id available.
        if let Some(ref pane_id) = wt.agent_pane_id {
            match std::process::Command::new("tmux")
                .args(["capture-pane", "-t", pane_id, "-p", "-S", "-500", "-e"])
                .output()
            {
                Ok(output) if output.status.success() => {
                    self.pane_content = String::from_utf8_lossy(&output.stdout).to_string();
                    return;
                }
                _ => {}
            }
        }

        // Daemon mode: show agent events.
        let session = self.sessions.first();
        let worktree_id = wt.id.clone();
        if let Some(session) = session {
            let events = discovery::read_agent_events(session, &worktree_id, 100);
            if events.is_empty() {
                let phase = wt.phase.as_deref().unwrap_or("unknown");
                self.pane_content = format!("Worker {worktree_id} ({phase}) — no events yet");
            } else {
                let mut lines = Vec::new();
                for e in &events {
                    match e.r#type.as_str() {
                        "assistant_text" => {
                            if let Some(ref text) = e.text {
                                lines.push(text.clone());
                            }
                        }
                        "tool_use" => {
                            let tool = e.tool.as_deref().unwrap_or("?");
                            lines.push(format!("── {tool} ──"));
                            if let Some(ref input) = e.input {
                                // Show first line of input only.
                                let first = input.lines().next().unwrap_or("");
                                if first.len() > 120 {
                                    lines.push(format!("  {}...", &first[..120]));
                                } else {
                                    lines.push(format!("  {first}"));
                                }
                            }
                        }
                        "tool_result" => {
                            if e.is_error {
                                let msg = e.output.as_deref().unwrap_or("error");
                                lines.push(format!("  ✗ {}", msg.lines().next().unwrap_or("")));
                            }
                        }
                        "error" => {
                            if let Some(ref msg) = e.message {
                                lines.push(format!("ERROR: {msg}"));
                            }
                        }
                        "complete" => {
                            let turns = e.turns.unwrap_or(0);
                            let cost = e.cost_usd.map(|c| format!(" ${c:.2}")).unwrap_or_default();
                            lines.push(format!("── Complete ({turns} turns{cost}) ──"));
                        }
                        _ => {}
                    }
                }
                self.pane_content = lines.join("\n");
            }
        } else {
            self.pane_content = "No workspace session".to_string();
        }
    }

    /// Whether a dispatch item is showing in the sidebar.
    fn has_dispatch(&self) -> bool {
        self.active_dispatch.is_some()
    }

    /// Total sidebar item count (Chat + optional Dispatch + workers).
    fn sidebar_count(&self) -> usize {
        1 + usize::from(self.has_dispatch()) + self.total_workers()
    }

    /// Total flat worker count across all sessions.
    fn total_workers(&self) -> usize {
        self.sessions.iter().map(|s| s.worktrees.len()).sum()
    }

    /// Current sidebar selection as a numeric index.
    /// Chat=0, Dispatch=1 (when present), Workers=1+has_dispatch+i.
    fn sidebar_index(&self) -> usize {
        let d = usize::from(self.has_dispatch());
        match self.sidebar_selection {
            SidebarItem::Chat => 0,
            SidebarItem::Dispatch => 1,
            SidebarItem::Worker(i) => 1 + d + i,
        }
    }

    /// Convert a numeric index back to a SidebarItem.
    fn sidebar_item_at(&self, index: usize) -> SidebarItem {
        if index == 0 {
            return SidebarItem::Chat;
        }
        if self.has_dispatch() && index == 1 {
            return SidebarItem::Dispatch;
        }
        let worker_offset = 1 + usize::from(self.has_dispatch());
        SidebarItem::Worker(index - worker_offset)
    }

    /// Select next sidebar item.
    pub fn select_next(&mut self) {
        let count = self.sidebar_count();
        if count > 0 {
            let next = (self.sidebar_index() + 1) % count;
            self.set_sidebar_selection(self.sidebar_item_at(next));
        }
    }

    /// Select previous sidebar item.
    pub fn select_prev(&mut self) {
        let count = self.sidebar_count();
        if count > 0 {
            let cur = self.sidebar_index();
            let prev = if cur == 0 { count - 1 } else { cur - 1 };
            self.set_sidebar_selection(self.sidebar_item_at(prev));
        }
    }

    /// Set the sidebar selection and refresh pane if needed.
    fn set_sidebar_selection(&mut self, item: SidebarItem) {
        if self.sidebar_selection != item {
            self.sidebar_selection = item;
            self.input.clear();
            self.input_cursor = 0;
            self.refresh_pane_content();
        }
    }

    /// Whether chat is the active sidebar item.
    pub fn is_chat_selected(&self) -> bool {
        self.sidebar_selection == SidebarItem::Chat
    }

    /// Whether dispatch review is the active sidebar item.
    pub fn is_dispatch_selected(&self) -> bool {
        self.sidebar_selection == SidebarItem::Dispatch
    }

    /// Switch to a workspace tab by index (0-based).
    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.workspaces.len() && idx != self.active_tab {
            self.active_tab = idx;
            self.sidebar_selection = SidebarItem::Chat;
            self.focus = Panel::Workers;
            self.chat_scroll = 0;

            // Update workspace root and reload chat history for the new tab.
            if let Some(entry) = self.workspaces.get(idx) {
                self.workspace_root = entry.path.clone();

                // Reload chat history from the new workspace.
                let saved = history::load_history(&self.workspace_root, 50);
                self.chat_history = vec![ChatLine::System(format!(
                    "Switched to {} workspace.",
                    entry.name
                ))];
                for msg in &saved {
                    let ts = msg
                        .ts
                        .with_timezone(&chrono::Local)
                        .format("%-I:%M %p")
                        .to_string();
                    match msg.role.as_str() {
                        "user" => self
                            .chat_history
                            .push(ChatLine::User(msg.content.clone(), ts)),
                        "assistant" => self
                            .chat_history
                            .push(ChatLine::Assistant(msg.content.clone(), ts)),
                        _ => {}
                    }
                }

                // Reset inbox position for the new workspace.
                self.inbox_pos =
                    std::fs::metadata(self.workspace_root.join(".hive/ui_inbox.jsonl"))
                        .map(|m| m.len())
                        .unwrap_or(0);
            }

            self.refresh_workers();
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.input
            .insert(self.input.len().min(self.input_cursor), c);
        self.input_cursor += c.len_utf8();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.input_cursor > 0 {
            // Find the previous char boundary.
            let prev = self.input[..self.input_cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.drain(prev..self.input_cursor);
            self.input_cursor = prev;
        }
    }

    /// Take the current input, clearing the buffer.
    pub fn take_input(&mut self) -> String {
        let text = self.input.clone();
        self.input.clear();
        self.input_cursor = 0;
        self.chat_scroll = 0;
        text
    }

    /// Scroll chat by N lines.
    pub fn scroll_chat_lines(&mut self, n: u16, up: bool) {
        if up {
            self.chat_scroll = self.chat_scroll.saturating_add(n);
        } else {
            self.chat_scroll = self.chat_scroll.saturating_sub(n);
        }
    }

    /// Scroll chat to bottom.
    pub fn scroll_chat_to_bottom(&mut self) {
        self.chat_scroll = 0;
    }

    /// Scroll chat to top (requires total lines and viewport height).
    pub fn scroll_chat_to_top(&mut self, total_lines: u16, viewport: u16) {
        self.chat_scroll = total_lines.saturating_sub(viewport);
    }

    pub fn clamp_sidebar_selection(&mut self) {
        let total = self.total_workers();
        match self.sidebar_selection {
            SidebarItem::Chat => {} // always valid
            SidebarItem::Dispatch => {
                if !self.has_dispatch() {
                    self.sidebar_selection = SidebarItem::Chat;
                }
            }
            SidebarItem::Worker(i) => {
                if total == 0 {
                    self.sidebar_selection = SidebarItem::Chat;
                } else if i >= total {
                    self.sidebar_selection = SidebarItem::Worker(total - 1);
                }
            }
        }
    }

    /// Get flat list of (session_name, worktree_info) for rendering.
    pub fn flat_workers(&self) -> Vec<(&str, &discovery::WorktreeInfo)> {
        self.sessions
            .iter()
            .flat_map(|s| {
                s.worktrees
                    .iter()
                    .map(move |w| (s.session_name.as_str(), w))
            })
            .collect()
    }

    /// Select first sidebar item.
    pub fn select_first(&mut self) {
        self.set_sidebar_selection(SidebarItem::Chat);
    }

    /// Select last sidebar item.
    pub fn select_last(&mut self) {
        let count = self.sidebar_count();
        if count > 0 {
            self.set_sidebar_selection(self.sidebar_item_at(count - 1));
        }
    }

    /// Half-page scroll up for chat.
    pub fn scroll_chat_half_up(&mut self, viewport: u16) {
        let half = (viewport / 2).max(1);
        self.chat_scroll = self.chat_scroll.saturating_add(half);
    }

    /// Half-page scroll down for chat.
    pub fn scroll_chat_half_down(&mut self, viewport: u16) {
        let half = (viewport / 2).max(1);
        self.chat_scroll = self.chat_scroll.saturating_sub(half);
    }

    /// Full-page scroll up for chat.
    pub fn scroll_chat_page_up(&mut self, viewport: u16) {
        self.chat_scroll = self.chat_scroll.saturating_add(viewport);
    }

    /// Full-page scroll down for chat.
    pub fn scroll_chat_page_down(&mut self, viewport: u16) {
        self.chat_scroll = self.chat_scroll.saturating_sub(viewport);
    }

    /// Set a flash message (3 seconds).
    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash_message = Some(FlashMessage {
            text: text.into(),
            expires: Instant::now() + std::time::Duration::from_secs(3),
        });
    }

    /// Enter the dispatch overlay (repo selection + prompt).
    pub fn enter_dispatch(&mut self) {
        let repos = discover_repos(&self.workspace_root);
        if repos.is_empty() {
            self.flash("No repos found in workspace");
            return;
        }
        let count = repos.len();
        self.dispatch = Some(DispatchState {
            phase: DispatchPhase::SelectRepos,
            repos,
            selected: vec![false; count],
            cursor: 0,
            prompt: String::new(),
            prompt_cursor: 0,
        });
    }

    /// Cancel the dispatch overlay and return to normal mode.
    pub fn dispatch_cancel(&mut self) {
        let back = if let Some(ref d) = self.dispatch {
            matches!(d.phase, DispatchPhase::EnterPrompt)
        } else {
            false
        };
        if back {
            // Go back to repo selection.
            if let Some(ref mut d) = self.dispatch {
                d.phase = DispatchPhase::SelectRepos;
            }
        } else {
            self.dispatch = None;
        }
    }

    /// Toggle the repo at the cursor in the dispatch overlay.
    pub fn dispatch_toggle_repo(&mut self) {
        if let Some(ref mut d) = self.dispatch
            && d.cursor < d.selected.len()
        {
            d.selected[d.cursor] = !d.selected[d.cursor];
        }
    }

    /// Move cursor down in repo list.
    pub fn dispatch_next(&mut self) {
        if let Some(ref mut d) = self.dispatch
            && d.cursor + 1 < d.repos.len()
        {
            d.cursor += 1;
        }
    }

    /// Move cursor up in repo list.
    pub fn dispatch_prev(&mut self) {
        if let Some(ref mut d) = self.dispatch {
            d.cursor = d.cursor.saturating_sub(1);
        }
    }

    /// Advance from repo selection to prompt entry (if at least one repo selected).
    pub fn dispatch_advance_phase(&mut self) {
        if let Some(ref mut d) = self.dispatch
            && d.selected.iter().any(|&s| s)
        {
            d.phase = DispatchPhase::EnterPrompt;
        }
    }

    /// Insert a character into the dispatch prompt.
    pub fn dispatch_insert_char(&mut self, c: char) {
        if let Some(ref mut d) = self.dispatch {
            d.prompt.insert(d.prompt.len().min(d.prompt_cursor), c);
            d.prompt_cursor += c.len_utf8();
        }
    }

    /// Delete the character before the cursor in the dispatch prompt.
    pub fn dispatch_backspace(&mut self) {
        if let Some(ref mut d) = self.dispatch
            && d.prompt_cursor > 0
        {
            let prev = d.prompt[..d.prompt_cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            d.prompt.drain(prev..d.prompt_cursor);
            d.prompt_cursor = prev;
        }
    }

    /// Returns the names of selected repos in the dispatch overlay.
    pub fn dispatch_selected_repos(&self) -> Vec<String> {
        self.dispatch
            .as_ref()
            .map(|d| {
                d.repos
                    .iter()
                    .zip(d.selected.iter())
                    .filter(|&(_, sel)| *sel)
                    .map(|(name, _)| name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Scroll dispatch review up (half-page).
    pub fn scroll_dispatch_up(&mut self, viewport: u16) {
        if let Some(ref mut d) = self.active_dispatch {
            let half = (viewport / 2).max(1);
            d.scroll = d.scroll.saturating_sub(half);
        }
    }

    /// Scroll dispatch review down (half-page).
    pub fn scroll_dispatch_down(&mut self, viewport: u16) {
        if let Some(ref mut d) = self.active_dispatch {
            let half = (viewport / 2).max(1);
            d.scroll = d.scroll.saturating_add(half);
        }
    }

    /// Toggle collapse/expand of the focused section in dispatch review.
    pub fn dispatch_toggle_section(&mut self) {
        if let Some(ref mut d) = self.active_dispatch
            && let Some(section) = d.sections.get_mut(d.focused_section)
        {
            section.collapsed = !section.collapsed;
        }
    }

    /// Toggle all sections collapsed/expanded in dispatch review.
    pub fn dispatch_toggle_all_sections(&mut self) {
        if let Some(ref mut d) = self.active_dispatch {
            // If any section is expanded, collapse all. Otherwise expand all.
            let any_expanded = d.sections.iter().any(|s| !s.collapsed);
            for s in &mut d.sections {
                s.collapsed = any_expanded;
            }
        }
    }

    /// Move to the next section in dispatch review.
    pub fn dispatch_next_section(&mut self) {
        if let Some(ref mut d) = self.active_dispatch
            && d.focused_section + 1 < d.sections.len()
        {
            d.focused_section += 1;
        }
    }

    /// Move to the previous section in dispatch review.
    pub fn dispatch_prev_section(&mut self) {
        if let Some(ref mut d) = self.active_dispatch {
            d.focused_section = d.focused_section.saturating_sub(1);
        }
    }

    /// Get the currently selected worker (id, info), if any.
    pub fn selected_worker(&self) -> Option<(&str, &WorktreeInfo)> {
        let SidebarItem::Worker(idx) = self.sidebar_selection else {
            return None;
        };
        let workers = self.flat_workers();
        workers.get(idx).map(|(_, wt)| (wt.id.as_str(), *wt))
    }
}

/// Current local timestamp formatted for display.
pub fn now_ts() -> String {
    chrono::Local::now().format("%-I:%M %p").to_string()
}

/// Discover repos by scanning workspace root for subdirectories containing `.git/`.
fn discover_repos(workspace_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(workspace_root) else {
        return Vec::new();
    };
    let mut repos: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) && e.path().join(".git").exists()
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    repos.sort();
    repos
}

//! App state machine for the apiari TUI.

use std::{
    collections::HashMap,
    io::{BufRead, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Instant,
};

use apiari_tui::{conversation::ConversationEntry, scroll::ScrollState};
use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use serde::Deserialize;

use crate::buzz::{
    coordinator::memory::MemoryStore,
    signal::{Severity, SignalRecord, store::SignalStore},
};

/// A tmux operation to be executed asynchronously after apply_worker_update.
pub(super) enum PendingShellOp {
    Create {
        tmux: crate::shells::TmuxManager,
        name: String,
        working_dir: PathBuf,
    },
    Kill {
        tmux: crate::shells::TmuxManager,
        name: String,
    },
}

use crate::{
    buzz::conversation::ConversationStore,
    config::{self, Workspace},
};

/// Maximum number of chat history messages to load from a previous session.
const CHAT_HISTORY_LIMIT: usize = 20;

// ── Types ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Dashboard,
    WorkerDetail(usize),
    WorkerChat(usize),
    SignalDetail(usize),
    SignalList,
    ReviewList,
    PrList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Home,
    Workers,
    Shells,
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

/// Which view is shown in the triage sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarView {
    /// Actionable triage items (signals + tasks in Triage stage).
    Triage,
    /// Chronological workspace activity feed.
    Activity,
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
    KillShell(String), // shell window name
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
    /// Tmux session name (Some if shells enabled).
    pub(super) tmux_session: Option<String>,
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
    ShellWindows(Vec<(String, Vec<crate::shells::ShellWindow>)>),
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
    /// Preview text for a single shell window (lazily captured).
    ShellPreview {
        workspace_name: String,
        window_name: String,
        preview: String,
    },
    Tasks(Vec<(String, Vec<crate::buzz::task::Task>)>),
    ActivityEvents(Vec<(String, Vec<crate::buzz::task::ActivityEvent>)>),
    TaskTimeline {
        workspace: String,
        task_id: String,
        events: Vec<crate::buzz::task::ActivityEvent>,
    },
    /// A single line streamed from a live worker output subscription.
    WorkerOutputLine {
        workspace_name: String,
        worker_id: String,
        line: OutputLine,
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
    /// Activity log (derived from prompt, PR, phase, etc.).
    pub activity: Vec<WorkerEvent>,
    /// Scroll state for activity log in split view.
    pub activity_scroll: ScrollState,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
}

// ── Worker activity log ───────────────────────────────────

#[derive(Debug, Clone)]
pub struct WorkerEvent {
    pub ts: Option<DateTime<Local>>,
    pub kind: WorkerEventKind,
    pub text: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum WorkerEventKind {
    Dispatched,
    BeeToWorker,
    UserToWorker,
    PrOpened,
    #[allow(dead_code)]
    CiFailed,
    #[allow(dead_code)]
    CiPassed,
    Merged,
    StatusChange,
}

// ── Kanban board ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KanbanStage {
    InProgress,
    InReview,
    HumanReview,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KanbanCard {
    pub id: String,
    pub stage: KanbanStage,
    pub icon: String,
    pub title: String,
    pub subtitle: String,
    pub source: String,
    pub url: Option<String>,
    pub entered_stage_at: chrono::DateTime<chrono::Utc>,
}

/// State for the task detail panel (shown when pressing Enter on a task kanban card).
#[derive(Debug, Clone)]
pub struct TaskDetailState {
    pub task_id: String,
    pub task_title: String,
    pub stage: String,
    pub worker_id: Option<String>,
    pub pr_number: Option<i64>,
    pub events: Vec<crate::buzz::task::ActivityEvent>,
    pub scroll: usize,
}

/// Style of a rendered worker output line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLineKind {
    Text,
    Thinking,
    ToolUse,
    ToolResult,
    Separator,
    Status,
    Error,
}

/// A single rendered line in the worker output panel.
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub kind: OutputLineKind,
    pub text: String,
}

/// State for the live worker output panel.
#[derive(Debug, Clone)]
pub struct WorkerOutputState {
    pub worker_id: String,
    pub lines: Vec<OutputLine>,
    pub scroll: usize,
    /// When true, new lines auto-scroll to the bottom.
    pub auto_scroll: bool,
}

impl WorkerOutputState {
    pub fn new(worker_id: String) -> Self {
        Self {
            worker_id,
            lines: Vec::new(),
            scroll: 0,
            auto_scroll: true,
        }
    }

    /// Maximum number of lines to keep in memory.
    pub const MAX_LINES: usize = 10_000;

    /// Append a line; enforce the cap and auto-scroll if enabled.
    pub fn push(&mut self, line: OutputLine) {
        if self.lines.len() >= Self::MAX_LINES {
            self.lines.drain(0..Self::MAX_LINES / 10);
            // Adjust scroll so we don't jump
            self.scroll = self.scroll.saturating_sub(Self::MAX_LINES / 10);
        }
        self.lines.push(line);
    }
}

/// Build kanban cards from tasks. Task cards take priority over worker/signal cards
/// for the same work item.
pub fn build_kanban_cards_from_tasks(tasks: &[crate::buzz::task::Task]) -> Vec<KanbanCard> {
    tasks
        .iter()
        .filter(|t| {
            matches!(
                t.stage,
                crate::buzz::task::TaskStage::InProgress
                    | crate::buzz::task::TaskStage::InAiReview
                    | crate::buzz::task::TaskStage::HumanReview
            )
        })
        .map(|t| {
            let stage = match t.stage {
                crate::buzz::task::TaskStage::InProgress => KanbanStage::InProgress,
                crate::buzz::task::TaskStage::InAiReview => KanbanStage::InReview,
                crate::buzz::task::TaskStage::HumanReview => KanbanStage::HumanReview,
                _ => unreachable!("filtered above"),
            };
            let icon = match t.source.as_deref() {
                Some("sentry") => "⚡",
                Some("github_issue") => "📋",
                Some("manual") => "📝",
                Some("email") => "📧",
                _ => "📋",
            };
            let subtitle = match t.stage {
                crate::buzz::task::TaskStage::InProgress => {
                    if t.pr_url.is_some() {
                        if let Some(pr_num) = t.pr_number {
                            format!("PR #{pr_num} · coding")
                        } else {
                            "coding · PR open".to_string()
                        }
                    } else if let Some(ref wid) = t.worker_id {
                        let short = if wid.len() > 12 {
                            &wid[wid.len() - 8..]
                        } else {
                            wid.as_str()
                        };
                        format!("{short} · coding")
                    } else {
                        "in progress".to_string()
                    }
                }
                crate::buzz::task::TaskStage::InAiReview => {
                    if let Some(pr_num) = t.pr_number {
                        format!("PR #{pr_num} · awaiting CI/review")
                    } else {
                        "awaiting review".to_string()
                    }
                }
                crate::buzz::task::TaskStage::HumanReview => {
                    if let Some(pr_num) = t.pr_number {
                        format!("PR #{pr_num} · ready to merge")
                    } else {
                        "ready to merge".to_string()
                    }
                }
                _ => unreachable!("filtered above"),
            };
            KanbanCard {
                id: format!("task:{}", t.id),
                stage,
                icon: icon.to_string(),
                title: truncate_title(&t.title, 40),
                subtitle,
                source: t.source.clone().unwrap_or_else(|| "task".to_string()),
                url: t.pr_url.clone().or_else(|| t.source_url.clone()),
                entered_stage_at: t.updated_at,
            }
        })
        .collect()
}

/// Build kanban cards by deriving them from worker and signal state.
pub fn build_kanban_cards(ws: &WorkspaceState) -> Vec<KanbanCard> {
    let now = chrono::Utc::now();
    let mut cards: Vec<KanbanCard> = Vec::new();

    // ── Workers → cards ──
    for w in &ws.workers {
        // Reviewer workers are internal to the review process — hide them from the board
        if w.prompt.starts_with("Review PR") {
            continue;
        }

        let phase = w.phase.as_deref().unwrap_or("");

        let elapsed = w
            .created_at
            .map(|t| {
                let mins = chrono::Utc::now()
                    .signed_duration_since(t.with_timezone(&chrono::Utc))
                    .num_minutes();
                if mins >= 60 {
                    format!("{}h{}m", mins / 60, mins % 60)
                } else {
                    format!("{mins}m")
                }
            })
            .unwrap_or_default();

        let created_utc = w
            .created_at
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or(now);

        let is_done_phase =
            phase.eq_ignore_ascii_case("completed") || phase.eq_ignore_ascii_case("closed");

        if let Some(pr) = &w.pr {
            let pr_done =
                pr.state.eq_ignore_ascii_case("closed") || pr.state.eq_ignore_ascii_case("merged");
            let waiting =
                phase == "waiting" || w.agent_session_status.as_deref() == Some("waiting");
            if is_done_phase || pr_done {
                // Done workers disappear from the board
                continue;
            } else if waiting {
                // Waiting worker with a PR → likely needs user attention
                let ci_sub = ci_status_for_pr(&ws.signals, pr.number as i64)
                    .map(|ok| {
                        if ok {
                            format!("PR #{} · CI ✅", pr.number)
                        } else {
                            format!("PR #{} · CI ❌", pr.number)
                        }
                    })
                    .unwrap_or_else(|| format!("PR #{} · merge?", pr.number));
                cards.push(KanbanCard {
                    id: format!("worker:{}", w.id),
                    stage: KanbanStage::HumanReview,
                    icon: "👷".to_string(),
                    title: short_id(&w.id),
                    subtitle: ci_sub,
                    source: "worker".to_string(),
                    url: Some(pr.url.clone()),
                    entered_stage_at: created_utc,
                });
            } else {
                cards.push(KanbanCard {
                    id: format!("worker:{}", w.id),
                    stage: KanbanStage::InProgress,
                    icon: "👷".to_string(),
                    title: short_id(&w.id),
                    subtitle: format!("PR #{} · coding", pr.number),
                    source: "worker".to_string(),
                    url: Some(pr.url.clone()),
                    entered_stage_at: created_utc,
                });
            }
        } else if is_done_phase {
            // Done workers without PR disappear from the board
            continue;
        } else {
            // Running, no PR yet
            let session_status = w.agent_session_status.as_deref();
            let subtitle = match (phase, session_status) {
                ("starting", _) => format!("starting · {elapsed}"),
                ("running", Some("waiting")) => format!("waiting for input · {elapsed}"),
                ("running", _) => format!("coding · {elapsed}"),
                _ => format!("running {elapsed}"),
            };
            cards.push(KanbanCard {
                id: format!("worker:{}", w.id),
                stage: KanbanStage::InProgress,
                icon: "👷".to_string(),
                title: short_id(&w.id),
                subtitle,
                source: "worker".to_string(),
                url: None,
                entered_stage_at: created_utc,
            });
        }
    }

    // ── Task cards: merge with priority over matching worker cards ──
    let mut task_cards = build_kanban_cards_from_tasks(&ws.tasks);
    // Enrich task card subtitles with live worker status
    for card in &mut task_cards {
        let task_id = card.id.strip_prefix("task:").unwrap_or("");
        let Some(task) = ws.tasks.iter().find(|t| t.id == task_id) else {
            continue;
        };
        match card.stage {
            KanbanStage::InProgress => {
                if let Some(ref wid) = task.worker_id
                    && let Some(worker) = ws.workers.iter().find(|w| &w.id == wid)
                {
                    let w_phase = worker.phase.as_deref().unwrap_or("");
                    let w_session = worker.agent_session_status.as_deref();
                    card.subtitle = if let Some(pr_num) = task.pr_number {
                        match w_session {
                            Some("waiting") => format!("PR #{pr_num} · waiting for input"),
                            _ => format!("PR #{pr_num} · coding"),
                        }
                    } else {
                        match (w_phase, w_session) {
                            ("starting", _) => "starting".to_string(),
                            ("running", Some("waiting")) => "waiting for input".to_string(),
                            ("running", _) => "coding".to_string(),
                            _ => card.subtitle.clone(),
                        }
                    };
                }
            }
            KanbanStage::InReview => {
                // Find reviewer worker whose PR matches this task's PR
                let reviewer = task.pr_number.and_then(|pr_num| {
                    ws.workers.iter().find(|w| {
                        w.prompt.starts_with("Review PR")
                            && w.pr.as_ref().map(|pr| pr.number as i64) == Some(pr_num)
                    })
                });
                if let Some(rev) = reviewer {
                    let rev_elapsed = rev
                        .created_at
                        .map(|t| {
                            let mins = now
                                .signed_duration_since(t.with_timezone(&chrono::Utc))
                                .num_minutes();
                            if mins >= 60 {
                                format!("{}h{}m", mins / 60, mins % 60)
                            } else {
                                format!("{mins}m")
                            }
                        })
                        .unwrap_or_default();
                    card.subtitle = match rev.agent_session_status.as_deref() {
                        Some("waiting") => "review complete".to_string(),
                        _ => format!("reviewing · {rev_elapsed}"),
                    };
                }
                // if no reviewer found, keep existing subtitle ("awaiting review", etc.)
            }
            KanbanStage::HumanReview => {
                if let Some(pr_num) = task.pr_number
                    && let Some(ci_ok) = ci_status_for_pr(&ws.signals, pr_num)
                {
                    card.subtitle = if ci_ok {
                        format!("PR #{pr_num} · CI ✅")
                    } else {
                        format!("PR #{pr_num} · CI ❌")
                    };
                }
            }
        }
    }
    if !task_cards.is_empty() {
        // Drop worker cards whose worker_id is covered by a task
        let covered_worker_ids: std::collections::HashSet<&str> = ws
            .tasks
            .iter()
            .filter_map(|t| t.worker_id.as_deref())
            .collect();
        cards.retain(|c| {
            if let Some(wid) = c.id.strip_prefix("worker:") {
                !covered_worker_ids.contains(wid)
            } else {
                true
            }
        });
        cards.extend(task_cards);
    }

    // Filter out dismissed cards
    cards.retain(|c| !ws.kanban_dismissed.contains(&c.id));

    cards
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TriageItem {
    pub id: String,
    pub icon: String,
    pub title: String,
    pub subtitle: String,
    pub url: Option<String>,
    pub age: chrono::Duration,
    pub source_label: String,
    pub severity: Severity,
}

/// Parse a GitHub URL into a short label like "repo #123".
fn parse_github_label(url: &str) -> Option<String> {
    // Strip query string and fragment before any other processing.
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);
    // Strip trailing slashes.
    let url = url.trim_end_matches('/');
    let parts: Vec<&str> = url.split('/').collect();
    // Expected shape: ["https:", "", "github.com", "owner", "repo", "issues"|"pull", "123"]
    if parts.len() >= 7 && parts[2] == "github.com" {
        let owner = parts[3];
        let repo = parts[4];
        let kind = parts[5];
        let number = parts[6];
        if !owner.is_empty()
            && !repo.is_empty()
            && (kind == "issues" || kind == "pull")
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
        {
            return Some(format!("{repo} #{number}"));
        }
    }
    None
}

/// Get items for the triage sidebar — tasks in Triage stage + unmatched signals.
pub fn triage_items(ws: &WorkspaceState) -> Vec<TriageItem> {
    let mut items = Vec::new();

    // Tasks in Triage stage
    for t in &ws.tasks {
        if t.stage == crate::buzz::task::TaskStage::Triage {
            let source_label = t
                .source_url
                .as_deref()
                .and_then(parse_github_label)
                .unwrap_or_else(|| t.source.clone().unwrap_or_else(|| "task".to_string()));
            items.push(TriageItem {
                id: format!("task:{}", t.id),
                icon: match t.source.as_deref() {
                    Some("sentry") => "⚡".to_string(),
                    Some("github_issue") => "📋".to_string(),
                    Some("email") => "📧".to_string(),
                    Some("manual") => "📝".to_string(),
                    _ => "📋".to_string(),
                },
                title: t.title.clone(),
                subtitle: t.source.clone().unwrap_or_else(|| "task".to_string()),
                url: t.source_url.clone(),
                age: chrono::Utc::now().signed_duration_since(t.created_at),
                source_label,
                severity: Severity::Info,
            });
        }
    }

    // Open signals — only show actionable sources in triage.
    for sig in &ws.signals {
        // Only allow explicitly actionable signal sources in triage.
        let actionable = matches!(
            sig.source.as_str(),
            "sentry" | "github_review_queue" | "email" | "linear"
        );
        if !actionable {
            continue;
        }
        let source_label = match sig.source.as_str() {
            "github_review_queue" => sig
                .url
                .as_deref()
                .and_then(parse_github_label)
                .unwrap_or_else(|| sig.source.clone()),
            "sentry" => {
                if sig.external_id.is_empty() {
                    "sentry".to_string()
                } else {
                    format!("sentry {}", sig.external_id)
                }
            }
            other => other.to_string(),
        };
        items.push(TriageItem {
            id: format!("signal:{}", sig.id),
            icon: match sig.source.as_str() {
                "sentry" => "⚡".to_string(),
                "github_review_queue" => "🔍".to_string(),
                "linear" => "📋".to_string(),
                "email" => "📧".to_string(),
                "notion" => "📓".to_string(),
                _ => "⚡".to_string(),
            },
            title: if sig.title.len() > 50 {
                format!("{}…", &sig.title[..49])
            } else {
                sig.title.clone()
            },
            subtitle: sig.source.clone(),
            url: sig.url.clone(),
            age: chrono::Utc::now().signed_duration_since(sig.created_at),
            source_label,
            severity: sig.severity.clone(),
        });
    }

    items
}

/// Remove stale entries from `kanban_dismissed` so it doesn't grow without
/// bound over a long TUI session.  An ID is kept only if its underlying
/// worker or task still exists in the workspace.
fn prune_kanban_dismissed(ws: &mut WorkspaceState) {
    ws.kanban_dismissed.retain(|id| {
        if let Some(worker_id) = id.strip_prefix("worker:") {
            ws.workers.iter().any(|w| w.id == worker_id)
        } else if let Some(task_id) = id.strip_prefix("task:") {
            ws.tasks.iter().any(|t| t.id == task_id)
        } else {
            false
        }
    });
}

/// Compute the ideal height (in terminal rows) for the kanban strip.
/// Called by the renderer for layout; also used as a fallback before the first frame.
pub fn compute_kanban_height(_ws: &WorkspaceState) -> u16 {
    9
}

/// How many cards in `stage` are actually visible given `strip_h` terminal rows.
/// Pass `App::kanban_allocated_height` (recorded by the renderer after layout) so
/// navigation never targets a card that ratatui shrank off-screen.
fn ws_kanban_visible_count(ws: &WorkspaceState, stage: KanbanStage, strip_h: u16) -> usize {
    let total = ws.kanban_cards.iter().filter(|c| c.stage == stage).count();
    if total == 0 {
        return 0;
    }
    // Use actual allocated height; fall back to ideal height before the first frame.
    let h = if strip_h > 0 {
        strip_h
    } else {
        compute_kanban_height(ws)
    };
    // inner = h - 2 borders; available for cards = inner - 1 header row
    let available = (h as usize).saturating_sub(3);
    let cards_fit = available / 2;
    cards_fit.min(total)
}

/// Return CI status for a PR number by scanning signals.
/// Returns Some(true) for CI pass, Some(false) for CI failure, None if unknown.
fn ci_status_for_pr(signals: &[SignalRecord], pr_number: i64) -> Option<bool> {
    for sig in signals.iter().rev() {
        let meta_pr = sig
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .and_then(|v| v.get("pr_number").and_then(|n| n.as_i64()));
        if meta_pr == Some(pr_number) {
            if sig.source == "github_ci_pass" {
                return Some(true);
            } else if sig.source == "github_ci_failure" {
                return Some(false);
            }
        }
    }
    None
}

/// Extract PR number from a kanban card subtitle like "PR #42 · merge?".
fn kanban_extract_pr_number(subtitle: &str) -> Option<u64> {
    let start = subtitle.find("PR #")? + 4;
    let end = subtitle[start..]
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| start + i)
        .unwrap_or(subtitle.len());
    subtitle[start..end].parse().ok()
}

fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}

fn truncate_title(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
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
    Workers,   // kanban strip brightens (workers & signals)
    Heartbeat, // remaining panels brighten
    Reviews,   // signals/reviews panels brighten (zoom mode)
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
                Panel::Shells,
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
                self.revealed_panels.insert(Panel::Home);
                Some(
                    "Above: the Kanban strip. Workers, signals, and PRs flow \
                     through four stages \u{2014} Incoming, In Progress, Needs Me, Done. \
                     Everything at a glance.\n\n\
                     Press enter to continue.",
                )
            }
            OnboardingStage::Workers => {
                self.stage = OnboardingStage::Heartbeat;
                self.revealed_panels.insert(Panel::Feed);
                self.revealed_panels.insert(Panel::Signals);
                self.revealed_panels.insert(Panel::Reviews);
                Some(
                    "Signals from GitHub (CI, PRs, releases), Sentry errors, \
                     Linear issues \u{2014} they all appear as kanban cards. \
                     The \u{201c}Needs Me\u{201d} column highlights what needs your attention.\n\n\
                     Press enter to continue.",
                )
            }
            OnboardingStage::Heartbeat => {
                self.stage = OnboardingStage::Reviews;
                Some(
                    "Want more detail? Press 'w' for workers, 's' for signals, \
                     'r' for reviews \u{2014} zoom hotkeys open full-screen panels.\n\n\
                     Press enter to continue.",
                )
            }
            OnboardingStage::Reviews => {
                self.stage = OnboardingStage::Complete;
                self.active = false;
                // Reveal everything
                self.revealed_panels.insert(Panel::Home);
                self.revealed_panels.insert(Panel::Workers);
                self.revealed_panels.insert(Panel::Shells);
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
    Done,
}

#[derive(Debug, Clone)]
pub struct SetupState {
    pub step: SetupStep,
    pub workspace_root: std::path::PathBuf,
    pub workspace_name: String,
    pub default_agent: String,
    /// True when adding a workspace to an existing setup (simplified flow).
    pub add_workspace: bool,
    /// Previous tab index to restore on cancel (add-workspace only).
    pub prev_active_tab: usize,
    /// Previous focused panel to restore on cancel (add-workspace only).
    pub prev_focused_panel: Panel,
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
    /// Token usage from the last coordinator turn.
    pub usage_input_tokens: u64,
    pub usage_output_tokens: u64,
    pub usage_cache_read_tokens: u64,
    pub usage_cost_usd: Option<f64>,
    pub usage_context_window: u64,
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
    /// True when this tab is a temporary placeholder for the add-workspace flow.
    pub is_setup_placeholder: bool,
    /// Tmux shell manager (Some if shells are enabled for this workspace).
    pub tmux: Option<crate::shells::TmuxManager>,
    /// Cached shell windows (refreshed periodically).
    pub shell_windows: Vec<crate::shells::ShellWindow>,
    /// Queued notifications waiting for streaming to finish.
    pub pending_notifications: Vec<String>,
    /// Derived kanban cards (rebuilt on signal/worker refresh).
    pub kanban_cards: Vec<KanbanCard>,
    /// Currently selected kanban card: (stage, card_index_within_that_stage).
    pub kanban_selected: Option<(KanbanStage, usize)>,
    /// Dismissed kanban card IDs (filtered out when building cards).
    pub kanban_dismissed: std::collections::HashSet<String>,
    /// Tasks loaded from SQLite (rebuilt on task refresh).
    pub tasks: Vec<crate::buzz::task::Task>,
    /// Whether the triage sidebar is visible.
    pub triage_sidebar_open: bool,
    /// Selected index in the triage sidebar list.
    pub triage_selected: usize,
    /// Scroll offset for the triage sidebar.
    pub triage_scroll: usize,
    /// Which view is shown in the sidebar (Triage or Activity).
    pub sidebar_view: SidebarView,
    /// Activity feed events loaded from SQLite.
    pub activity_events: Vec<crate::buzz::task::ActivityEvent>,
    /// Selected index in the activity feed list.
    pub activity_selected: usize,
    /// Scroll offset for the activity feed.
    pub activity_scroll: usize,
    /// Task detail panel state (Some when viewing a task's timeline).
    pub viewing_task: Option<TaskDetailState>,
    /// Live worker output panel state (Some when streaming a worker's output).
    pub viewing_worker_output: Option<WorkerOutputState>,
}

// ── App ───────────────────────────────────────────────────

pub struct App {
    pub workspaces: Vec<WorkspaceState>,
    /// Cached workspace name → index for O(1) lookup on hot path.
    pub(super) ws_name_index: HashMap<String, usize>,
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
    // Shell management
    pub shell_selection: usize,
    pub shell_input_active: bool,
    pub shell_input: String,
    /// When true, scroll/focus targets the activity pane (left) in WorkerDetail split.
    pub worker_activity_focused: bool,
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
    /// Actual height (rows) allocated to the kanban strip by ratatui's layout engine.
    /// Set during each render pass so navigation never targets a card clipped by
    /// the `Min(5)` chat constraint.
    pub kanban_allocated_height: std::cell::Cell<u16>,
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
    /// Background task streaming a worker's live output (aborted when output panel closes).
    pub worker_output_task: Option<tokio::task::JoinHandle<()>>,
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
            .map(|ws| {
                let tmux = if ws.config.shells.enabled {
                    let session =
                        crate::shells::TmuxManager::session_name_for(&ws.name, &ws.config.shells);
                    Some(crate::shells::TmuxManager::new(&session))
                } else {
                    None
                };
                WorkspaceState {
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
                    usage_input_tokens: 0,
                    usage_output_tokens: 0,
                    usage_cache_read_tokens: 0,
                    usage_cost_usd: None,
                    usage_context_window: 0,
                    sparkline_data: vec![0; 24],
                    watcher_health: Vec::new(),
                    feed: Vec::new(),
                    prev_worker_phases: std::collections::HashMap::new(),
                    prev_signal_ids: std::collections::HashSet::new(),
                    prev_pr_workers: std::collections::HashSet::new(),
                    feed_scroll: ScrollState::new(),
                    thoughts: Vec::new(),
                    is_setup_placeholder: false,
                    tmux,
                    shell_windows: Vec::new(),
                    pending_notifications: Vec::new(),
                    kanban_cards: Vec::new(),
                    kanban_selected: None,
                    kanban_dismissed: std::collections::HashSet::new(),
                    tasks: Vec::new(),
                    triage_sidebar_open: true,
                    triage_selected: 0,
                    triage_scroll: 0,
                    sidebar_view: SidebarView::Triage,
                    activity_events: Vec::new(),
                    activity_selected: 0,
                    activity_scroll: 0,
                    viewing_task: None,
                    viewing_worker_output: None,
                }
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
            ws_name_index: HashMap::new(),
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
            shell_selection: 0,
            shell_input_active: false,
            shell_input: String::new(),
            worker_activity_focused: false,
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
            kanban_allocated_height: std::cell::Cell::new(0),
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
            worker_output_task: None,
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

        app.rebuild_ws_name_index();

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
            usage_input_tokens: 0,
            usage_output_tokens: 0,
            usage_cache_read_tokens: 0,
            usage_cost_usd: None,
            usage_context_window: 0,
            prev_worker_phases: std::collections::HashMap::new(),
            prev_signal_ids: std::collections::HashSet::new(),
            prev_pr_workers: std::collections::HashSet::new(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            thoughts: Vec::new(),
            is_setup_placeholder: true,
            tmux: None,
            shell_windows: Vec::new(),
            pending_notifications: Vec::new(),
            kanban_cards: Vec::new(),
            kanban_selected: None,
            kanban_dismissed: std::collections::HashSet::new(),
            tasks: Vec::new(),
            triage_sidebar_open: true,
            triage_selected: 0,
            triage_scroll: 0,
            sidebar_view: SidebarView::Triage,
            activity_events: Vec::new(),
            activity_selected: 0,
            activity_scroll: 0,
            viewing_task: None,
            viewing_worker_output: None,
        };

        let setup = SetupState {
            step: SetupStep::AskRoot,
            workspace_root: cwd.clone(),
            workspace_name: ws_name,
            default_agent: "claude".to_string(),
            add_workspace: false,
            prev_active_tab: 0,
            prev_focused_panel: Panel::Workers,
        };

        let cwd_display = cwd.display().to_string();

        let mut app = Self {
            workspaces: vec![ws_state],
            ws_name_index: HashMap::new(),
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
            shell_selection: 0,
            shell_input_active: false,
            shell_input: String::new(),
            worker_activity_focused: false,
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
            kanban_allocated_height: std::cell::Cell::new(0),
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
            worker_output_task: None,
        };

        app.rebuild_ws_name_index();

        // Inject first setup message
        if let Some(ws) = app.workspaces.get_mut(0) {
            ws.chat_history.push(ChatLine::Assistant(
                setup_greeting(&cwd_display),
                now_ts(),
                None,
            ));
            ws.chat_scroll.scroll_to_bottom();
        }

        app
    }

    /// Enter add-workspace mode on an existing app.
    /// Adds a placeholder workspace tab, switches to it,
    /// and starts the simplified setup flow.
    /// `name_override` optionally pre-fills the workspace name.
    pub fn enter_add_workspace(&mut self, dir: std::path::PathBuf, name_override: Option<&str>) {
        let ws_name = name_override.map(|n| n.to_string()).unwrap_or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string()
        });

        let dir_display = dir.display().to_string();

        let config: config::WorkspaceConfig =
            serde_json::from_value(serde_json::json!({"root": dir.to_string_lossy()}))
                .unwrap_or_else(|_| {
                    serde_json::from_value(serde_json::json!({"root": "."}))
                        .expect("hardcoded fallback config")
                });

        let mut ws_state = WorkspaceState {
            name: "(new workspace)".to_string(),
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
            usage_input_tokens: 0,
            usage_output_tokens: 0,
            usage_cache_read_tokens: 0,
            usage_cost_usd: None,
            usage_context_window: 0,
            prev_worker_phases: std::collections::HashMap::new(),
            prev_signal_ids: std::collections::HashSet::new(),
            prev_pr_workers: std::collections::HashSet::new(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            thoughts: Vec::new(),
            is_setup_placeholder: true,
            tmux: None,
            shell_windows: Vec::new(),
            pending_notifications: Vec::new(),
            kanban_cards: Vec::new(),
            kanban_selected: None,
            kanban_dismissed: std::collections::HashSet::new(),
            tasks: Vec::new(),
            triage_sidebar_open: true,
            triage_selected: 0,
            triage_scroll: 0,
            sidebar_view: SidebarView::Triage,
            activity_events: Vec::new(),
            activity_selected: 0,
            activity_scroll: 0,
            viewing_task: None,
            viewing_worker_output: None,
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

        let prev_active_tab = self.active_tab;
        let prev_focused_panel = self.focused_panel;

        self.workspaces.push(ws_state);
        self.rebuild_ws_name_index();
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
            add_workspace: true,
            prev_active_tab,
            prev_focused_panel,
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
                    let user_changed_root = !input.is_empty();
                    if user_changed_root {
                        setup.workspace_root = std::path::PathBuf::from(input);
                    }
                    // Only derive name from directory if user changed the root
                    // or no name was pre-filled (e.g. from --name)
                    if user_changed_root || setup.workspace_name.is_empty() {
                        setup.workspace_name = setup
                            .workspace_root
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("workspace")
                            .to_string();
                    }
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
                        // First-run: accept root and finish immediately
                        setup.step = SetupStep::Done;
                        (
                            format!(
                                "Got it \u{2014} setting up your workspace at {root}.\n\n\
                                 Writing config..."
                            ),
                            true,
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

        // Write .onboarded marker (only for first-run setup, not add-workspace)
        if !is_add && let Err(e) = super::onboarding::mark_onboarded() {
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
        let tmux = if ws.config.shells.enabled {
            let session = crate::shells::TmuxManager::session_name_for(&ws.name, &ws.config.shells);
            Some(crate::shells::TmuxManager::new(&session))
        } else {
            None
        };
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
            usage_input_tokens: 0,
            usage_output_tokens: 0,
            usage_cache_read_tokens: 0,
            usage_cost_usd: None,
            usage_context_window: 0,
            prev_worker_phases: std::collections::HashMap::new(),
            prev_signal_ids: std::collections::HashSet::new(),
            prev_pr_workers: std::collections::HashSet::new(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            is_setup_placeholder: false,
            thoughts: Vec::new(),
            tmux,
            shell_windows: Vec::new(),
            pending_notifications: Vec::new(),
            kanban_cards: Vec::new(),
            kanban_selected: None,
            kanban_dismissed: std::collections::HashSet::new(),
            tasks: Vec::new(),
            triage_sidebar_open: true,
            triage_selected: 0,
            triage_scroll: 0,
            sidebar_view: SidebarView::Triage,
            activity_events: Vec::new(),
            activity_selected: 0,
            activity_scroll: 0,
            viewing_task: None,
            viewing_worker_output: None,
        };

        if is_add {
            // Add-workspace: simple completion message
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

            if let Some(idx) = self.workspaces.iter().position(|w| w.is_setup_placeholder) {
                self.workspaces[idx] = ws_state;
                self.active_tab = idx;
            } else {
                self.workspaces.push(ws_state);
                self.active_tab = self.workspaces.len() - 1;
            }
            self.rebuild_ws_name_index();
            self.focused_panel = Panel::Workers;
            self.chat_focused = false;
        } else {
            // First-run setup: tour narration messages
            ws_state.chat_history.push(ChatLine::Assistant(
                format!(
                    "\u{2713} Workspace '{}' created! Config written to {}\n\n\
                     Let me show you around \u{1f41d}",
                    setup.workspace_name, config_display
                ),
                now_ts(),
                None,
            ));

            ws_state.chat_history.push(ChatLine::Assistant(
                "\u{1f448} Workers panel \u{2014} when you dispatch AI coding tasks, \
                 they appear here. You can chat with workers, check their PRs, \
                 and monitor progress.\n\n\
                 \u{1f4ca} Signals panel \u{2014} GitHub CI results, Sentry errors, \
                 and other watcher events surface here automatically.\n\n\
                 \u{1f493} Heartbeat \u{2014} a live feed of watcher activity so you \
                 always know what's being monitored."
                    .to_string(),
                now_ts(),
                None,
            ));

            ws_state.chat_history.push(ChatLine::Assistant(
                "I'm using Claude as your AI agent by default.\n\n\
                 You can add integrations anytime by chatting with me:\n\
                 \u{2022} \"set up Telegram notifications\"\n\
                 \u{2022} \"watch my GitHub repos\"\n\
                 \u{2022} \"connect Sentry\"\n\n\
                 What would you like to work on?"
                    .to_string(),
                now_ts(),
                None,
            ));
            ws_state.chat_scroll.scroll_to_bottom();

            // Preserve chat history from setup conversation
            let chat_history = self
                .workspaces
                .first()
                .map(|w| w.chat_history.clone())
                .unwrap_or_default();
            ws_state.chat_history.splice(0..0, chat_history);

            self.workspaces = vec![ws_state];
            self.rebuild_ws_name_index();
            self.active_tab = 0;
            self.onboarding = OnboardingState::completed();
            self.focused_panel = Panel::Chat;
            self.chat_focused = true;
        }

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
                    if let Some(zoomed) = self.zoomed_panel {
                        self.focused_panel = zoomed;
                    }
                    // Otherwise preserve existing focused_panel
                    self.worker_selection = 0;
                    self.signal_selection = 0;
                }
            }
            // Clamp: if focused on Reviews but new workspace has no review queue, fall back
            if self.focused_panel == Panel::Reviews && !self.has_review_queue() {
                self.focused_panel = Panel::Signals;
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
        let has_shells = self.current_ws_has_shells();
        self.focused_panel = match self.focused_panel {
            Panel::Home => Panel::Workers,
            Panel::Workers => {
                if has_shells {
                    Panel::Shells
                } else if self.has_review_queue() {
                    Panel::Reviews
                } else {
                    Panel::Signals
                }
            }
            Panel::Shells => {
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
        let has_shells = self.current_ws_has_shells();
        self.focused_panel = match self.focused_panel {
            Panel::Home => Panel::Chat,
            Panel::Workers => Panel::Home,
            Panel::Shells => Panel::Workers,
            Panel::Reviews => {
                if has_shells {
                    Panel::Shells
                } else {
                    Panel::Workers
                }
            }
            Panel::Signals => {
                if self.has_review_queue() {
                    Panel::Reviews
                } else if has_shells {
                    Panel::Shells
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
            Panel::Shells => {
                let count = self.current_ws().map_or(0, |ws| ws.shell_windows.len());
                if count > 0 && self.shell_selection + 1 < count {
                    self.shell_selection += 1;
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
            Panel::Shells => {
                self.shell_selection = self.shell_selection.saturating_sub(1);
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

    // ── Kanban navigation ─────────────────────────────────

    const KANBAN_STAGES: [KanbanStage; 3] = [
        KanbanStage::InProgress,
        KanbanStage::InReview,
        KanbanStage::HumanReview,
    ];

    /// Move kanban selection to the next non-empty column (right).
    pub fn kanban_navigate_right(&mut self) {
        let ws = match self.current_ws() {
            Some(w) => w,
            None => return,
        };
        let current_stage = ws
            .kanban_selected
            .map(|(s, _)| s)
            .unwrap_or(Self::KANBAN_STAGES[0]);
        let current_idx = Self::KANBAN_STAGES
            .iter()
            .position(|&s| s == current_stage)
            .unwrap_or(0);
        for &stage in &Self::KANBAN_STAGES[current_idx + 1..] {
            if ws.kanban_cards.iter().any(|c| c.stage == stage) {
                if let Some(ws) = self.current_ws_mut() {
                    ws.kanban_selected = Some((stage, 0));
                }
                self.needs_redraw = true;
                return;
            }
        }
    }

    /// Move kanban selection to the previous non-empty column (left).
    pub fn kanban_navigate_left(&mut self) {
        let ws = match self.current_ws() {
            Some(w) => w,
            None => return,
        };
        let current_stage = ws
            .kanban_selected
            .map(|(s, _)| s)
            .unwrap_or(Self::KANBAN_STAGES[0]);
        let current_idx = Self::KANBAN_STAGES
            .iter()
            .position(|&s| s == current_stage)
            .unwrap_or(0);
        for &stage in Self::KANBAN_STAGES[..current_idx].iter().rev() {
            if ws.kanban_cards.iter().any(|c| c.stage == stage) {
                if let Some(ws) = self.current_ws_mut() {
                    ws.kanban_selected = Some((stage, 0));
                }
                self.needs_redraw = true;
                return;
            }
        }
    }

    /// Move kanban selection down within the current column (wraps).
    pub fn kanban_navigate_down(&mut self) {
        let ws = match self.current_ws() {
            Some(w) => w,
            None => return,
        };
        let (stage, idx) = match ws.kanban_selected {
            Some(s) => s,
            None => {
                // Auto-select first card in first non-empty column
                for &stage in &Self::KANBAN_STAGES {
                    if ws.kanban_cards.iter().any(|c| c.stage == stage) {
                        if let Some(ws) = self.current_ws_mut() {
                            ws.kanban_selected = Some((stage, 0));
                        }
                        self.needs_redraw = true;
                        return;
                    }
                }
                return;
            }
        };
        let visible = ws_kanban_visible_count(ws, stage, self.kanban_allocated_height.get());
        if visible > 0 {
            let clamped = idx.min(visible - 1);
            let new_idx = (clamped + 1) % visible;
            if let Some(ws) = self.current_ws_mut() {
                ws.kanban_selected = Some((stage, new_idx));
            }
            self.needs_redraw = true;
        }
    }

    /// Move kanban selection up within the current column (wraps).
    pub fn kanban_navigate_up(&mut self) {
        let ws = match self.current_ws() {
            Some(w) => w,
            None => return,
        };
        let (stage, idx) = match ws.kanban_selected {
            Some(s) => s,
            None => {
                // Auto-select first card in first non-empty column
                for &stage in &Self::KANBAN_STAGES {
                    if ws.kanban_cards.iter().any(|c| c.stage == stage) {
                        if let Some(ws) = self.current_ws_mut() {
                            ws.kanban_selected = Some((stage, 0));
                        }
                        self.needs_redraw = true;
                        return;
                    }
                }
                return;
            }
        };
        let visible = ws_kanban_visible_count(ws, stage, self.kanban_allocated_height.get());
        if visible > 0 {
            let clamped = idx.min(visible - 1);
            let new_idx = if clamped == 0 {
                visible - 1
            } else {
                clamped - 1
            };
            if let Some(ws) = self.current_ws_mut() {
                ws.kanban_selected = Some((stage, new_idx));
            }
            self.needs_redraw = true;
        }
    }

    /// Return the chat message to inject for the currently selected kanban card, or None.
    pub fn kanban_action_selected(&self) -> Option<String> {
        let ws = self.current_ws()?;
        let (stage, idx) = ws.kanban_selected?;
        let visible = ws_kanban_visible_count(ws, stage, self.kanban_allocated_height.get());
        if idx >= visible {
            return None;
        }
        let card = ws
            .kanban_cards
            .iter()
            .filter(|c| c.stage == stage)
            .nth(idx)?;
        let msg = match stage {
            KanbanStage::HumanReview => {
                if card.source == "worker" {
                    // subtitle like "PR #42 · merge?"
                    if let Some(num) = kanban_extract_pr_number(&card.subtitle) {
                        format!("Merge PR #{num}")
                    } else {
                        format!("What's the status of {}?", card.title)
                    }
                } else if card.source == "github_review_queue" {
                    format!("Review {}", card.title)
                } else {
                    format!("Tell me about this signal: {}", card.title)
                }
            }
            KanbanStage::InReview => {
                if let Some(num) = kanban_extract_pr_number(&card.subtitle) {
                    format!("What's the review status of PR #{num}?")
                } else {
                    format!("What's the review status of {}?", card.title)
                }
            }
            KanbanStage::InProgress => {
                if card.source == "worker" {
                    let worker_id = card.id.strip_prefix("worker:").unwrap_or(&card.id);
                    format!("What's the status of worker {worker_id}?")
                } else {
                    format!("Tell me about this signal: {}", card.title)
                }
            }
        };
        Some(msg)
    }

    /// Dismiss the currently selected kanban card (remove from board).
    pub fn kanban_dismiss_selected(&mut self) {
        let allocated_h = self.kanban_allocated_height.get();
        let ws = match self.current_ws_mut() {
            Some(w) => w,
            None => return,
        };
        let (stage, idx) = match ws.kanban_selected {
            Some(s) => s,
            None => return,
        };
        let visible = ws_kanban_visible_count(ws, stage, allocated_h);
        if idx >= visible {
            return;
        }
        let card_id = ws
            .kanban_cards
            .iter()
            .filter(|c| c.stage == stage)
            .nth(idx)
            .map(|c| c.id.clone());
        if let Some(id) = card_id {
            ws.kanban_dismissed.insert(id.clone());
            ws.kanban_cards.retain(|c| c.id != id);
            let count = ws.kanban_cards.iter().filter(|c| c.stage == stage).count();
            ws.kanban_selected = if count == 0 {
                None
            } else {
                Some((stage, idx.min(count - 1)))
            };
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

        let shell_count = self.current_ws().map_or(0, |ws| ws.shell_windows.len());
        if shell_count == 0 {
            self.shell_selection = 0;
        } else if self.shell_selection >= shell_count {
            self.shell_selection = shell_count - 1;
        }

        // Clamp kanban selection to the visible range (not just total count).
        // This ensures Enter/Dismiss never act on an off-screen card.
        let allocated_h = self.kanban_allocated_height.get();
        if let Some(ws) = self.current_ws_mut()
            && let Some((stage, idx)) = ws.kanban_selected
        {
            let visible = ws_kanban_visible_count(ws, stage, allocated_h);
            ws.kanban_selected = if visible == 0 {
                None
            } else if idx >= visible {
                Some((stage, visible - 1))
            } else {
                Some((stage, idx))
            };
        }
    }

    // ── View transitions ──────────────────────────────────

    pub fn enter_worker_detail(&mut self, idx: usize) {
        self.view = View::WorkerDetail(idx);
        self.content_scroll = 0;
        self.worker_input.clear();
        self.worker_input_active = false;
        self.worker_activity_focused = false;
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

    /// Whether the current workspace has shells enabled in config.
    /// The panel is shown even if tmux is not installed (with a helpful message).
    pub fn current_ws_has_shells(&self) -> bool {
        self.current_ws().is_some_and(|ws| ws.config.shells.enabled)
    }

    /// Whether tmux is actually available for the current workspace.
    pub fn current_ws_tmux_available(&self) -> bool {
        self.current_ws()
            .is_some_and(|ws| ws.tmux.as_ref().is_some_and(|tmux| tmux.is_available()))
    }

    pub fn back_to_dashboard(&mut self) {
        match self.view {
            View::WorkerDetail(_) | View::WorkerChat(_) | View::PrList => {
                self.focused_panel = Panel::Workers
            }
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
            Panel::Home | Panel::Shells => {} // Handled elsewhere or no drill-in
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

    /// Tasks that have a PR URL set.
    pub fn tasks_with_prs(&self) -> Vec<&crate::buzz::task::Task> {
        match self.current_ws() {
            Some(ws) => ws
                .tasks
                .iter()
                .filter(|t| t.pr_url.as_ref().is_some_and(|u| !u.is_empty()))
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

    pub fn push_system_message(&mut self, text: String) {
        if let Some(ws) = self.current_ws_mut() {
            ws.chat_history.push(ChatLine::System(text));
            ws.streaming = false;
            let queued = std::mem::take(&mut ws.pending_notifications);
            for note in queued {
                ws.chat_history.push(ChatLine::System(note));
                ws.has_unread_response = true;
            }
            self.needs_redraw = true;
        }
    }

    /// Rebuild the workspace name → index cache. Call after any mutation of
    /// `self.workspaces` (push, remove, replace).
    fn rebuild_ws_name_index(&mut self) {
        self.ws_name_index.clear();
        for (i, ws) in self.workspaces.iter().enumerate() {
            self.ws_name_index.insert(ws.name.clone(), i);
        }
    }

    /// Find workspace index by name (O(1) via cached HashMap).
    /// Falls back to active_tab only when the workspace string is empty
    /// (system-wide messages or old daemons without workspace field).
    /// Returns `None` when a non-empty name doesn't match any workspace so
    /// callers silently drop the message rather than mis-routing it.
    fn ws_index(&self, workspace: &str) -> Option<usize> {
        if workspace.is_empty() {
            Some(self.active_tab)
        } else {
            self.ws_name_index.get(workspace).copied()
        }
    }

    /// Append a streaming token to the correct workspace's chat history.
    ///
    /// Only appends to an existing Assistant entry if it came from the TUI's
    /// own streaming (source=None). Activity-sourced entries (Telegram, System)
    /// are treated as complete — a new entry is created instead.
    pub fn append_assistant_token_to(&mut self, workspace: &str, token: &str) {
        if let Some(idx) = self.ws_index(workspace)
            && let Some(ws) = self.workspaces.get_mut(idx)
        {
            let should_append = matches!(
                ws.chat_history.last(),
                Some(ChatLine::Assistant(_, _, src))
                    if !matches!(src, Some(MessageSource::Telegram) | Some(MessageSource::System))
            );
            if should_append {
                if let Some(ChatLine::Assistant(s, _, _)) = ws.chat_history.last_mut() {
                    s.push_str(token);
                }
            } else {
                ws.chat_history
                    .push(ChatLine::Assistant(token.to_string(), now_ts(), None));
            }
            ws.chat_scroll.scroll_to_bottom();
            self.needs_redraw = true;
        }
    }

    /// Finish the assistant message on the correct workspace.
    pub fn finish_assistant_message_for(&mut self, workspace: &str) {
        if let Some(idx) = self.ws_index(workspace)
            && let Some(ws) = self.workspaces.get_mut(idx)
        {
            let is_active = idx == self.active_tab;
            let is_chat_visible = is_active && matches!(self.view, View::Dashboard);
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

            // Flush any notifications that arrived during streaming.
            let queued = std::mem::take(&mut ws.pending_notifications);
            for note in queued {
                ws.chat_history.push(ChatLine::System(note));
                ws.has_unread_response = true;
            }

            self.needs_redraw = true;
        }
    }

    /// Push a system/error message to the correct workspace's chat history.
    pub fn push_system_message_to(&mut self, workspace: &str, text: String) {
        if let Some(idx) = self.ws_index(workspace)
            && let Some(ws) = self.workspaces.get_mut(idx)
        {
            ws.chat_history.push(ChatLine::System(text));
            ws.streaming = false;
            let queued = std::mem::take(&mut ws.pending_notifications);
            for note in queued {
                ws.chat_history.push(ChatLine::System(note));
                ws.has_unread_response = true;
            }
            self.needs_redraw = true;
        }
    }

    /// Update token usage stats for a workspace.
    pub fn update_usage(
        &mut self,
        workspace: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        total_cost_usd: Option<f64>,
        context_window: u64,
    ) {
        if let Some(idx) = self.ws_index(workspace)
            && let Some(ws) = self.workspaces.get_mut(idx)
        {
            ws.usage_input_tokens = input_tokens;
            ws.usage_output_tokens = output_tokens;
            ws.usage_cache_read_tokens = cache_read_tokens;
            ws.usage_cost_usd = total_cost_usd;
            ws.usage_context_window = context_window;
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
                "notification" => {
                    // Signal arrival notifications (e.g. "! 3 new signals: github (2)").
                    // Follow-through *responses* are now broadcast as "assistant_message".
                    if ws.streaming {
                        ws.pending_notifications.push(text.to_string());
                    } else {
                        ws.chat_history.push(ChatLine::System(text.to_string()));
                        ws.has_unread_response = true;
                    }
                }
                "watcher_poll_complete" => {
                    // Update feed heartbeat timestamps from daemon's watcher cursors.
                    // Text format: "watcher1=2024-01-01T00:00:00Z,watcher2=..."
                    let now_utc = Utc::now();
                    for entry in text.split(',') {
                        if let Some((watcher, ts_str)) = entry.split_once('=') {
                            if !is_top_level_watcher(watcher) {
                                continue;
                            }
                            let when = chrono::DateTime::parse_from_rfc3339(ts_str)
                                .map(|dt| dt.with_timezone(&Utc))
                                .unwrap_or(now_utc);
                            // Update or insert the heartbeat feed item for this watcher
                            let feed_text = format!("{watcher} checked");
                            if let Some(existing) = ws.feed.iter_mut().find(|item| {
                                matches!(&item.kind, FeedKind::Heartbeat) && item.text == feed_text
                            }) {
                                existing.when = when;
                            } else {
                                ws.feed.push(FeedItem {
                                    when,
                                    kind: FeedKind::Heartbeat,
                                    text: feed_text,
                                });
                            }
                        }
                    }
                    // Re-sort so newest items are first
                    ws.feed.sort_by(|a, b| b.when.cmp(&a.when));
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
        if let View::WorkerDetail(idx) | View::WorkerChat(idx) = self.view
            && let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            worker.conv_scroll.scroll_up(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_worker_conv_down(&mut self, amount: u16) {
        if let View::WorkerDetail(idx) | View::WorkerChat(idx) = self.view
            && let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            worker.conv_scroll.scroll_down(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_worker_activity_up(&mut self, amount: u16) {
        if let View::WorkerDetail(idx) | View::WorkerChat(idx) = self.view
            && let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            worker.activity_scroll.scroll_up(amount as u32);
            self.needs_redraw = true;
        }
    }

    pub fn scroll_worker_activity_down(&mut self, amount: u16) {
        if let View::WorkerDetail(idx) | View::WorkerChat(idx) = self.view
            && let Some(ws) = self.workspaces.get_mut(self.active_tab)
            && let Some(worker) = ws.workers.get_mut(idx)
        {
            worker.activity_scroll.scroll_down(amount as u32);
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
            // Rebuild kanban cards from updated state (prune stale dismissed IDs first)
            prune_kanban_dismissed(ws);
            ws.kanban_cards = build_kanban_cards(ws);
        }
        self.last_signal_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Apply worker data from background refresh.
    /// Returns any pending tmux operations that should be executed off the main thread.
    pub(super) fn apply_worker_update(
        &mut self,
        data: Vec<(String, Vec<WorkerInfo>)>,
    ) -> Vec<PendingShellOp> {
        let mut shell_ops = Vec::new();
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

                // Tmux auto-worker-shells: collect create/kill ops for async execution.
                if ws.config.shells.enabled
                    && ws.config.shells.auto_worker_shells
                    && !is_first_load
                    && let Some(ref tmux) = ws.tmux
                    && tmux.is_available()
                {
                    let old_ids: std::collections::HashSet<&String> =
                        ws.prev_worker_phases.keys().collect();
                    let new_ids: std::collections::HashSet<&String> =
                        new_workers.iter().map(|w| &w.id).collect();

                    // New workers: queue create ops
                    for worker in &new_workers {
                        if !old_ids.contains(&worker.id) {
                            let wt_path = ws.config.root.join(".swarm").join("wt").join(&worker.id);
                            shell_ops.push(PendingShellOp::Create {
                                tmux: tmux.clone(),
                                name: worker.id.clone(),
                                working_dir: wt_path,
                            });
                        }
                    }

                    // Closed workers: queue kill ops
                    for old_id in &old_ids {
                        if !new_ids.contains(*old_id) {
                            shell_ops.push(PendingShellOp::Kill {
                                tmux: tmux.clone(),
                                name: (*old_id).clone(),
                            });
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
                // Rebuild kanban cards from updated state (prune stale dismissed IDs first)
                prune_kanban_dismissed(ws);
                ws.kanban_cards = build_kanban_cards(ws);
            }
        }
        self.last_worker_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
        shell_ops
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
                // Rebuild kanban cards from updated state (prune stale dismissed IDs first)
                prune_kanban_dismissed(ws);
                ws.kanban_cards = build_kanban_cards(ws);
            }
        }
        self.last_signal_refresh = Instant::now();
        self.clamp_selections();
        self.needs_redraw = true;
    }

    /// Apply task data from background refresh.
    pub(super) fn apply_task_update(&mut self, data: Vec<(String, Vec<crate::buzz::task::Task>)>) {
        for (name, tasks) in data {
            if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == name) {
                ws.tasks = tasks;
                prune_kanban_dismissed(ws);
                ws.kanban_cards = build_kanban_cards(ws);
            }
        }
        self.needs_redraw = true;
    }

    /// Apply activity feed data from background refresh.
    pub(super) fn apply_activity_update(
        &mut self,
        data: Vec<(String, Vec<crate::buzz::task::ActivityEvent>)>,
    ) {
        for (name, events) in data {
            if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == name) {
                ws.activity_events = events;
            }
        }
        self.needs_redraw = true;
    }

    /// Apply a task timeline update to the detail panel.
    pub(super) fn apply_task_timeline_update(
        &mut self,
        workspace: &str,
        task_id: &str,
        events: Vec<crate::buzz::task::ActivityEvent>,
    ) {
        if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.name == workspace)
            && let Some(ref mut detail) = ws.viewing_task
            && detail.task_id == task_id
        {
            detail.events = events;
        }
        self.needs_redraw = true;
    }

    /// Append a line to the live worker output panel for the given workspace/worker.
    pub(super) fn apply_worker_output_line(
        &mut self,
        workspace_name: &str,
        worker_id: &str,
        line: OutputLine,
    ) {
        if let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.name == workspace_name)
            && let Some(ref mut out) = ws.viewing_worker_output
            && out.worker_id == worker_id
        {
            out.push(line);
            if out.auto_scroll {
                let total = out.lines.len();
                out.scroll = total.saturating_sub(1);
            }
            self.needs_redraw = true;
        }
    }

    /// Open the task detail panel for the currently selected kanban card.
    ///
    /// Returns `Some((workspace_name, task_id))` if a task card was selected,
    /// so the caller can spawn a background timeline load.
    pub fn open_task_detail_for_selected(&mut self) -> Option<(String, String)> {
        let allocated_h = self.kanban_allocated_height.get();
        let ws = self.current_ws_mut()?;
        let (stage, idx) = ws.kanban_selected?;
        let visible = ws_kanban_visible_count(ws, stage, allocated_h);
        if idx >= visible {
            return None;
        }
        let card = ws
            .kanban_cards
            .iter()
            .filter(|c| c.stage == stage)
            .nth(idx)?;
        let task_id = card.id.strip_prefix("task:")?.to_string();
        let title = card.title.clone();

        // Look up full task for extra metadata
        let task = ws.tasks.iter().find(|t| t.id == task_id);
        let worker_id = task.and_then(|t| t.worker_id.clone());
        let pr_number = task.and_then(|t| t.pr_number);
        let stage_str = task
            .map(|t| t.stage.as_str().to_string())
            .unwrap_or_default();
        let workspace_name = ws.name.clone();

        ws.viewing_task = Some(TaskDetailState {
            task_id: task_id.clone(),
            task_title: title,
            stage: stage_str,
            worker_id,
            pr_number,
            events: Vec::new(),
            scroll: 0,
        });
        self.needs_redraw = true;
        Some((workspace_name, task_id))
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

                // Sort newest-first, then dedup heartbeats by text so only
                // the freshest entry for each watcher survives.  This
                // prevents stale heartbeat items from persisting across
                // refresh cycles.
                feed.sort_by(|a, b| b.when.cmp(&a.when));
                let mut seen_heartbeats = std::collections::HashSet::new();
                feed.retain(|item| {
                    if matches!(&item.kind, FeedKind::Heartbeat) {
                        seen_heartbeats.insert(item.text.clone())
                    } else {
                        true
                    }
                });
                feed.truncate(20);

                if tracing::enabled!(tracing::Level::DEBUG) {
                    for item in &feed {
                        if matches!(&item.kind, FeedKind::Heartbeat) {
                            tracing::debug!(
                                workspace = %name,
                                text = %item.text,
                                when = %item.when,
                                "heartbeat feed item applied"
                            );
                        }
                    }
                }

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
                // Only apply if the user hasn't started chatting in THIS TUI session.
                // Messages from daemon activity broadcasts (Telegram, System) should
                // NOT block history loading — they arrive via the socket before the
                // background history task completes.
                let has_local_user = ws.chat_history.iter().any(|m| {
                    matches!(
                        m,
                        ChatLine::User(_, _, src)
                            if !matches!(src, Some(MessageSource::Telegram) | Some(MessageSource::System))
                    )
                });
                // Count only assistant messages from direct TUI interaction (source=None).
                // Telegram/System-sourced assistants come from daemon broadcasts.
                let real_assistant_count = ws
                    .chat_history
                    .iter()
                    .filter(|m| {
                        matches!(
                            m,
                            ChatLine::Assistant(_, _, src)
                                if !matches!(src, Some(MessageSource::System) | Some(MessageSource::Telegram))
                        )
                    })
                    .count();
                // The single onboarding assistant message doesn't count as real chat.
                let has_real_assistant = real_assistant_count > 0
                    && !(self.onboarding.active && real_assistant_count <= 1);
                if has_local_user || has_real_assistant {
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
                tmux_session: if ws.config.shells.enabled {
                    Some(crate::shells::TmuxManager::session_name_for(
                        &ws.name,
                        &ws.config.shells,
                    ))
                } else {
                    None
                },
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
            View::WorkerDetail(i) | View::WorkerChat(i) => self.current_ws()?.workers.get(*i),
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
        // PR list: source URLs from tasks (survives worker closure).
        if self.view == View::PrList {
            let tasks = self.tasks_with_prs();
            return tasks
                .get(self.pr_list_selection)
                .and_then(|t| t.pr_url.clone());
        }
        // Kanban panel: open the selected card's URL (pr_url preferred over source_url).
        if self.view == View::Dashboard
            && self.focused_panel == Panel::Home
            && let Some(ws) = self.current_ws()
            && let Some((stage, idx)) = ws.kanban_selected
        {
            let card = ws.kanban_cards.iter().filter(|c| c.stage == stage).nth(idx);
            if let Some(card) = card {
                return card.url.clone();
            }
        }
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
/// First-run greeting from Bee — single source of truth for the initial setup prompt.
pub(super) fn setup_greeting(dir_display: &str) -> String {
    format!(
        "Hi! I'm Bee \u{1f41d} \u{2014} your dev workspace coordinator.\n\n\
         apiari watches your repos, dispatches AI coding agents, and keeps \
         you in the loop \u{2014} all from this dashboard.\n\n\
         Let's get you set up. Where's your project?\n\
         (Press Enter for {dir_display})"
    )
}

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

    // [watchers]
    let mut watchers = Table::new();

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
                activity: Vec::new(), // populated after load
                activity_scroll: ScrollState::new(),
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

/// Build activity events from the data already available in `WorkerInfo`.
fn build_worker_activity(worker: &WorkerInfo) -> Vec<WorkerEvent> {
    let mut events = Vec::new();

    // 1. Task dispatched (from prompt + created_at)
    events.push(WorkerEvent {
        ts: worker.created_at,
        kind: WorkerEventKind::Dispatched,
        text: truncate_preview(&worker.prompt, 120),
    });

    // 2. Phase changes → status entries
    if let Some(ref phase) = worker.phase {
        let phase_text = match phase.as_str() {
            "running" => "Agent is running",
            "waiting" => "Waiting for input",
            "completed" => "Task completed",
            "failed" => "Task failed",
            _ => phase.as_str(),
        };
        events.push(WorkerEvent {
            ts: None,
            kind: WorkerEventKind::StatusChange,
            text: phase_text.to_string(),
        });
    }

    // 3. PR opened
    if let Some(ref pr) = worker.pr {
        events.push(WorkerEvent {
            ts: None,
            kind: WorkerEventKind::PrOpened,
            text: format!("PR #{} opened: {}", pr.number, pr.title),
        });

        // 4. Check if merged
        if pr.state == "MERGED" || pr.state == "merged" {
            events.push(WorkerEvent {
                ts: None,
                kind: WorkerEventKind::Merged,
                text: format!("PR #{} merged", pr.number),
            });
        }
    }

    events
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
                worker.activity = build_worker_activity(worker);
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

/// Load active tasks for all workspaces (blocking SQLite queries).
pub(super) fn load_all_tasks_blocking(
    db_path: &Path,
    names: &[String],
) -> Vec<(String, Vec<crate::buzz::task::Task>)> {
    names
        .iter()
        .map(|name| {
            let tasks = if let Ok(store) = crate::buzz::task::store::TaskStore::open(db_path) {
                store.get_active_tasks(name).unwrap_or_default()
            } else {
                Vec::new()
            };
            (name.clone(), tasks)
        })
        .collect()
}

/// Load activity feed events for all workspaces (blocking SQLite queries).
pub(super) fn load_all_activity_blocking(
    db_path: &Path,
    names: &[String],
) -> Vec<(String, Vec<crate::buzz::task::ActivityEvent>)> {
    names
        .iter()
        .map(|name| {
            let events = if let Ok(store) = crate::buzz::task::ActivityEventStore::open(db_path) {
                store.get_activity_feed(name, 50, 0).unwrap_or_default()
            } else {
                Vec::new()
            };
            (name.clone(), events)
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
                        .filter(|row| {
                            // Skip empty assistant responses (tool-only turns)
                            !(row.role == "assistant" && row.content.trim().is_empty())
                        })
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
    use chrono::Utc;

    use super::*;
    use crate::buzz::signal::{Severity, SignalRecord, SignalStatus};

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

    #[test]
    fn test_apply_chat_history_loads_despite_telegram_activity() {
        let mut app = make_app(&["ws1"], false);
        // Simulate a Telegram user_message activity arriving before history loads
        app.workspaces[0].chat_history.push(ChatLine::User(
            "telegram msg".into(),
            "12:00".into(),
            Some(MessageSource::Telegram),
        ));
        // Simulate a Telegram assistant_message activity
        app.workspaces[0].chat_history.push(ChatLine::Assistant(
            "telegram response".into(),
            "12:01".into(),
            Some(MessageSource::Telegram),
        ));

        let history = vec![
            ChatLine::User("old question".into(), "10:00".into(), None),
            ChatLine::Assistant("old answer".into(), "10:01".into(), None),
        ];
        app.apply_chat_history(vec![("ws1".into(), history, None)]);

        // History should load despite Telegram activity messages.
        // 2 dividers + 2 history + 2 re-appended Telegram messages = 6
        assert_eq!(app.workspaces[0].chat_history.len(), 6);
        assert!(matches!(
            &app.workspaces[0].chat_history[0],
            ChatLine::System(s) if s.contains("previous session")
        ));
    }

    #[test]
    fn test_append_assistant_token_skips_telegram_entry() {
        let mut app = make_app(&["ws1"], false);
        // Simulate a Telegram assistant message already in history
        app.workspaces[0].chat_history.push(ChatLine::Assistant(
            "telegram response".into(),
            "12:00".into(),
            Some(MessageSource::Telegram),
        ));

        // Append a streaming token — should NOT append to the Telegram entry
        app.append_assistant_token_to("ws1", "hello from coordinator");

        // Should be 2 entries: telegram + new streaming entry
        assert_eq!(app.workspaces[0].chat_history.len(), 2);
        if let ChatLine::Assistant(content, _, src) = &app.workspaces[0].chat_history[0] {
            assert_eq!(content, "telegram response");
            assert!(matches!(src, Some(MessageSource::Telegram)));
        } else {
            panic!("expected telegram assistant");
        }
        if let ChatLine::Assistant(content, _, src) = &app.workspaces[0].chat_history[1] {
            assert_eq!(content, "hello from coordinator");
            assert!(matches!(src, None));
        } else {
            panic!("expected streaming assistant");
        }
    }

    // ── Notification queuing tests ────────────────────────────

    #[test]
    fn test_notification_queued_during_streaming() {
        let mut app = make_app(&["ws1"], false);
        app.workspaces[0].streaming = true;

        // Push a notification while streaming — should be queued, not in chat_history
        app.push_activity(
            "ws1",
            "signal",
            "notification",
            "[signal: github] PR reviewed",
        );

        assert!(app.workspaces[0].pending_notifications.len() == 1);
        assert_eq!(
            app.workspaces[0].pending_notifications[0],
            "[signal: github] PR reviewed"
        );
        // Should NOT appear in chat_history yet
        assert!(app.workspaces[0].chat_history.is_empty());
    }

    #[test]
    fn test_notification_shown_immediately_when_not_streaming() {
        let mut app = make_app(&["ws1"], false);
        assert!(!app.workspaces[0].streaming);

        app.push_activity(
            "ws1",
            "signal",
            "notification",
            "[signal: github] PR reviewed",
        );

        assert!(app.workspaces[0].pending_notifications.is_empty());
        assert_eq!(app.workspaces[0].chat_history.len(), 1);
        assert!(matches!(
            &app.workspaces[0].chat_history[0],
            ChatLine::System(s) if s == "[signal: github] PR reviewed"
        ));
        assert!(app.workspaces[0].has_unread_response);
    }

    #[test]
    fn test_finish_assistant_message_flushes_queued_notifications() {
        let mut app = make_app(&["ws1"], false);
        // Simulate streaming with a token
        app.workspaces[0].streaming = true;
        app.append_assistant_token_to("ws1", "response text");

        // Queue two notifications during streaming
        app.push_activity("ws1", "signal", "notification", "note 1");
        app.push_activity("ws1", "signal", "notification", "note 2");
        assert_eq!(app.workspaces[0].pending_notifications.len(), 2);

        // Finish streaming — should flush queued notifications
        app.finish_assistant_message_for("ws1");

        assert!(!app.workspaces[0].streaming);
        assert!(app.workspaces[0].pending_notifications.is_empty());
        // chat_history: 1 assistant + 2 flushed notifications = 3
        assert_eq!(app.workspaces[0].chat_history.len(), 3);
        assert!(matches!(
            &app.workspaces[0].chat_history[1],
            ChatLine::System(s) if s == "note 1"
        ));
        assert!(matches!(
            &app.workspaces[0].chat_history[2],
            ChatLine::System(s) if s == "note 2"
        ));
    }

    #[test]
    fn test_push_system_message_to_flushes_queued_notifications() {
        let mut app = make_app(&["ws1"], false);
        app.workspaces[0].streaming = true;

        // Queue a notification
        app.push_activity("ws1", "signal", "notification", "queued note");
        assert_eq!(app.workspaces[0].pending_notifications.len(), 1);

        // Error ends streaming via push_system_message_to
        app.push_system_message_to("ws1", "Error occurred".into());

        assert!(!app.workspaces[0].streaming);
        assert!(app.workspaces[0].pending_notifications.is_empty());
        // chat_history: 1 error system msg + 1 flushed notification = 2
        assert_eq!(app.workspaces[0].chat_history.len(), 2);
        assert!(matches!(
            &app.workspaces[0].chat_history[1],
            ChatLine::System(s) if s == "queued note"
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
    fn test_setup_default_root() {
        // First-run: just press Enter to accept default root → done in one step
        let (_, toml_str) = run_setup(&[
            "", // accept default root
        ]);
        let cfg = assert_valid_config(&toml_str);
        assert_eq!(cfg.swarm.default_agent, "claude");
        assert!(cfg.telegram.is_none());
    }

    #[test]
    fn test_setup_custom_root() {
        let (_, toml_str) = run_setup(&[
            "/tmp/myproject", // custom root
        ]);
        let cfg = assert_valid_config(&toml_str);
        assert_eq!(cfg.root, std::path::PathBuf::from("/tmp/myproject"));
        assert_eq!(cfg.swarm.default_agent, "claude");
    }

    #[test]
    fn test_setup_toml_special_chars_in_root() {
        // Paths with quotes/backslashes should be safely encoded
        let (_, toml_str) = run_setup(&["/tmp/my \"project\""]);
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

    #[test]
    fn test_add_workspace_flow_steps() {
        // Test the step transitions for the add_workspace=true path
        let mut app = App::new_setup();
        // Override to add-workspace mode
        app.setup.as_mut().unwrap().add_workspace = true;

        // AskRoot → AskName (simplified flow, not AskProvider)
        let done = app.process_setup_input(""); // accept default root
        assert!(!done);
        assert_eq!(app.setup.as_ref().unwrap().step, SetupStep::AskName);

        // AskName with default → Done
        let done = app.process_setup_input(""); // accept default name
        assert!(done);
        let setup = app.setup.as_ref().unwrap();
        assert_eq!(setup.step, SetupStep::Done);
        // TOML should be valid
        let toml_str = build_setup_toml(setup);
        assert_valid_config(&toml_str);
    }

    #[test]
    fn test_add_workspace_flow_custom_name() {
        let mut app = App::new_setup();
        app.setup.as_mut().unwrap().add_workspace = true;

        // AskRoot with custom path
        let done = app.process_setup_input("/tmp/my-project");
        assert!(!done);
        assert_eq!(app.setup.as_ref().unwrap().step, SetupStep::AskName);
        assert_eq!(
            app.setup.as_ref().unwrap().workspace_root,
            std::path::PathBuf::from("/tmp/my-project")
        );

        // AskName with custom name
        let done = app.process_setup_input("custom-ws");
        assert!(done);
        assert_eq!(app.setup.as_ref().unwrap().workspace_name, "custom-ws");
    }

    #[test]
    fn test_add_workspace_placeholder_flag() {
        let mut app = App::new_setup();
        // new_setup sets is_setup_placeholder = true
        assert!(app.workspaces[0].is_setup_placeholder);

        // enter_add_workspace also sets the flag
        let config: crate::config::WorkspaceConfig =
            serde_json::from_str(r#"{"root":"/tmp"}"#).unwrap();
        let ws = WorkspaceState {
            name: "existing".into(),
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
            usage_input_tokens: 0,
            usage_output_tokens: 0,
            usage_cache_read_tokens: 0,
            usage_cost_usd: None,
            usage_context_window: 0,
            prev_worker_phases: Default::default(),
            prev_signal_ids: Default::default(),
            prev_pr_workers: Default::default(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: ScrollState::new(),
            thoughts: Vec::new(),
            is_setup_placeholder: false,
            tmux: None,
            shell_windows: Vec::new(),
            pending_notifications: Vec::new(),
            kanban_cards: Vec::new(),
            kanban_selected: None,
            kanban_dismissed: Default::default(),
            tasks: Vec::new(),
            triage_sidebar_open: true,
            triage_selected: 0,
            triage_scroll: 0,
            sidebar_view: SidebarView::Triage,
            activity_events: Vec::new(),
            activity_selected: 0,
            activity_scroll: 0,
            viewing_task: None,
            viewing_worker_output: None,
        };
        app.workspaces = vec![ws];
        app.setup = None;
        app.enter_add_workspace(std::path::PathBuf::from("/tmp/new"), None);
        assert_eq!(app.workspaces.len(), 2);
        assert!(!app.workspaces[0].is_setup_placeholder);
        assert!(app.workspaces[1].is_setup_placeholder);
    }

    fn test_worker(prompt: &str, phase: Option<&str>, pr: Option<PrInfo>) -> WorkerInfo {
        WorkerInfo {
            id: "w-123".into(),
            branch: "swarm/test".into(),
            prompt: prompt.into(),
            agent_kind: "claude".into(),
            phase: phase.map(|s| s.into()),
            agent_session_status: None,
            summary: None,
            created_at: Some(Local::now()),
            pr,
            last_activity: None,
            conversation: Vec::new(),
            conv_scroll: ScrollState::new(),
            activity: Vec::new(),
            activity_scroll: ScrollState::new(),
        }
    }

    #[test]
    fn test_build_worker_activity_basic_ordering() {
        let worker = test_worker("Fix the login bug", Some("running"), None);
        let events = build_worker_activity(&worker);

        // Should have at least: dispatched + status change
        assert!(events.len() >= 2);
        assert!(matches!(events[0].kind, WorkerEventKind::Dispatched));
        assert!(events[0].text.contains("Fix the login bug"));
        assert!(matches!(events[1].kind, WorkerEventKind::StatusChange));
    }

    #[test]
    fn test_build_worker_activity_with_pr() {
        let pr = PrInfo {
            number: 42,
            title: "fix: login bug".into(),
            state: "OPEN".into(),
            url: "https://github.com/test/repo/pull/42".into(),
        };
        let worker = test_worker("Fix the login bug", Some("running"), Some(pr));
        let events = build_worker_activity(&worker);

        // Should have dispatched + status + PR opened
        let pr_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.kind, WorkerEventKind::PrOpened))
            .collect();
        assert_eq!(pr_events.len(), 1);
        assert!(pr_events[0].text.contains("#42"));
    }

    #[test]
    fn test_build_worker_activity_merged_state() {
        let pr = PrInfo {
            number: 42,
            title: "fix: login bug".into(),
            state: "MERGED".into(),
            url: "https://github.com/test/repo/pull/42".into(),
        };
        let worker = test_worker("Fix the login bug", Some("completed"), Some(pr));
        let events = build_worker_activity(&worker);

        // Should detect merged state
        let merged: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.kind, WorkerEventKind::Merged))
            .collect();
        assert_eq!(merged.len(), 1);
        assert!(merged[0].text.contains("#42"));
    }

    #[test]
    fn test_build_worker_activity_no_phase() {
        let worker = test_worker("Fix something", None, None);
        let events = build_worker_activity(&worker);

        // Only dispatched, no status change
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].kind, WorkerEventKind::Dispatched));
    }

    #[test]
    fn test_build_worker_activity_event_order() {
        let pr = PrInfo {
            number: 10,
            title: "feat: something".into(),
            state: "MERGED".into(),
            url: "https://github.com/test/repo/pull/10".into(),
        };
        let worker = test_worker("Add feature", Some("completed"), Some(pr));
        let events = build_worker_activity(&worker);

        // Verify ordering: dispatched first, then status, then PR, then merged
        let kinds: Vec<_> = events
            .iter()
            .map(|e| std::mem::discriminant(&e.kind))
            .collect();
        let dispatched_pos = kinds
            .iter()
            .position(|k| *k == std::mem::discriminant(&WorkerEventKind::Dispatched))
            .unwrap();
        let pr_pos = kinds
            .iter()
            .position(|k| *k == std::mem::discriminant(&WorkerEventKind::PrOpened))
            .unwrap();
        let merged_pos = kinds
            .iter()
            .position(|k| *k == std::mem::discriminant(&WorkerEventKind::Merged))
            .unwrap();
        assert!(dispatched_pos < pr_pos);
        assert!(pr_pos < merged_pos);
    }

    // ── heartbeat dedup tests ─────────────────────────────

    #[test]
    fn test_heartbeat_dedup_keeps_freshest() {
        let old = Utc::now() - chrono::Duration::hours(10);
        let fresh = Utc::now() - chrono::Duration::seconds(30);

        let mut feed = vec![
            FeedItem {
                when: old,
                kind: FeedKind::Heartbeat,
                text: "github checked".into(),
            },
            FeedItem {
                when: fresh,
                kind: FeedKind::Heartbeat,
                text: "github checked".into(),
            },
            FeedItem {
                when: old,
                kind: FeedKind::Heartbeat,
                text: "sentry checked".into(),
            },
            FeedItem {
                when: Utc::now(),
                kind: FeedKind::Signal,
                text: "some signal".into(),
            },
        ];

        // Replicate the dedup logic from apply_extras_update
        feed.sort_by(|a, b| b.when.cmp(&a.when));
        let mut seen_heartbeats = std::collections::HashSet::new();
        feed.retain(|item| {
            if matches!(&item.kind, FeedKind::Heartbeat) {
                seen_heartbeats.insert(item.text.clone())
            } else {
                true
            }
        });

        // Signal survives untouched
        assert_eq!(
            feed.iter()
                .filter(|i| matches!(&i.kind, FeedKind::Signal))
                .count(),
            1
        );

        // Each watcher has exactly one heartbeat (the freshest)
        let hb: Vec<_> = feed
            .iter()
            .filter(|i| matches!(&i.kind, FeedKind::Heartbeat))
            .collect();
        assert_eq!(hb.len(), 2, "expected one heartbeat per watcher");

        let github_hb = hb.iter().find(|i| i.text == "github checked").unwrap();
        assert_eq!(
            github_hb.when, fresh,
            "stale github heartbeat should be removed"
        );

        let sentry_hb = hb.iter().find(|i| i.text == "sentry checked").unwrap();
        assert_eq!(sentry_hb.when, old, "sole sentry heartbeat should survive");
    }

    // ── build_kanban_cards tests ─────────────────────────────

    fn make_worker(id: &str, phase: &str, pr: Option<PrInfo>) -> WorkerInfo {
        WorkerInfo {
            id: id.to_string(),
            branch: format!("swarm/{id}"),
            prompt: "test".to_string(),
            agent_kind: "claude".to_string(),
            phase: Some(phase.to_string()),
            agent_session_status: None,
            summary: None,
            created_at: Some(chrono::Local::now()),
            pr,
            last_activity: None,
            conversation: Vec::new(),
            conv_scroll: apiari_tui::scroll::ScrollState::new(),
            activity: Vec::new(),
            activity_scroll: apiari_tui::scroll::ScrollState::new(),
        }
    }

    fn make_pr(number: u64, state: &str) -> PrInfo {
        PrInfo {
            number,
            title: format!("PR #{number}"),
            state: state.to_string(),
            url: format!("https://github.com/test/repo/pull/{number}"),
        }
    }

    fn empty_ws() -> WorkspaceState {
        let config: config::WorkspaceConfig = serde_json::from_str(r#"{"root":"/tmp"}"#).unwrap();
        WorkspaceState {
            name: "test".into(),
            config,
            signals: Vec::new(),
            workers: Vec::new(),
            chat_history: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            chat_scroll: apiari_tui::scroll::ScrollState::new(),
            streaming: false,
            coordinator_preview: None,
            has_unread_response: false,
            coordinator_turns: 0,
            usage_input_tokens: 0,
            usage_output_tokens: 0,
            usage_cache_read_tokens: 0,
            usage_cost_usd: None,
            usage_context_window: 0,
            prev_worker_phases: Default::default(),
            prev_signal_ids: Default::default(),
            prev_pr_workers: Default::default(),
            sparkline_data: vec![0; 24],
            watcher_health: Vec::new(),
            feed: Vec::new(),
            feed_scroll: apiari_tui::scroll::ScrollState::new(),
            thoughts: Vec::new(),
            is_setup_placeholder: false,
            tmux: None,
            shell_windows: Vec::new(),
            pending_notifications: Vec::new(),
            kanban_cards: Vec::new(),
            kanban_selected: None,
            kanban_dismissed: Default::default(),
            tasks: Vec::new(),
            triage_sidebar_open: true,
            triage_selected: 0,
            triage_scroll: 0,
            sidebar_view: SidebarView::Triage,
            activity_events: Vec::new(),
            activity_selected: 0,
            activity_scroll: 0,
            viewing_task: None,
            viewing_worker_output: None,
        }
    }

    #[test]
    fn test_kanban_running_worker_maps_to_in_progress() {
        let mut ws = empty_ws();
        ws.workers = vec![make_worker("abc-1234", "running", None)];
        let cards = build_kanban_cards(&ws);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].stage, KanbanStage::InProgress);
        assert!(cards[0].subtitle.contains("coding"));
    }

    #[test]
    fn test_kanban_worker_with_pr_running_maps_to_in_progress() {
        let mut ws = empty_ws();
        ws.workers = vec![make_worker(
            "abc-1234",
            "running",
            Some(make_pr(42, "open")),
        )];
        let cards = build_kanban_cards(&ws);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].stage, KanbanStage::InProgress);
        assert!(cards[0].subtitle.contains("PR #42"));
    }

    #[test]
    fn test_kanban_waiting_worker_with_pr_maps_to_needs_me() {
        let mut ws = empty_ws();
        ws.workers = vec![make_worker(
            "abc-1234",
            "waiting",
            Some(make_pr(42, "open")),
        )];
        let cards = build_kanban_cards(&ws);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].stage, KanbanStage::HumanReview);
        assert!(cards[0].subtitle.contains("merge?"));
    }

    #[test]
    fn test_kanban_pr_state_case_insensitive() {
        let mut ws = empty_ws();
        // Test uppercase "CLOSED" — done workers disappear from board
        ws.workers = vec![make_worker(
            "abc-1234",
            "running",
            Some(make_pr(42, "CLOSED")),
        )];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());

        // Test "MERGED"
        ws.workers = vec![make_worker(
            "abc-5678",
            "running",
            Some(make_pr(43, "MERGED")),
        )];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());

        // Test mixed case "Closed"
        ws.workers = vec![make_worker(
            "abc-9999",
            "running",
            Some(make_pr(44, "Closed")),
        )];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());
    }

    #[test]
    fn test_kanban_done_cards_not_immediately_filtered() {
        // Completed workers without PR disappear from the board
        let mut ws = empty_ws();
        ws.workers = vec![make_worker("abc-1234", "completed", None)];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());
    }

    #[test]
    fn test_kanban_old_done_signal_filtered() {
        // Signals don't appear in kanban at all (they go to the triage sidebar instead)
        let mut ws = empty_ws();
        let mut sig = make_signal("github_merged_pr", "merge-123", None);
        sig.created_at = Utc::now() - chrono::Duration::hours(1);
        ws.signals = vec![sig];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty(), "Signals should not create kanban cards");
    }

    #[test]
    fn test_kanban_noise_signals_excluded() {
        let mut ws = empty_ws();
        ws.signals = vec![
            make_signal("github_ci_pass", "ci-123", None),
            make_signal("github_bot_review", "bot-456", None),
        ];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty(), "Noise signals should not create cards");
    }

    #[test]
    fn test_kanban_signal_dedup_with_worker_pr() {
        let mut ws = empty_ws();
        ws.workers = vec![make_worker(
            "abc-1234",
            "running",
            Some(make_pr(42, "open")),
        )];
        // CI failure signal — signals don't create kanban cards; only worker card appears
        let mut sig = make_signal("github_ci_failure", "ci-org/repo-42", None);
        sig.metadata = Some(r#"{"pr_number":42}"#.to_string());
        ws.signals = vec![sig];
        let cards = build_kanban_cards(&ws);
        assert_eq!(
            cards.len(),
            1,
            "Only worker card should appear; signals don't create kanban cards"
        );
        assert_eq!(cards[0].id, "worker:abc-1234");
    }

    #[test]
    fn test_kanban_sentry_signal_maps_to_incoming() {
        // Sentry signals now go to the triage sidebar, not kanban
        let mut ws = empty_ws();
        ws.signals = vec![make_signal("sentry", "sentry-err-1", None)];
        let cards = build_kanban_cards(&ws);
        assert!(
            cards.is_empty(),
            "Sentry signals should not create kanban cards"
        );
    }

    #[test]
    fn test_kanban_review_requested_maps_to_needs_me() {
        // Signals no longer create kanban cards — they go to the triage sidebar
        let mut ws = empty_ws();
        ws.signals = vec![make_signal(
            "github_review_queue",
            "rq-org/repo-10",
            Some(r#"{"query_name":"Review Requested","repo":"org/repo","pr_number":10}"#),
        )];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty(), "Signals should not create kanban cards");
    }

    #[test]
    fn test_kanban_signal_icon_mappings() {
        // Signals no longer create kanban cards — they go to the triage sidebar
        let mut ws = empty_ws();

        ws.signals = vec![make_signal("github_release", "rel-v1.0", None)];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());

        ws.signals = vec![make_signal("email", "email-1", None)];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());

        ws.signals = vec![make_signal("notion", "notion-1", None)];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());

        ws.signals = vec![make_signal("linear", "lin-1", None)];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());

        ws.signals = vec![make_signal("unknown_source", "unk-1", None)];
        let cards = build_kanban_cards(&ws);
        assert!(cards.is_empty());
    }

    #[test]
    fn test_kanban_dismissed_cards_filtered() {
        let mut ws = empty_ws();
        ws.workers = vec![
            make_worker("abc-1234", "running", None),
            make_worker("def-5678", "running", None),
        ];
        let cards = build_kanban_cards(&ws);
        assert_eq!(cards.len(), 2);

        // Dismiss the first card
        ws.kanban_dismissed.insert(cards[0].id.clone());
        let cards = build_kanban_cards(&ws);
        assert_eq!(cards.len(), 1, "dismissed card should be filtered out");
        assert!(!cards[0].id.contains("abc-1234"));
    }

    #[test]
    fn test_kanban_reviewer_worker_excluded() {
        let mut ws = empty_ws();
        // A reviewer worker has a prompt starting with "Review PR #"
        let mut reviewer = make_worker("rev-1234", "running", None);
        reviewer.prompt = "Review PR #42: verify the implementation".to_string();
        // A regular worker should still appear
        let regular = make_worker("abc-5678", "running", None);
        ws.workers = vec![reviewer, regular];
        let cards = build_kanban_cards(&ws);
        assert_eq!(
            cards.len(),
            1,
            "reviewer worker should not create a kanban card"
        );
        assert_eq!(cards[0].id, "worker:abc-5678");
    }

    // ── build_kanban_cards_from_tasks tests ──────────────────

    fn make_task_for_test(
        id: &str,
        stage: crate::buzz::task::TaskStage,
        worker_id: Option<&str>,
    ) -> crate::buzz::task::Task {
        let now = chrono::Utc::now();
        crate::buzz::task::Task {
            id: id.to_string(),
            workspace: "test".to_string(),
            title: format!("Task {id}"),
            stage,
            source: Some("manual".to_string()),
            source_url: None,
            worker_id: worker_id.map(str::to_string),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    #[test]
    fn test_build_kanban_cards_from_tasks_stage_mapping() {
        use crate::buzz::task::TaskStage;
        let tasks = vec![
            make_task_for_test("t1", TaskStage::Triage, None),
            make_task_for_test("t2", TaskStage::InProgress, None),
            make_task_for_test("t3", TaskStage::InAiReview, None),
            make_task_for_test("t4", TaskStage::HumanReview, None),
            make_task_for_test("t5", TaskStage::Merged, None),
            make_task_for_test("t6", TaskStage::Dismissed, None),
        ];
        let cards = build_kanban_cards_from_tasks(&tasks);
        // Only InProgress, InAiReview, HumanReview appear in kanban
        // Triage, Merged, Dismissed are filtered out
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].stage, KanbanStage::InProgress); // InProgress
        assert_eq!(cards[1].stage, KanbanStage::InReview); // InAiReview
        assert_eq!(cards[2].stage, KanbanStage::HumanReview); // HumanReview
    }

    #[test]
    fn test_build_kanban_cards_task_replaces_worker_card() {
        use crate::buzz::task::TaskStage;
        let mut ws = empty_ws();
        ws.workers = vec![make_worker("abc-1234", "running", None)];
        ws.tasks = vec![{
            let mut t = make_task_for_test("task-uuid-1", TaskStage::InProgress, Some("abc-1234"));
            t.title = "Fix the bug".to_string();
            t
        }];
        let cards = build_kanban_cards(&ws);
        // Worker card should be replaced by task card
        assert_eq!(cards.len(), 1);
        assert!(
            cards[0].id.starts_with("task:"),
            "should be a task card, got {}",
            cards[0].id
        );
        assert_eq!(cards[0].stage, KanbanStage::InProgress);
    }

    // ── parse_github_label tests ──────────────────────────────

    #[test]
    fn test_parse_github_label_issue() {
        assert_eq!(
            parse_github_label("https://github.com/owner/repo/issues/123"),
            Some("repo #123".to_string())
        );
    }

    #[test]
    fn test_parse_github_label_pull() {
        assert_eq!(
            parse_github_label("https://github.com/owner/repo/pull/456"),
            Some("repo #456".to_string())
        );
    }

    #[test]
    fn test_parse_github_label_trailing_slash() {
        assert_eq!(
            parse_github_label("https://github.com/owner/repo/issues/123/"),
            Some("repo #123".to_string())
        );
    }

    #[test]
    fn test_parse_github_label_query_and_fragment() {
        assert_eq!(
            parse_github_label("https://github.com/owner/repo/issues/789?query=1#fragment"),
            Some("repo #789".to_string())
        );
    }

    #[test]
    fn test_parse_github_label_non_github_returns_none() {
        assert_eq!(
            parse_github_label("https://example.com/owner/repo/issues/1"),
            None
        );
    }

    #[test]
    fn test_parse_github_label_tree_returns_none() {
        assert_eq!(
            parse_github_label("https://github.com/owner/repo/tree/main"),
            None
        );
    }

    #[test]
    fn test_parse_github_label_non_numeric_segment_returns_none() {
        assert_eq!(
            parse_github_label("https://github.com/owner/repo/issues/abc"),
            None
        );
    }

    #[test]
    fn test_parse_github_label_empty_repo_segment_returns_none() {
        // Double slash creates an empty repo segment; must not produce " #123"
        assert_eq!(
            parse_github_label("https://github.com/owner//issues/123"),
            None
        );
    }
}

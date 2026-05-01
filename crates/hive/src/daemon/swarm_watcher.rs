//! Swarm event watcher — tails `.swarm/events.jsonl` for lifecycle events
//! and generates typed notifications for the daemon.
//!
//! Swarm emits discrete lifecycle events (PhaseChanged, PrDetected, WorktreeClosed)
//! to `.swarm/events.jsonl`. This module tails that file using a cursor-based
//! JSONL reader and maps each event to a `SwarmNotification`.
//!
//! State.json is still read for context (branch names, summaries, PR URLs) and
//! for daemon-restart re-notifications (workers already waiting or with unnotified PRs).

use apiari_common::ipc::JsonlReader;
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// CI check status for a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiStatus {
    /// All checks passed.
    Passing,
    /// At least one check failed.
    Failing,
    /// At least one check is still pending/in-progress (none failing).
    Pending,
}

impl CiStatus {
    /// Format as a one-line CI status string for Telegram.
    pub fn status_line(&self) -> &'static str {
        match self {
            Self::Passing => "CI: ✅ passing",
            Self::Failing => "CI: ❌ failing",
            Self::Pending => "CI: ⏳ running",
        }
    }
}

/// Fetch CI check status for a PR URL using `gh pr checks`.
///
/// Returns `None` if the command fails, times out, or there are no checks.
/// Uses a 10-second timeout to avoid blocking notifications.
pub async fn fetch_ci_status(pr_url: &str) -> Option<CiStatus> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("gh")
            .args([
                "pr",
                "checks",
                pr_url,
                "--json",
                "name,state,conclusion",
                "--jq",
                "[.[] | {name,conclusion}]",
            ])
            .output(),
    )
    .await
    .ok()? // timeout
    .ok()?; // command error

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let checks: Vec<CiCheck> = serde_json::from_str(stdout.trim()).ok()?;

    if checks.is_empty() {
        return None;
    }

    let has_failure = checks.iter().any(|c| {
        matches!(
            c.conclusion.as_deref(),
            Some("failure")
                | Some("FAILURE")
                | Some("cancelled")
                | Some("CANCELLED")
                | Some("timed_out")
                | Some("TIMED_OUT")
                | Some("action_required")
                | Some("ACTION_REQUIRED")
                | Some("startup_failure")
                | Some("STARTUP_FAILURE")
        )
    });
    if has_failure {
        return Some(CiStatus::Failing);
    }

    let has_pending = checks.iter().any(|c| {
        matches!(
            c.conclusion.as_deref(),
            None | Some("") | Some("pending") | Some("PENDING")
        )
    });
    if has_pending {
        return Some(CiStatus::Pending);
    }

    Some(CiStatus::Passing)
}

#[derive(Debug, Deserialize)]
struct CiCheck {
    #[allow(dead_code)]
    name: Option<String>,
    conclusion: Option<String>,
}

/// Typed swarm event notifications emitted by `SwarmWatcher::poll()`.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum SwarmNotification {
    /// A new agent worktree appeared.
    AgentSpawned {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
    },
    /// An agent's pane disappeared (work finished, pane exited).
    AgentCompleted {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
        duration: String,
        pr_url: Option<String>,
    },
    /// A worktree was removed entirely (closed or merged).
    AgentClosed {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
        duration: String,
        pr_url: Option<String>,
    },
    /// A PR was opened by an agent (pr_url transitioned from None to Some).
    PrOpened {
        worktree_id: String,
        branch: String,
        pr_url: String,
        pr_title: Option<String>,
        duration: String,
    },
    /// An agent appears stalled.
    AgentStalled {
        worktree_id: String,
        branch: String,
        stall_kind: StallKind,
    },
    /// A claude-tui agent is waiting for user input.
    AgentWaiting {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
        pr_url: Option<String>,
    },
}

/// What kind of stall was detected.
#[derive(Debug, Clone)]
pub enum StallKind {
    /// No events for longer than the configured timeout.
    Idle { minutes: u64 },
    /// Last event is a ToolUse with no corresponding ToolResult.
    PermissionBlock { tool: String },
}

impl SwarmNotification {
    /// Format as a Telegram-friendly notification string.
    pub fn format_telegram(&self) -> String {
        match self {
            Self::AgentSpawned {
                branch, summary, ..
            } => {
                let lead = summary_or_short(summary, branch);
                format!("🐝 *Agent spawned* — {lead}\nBranch: `{branch}`")
            }
            Self::AgentCompleted {
                branch,
                summary,
                duration,
                pr_url,
                ..
            } => {
                let lead = summary_or_short(summary, branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!("✅ *Agent finished* — {lead}\nBranch: `{branch}` · {duration}{pr_line}")
            }
            Self::AgentClosed {
                branch,
                summary,
                duration,
                pr_url,
                ..
            } => {
                let lead = summary_or_short(summary, branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!("🗑 *Worker closed* — {lead}\nBranch: `{branch}` · {duration}{pr_line}")
            }
            Self::PrOpened {
                branch,
                pr_url,
                pr_title,
                duration,
                ..
            } => {
                let lead = pr_title.as_deref().unwrap_or_else(|| short_branch(branch));
                format!("🔗 *PR opened* — {lead}\n{pr_url}\nBranch: `{branch}` · {duration}")
            }
            Self::AgentStalled {
                branch, stall_kind, ..
            } => {
                let short = short_branch(branch);
                let detail = match stall_kind {
                    StallKind::Idle { minutes } => format!("Idle for {minutes}m"),
                    StallKind::PermissionBlock { tool } => {
                        format!("Waiting on permission for `{tool}`")
                    }
                };
                format!("⚠️ *Agent stalled* — {short}\nBranch: `{branch}`\n{detail}")
            }
            Self::AgentWaiting {
                branch,
                summary,
                pr_url,
                ..
            } => {
                let lead = summary_or_short(summary, branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!(
                    "⏳ *Worker waiting* — {lead}\nBranch: `{branch}`{pr_line}\nReply here to send a message to this worker."
                )
            }
        }
    }

    /// One-line summary for use in batched notification messages.
    pub fn summary_line(&self) -> String {
        match self {
            Self::AgentSpawned {
                branch, summary, ..
            } => {
                let short = short_branch(branch);
                match summary {
                    Some(s) => format!("New agent spawned — {short} ({s})"),
                    None => format!("New agent spawned — {short}"),
                }
            }
            Self::AgentCompleted { branch, pr_url, .. } => {
                let short = short_branch(branch);
                match pr_url {
                    Some(url) => format!("Agent completed — {short} (PR: {url})"),
                    None => format!("Agent completed — {short}"),
                }
            }
            Self::AgentClosed { branch, .. } => {
                let short = short_branch(branch);
                format!("Agent closed — {short}")
            }
            Self::PrOpened { branch, pr_url, .. } => {
                let short = short_branch(branch);
                format!("PR opened — {short} ({pr_url})")
            }
            Self::AgentStalled {
                branch, stall_kind, ..
            } => {
                let short = short_branch(branch);
                match stall_kind {
                    StallKind::Idle { minutes } => {
                        format!("Agent stalled — {short} (idle {minutes}m)")
                    }
                    StallKind::PermissionBlock { tool } => {
                        format!("Agent stalled — {short} (permission: {tool})")
                    }
                }
            }
            Self::AgentWaiting { branch, .. } => {
                let short = short_branch(branch);
                format!("Worker waiting — {short}")
            }
        }
    }

    /// Return inline keyboard button rows for this notification type.
    ///
    /// Returns empty vec for notification types that don't need action buttons.
    pub fn inline_buttons(&self) -> Vec<Vec<crate::channel::InlineButton>> {
        use crate::channel::InlineButton;

        match self {
            Self::PrOpened { worktree_id, .. } => {
                vec![vec![
                    InlineButton {
                        text: "✅ Merge PR".into(),
                        callback_data: format!("merge_pr:{worktree_id}"),
                    },
                    InlineButton {
                        text: "🗑 Close Worker".into(),
                        callback_data: format!("close_worker:{worktree_id}"),
                    },
                ]]
            }
            Self::AgentWaiting {
                worktree_id,
                pr_url,
                ..
            } => {
                let mut row = Vec::new();
                if pr_url.is_some() {
                    row.push(InlineButton {
                        text: "✅ Merge PR".into(),
                        callback_data: format!("merge_pr:{worktree_id}"),
                    });
                }
                row.push(InlineButton {
                    text: "🗑 Close Worker".into(),
                    callback_data: format!("close_worker:{worktree_id}"),
                });
                vec![row]
            }
            Self::AgentStalled { worktree_id, .. } => {
                vec![vec![InlineButton {
                    text: "🗑 Close Worker".into(),
                    callback_data: format!("close_worker:{worktree_id}"),
                }]]
            }
            Self::AgentSpawned { .. } | Self::AgentCompleted { .. } | Self::AgentClosed { .. } => {
                vec![]
            }
        }
    }

    /// Return the worktree_id associated with this notification.
    pub fn worktree_id(&self) -> &str {
        match self {
            Self::AgentSpawned { worktree_id, .. }
            | Self::AgentCompleted { worktree_id, .. }
            | Self::AgentClosed { worktree_id, .. }
            | Self::PrOpened { worktree_id, .. }
            | Self::AgentStalled { worktree_id, .. }
            | Self::AgentWaiting { worktree_id, .. } => worktree_id,
        }
    }

    /// Format a PrOpened notification with CI status appended.
    /// For non-PrOpened notifications, falls back to `format_telegram()`.
    pub fn format_telegram_with_ci(&self, ci: Option<&CiStatus>) -> String {
        match self {
            Self::PrOpened {
                branch,
                pr_url,
                pr_title,
                duration,
                ..
            } => {
                let lead = pr_title.as_deref().unwrap_or_else(|| short_branch(branch));
                let ci_line = ci
                    .map(|s| format!("\n{}", s.status_line()))
                    .unwrap_or_default();
                format!(
                    "🔗 *PR opened* — {lead}\n{pr_url}\nBranch: `{branch}` · {duration}{ci_line}"
                )
            }
            _ => self.format_telegram(),
        }
    }

    /// Return inline keyboard buttons for a PrOpened notification with CI awareness.
    /// For non-PrOpened notifications, falls back to `inline_buttons()`.
    pub fn inline_buttons_with_ci(
        &self,
        ci: Option<&CiStatus>,
    ) -> Vec<Vec<crate::channel::InlineButton>> {
        use crate::channel::InlineButton;

        match self {
            Self::PrOpened { worktree_id, .. } => {
                let merge_btn = match ci {
                    Some(CiStatus::Passing) | None => InlineButton {
                        text: "✅ Merge PR".into(),
                        callback_data: format!("merge_pr:{worktree_id}"),
                    },
                    Some(CiStatus::Pending) => InlineButton {
                        text: "⏳ CI still running".into(),
                        callback_data: format!("ci_pending:{worktree_id}"),
                    },
                    Some(CiStatus::Failing) => InlineButton {
                        text: "❌ CI failing — merge anyway?".into(),
                        callback_data: format!("ci_failing_merge:{worktree_id}"),
                    },
                };
                vec![vec![
                    merge_btn,
                    InlineButton {
                        text: "🗑 Close Worker".into(),
                        callback_data: format!("close_worker:{worktree_id}"),
                    },
                ]]
            }
            _ => self.inline_buttons(),
        }
    }
}

/// Watches `.swarm/events.jsonl` for lifecycle events and generates notifications.
///
/// On each `poll()`:
/// 1. Reads `state.json` for context (branch, summary, PR URL, created_at)
/// 2. On first poll: initializes known state, skips events to end-of-file
/// 3. On subsequent polls: tails new events and maps them to notifications
/// 4. Re-emits notifications for workers waiting/with-PRs at daemon startup
/// 5. Checks for stalled agents (idle too long, permission blocked)
pub struct SwarmWatcher {
    state_path: PathBuf,
    /// Tracked worktrees from the previous poll (for context lookups on closed workers).
    known: HashMap<String, TrackedWorktree>,
    /// Whether we've done the initial load (skip notifications on first poll).
    initialized: bool,
    /// Base directory for reading agent events (parent of state.json, i.e. `.swarm/`).
    swarm_dir: PathBuf,
    /// Stall timeout in seconds (0 = disabled).
    stall_timeout_secs: u64,
    /// Workers that were already in "waiting" state at initialization.
    /// Drained on the first poll after init to re-emit AgentWaiting.
    waiting_at_init: HashSet<String>,
    /// Persisted set of pr_urls that have already triggered a PrOpened notification.
    pr_notified: HashSet<String>,
    /// Worktree IDs with unnotified pr_urls at init.
    /// Drained on the first poll after init to emit PrOpened (handles daemon restarts).
    prs_at_init: HashSet<String>,
    /// Directory for persisting `pr_notified.json` (e.g. `.hive/`).
    hive_dir: Option<PathBuf>,
    /// JSONL reader for `.swarm/events.jsonl`.
    events_reader: Option<JsonlReader<SwarmEventMirror>>,
}

/// Snapshot of a worktree's state for context lookups.
#[derive(Debug, Clone)]
struct TrackedWorktree {
    branch: String,
    had_agent: bool,
    created_at: DateTime<Local>,
    /// Whether a stall notification has already been sent for this worktree.
    stall_notified: bool,
    agent_kind: Option<String>,
    pr_url: Option<String>,
    summary: Option<String>,
}

// Mirror types for deserializing .swarm/state.json.
// Intentionally separate from swarm's own types to avoid cross-crate dependency.

#[derive(Debug, Deserialize)]
struct SwarmState {
    #[serde(default)]
    worktrees: Vec<WorktreeState>,
}

#[derive(Debug, Deserialize)]
struct WorktreeState {
    id: String,
    branch: String,
    #[serde(default)]
    agent: Option<PaneState>,
    created_at: DateTime<Local>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    agent_kind: Option<String>,
    #[serde(default)]
    pr: Option<WtPrInfo>,
    #[serde(default)]
    #[allow(dead_code)]
    agent_session_status: Option<String>,
    /// Worker lifecycle phase from swarm's state machine.
    #[serde(default)]
    phase: Option<WorkerPhase>,
}

impl WorktreeState {
    fn pr_url(&self) -> Option<String> {
        self.pr.as_ref().map(|p| p.url.clone())
    }
    fn pr_title(&self) -> Option<String> {
        self.pr.as_ref().and_then(|p| p.title.clone())
    }
}

#[derive(Debug, Deserialize)]
struct WtPrInfo {
    url: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaneState {
    #[allow(dead_code)]
    pane_id: String,
}

// ── Mirror types for swarm event tailing ─────────────────────

/// Mirror of swarm's `WorkerPhase` enum for deserializing events.jsonl.
/// Uses `Unknown` catch-all for forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhase {
    Creating,
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    /// Forward-compat catch-all for phases added in future swarm versions.
    #[serde(other)]
    Unknown,
}

/// Mirror of swarm's `SwarmEvent` enum for deserializing `.swarm/events.jsonl`.
/// Only includes variants we act on; unknown events silently deserialize as `Unknown`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum SwarmEventMirror {
    PhaseChanged {
        worktree: String,
        from: WorkerPhase,
        to: WorkerPhase,
        #[allow(dead_code)]
        timestamp: DateTime<Local>,
    },
    PrDetected {
        worktree: String,
        pr_url: String,
        pr_title: String,
        #[allow(dead_code)]
        #[serde(default)]
        pr_number: u64,
        #[allow(dead_code)]
        timestamp: DateTime<Local>,
    },
    WorktreeClosed {
        worktree: String,
        #[allow(dead_code)]
        timestamp: DateTime<Local>,
    },
    WorktreeMerged {
        worktree: String,
        #[allow(dead_code)]
        timestamp: DateTime<Local>,
    },
    /// Catch-all for unknown/future event types.
    #[serde(other)]
    Unknown,
}

/// Context data for building notifications from events.
struct EventContext {
    branch: String,
    summary: Option<String>,
    pr_url: Option<String>,
    duration: String,
}

/// A single event entry from `.swarm/agents/<id>/events.jsonl`.
#[derive(Debug, Deserialize)]
struct AgentEvent {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    timestamp: Option<DateTime<Utc>>,
    /// For tool_use events — the tool name.
    #[serde(default)]
    tool: Option<String>,
}

impl SwarmWatcher {
    pub fn new(state_path: PathBuf) -> Self {
        let swarm_dir = state_path
            .parent()
            .unwrap_or(Path::new(".swarm"))
            .to_path_buf();
        Self {
            state_path,
            known: HashMap::new(),
            initialized: false,
            swarm_dir,
            stall_timeout_secs: 0,
            waiting_at_init: HashSet::new(),
            pr_notified: HashSet::new(),
            prs_at_init: HashSet::new(),
            hive_dir: None,
            events_reader: None,
        }
    }

    /// Set the stall detection timeout (in seconds). 0 disables stall detection.
    pub fn set_stall_timeout(&mut self, secs: u64) {
        self.stall_timeout_secs = secs;
    }

    /// Set the hive directory for persisting PR notification state.
    /// When set, loads/saves `pr_notified.json` to track which PrOpened
    /// notifications have already been sent, avoiding duplicates across restarts.
    pub fn set_hive_dir(&mut self, dir: PathBuf) {
        self.hive_dir = Some(dir);
    }

    /// Poll for new events and return typed notifications.
    ///
    /// Reads `state.json` for context, then tails `events.jsonl` for new lifecycle
    /// events. On the first poll after daemon restart, re-emits notifications for
    /// workers already waiting or with unnotified PRs.
    pub fn poll(&mut self) -> Vec<SwarmNotification> {
        let state = match Self::load_state(&self.state_path) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let current: HashMap<String, &WorktreeState> = state
            .worktrees
            .iter()
            .map(|wt| (wt.id.clone(), wt))
            .collect();

        // First poll: populate known state, initialize events reader, no notifications.
        if !self.initialized {
            self.initialize(&state);
            return Vec::new();
        }

        let mut notifications = Vec::new();
        let mut pr_notified_changed = false;

        // Tail events.jsonl for new lifecycle events.
        self.process_events(&current, &mut notifications, &mut pr_notified_changed);

        // Update known state from current state.json.
        self.update_known(&state);

        // On first poll after init, re-emit for workers already waiting or with PRs.
        self.emit_restart_notifications(&current, &mut notifications, &mut pr_notified_changed);

        // Check for stalled agents.
        if self.stall_timeout_secs > 0 {
            notifications.extend(self.check_stalls());
        }

        if pr_notified_changed {
            self.save_pr_notified();
        }

        notifications
    }

    /// First-poll initialization: populate known state, skip existing events.
    fn initialize(&mut self, state: &SwarmState) {
        self.initialized = true;

        for wt in &state.worktrees {
            self.known.insert(
                wt.id.clone(),
                TrackedWorktree {
                    branch: wt.branch.clone(),
                    had_agent: wt.agent.is_some(),
                    created_at: wt.created_at,
                    stall_notified: false,
                    agent_kind: wt.agent_kind.clone(),
                    pr_url: wt.pr_url(),
                    summary: wt.summary.clone(),
                },
            );

            // Track workers already waiting so we can re-emit on next poll.
            if wt.phase == Some(WorkerPhase::Waiting) {
                self.waiting_at_init.insert(wt.id.clone());
            }
        }

        // Load persisted PR notification set and track unnotified PRs.
        if let Some(dir) = &self.hive_dir {
            self.pr_notified = Self::load_pr_notified(&dir.join("pr_notified.json"));
            for wt in &state.worktrees {
                if let Some(url) = &wt.pr_url()
                    && !self.pr_notified.contains(url)
                {
                    self.prs_at_init.insert(wt.id.clone());
                }
            }
        }

        // Initialize events reader — skip to end so we don't replay history.
        let events_path = self.swarm_dir.join("events.jsonl");
        let mut reader = JsonlReader::<SwarmEventMirror>::new(&events_path);
        let _ = reader.skip_to_end();
        self.events_reader = Some(reader);

        if !self.known.is_empty() {
            tracing::info!(count = self.known.len(), "Swarm watcher initialized");
        }
    }

    /// Tail new events from `events.jsonl` and map them to notifications.
    fn process_events(
        &mut self,
        current: &HashMap<String, &WorktreeState>,
        notifications: &mut Vec<SwarmNotification>,
        pr_notified_changed: &mut bool,
    ) {
        let reader = match self.events_reader.as_mut() {
            Some(r) => r,
            None => return,
        };

        let events = match reader.poll() {
            Ok(events) => events,
            Err(_) => return,
        };

        for event in events {
            match event {
                SwarmEventMirror::PhaseChanged {
                    ref worktree,
                    ref from,
                    ref to,
                    ..
                } => match to {
                    WorkerPhase::Running
                        if matches!(from, WorkerPhase::Creating | WorkerPhase::Starting) =>
                    {
                        let ctx = self.event_context(worktree, current);
                        notifications.push(SwarmNotification::AgentSpawned {
                            worktree_id: worktree.clone(),
                            branch: ctx.branch,
                            summary: ctx.summary,
                        });
                    }
                    WorkerPhase::Waiting => {
                        let ctx = self.event_context(worktree, current);
                        // Suppress AgentWaiting when PR is set — PrOpened is the signal.
                        if ctx.pr_url.is_none() {
                            notifications.push(SwarmNotification::AgentWaiting {
                                worktree_id: worktree.clone(),
                                branch: ctx.branch,
                                summary: ctx.summary,
                                pr_url: None,
                            });
                        }
                    }
                    WorkerPhase::Completed | WorkerPhase::Failed => {
                        let ctx = self.event_context(worktree, current);
                        notifications.push(SwarmNotification::AgentCompleted {
                            worktree_id: worktree.clone(),
                            branch: ctx.branch,
                            summary: ctx.summary,
                            duration: ctx.duration,
                            pr_url: ctx.pr_url,
                        });
                    }
                    _ => {} // Creating, Starting, Unknown — no notification
                },
                SwarmEventMirror::PrDetected {
                    ref worktree,
                    ref pr_url,
                    ref pr_title,
                    ..
                } => {
                    if !self.pr_notified.contains(pr_url) {
                        let ctx = self.event_context(worktree, current);
                        notifications.push(SwarmNotification::PrOpened {
                            worktree_id: worktree.clone(),
                            branch: ctx.branch,
                            pr_url: pr_url.clone(),
                            pr_title: Some(pr_title.clone()),
                            duration: ctx.duration,
                        });
                        self.pr_notified.insert(pr_url.clone());
                        *pr_notified_changed = true;
                    }
                }
                SwarmEventMirror::WorktreeClosed { ref worktree, .. }
                | SwarmEventMirror::WorktreeMerged { ref worktree, .. } => {
                    let ctx = self.event_context(worktree, current);
                    notifications.push(SwarmNotification::AgentClosed {
                        worktree_id: worktree.clone(),
                        branch: ctx.branch,
                        summary: ctx.summary,
                        duration: ctx.duration,
                        pr_url: ctx.pr_url,
                    });
                }
                SwarmEventMirror::Unknown => {} // Forward compat: silently skip
            }
        }
    }

    /// Update known state from current state.json snapshot.
    fn update_known(&mut self, state: &SwarmState) {
        let old_stall: HashMap<String, bool> = self
            .known
            .iter()
            .map(|(id, t)| (id.clone(), t.stall_notified))
            .collect();
        self.known.clear();
        for wt in &state.worktrees {
            self.known.insert(
                wt.id.clone(),
                TrackedWorktree {
                    branch: wt.branch.clone(),
                    had_agent: wt.agent.is_some(),
                    created_at: wt.created_at,
                    stall_notified: old_stall.get(&wt.id).copied().unwrap_or(false),
                    agent_kind: wt.agent_kind.clone(),
                    pr_url: wt.pr_url(),
                    summary: wt.summary.clone(),
                },
            );
        }
    }

    /// Re-emit notifications for workers that were already waiting or had PRs at init.
    /// Drained on first call so this only fires once after daemon restart.
    fn emit_restart_notifications(
        &mut self,
        current: &HashMap<String, &WorktreeState>,
        notifications: &mut Vec<SwarmNotification>,
        pr_notified_changed: &mut bool,
    ) {
        // Re-emit AgentWaiting for workers waiting at daemon startup.
        if !self.waiting_at_init.is_empty() {
            let init_ids: HashSet<String> = self.waiting_at_init.drain().collect();
            for id in init_ids {
                if let Some(wt) = current.get(&id)
                    && wt.phase == Some(WorkerPhase::Waiting)
                    && wt.pr_url().is_none()
                {
                    notifications.push(SwarmNotification::AgentWaiting {
                        worktree_id: id,
                        branch: wt.branch.clone(),
                        summary: wt.summary.clone(),
                        pr_url: None,
                    });
                }
            }
        }

        // Re-emit PrOpened for PRs that existed at init but weren't yet notified.
        if !self.prs_at_init.is_empty() {
            let init_ids: HashSet<String> = self.prs_at_init.drain().collect();
            for id in init_ids {
                if let Some(wt) = current.get(&id)
                    && let Some(pr_url) = &wt.pr_url()
                {
                    notifications.push(SwarmNotification::PrOpened {
                        worktree_id: id,
                        branch: wt.branch.clone(),
                        pr_url: pr_url.clone(),
                        pr_title: wt.pr_title(),
                        duration: format_duration(wt.created_at),
                    });
                    self.pr_notified.insert(pr_url.clone());
                    *pr_notified_changed = true;
                }
            }
        }
    }

    /// Look up context data for a worktree from current state.json or known state.
    fn event_context(
        &self,
        worktree_id: &str,
        current: &HashMap<String, &WorktreeState>,
    ) -> EventContext {
        // Try current state first (worktree still exists).
        if let Some(wt) = current.get(worktree_id) {
            return EventContext {
                branch: wt.branch.clone(),
                summary: wt.summary.clone(),
                pr_url: wt.pr_url(),
                duration: format_duration(wt.created_at),
            };
        }
        // Fall back to known state (worktree was closed/removed).
        if let Some(tracked) = self.known.get(worktree_id) {
            return EventContext {
                branch: tracked.branch.clone(),
                summary: tracked.summary.clone(),
                pr_url: tracked.pr_url.clone(),
                duration: format_duration(tracked.created_at),
            };
        }
        // No context available (shouldn't happen in practice).
        EventContext {
            branch: format!("swarm/{worktree_id}"),
            summary: None,
            pr_url: None,
            duration: "?".to_string(),
        }
    }

    /// Check for stalled agents by reading their events.jsonl files.
    fn check_stalls(&mut self) -> Vec<SwarmNotification> {
        let mut stalls = Vec::new();
        let agents_dir = self.swarm_dir.join("agents");

        for (id, tracked) in self.known.iter_mut() {
            // Only check agents that are alive and haven't been notified yet.
            if !tracked.had_agent || tracked.stall_notified {
                continue;
            }

            // claude-tui agents sit idle waiting for user input — not a stall.
            if tracked.agent_kind.as_deref() == Some("claude-tui") {
                continue;
            }

            let events_path = agents_dir.join(id).join("events.jsonl");
            let events = Self::read_recent_events(&events_path);

            if events.is_empty() {
                // No events file — can't determine stall state.
                continue;
            }

            let last = &events[events.len() - 1];

            // Check for permission block: last event is tool_use with no tool_result after.
            if last.r#type == "tool_use"
                && let Some(tool) = &last.tool
            {
                stalls.push(SwarmNotification::AgentStalled {
                    worktree_id: id.clone(),
                    branch: tracked.branch.clone(),
                    stall_kind: StallKind::PermissionBlock { tool: tool.clone() },
                });
                tracked.stall_notified = true;
                continue;
            }

            // Check for idle stall: last event timestamp is old.
            if let Some(ts) = &last.timestamp {
                let elapsed = Utc::now() - *ts;
                let elapsed_secs = elapsed.num_seconds().max(0) as u64;
                if elapsed_secs >= self.stall_timeout_secs {
                    stalls.push(SwarmNotification::AgentStalled {
                        worktree_id: id.clone(),
                        branch: tracked.branch.clone(),
                        stall_kind: StallKind::Idle {
                            minutes: elapsed_secs / 60,
                        },
                    });
                    tracked.stall_notified = true;
                }
            }
        }

        stalls
    }

    /// Read recent events from an agent's events.jsonl (last 20 lines).
    fn read_recent_events(path: &Path) -> Vec<AgentEvent> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        content
            .lines()
            .rev()
            .take(20)
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return None;
                }
                serde_json::from_str::<AgentEvent>(trimmed).ok()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn load_state(path: &Path) -> Option<SwarmState> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn load_pr_notified(path: &Path) -> HashSet<String> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_pr_notified(&self) {
        if let Some(dir) = &self.hive_dir {
            let path = dir.join("pr_notified.json");
            if let Ok(json) = serde_json::to_string_pretty(&self.pr_notified) {
                let _ = std::fs::write(&path, json);
            }
        }
    }
}

/// Extract a short human-readable name from a branch like "swarm/fix-auth-bug".
fn short_branch(branch: &str) -> &str {
    branch.strip_prefix("swarm/").unwrap_or(branch)
}

/// Return summary if set, else the short branch name.
fn summary_or_short<'a>(summary: &'a Option<String>, branch: &'a str) -> &'a str {
    summary.as_deref().unwrap_or_else(|| short_branch(branch))
}

/// Format the duration since a worktree was created.
fn format_duration(created_at: DateTime<Local>) -> String {
    let elapsed = Utc::now() - created_at.with_timezone(&Utc);
    let mins = elapsed.num_minutes();
    if mins < 1 {
        "< 1m".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else {
        let hours = mins / 60;
        let remaining = mins % 60;
        format!("{hours}h {remaining}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_state(file: &mut NamedTempFile, json: &str) {
        file.as_file().set_len(0).unwrap();
        std::io::Seek::seek(file, std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(json.as_bytes()).unwrap();
        file.flush().unwrap();
    }

    #[test]
    fn test_first_poll_no_notifications() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/test","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        let notes = watcher.poll();
        assert!(
            notes.is_empty(),
            "first poll should not generate notifications"
        );
        assert!(watcher.initialized);
        assert_eq!(watcher.known.len(), 1);
    }

    #[test]
    fn test_missing_state_file() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/nonexistent/state.json"));
        let notes = watcher.poll();
        assert!(notes.is_empty());
    }

    #[test]
    fn test_short_branch() {
        assert_eq!(short_branch("swarm/fix-bug"), "fix-bug");
        assert_eq!(short_branch("main"), "main");
        assert_eq!(short_branch("feature/auth"), "feature/auth");
    }

    #[test]
    fn test_notification_worktree_id() {
        let n = SwarmNotification::AgentSpawned {
            worktree_id: "abc".into(),
            branch: "swarm/test".into(),
            summary: None,
        };
        assert_eq!(n.worktree_id(), "abc");
    }

    #[test]
    fn test_notification_formatting() {
        let spawned = SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/new".into(),
            summary: Some("Do the thing".into()),
        };
        assert!(spawned.format_telegram().contains("Agent spawned"));
        assert!(spawned.format_telegram().contains("Do the thing"));

        let completed = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };
        assert!(completed.format_telegram().contains("Agent finished"));
        assert!(completed.format_telegram().contains("5m"));
        assert!(!completed.format_telegram().contains("PR:"));

        let stalled_idle = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/stuck".into(),
            stall_kind: StallKind::Idle { minutes: 10 },
        };
        assert!(stalled_idle.format_telegram().contains("Agent stalled"));
        assert!(stalled_idle.format_telegram().contains("Idle for 10m"));

        let stalled_perm = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/blocked".into(),
            stall_kind: StallKind::PermissionBlock {
                tool: "Write".into(),
            },
        };
        assert!(stalled_perm.format_telegram().contains("permission"));
        assert!(stalled_perm.format_telegram().contains("Write"));
    }

    #[test]
    fn test_pr_url_in_completed_notification() {
        let completed = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/feat".into(),
            summary: None,
            duration: "12m".into(),
            pr_url: Some("https://github.com/ApiariTools/hive/pull/42".into()),
        };
        let msg = completed.format_telegram();
        assert!(msg.contains("PR: https://github.com/ApiariTools/hive/pull/42"));
    }

    #[test]
    fn test_pr_url_in_closed_notification() {
        let closed = SwarmNotification::AgentClosed {
            worktree_id: "1".into(),
            branch: "swarm/feat".into(),
            summary: None,
            duration: "8m".into(),
            pr_url: Some("https://github.com/ApiariTools/hive/pull/99".into()),
        };
        let msg = closed.format_telegram();
        assert!(msg.contains("PR: https://github.com/ApiariTools/hive/pull/99"));
    }

    #[test]
    fn test_agent_kind_deserialized() {
        let json = r#"{"worktrees":[{"id":"1","branch":"swarm/tui","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui"}]}"#;
        let state: SwarmState = serde_json::from_str(json).unwrap();
        assert_eq!(state.worktrees[0].agent_kind.as_deref(), Some("claude-tui"));
    }

    #[test]
    fn test_claude_tui_skips_stall_detection() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/tui-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_stall_timeout(1); // 1 second — would trigger immediately
        watcher.poll(); // Initialize

        // Poll again — agent is still running, but should NOT get a stall notification.
        let notes = watcher.poll();
        let stall_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentStalled { .. }))
            .collect();
        assert!(
            stall_notes.is_empty(),
            "claude-tui agents should not trigger stall notifications"
        );
    }

    #[test]
    fn test_stall_notification_not_repeated() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/stuck","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","agent_kind":"claude"}]}"#,
        );
        watcher.set_stall_timeout(1); // 1 second — triggers immediately

        // Create a stale agent-events file so idle stall fires.
        let agents_dir = dir.path().join(".swarm").join("agents").join("w1");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("events.jsonl"),
            r#"{"type":"result","timestamp":"2025-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        // First non-init poll — should get a stall notification.
        let notes = watcher.poll();
        let stalls: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentStalled { .. }))
            .collect();
        assert_eq!(stalls.len(), 1, "first poll should fire stall notification");

        // Second poll — same stalled state, should NOT re-fire.
        let notes = watcher.poll();
        let stalls: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentStalled { .. }))
            .collect();
        assert!(
            stalls.is_empty(),
            "stall notification should not repeat on subsequent polls"
        );
    }

    #[test]
    fn test_pr_opened_format_telegram() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/add-auth".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/42".into(),
            pr_title: Some("Add OAuth support".into()),
            duration: "15m".into(),
        };
        let msg = pr.format_telegram();
        assert_eq!(
            msg,
            "🔗 *PR opened* — Add OAuth support\nhttps://github.com/ApiariTools/hive/pull/42\nBranch: `swarm/add-auth` · 15m"
        );

        // Without pr_title — falls back to short branch.
        let pr_no_title = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix-bug".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/7".into(),
            pr_title: None,
            duration: "3m".into(),
        };
        let msg2 = pr_no_title.format_telegram();
        assert_eq!(
            msg2,
            "🔗 *PR opened* — fix-bug\nhttps://github.com/ApiariTools/hive/pull/7\nBranch: `swarm/fix-bug` · 3m"
        );
    }

    #[test]
    fn test_pr_opened_worktree_id() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "xyz".into(),
            branch: "swarm/test".into(),
            pr_url: "https://example.com/pr/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        assert_eq!(pr.worktree_id(), "xyz");
    }

    #[test]
    fn test_agent_waiting_format_no_summary() {
        let waiting = SwarmNotification::AgentWaiting {
            worktree_id: "abc".into(),
            branch: "swarm/my-task".into(),
            summary: None,
            pr_url: None,
        };
        let msg = waiting.format_telegram();
        assert_eq!(
            msg,
            "⏳ *Worker waiting* — my-task\nBranch: `swarm/my-task`\nReply here to send a message to this worker."
        );
    }

    #[test]
    fn test_summary_line_spawned() {
        let n = SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/fix-auth".into(),
            summary: Some("Fix authentication bug".into()),
        };
        assert_eq!(
            n.summary_line(),
            "New agent spawned — fix-auth (Fix authentication bug)"
        );

        let n2 = SwarmNotification::AgentSpawned {
            worktree_id: "2".into(),
            branch: "swarm/cleanup".into(),
            summary: None,
        };
        assert_eq!(n2.summary_line(), "New agent spawned — cleanup");
    }

    #[test]
    fn test_summary_line_completed() {
        let n = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: Some("https://github.com/example/pull/1".into()),
        };
        assert_eq!(
            n.summary_line(),
            "Agent completed — done (PR: https://github.com/example/pull/1)"
        );

        let n2 = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };
        assert_eq!(n2.summary_line(), "Agent completed — done");
    }

    #[test]
    fn test_summary_line_closed() {
        let n = SwarmNotification::AgentClosed {
            worktree_id: "1".into(),
            branch: "swarm/old".into(),
            summary: None,
            duration: "10m".into(),
            pr_url: None,
        };
        assert_eq!(n.summary_line(), "Agent closed — old");
    }

    #[test]
    fn test_summary_line_pr_opened() {
        let n = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://github.com/example/pull/42".into(),
            pr_title: Some("Add feature".into()),
            duration: "8m".into(),
        };
        assert_eq!(
            n.summary_line(),
            "PR opened — feat (https://github.com/example/pull/42)"
        );
    }

    #[test]
    fn test_summary_line_stalled() {
        let idle = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/stuck".into(),
            stall_kind: StallKind::Idle { minutes: 15 },
        };
        assert_eq!(idle.summary_line(), "Agent stalled — stuck (idle 15m)");

        let perm = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/blocked".into(),
            stall_kind: StallKind::PermissionBlock {
                tool: "Write".into(),
            },
        };
        assert_eq!(
            perm.summary_line(),
            "Agent stalled — blocked (permission: Write)"
        );
    }

    // --- Tests for waiting_at_init re-notification ---

    #[test]
    fn test_waiting_at_init_fires_on_second_poll() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already in "waiting" when daemon starts.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/stale-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","phase":"waiting","summary":"Do the thing"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        let notes = watcher.poll(); // Initialize
        assert!(notes.is_empty(), "first poll should not notify");
        assert!(watcher.waiting_at_init.contains("w1"));

        // Second poll — still waiting, no PR → should re-emit AgentWaiting.
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].worktree_id(), "w1");
        let msg = waiting[0].format_telegram();
        assert!(msg.contains("Worker waiting"));
        assert!(msg.contains("Do the thing"));
    }

    #[test]
    fn test_waiting_at_init_with_pr_url_no_notify() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already waiting AND already has a PR.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/done-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","phase":"waiting","pr":{"url":"https://github.com/ApiariTools/hive/pull/99"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — still waiting but has pr_url → no re-notify.
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "should NOT re-notify waiting worker that already has a PR"
        );
    }

    #[test]
    fn test_waiting_at_init_gone_before_second_poll() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already waiting at init.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/ephemeral","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","phase":"waiting"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize
        assert!(watcher.waiting_at_init.contains("w1"));

        // Worker disappears before second poll.
        write_state(&mut file, r#"{"worktrees":[]}"#);
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "should NOT notify for worker that disappeared before second poll"
        );
        // AgentClosed comes from WorktreeClosed event, not from state-diff.
        // Without an event, no AgentClosed fires — which is correct.
    }

    #[test]
    fn test_waiting_at_init_processed_only_once() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already waiting at init.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/once","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","phase":"waiting"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize
        assert!(!watcher.waiting_at_init.is_empty());

        // Second poll — fires re-notify, drains waiting_at_init.
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
                .count(),
            1
        );
        assert!(
            watcher.waiting_at_init.is_empty(),
            "waiting_at_init should be drained after second poll"
        );

        // Third poll — same state, no re-notify.
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
                .count(),
            0,
            "waiting_at_init should not produce notifications on subsequent polls"
        );

        // Fourth poll — still no re-notify.
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
                .count(),
            0,
            "waiting_at_init must remain empty after being drained"
        );
    }

    // --- Edge case tests ---

    #[test]
    fn test_state_file_disappears_no_panic() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let path = file.path().to_path_buf();
        let mut watcher = SwarmWatcher::new(path.clone());
        watcher.poll(); // Initialize with one worktree.
        assert_eq!(watcher.known.len(), 1);

        // Delete the state file.
        std::fs::remove_file(&path).unwrap();

        // Should not panic, returns empty vec (load_state returns None).
        let notes = watcher.poll();
        assert!(
            notes.is_empty(),
            "should return empty notifications when state file disappears"
        );

        // Known state is preserved (stale) since poll short-circuited.
        assert_eq!(
            watcher.known.len(),
            1,
            "known state should be unchanged when file is missing"
        );
    }

    // --- Tests for PR notification on daemon startup ---

    #[test]
    fn test_pr_opened_fires_on_startup_for_existing_pr() {
        let mut file = NamedTempFile::new().unwrap();
        let hive_dir = tempfile::tempdir().unwrap();

        // Worker has pr_url from the start — no pr_notified.json exists.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/1","title":"Add feature"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_hive_dir(hive_dir.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — should emit PrOpened for the unnotified PR.
        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(
            pr_notes.len(),
            1,
            "should emit PrOpened for existing PR on startup"
        );

        if let SwarmNotification::PrOpened {
            pr_url, pr_title, ..
        } = &pr_notes[0]
        {
            assert_eq!(pr_url, "https://github.com/ApiariTools/hive/pull/1");
            assert_eq!(pr_title.as_deref(), Some("Add feature"));
        }

        // Verify pr_notified.json was written.
        let notified_path = hive_dir.path().join("pr_notified.json");
        assert!(notified_path.exists(), "pr_notified.json should be created");
        let content: HashSet<String> =
            serde_json::from_str(&std::fs::read_to_string(&notified_path).unwrap()).unwrap();
        assert!(content.contains("https://github.com/ApiariTools/hive/pull/1"));
    }

    #[test]
    fn test_pr_opened_not_duplicated_after_restart() {
        let mut file = NamedTempFile::new().unwrap();
        let hive_dir = tempfile::tempdir().unwrap();

        // Pre-populate pr_notified.json with the URL (simulates previous daemon run).
        let notified_path = hive_dir.path().join("pr_notified.json");
        std::fs::write(
            &notified_path,
            r#"["https://github.com/ApiariTools/hive/pull/1"]"#,
        )
        .unwrap();

        // Worker has the same pr_url.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/1"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_hive_dir(hive_dir.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — should NOT emit PrOpened (already notified).
        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(
            pr_notes.is_empty(),
            "should NOT emit PrOpened for already-notified PR"
        );
    }

    #[test]
    fn test_pr_detected_event_after_restart() {
        // After restart, new PR events are picked up via events.jsonl.
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[
                {"id":"w1","branch":"swarm/old-feat","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"},
                {"id":"w2","branch":"swarm/new-feat","agent":{"pane_id":"%2"},"created_at":"2026-02-26T11:00:00-08:00"}
            ]}"#,
        );

        // PrDetected event for w2.
        update_state(
            &dir,
            r#"{"worktrees":[
                {"id":"w1","branch":"swarm/old-feat","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"},
                {"id":"w2","branch":"swarm/new-feat","agent":{"pane_id":"%2"},"created_at":"2026-02-26T11:00:00-08:00","pr":{"url":"https://github.com/ex/pull/42","title":"New feature"}}
            ]}"#,
        );
        append_event(
            &dir,
            r#"{"event":"pr_detected","worktree":"w2","pr_url":"https://github.com/ex/pull/42","pr_title":"New feature","pr_number":42,"timestamp":"2026-02-26T11:05:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(pr_notes.len(), 1, "should emit PrOpened for new PR");
        assert_eq!(pr_notes[0].worktree_id(), "w2");
    }

    // --- Tests for CI status ---

    #[test]
    fn test_ci_status_line() {
        assert_eq!(CiStatus::Passing.status_line(), "CI: ✅ passing");
        assert_eq!(CiStatus::Failing.status_line(), "CI: ❌ failing");
        assert_eq!(CiStatus::Pending.status_line(), "CI: ⏳ running");
    }

    #[test]
    fn test_format_telegram_with_ci_passing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/add-auth".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/42".into(),
            pr_title: Some("Add OAuth support".into()),
            duration: "15m".into(),
        };
        let msg = pr.format_telegram_with_ci(Some(&CiStatus::Passing));
        assert_eq!(
            msg,
            "🔗 *PR opened* — Add OAuth support\nhttps://github.com/ApiariTools/hive/pull/42\nBranch: `swarm/add-auth` · 15m\nCI: ✅ passing"
        );
    }

    #[test]
    fn test_format_telegram_with_ci_failing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let msg = pr.format_telegram_with_ci(Some(&CiStatus::Failing));
        assert!(msg.contains("CI: ❌ failing"));
    }

    #[test]
    fn test_format_telegram_with_ci_pending() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let msg = pr.format_telegram_with_ci(Some(&CiStatus::Pending));
        assert!(msg.contains("CI: ⏳ running"));
    }

    #[test]
    fn test_format_telegram_with_ci_none() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let msg = pr.format_telegram_with_ci(None);
        // Should be identical to format_telegram when CI is None.
        assert_eq!(msg, pr.format_telegram());
        assert!(!msg.contains("CI:"));
    }

    #[test]
    fn test_format_telegram_with_ci_non_pr_falls_back() {
        let spawned = SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/task".into(),
            summary: Some("Do stuff".into()),
        };
        assert_eq!(
            spawned.format_telegram_with_ci(Some(&CiStatus::Passing)),
            spawned.format_telegram()
        );
    }

    #[test]
    fn test_inline_buttons_with_ci_passing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(Some(&CiStatus::Passing));
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].len(), 2);
        assert_eq!(buttons[0][0].callback_data, "merge_pr:test-1");
        assert!(buttons[0][0].text.contains("Merge PR"));
    }

    #[test]
    fn test_inline_buttons_with_ci_pending() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(Some(&CiStatus::Pending));
        assert_eq!(buttons[0][0].callback_data, "ci_pending:test-1");
        assert!(buttons[0][0].text.contains("CI still running"));
    }

    #[test]
    fn test_inline_buttons_with_ci_failing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(Some(&CiStatus::Failing));
        assert_eq!(buttons[0][0].callback_data, "ci_failing_merge:test-1");
        assert!(buttons[0][0].text.contains("CI failing"));
    }

    #[test]
    fn test_inline_buttons_with_ci_none_shows_merge() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(None);
        // None CI → show normal merge button (same as no CI info).
        assert_eq!(buttons[0][0].callback_data, "merge_pr:test-1");
    }

    #[test]
    fn test_inline_buttons_with_ci_non_pr_falls_back() {
        let waiting = SwarmNotification::AgentWaiting {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            summary: None,
            pr_url: Some("https://example.com/pull/1".into()),
        };
        let buttons_ci = waiting.inline_buttons_with_ci(Some(&CiStatus::Passing));
        let buttons_plain = waiting.inline_buttons();
        assert_eq!(buttons_ci.len(), buttons_plain.len());
    }

    // ===============================================================
    // Event-driven mode tests (using .swarm/events.jsonl)
    // ===============================================================

    /// Helper: set up a SwarmWatcher with both state.json and events.jsonl in a temp dir.
    /// Returns (watcher, swarm_dir) so tests can write events and state.
    fn setup_event_watcher(initial_state: &str) -> (SwarmWatcher, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let swarm_dir = dir.path().join(".swarm");
        std::fs::create_dir_all(&swarm_dir).unwrap();

        // Write initial state.json.
        std::fs::write(swarm_dir.join("state.json"), initial_state).unwrap();

        // Create an empty events.jsonl so event-driven mode activates.
        std::fs::write(swarm_dir.join("events.jsonl"), "").unwrap();

        let state_path = swarm_dir.join("state.json");
        let mut watcher = SwarmWatcher::new(state_path);
        watcher.poll(); // Initialize
        (watcher, dir)
    }

    fn append_event(dir: &tempfile::TempDir, event_json: &str) {
        use std::io::Write;
        let events_path = dir.path().join(".swarm").join("events.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(events_path)
            .unwrap();
        writeln!(f, "{event_json}").unwrap();
    }

    fn update_state(dir: &tempfile::TempDir, state_json: &str) {
        let state_path = dir.path().join(".swarm").join("state.json");
        std::fs::write(state_path, state_json).unwrap();
    }

    #[test]
    fn test_event_phase_running_emits_spawned() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/new-feat","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","summary":"Add feature"}]}"#,
        );

        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w1","from":"starting","to":"running","timestamp":"2026-02-26T10:00:05-08:00"}"#,
        );

        let notes = watcher.poll();
        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        assert_eq!(
            spawned.len(),
            1,
            "PhaseChanged to Running should emit AgentSpawned"
        );
        assert_eq!(spawned[0].worktree_id(), "w1");
        let msg = spawned[0].format_telegram();
        assert!(msg.contains("Agent spawned"));
        assert!(msg.contains("Add feature"));
    }

    #[test]
    fn test_event_phase_waiting_emits_waiting() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","summary":"Do the thing"}]}"#,
        );

        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w1","from":"running","to":"waiting","timestamp":"2026-02-26T10:05:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting.len(),
            1,
            "PhaseChanged to Waiting should emit AgentWaiting"
        );
        let msg = waiting[0].format_telegram();
        assert!(msg.contains("Worker waiting"));
        assert!(msg.contains("Do the thing"));
    }

    #[test]
    fn test_event_phase_waiting_suppressed_when_pr_set() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","pr":{"url":"https://github.com/ex/pull/1"}}]}"#,
        );

        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w1","from":"running","to":"waiting","timestamp":"2026-02-26T10:05:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "AgentWaiting should be suppressed when PR is set"
        );
    }

    #[test]
    fn test_event_phase_completed_emits_completed() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/done","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","summary":"Finish task"}]}"#,
        );

        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w1","from":"running","to":"completed","timestamp":"2026-02-26T10:10:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let completed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentCompleted { .. }))
            .collect();
        assert_eq!(
            completed.len(),
            1,
            "PhaseChanged to Completed should emit AgentCompleted"
        );
        let msg = completed[0].format_telegram();
        assert!(msg.contains("Agent finished"));
    }

    #[test]
    fn test_event_pr_detected_emits_pr_opened() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/pr-test","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"}]}"#,
        );

        // Also update state.json with the PR (so context can find it).
        update_state(
            &dir,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/pr-test","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","pr":{"url":"https://github.com/ex/pull/42","title":"Add auth"}}]}"#,
        );

        append_event(
            &dir,
            r#"{"event":"pr_detected","worktree":"w1","pr_url":"https://github.com/ex/pull/42","pr_title":"Add auth","pr_number":42,"timestamp":"2026-02-26T10:08:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(pr_notes.len(), 1, "PrDetected should emit PrOpened");

        if let SwarmNotification::PrOpened {
            pr_url, pr_title, ..
        } = &pr_notes[0]
        {
            assert_eq!(pr_url, "https://github.com/ex/pull/42");
            assert_eq!(pr_title.as_deref(), Some("Add auth"));
        }
    }

    #[test]
    fn test_event_pr_detected_dedup() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"}]}"#,
        );

        // First PrDetected event.
        append_event(
            &dir,
            r#"{"event":"pr_detected","worktree":"w1","pr_url":"https://github.com/ex/pull/1","pr_title":"PR 1","pr_number":1,"timestamp":"2026-02-26T10:08:00-08:00"}"#,
        );
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
                .count(),
            1,
            "first PrDetected should fire"
        );

        // Same PrDetected event again — should NOT emit.
        append_event(
            &dir,
            r#"{"event":"pr_detected","worktree":"w1","pr_url":"https://github.com/ex/pull/1","pr_title":"PR 1","pr_number":1,"timestamp":"2026-02-26T10:09:00-08:00"}"#,
        );
        let notes = watcher.poll();
        assert!(
            notes
                .iter()
                .all(|n| !matches!(n, SwarmNotification::PrOpened { .. })),
            "duplicate PrDetected should be suppressed"
        );
    }

    #[test]
    fn test_event_worktree_closed_emits_closed() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/old-task","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","summary":"Old task"}]}"#,
        );

        // Remove worktree from state.json (closed).
        update_state(&dir, r#"{"worktrees":[]}"#);

        append_event(
            &dir,
            r#"{"event":"worktree_closed","worktree":"w1","timestamp":"2026-02-26T10:15:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let closed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentClosed { .. }))
            .collect();
        assert_eq!(
            closed.len(),
            1,
            "WorktreeClosed event should emit AgentClosed"
        );
        let msg = closed[0].format_telegram();
        assert!(msg.contains("Worker closed"));
        assert!(msg.contains("Old task"));
    }

    #[test]
    fn test_event_worktree_merged_emits_closed() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/merged-task","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","summary":"Merged task"}]}"#,
        );

        // Remove worktree from state.json (merged).
        update_state(&dir, r#"{"worktrees":[]}"#);

        append_event(
            &dir,
            r#"{"event":"worktree_merged","worktree":"w1","timestamp":"2026-02-26T10:15:00-08:00"}"#,
        );

        let notes = watcher.poll();
        let closed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentClosed { .. }))
            .collect();
        assert_eq!(
            closed.len(),
            1,
            "WorktreeMerged event should emit AgentClosed"
        );
        let msg = closed[0].format_telegram();
        assert!(msg.contains("Worker closed"));
        assert!(msg.contains("Merged task"));
    }

    #[test]
    fn test_event_unknown_events_skipped() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/test","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"}]}"#,
        );

        // Write an unknown event type.
        append_event(
            &dir,
            r#"{"event":"some_future_event","worktree":"w1","timestamp":"2026-02-26T10:05:00-08:00"}"#,
        );

        let notes = watcher.poll();
        assert!(
            notes.is_empty(),
            "unknown events should be silently skipped"
        );
    }

    #[test]
    fn test_event_multiple_events_in_one_poll() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[
                {"id":"w1","branch":"swarm/task-a","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00","summary":"Task A"},
                {"id":"w2","branch":"swarm/task-b","agent":{"pane_id":"%2"},"created_at":"2026-02-26T10:01:00-08:00","summary":"Task B"}
            ]}"#,
        );

        // Multiple events in one poll cycle.
        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w1","from":"starting","to":"running","timestamp":"2026-02-26T10:00:05-08:00"}"#,
        );
        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w2","from":"running","to":"waiting","timestamp":"2026-02-26T10:05:00-08:00"}"#,
        );

        let notes = watcher.poll();
        assert_eq!(notes.len(), 2, "should emit one notification per event");

        let spawned = notes.iter().any(|n| matches!(n, SwarmNotification::AgentSpawned { worktree_id, .. } if worktree_id == "w1"));
        let waiting = notes.iter().any(|n| matches!(n, SwarmNotification::AgentWaiting { worktree_id, .. } if worktree_id == "w2"));
        assert!(spawned, "w1 should have AgentSpawned");
        assert!(waiting, "w2 should have AgentWaiting");
    }

    #[test]
    fn test_event_no_events_returns_empty() {
        let (mut watcher, _dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/test","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"}]}"#,
        );

        // No events written — should return empty.
        let notes = watcher.poll();
        assert!(
            notes.is_empty(),
            "no events should produce no notifications"
        );
    }

    #[test]
    fn test_event_phase_failed_emits_completed() {
        let (mut watcher, dir) = setup_event_watcher(
            r#"{"worktrees":[{"id":"w1","branch":"swarm/broken","agent":{"pane_id":"%1"},"created_at":"2026-02-26T10:00:00-08:00"}]}"#,
        );

        append_event(
            &dir,
            r#"{"event":"phase_changed","worktree":"w1","from":"creating","to":"failed","timestamp":"2026-02-26T10:00:03-08:00"}"#,
        );

        let notes = watcher.poll();
        let completed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentCompleted { .. }))
            .collect();
        assert_eq!(
            completed.len(),
            1,
            "Failed phase should emit AgentCompleted"
        );
    }

    #[test]
    fn test_event_worker_phase_serde_roundtrip() {
        // Verify all known variants deserialize correctly.
        let variants = [
            ("\"creating\"", WorkerPhase::Creating),
            ("\"starting\"", WorkerPhase::Starting),
            ("\"running\"", WorkerPhase::Running),
            ("\"waiting\"", WorkerPhase::Waiting),
            ("\"completed\"", WorkerPhase::Completed),
            ("\"failed\"", WorkerPhase::Failed),
        ];
        for (json, expected) in &variants {
            let parsed: WorkerPhase = serde_json::from_str(json).unwrap();
            assert_eq!(&parsed, expected, "failed to parse {json}");
        }

        // Unknown variant falls through to Unknown.
        let unknown: WorkerPhase = serde_json::from_str("\"some_future_phase\"").unwrap();
        assert_eq!(unknown, WorkerPhase::Unknown);
    }

    #[test]
    fn test_event_swarm_event_mirror_serde() {
        // PhaseChanged
        let json = r#"{"event":"phase_changed","worktree":"w1","from":"running","to":"waiting","timestamp":"2026-02-26T10:00:00-08:00"}"#;
        let event: SwarmEventMirror = serde_json::from_str(json).unwrap();
        assert!(matches!(event, SwarmEventMirror::PhaseChanged { .. }));

        // PrDetected
        let json = r#"{"event":"pr_detected","worktree":"w1","pr_url":"https://ex.com/1","pr_title":"Test","pr_number":1,"timestamp":"2026-02-26T10:00:00-08:00"}"#;
        let event: SwarmEventMirror = serde_json::from_str(json).unwrap();
        assert!(matches!(event, SwarmEventMirror::PrDetected { .. }));

        // WorktreeClosed
        let json = r#"{"event":"worktree_closed","worktree":"w1","timestamp":"2026-02-26T10:00:00-08:00"}"#;
        let event: SwarmEventMirror = serde_json::from_str(json).unwrap();
        assert!(matches!(event, SwarmEventMirror::WorktreeClosed { .. }));

        // WorktreeMerged
        let json = r#"{"event":"worktree_merged","worktree":"w1","timestamp":"2026-02-26T10:00:00-08:00"}"#;
        let event: SwarmEventMirror = serde_json::from_str(json).unwrap();
        assert!(matches!(event, SwarmEventMirror::WorktreeMerged { .. }));

        // Unknown event
        let json = r#"{"event":"agent_started","worktree":"w1","pane_id":"%1","timestamp":"2026-02-26T10:00:00-08:00"}"#;
        let event: SwarmEventMirror = serde_json::from_str(json).unwrap();
        assert!(matches!(event, SwarmEventMirror::Unknown));
    }
}

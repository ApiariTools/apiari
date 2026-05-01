//! Swarm session and worker discovery.
//!
//! Reads `.swarm/state.json` to discover running swarm workers.
//! Optionally reads buzz signals from `.buzz/signals.jsonl`.
//! This module is strictly read-only -- it never modifies swarm state.
//!
//! Legacy: also contains tmux-based discovery functions for backwards compat.

use chrono::{DateTime, Local, Utc};
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

// ── Types mirroring swarm's state.json ────────────────────

/// Persisted pane state (subset of swarm's `PaneState`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneState {
    pub pane_id: String,
}

/// Persisted worktree state (subset of swarm's `WorktreeState`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeState {
    pub id: String,
    pub branch: String,
    pub prompt: String,
    #[serde(default)]
    pub agent_kind: String,
    #[serde(default)]
    pub repo_path: Option<PathBuf>,
    pub worktree_path: PathBuf,
    #[serde(default)]
    pub created_at: Option<DateTime<Local>>,
    #[serde(default)]
    pub agent: Option<PaneState>,
    #[serde(default)]
    pub terminals: Vec<PaneState>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub pr: Option<WtPrInfo>,
    /// Worker lifecycle phase from swarm's state machine.
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub agent_session_status: Option<String>,
}

/// All swarm state for a workspace (subset of swarm's `SwarmState`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SwarmStateFile {
    pub session_name: String,
    #[serde(default)]
    pub sidebar_pane_id: Option<String>,
    pub worktrees: Vec<WorktreeState>,
}

// ── Buzz signal types (read from `.buzz/signals.jsonl`) ───

/// Severity levels for buzz signals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Critical => write!(f, "critical"),
            Self::Warning => write!(f, "warning"),
            Self::Info => write!(f, "info"),
        }
    }
}

/// A signal read from buzz output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuzzSignal {
    pub id: String,
    pub source: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub url: Option<String>,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub dedup_key: Option<String>,
}

/// Summary of buzz signals for display.
#[derive(Debug, Clone, Default)]
pub struct BuzzSummary {
    pub signals: Vec<BuzzSignal>,
    pub critical_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
}

// ── Agent event types (read from `.swarm/agents/<id>/events.jsonl`) ──

/// A single event from an agent's structured event log.
///
/// Fields match the `AgentEvent` enum in swarm's `agent_tui/events.rs`.
/// All fields are optional/defaulted for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventEntry {
    /// Event type: "start", "assistant_text", "tool_use", "tool_result", "complete", "error".
    #[serde(default)]
    pub r#type: String,
    /// When the event occurred.
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
    /// For tool_use/tool_result — the tool name.
    #[serde(default)]
    pub tool: Option<String>,
    /// For assistant_text — the text content.
    #[serde(default)]
    pub text: Option<String>,
    /// For tool_use — JSON string of tool input.
    #[serde(default)]
    pub input: Option<String>,
    /// For tool_result — output text.
    #[serde(default)]
    pub output: Option<String>,
    /// For tool_result — whether the tool errored.
    #[serde(default)]
    pub is_error: bool,
    /// For error — the error message.
    #[serde(default)]
    pub message: Option<String>,
    /// For start — the initial prompt.
    #[serde(default)]
    pub prompt: Option<String>,
    /// For start — the model name.
    #[serde(default)]
    pub model: Option<String>,
    /// For complete — number of turns.
    #[serde(default)]
    pub turns: Option<u64>,
    /// For complete — total cost in USD.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// For complete — session ID.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Read recent agent events from `.swarm/agents/<id>/events.jsonl`.
/// Returns the most recent `limit` events, newest last.
/// Deduplicates consecutive events with same type+content within 1s,
/// and filters out noise (e.g. TodoWrite tool events).
pub fn read_agent_events(
    session: &SwarmSession,
    worktree_id: &str,
    limit: usize,
) -> Vec<AgentEventEntry> {
    let events_path = session
        .project_dir
        .join(".swarm")
        .join("agents")
        .join(worktree_id)
        .join("events.jsonl");

    let content = match std::fs::read_to_string(&events_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut events: Vec<AgentEventEntry> = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str(trimmed).ok()
        })
        .collect();

    // Filter out noise: TodoWrite tool events are not useful for humans.
    events.retain(|e| {
        if let Some(ref tool) = e.tool {
            !tool.eq_ignore_ascii_case("todowrite")
        } else {
            true
        }
    });

    // Deduplicate consecutive events with same type+content within 1 second.
    // This fixes the double-logging from swarm (Event::Stream + Event::Assistant).
    events.dedup_by(|b, a| {
        if a.r#type != b.r#type {
            return false;
        }
        // Check timestamps are within 1s of each other.
        if let (Some(ta), Some(tb)) = (a.timestamp, b.timestamp) {
            let diff = (tb - ta).num_milliseconds().unsigned_abs();
            if diff > 1000 {
                return false;
            }
        }
        // Compare content for the event type.
        match a.r#type.as_str() {
            "assistant_text" => a.text == b.text,
            "tool_use" => a.tool == b.tool && a.input == b.input,
            "tool_result" => a.tool == b.tool,
            _ => false,
        }
    });

    // Keep only the last `limit` events.
    if events.len() > limit {
        events.drain(..events.len() - limit);
    }

    events
}

// ── PR info ───────────────────────────────────────────────

/// PR info as serialized in swarm's state.json (`pr: {number, title, state, url}`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WtPrInfo {
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Pull request information discovered via `gh` or state.json.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
}

// ── Keeper's own types ────────────────────────────────────

/// A discovered worktree with live status.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WorktreeInfo {
    pub id: String,
    pub branch: String,
    pub prompt: String,
    pub agent_kind: String,
    pub worktree_path: PathBuf,
    pub agent_pane_id: Option<String>,
    pub agent_alive: bool,
    pub terminal_count: usize,
    pub summary: Option<String>,
    pub created_at: Option<DateTime<Local>>,
    pub pr: Option<PrInfo>,
    /// Worker lifecycle phase (e.g. "creating", "running", "waiting", "completed", "failed").
    pub phase: Option<String>,
    pub agent_session_status: Option<String>,
}

/// A discovered swarm session with all its worktrees.
#[derive(Debug, Clone)]
pub struct SwarmSession {
    pub session_name: String,
    pub project_dir: PathBuf,
    pub worktrees: Vec<WorktreeInfo>,
}

/// Top-level discovery result.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub sessions: Vec<SwarmSession>,
    pub buzz: Option<BuzzSummary>,
}

// ── Discovery ─────────────────────────────────────────────

/// Discover all running swarm sessions, their state, and optional buzz signals.
pub fn discover_all() -> Result<DiscoveryResult> {
    let sessions = discover_sessions()?;

    // Try to find buzz signals from any project directory, or cwd.
    let buzz = discover_buzz_signals(&sessions);

    Ok(DiscoveryResult { sessions, buzz })
}

/// Discover all running swarm sessions and their state.
pub fn discover_sessions() -> Result<Vec<SwarmSession>> {
    let session_names = list_swarm_sessions()?;
    let mut sessions = Vec::new();

    for name in session_names {
        if let Some(session) = read_session(&name) {
            sessions.push(session);
        }
    }

    Ok(sessions)
}

/// Look for buzz signals across discovered session project directories.
fn discover_buzz_signals(sessions: &[SwarmSession]) -> Option<BuzzSummary> {
    // Check each session's project directory for .buzz/signals.jsonl
    for session in sessions {
        let signals_path = session.project_dir.join(".buzz").join("signals.jsonl");
        if signals_path.exists()
            && let Some(summary) = read_buzz_signals(&signals_path)
        {
            return Some(summary);
        }
    }

    // Also check cwd
    let cwd_signals = PathBuf::from(".buzz").join("signals.jsonl");
    if cwd_signals.exists() {
        return read_buzz_signals(&cwd_signals);
    }

    None
}

/// Read buzz signals from a JSONL file and summarize.
fn read_buzz_signals(path: &std::path::Path) -> Option<BuzzSummary> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut signals = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(signal) = serde_json::from_str::<BuzzSignal>(trimmed) {
            signals.push(signal);
        }
    }

    if signals.is_empty() {
        return None;
    }

    // Sort by timestamp descending (newest first), limit to recent 50.
    signals.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    signals.truncate(50);

    let critical_count = signals
        .iter()
        .filter(|s| s.severity == Severity::Critical)
        .count();
    let warning_count = signals
        .iter()
        .filter(|s| s.severity == Severity::Warning)
        .count();
    let info_count = signals
        .iter()
        .filter(|s| s.severity == Severity::Info)
        .count();

    Some(BuzzSummary {
        signals,
        critical_count,
        warning_count,
        info_count,
    })
}

/// Query PR info for a branch using `gh pr view`.
/// Returns None on any failure -- this is best-effort.
fn query_pr_for_branch(project_dir: &std::path::Path, branch: &str) -> Option<PrInfo> {
    let output = Command::new("gh")
        .args(["pr", "view", branch, "--json", "number,title,state,url"])
        .current_dir(project_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    #[derive(Deserialize)]
    struct GhPr {
        number: u64,
        title: String,
        state: String,
        url: String,
    }

    let pr: GhPr = serde_json::from_slice(&output.stdout).ok()?;

    Some(PrInfo {
        number: pr.number,
        title: pr.title,
        state: pr.state,
        url: pr.url,
    })
}

/// Batch-query PR info for all worktrees in a session.
/// This is relatively slow (one gh call per worktree), so the caller
/// should run it sparingly (e.g. every 30s).
pub fn refresh_pr_info(session: &mut SwarmSession) {
    for wt in &mut session.worktrees {
        // Only query if branch looks like a swarm branch
        if wt.branch.starts_with("swarm/") {
            wt.pr = query_pr_for_branch(&session.project_dir, &wt.branch);
        }
    }
}

/// List tmux sessions whose names start with `swarm-` (legacy discovery path).
fn list_swarm_sessions() -> Result<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            // tmux not installed or not in PATH
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(Vec::new());
            }
            // Other error (e.g. permission denied)
            return Ok(Vec::new());
        }
    };

    if !output.status.success() {
        // No tmux server running -- that's fine, just no sessions.
        return Ok(Vec::new());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let names: Vec<String> = text
        .lines()
        .filter(|l| l.starts_with("swarm-"))
        .map(|l| l.to_string())
        .collect();

    Ok(names)
}

/// Read a single swarm session's state.
/// Returns `None` if the state file cannot be read (session may be stale).
fn read_session(session_name: &str) -> Option<SwarmSession> {
    // Determine the project directory by querying tmux for the session's
    // starting directory (pane_current_path of the first pane).
    let project_dir = session_project_dir(session_name)?;

    let state_path = project_dir.join(".swarm").join("state.json");
    let data = match std::fs::read_to_string(&state_path) {
        Ok(d) => d,
        Err(_) => {
            // State file missing or unreadable -- session is stale or just started.
            // Return an empty session so it still shows up.
            return Some(SwarmSession {
                session_name: session_name.to_string(),
                project_dir,
                worktrees: Vec::new(),
            });
        }
    };

    let state: SwarmStateFile = match serde_json::from_str(&data) {
        Ok(s) => s,
        Err(_) => {
            // Corrupt state file -- show session with no worktrees.
            return Some(SwarmSession {
                session_name: session_name.to_string(),
                project_dir,
                worktrees: Vec::new(),
            });
        }
    };

    // Collect the set of live pane IDs in this session.
    let live_panes = list_session_pane_ids(session_name);

    let worktrees = state
        .worktrees
        .into_iter()
        .map(|wt| {
            let agent_pane_id = wt.agent.as_ref().map(|a| a.pane_id.clone());
            let agent_alive = agent_pane_id
                .as_ref()
                .is_some_and(|id| live_panes.contains(id));

            let terminal_count = wt
                .terminals
                .iter()
                .filter(|t| live_panes.contains(&t.pane_id))
                .count();

            // Convert WtPrInfo from state.json → PrInfo when url is present.
            let pr = wt.pr.and_then(|wtp| {
                let url = wtp.url?;
                if url.is_empty() {
                    return None;
                }
                Some(PrInfo {
                    number: wtp.number.unwrap_or(0),
                    title: wtp.title.unwrap_or_default(),
                    state: wtp.state.unwrap_or_else(|| "OPEN".into()),
                    url,
                })
            });

            WorktreeInfo {
                id: wt.id,
                branch: wt.branch,
                prompt: wt.prompt,
                agent_kind: wt.agent_kind.to_string(),
                worktree_path: wt.worktree_path,
                agent_pane_id,
                agent_alive,
                terminal_count,
                summary: wt.summary,
                created_at: wt.created_at,
                pr,
                phase: wt.phase,
                agent_session_status: wt.agent_session_status,
            }
        })
        .collect();

    Some(SwarmSession {
        session_name: session_name.to_string(),
        project_dir,
        worktrees,
    })
}

/// Get the project directory for a tmux session (legacy discovery path).
fn session_project_dir(session_name: &str) -> Option<PathBuf> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-t",
            session_name,
            "-F",
            "#{pane_current_path}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let first_path = text.lines().next()?.trim();

    if first_path.is_empty() {
        return None;
    }

    Some(PathBuf::from(first_path))
}

/// List all pane IDs in a tmux session (legacy discovery path).
fn list_session_pane_ids(session_name: &str) -> Vec<String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-s", "-t", session_name, "-F", "#{pane_id}"])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };

    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Discover workers from `.swarm/state.json` directly.
///
/// This is the primary discovery path. The workspace root is known from the
/// workspace registry, so we just read the state file and convert worktrees
/// into `WorktreeInfo`.
pub fn discover_from_state_file(workspace_root: &std::path::Path) -> Option<SwarmSession> {
    let state_path = workspace_root.join(".swarm").join("state.json");
    let data = std::fs::read_to_string(&state_path).ok()?;
    let state: SwarmStateFile = serde_json::from_str(&data).ok()?;

    let worktrees = state
        .worktrees
        .into_iter()
        .map(|wt| {
            // Determine agent liveness from phase field.
            let phase_str = wt.phase.as_deref().unwrap_or("unknown");
            let agent_alive = matches!(phase_str, "running" | "waiting" | "creating" | "starting");

            let pr = wt.pr.and_then(|wtp| {
                let url = wtp.url?;
                if url.is_empty() {
                    return None;
                }
                Some(PrInfo {
                    number: wtp.number.unwrap_or(0),
                    title: wtp.title.unwrap_or_default(),
                    state: wtp.state.unwrap_or_else(|| "OPEN".into()),
                    url,
                })
            });

            WorktreeInfo {
                id: wt.id,
                branch: wt.branch,
                prompt: wt.prompt,
                agent_kind: wt.agent_kind.to_string(),
                worktree_path: wt.worktree_path,
                agent_pane_id: None,
                agent_alive,
                terminal_count: 0,
                summary: wt.summary,
                created_at: wt.created_at,
                pr,
                phase: wt.phase,
                agent_session_status: wt.agent_session_status,
            }
        })
        .collect();

    Some(SwarmSession {
        session_name: state.session_name,
        project_dir: workspace_root.to_path_buf(),
        worktrees,
    })
}

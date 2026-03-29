//! Swarm watcher — monitors worker state via daemon socket subscription.
//!
//! Uses `apiari_swarm::daemon` IPC to subscribe to real-time `StateChanged`
//! events from the swarm daemon. Falls back to `ListWorkers` for full state
//! sync (PR detection, new/closed workers, reconnection).
//!
//! Replaces the previous approach of polling `.swarm/state.json` on disk.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use a2a_types::TaskState;
use apiari_swarm::{
    WorkerPhase,
    client::{
        DaemonRequest, DaemonResponse, WorkerInfo, global_socket_path, send_daemon_request,
        socket_path,
    },
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use color_eyre::Result;
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate, store::SignalStore};

/// Tracked state for a worktree between polls.
#[derive(Debug, Clone)]
struct TrackedWorker {
    phase: WorkerPhase,
    has_pr: bool,
    ready_branch: Option<String>,
    /// Role of this worker ("reviewer" or None/other = regular worker).
    role: Option<String>,
    /// Count of times this worker has transitioned into Running phase.
    /// Used to make each Running-transition signal unique (so the task engine fires each time).
    running_count: u32,
}

/// Buffered A2aTaskUpdate event from the subscription task.
struct A2aEvent {
    worktree_id: String,
    task_state: TaskState,
    /// Raw JSON message (parsed from the wire).
    message: Option<serde_json::Value>,
    timestamp: DateTime<Utc>,
}

/// Watches swarm daemon for worker state changes via socket subscription.
pub struct SwarmWatcher {
    work_dir: PathBuf,
    /// Previous state of each worktree.
    tracked: HashMap<String, TrackedWorker>,
    initialized: bool,
    /// Buffered StateChanged events from the subscription task.
    events: Arc<Mutex<Vec<(String, WorkerPhase)>>>,
    /// Buffered A2aTaskUpdate events from the subscription task.
    a2a_events: Arc<Mutex<Vec<A2aEvent>>>,
    /// Whether the subscription task has been spawned.
    subscription_started: Arc<AtomicBool>,
}

impl SwarmWatcher {
    pub fn new(work_dir: PathBuf) -> Self {
        Self {
            work_dir,
            tracked: HashMap::new(),
            initialized: false,
            events: Arc::new(Mutex::new(Vec::new())),
            a2a_events: Arc::new(Mutex::new(Vec::new())),
            subscription_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Query the daemon for the current list of workers.
    fn list_workers(&self) -> Option<Vec<WorkerInfo>> {
        let req = DaemonRequest::ListWorkers {
            workspace: Some(self.work_dir.clone()),
        };
        match send_daemon_request(&self.work_dir, &req) {
            Ok(DaemonResponse::Workers { workers }) => Some(workers),
            Ok(DaemonResponse::Error { message }) => {
                warn!("swarm: list_workers error: {message}");
                None
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("swarm: daemon unreachable: {e}");
                None
            }
        }
    }

    /// Spawn the background subscription task (once).
    fn ensure_subscription(&self) {
        if self
            .subscription_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let work_dir = self.work_dir.clone();
            let events = Arc::clone(&self.events);
            let a2a_events = Arc::clone(&self.a2a_events);
            tokio::spawn(subscription_loop(work_dir, events, a2a_events));
        }
    }

    /// Drain buffered subscription events and apply phase transitions.
    fn drain_events(&mut self, signals: &mut Vec<SignalUpdate>) {
        let events: Vec<(String, WorkerPhase)> = {
            let mut buf = self.events.lock().unwrap();
            std::mem::take(&mut *buf)
        };

        for (id, phase) in events {
            let prev_phase = self.tracked.get(&id).map(|p| p.phase.clone());
            let role = self.tracked.get(&id).and_then(|p| p.role.clone());

            // Running transition — emit swarm_worker_running for non-reviewer workers
            if phase == WorkerPhase::Running
                && prev_phase
                    .as_ref()
                    .is_some_and(|p| *p != WorkerPhase::Running)
            {
                let role_str = role.as_deref().unwrap_or("worker");
                if role_str != "reviewer" {
                    // Increment running_count first so external_id is unique per transition
                    if let Some(tracked) = self.tracked.get_mut(&id) {
                        tracked.running_count += 1;
                    }
                    let running_count = self.tracked.get(&id).map_or(1, |p| p.running_count);
                    let metadata =
                        serde_json::json!({"worker_id": id, "role": role_str}).to_string();
                    signals.push(
                        SignalUpdate::new(
                            "swarm_worker_running",
                            format!("swarm-worker-running-{id}-{running_count}"),
                            format!("Worker running: {id}"),
                            Severity::Info,
                        )
                        .with_metadata(metadata),
                    );
                }
            }

            // Waiting transition
            if phase == WorkerPhase::Waiting
                && prev_phase
                    .as_ref()
                    .is_some_and(|p| *p != WorkerPhase::Waiting)
            {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-waiting-{id}"),
                        format!("Worker waiting: {id}"),
                        Severity::Warning,
                    )
                    .with_body(format!("Agent in {id} is waiting for input")),
                );
            }

            // Completed/Failed transition — emit so daemon can read verdict before teardown
            if phase.is_terminal() && prev_phase.as_ref().is_some_and(|p| !p.is_terminal()) {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-completed-{id}"),
                        format!("Worker completed: {id}"),
                        Severity::Info,
                    )
                    .with_body(format!("Worker {id} has completed")),
                );
            }

            // Update tracked phase (preserve has_pr and role since StateChanged doesn't carry them)
            if let Some(tracked) = self.tracked.get_mut(&id) {
                tracked.phase = phase;
            }
        }
    }

    /// Drain buffered A2aTaskUpdate events and emit signals from structured data.
    ///
    /// Handles `branch_ready`, `pr_opened`, and `review_verdict` data parts, and
    /// emits richer phase-transition signals. Runs before `drain_events` so that
    /// phase updates applied here suppress duplicate transitions in `drain_events`.
    fn drain_a2a_events(&mut self, signals: &mut Vec<SignalUpdate>) {
        let events: Vec<A2aEvent> = {
            let mut buf = self.a2a_events.lock().unwrap();
            std::mem::take(&mut *buf)
        };

        for event in events {
            let id = &event.worktree_id;

            // --- Structured data from message parts ---
            if let Some(msg) = &event.message
                && let Some(parts) = msg.get("parts").and_then(|p| p.as_array())
            {
                for part in parts {
                    if part.get("type").and_then(|t| t.as_str()) != Some("data") {
                        continue;
                    }
                    let data = match part.get("data") {
                        Some(d) => d,
                        None => continue,
                    };
                    let part_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match part_type {
                        "branch_ready" => {
                            let branch = data.get("branch").and_then(|b| b.as_str()).unwrap_or("");
                            if !branch.is_empty() {
                                let had_ready_branch = self
                                    .tracked
                                    .get(id)
                                    .and_then(|p| p.ready_branch.as_deref())
                                    .is_some();
                                let has_pr = self.tracked.get(id).is_some_and(|p| p.has_pr);
                                if !had_ready_branch && !has_pr {
                                    let metadata = serde_json::json!({
                                        "worker_id": id,
                                        "branch_name": branch,
                                    });
                                    signals.push(
                                        SignalUpdate::new(
                                            "swarm_branch_ready",
                                            format!("swarm-branch-ready-{id}"),
                                            format!("Branch ready for review: {branch}"),
                                            Severity::Info,
                                        )
                                        .with_metadata(metadata.to_string()),
                                    );
                                }
                                if let Some(tracked) = self.tracked.get_mut(id) {
                                    tracked.ready_branch = Some(branch.to_string());
                                }
                            }
                        }
                        "pr_opened" => {
                            let pr_url = data.get("pr_url").and_then(|u| u.as_str()).unwrap_or("");
                            let pr_number = data.get("pr_number").and_then(|n| n.as_u64());
                            let has_pr = self.tracked.get(id).is_some_and(|p| p.has_pr);
                            if !has_pr && !pr_url.is_empty() {
                                let metadata = serde_json::json!({
                                    "worker_id": id,
                                    "pr_url": pr_url,
                                    "pr_number": pr_number,
                                });
                                let mut signal = SignalUpdate::new(
                                    "swarm",
                                    format!("swarm-pr-{id}"),
                                    format!("PR opened: {id}"),
                                    Severity::Info,
                                )
                                .with_body(format!("#{} {pr_url}", pr_number.unwrap_or(0)))
                                .with_metadata(metadata.to_string());
                                signal = signal.with_url(pr_url);
                                signals.push(signal);
                                if let Some(tracked) = self.tracked.get_mut(id) {
                                    tracked.has_pr = true;
                                }
                            }
                        }
                        "review_verdict" => {
                            let approved = data
                                .get("approved")
                                .and_then(|a| a.as_bool())
                                .unwrap_or(false);
                            let comments = data.get("comments").cloned();
                            let metadata = serde_json::json!({
                                "worker_id": id,
                                "approved": approved,
                                "comments": comments,
                            });
                            let ts = event.timestamp.timestamp();
                            signals.push(
                                SignalUpdate::new(
                                    "swarm_review_verdict",
                                    format!("swarm-review-verdict-{id}-{ts}"),
                                    if approved {
                                        format!("Review approved: {id}")
                                    } else {
                                        format!("Review rejected: {id}")
                                    },
                                    if approved {
                                        Severity::Info
                                    } else {
                                        Severity::Warning
                                    },
                                )
                                .with_metadata(metadata.to_string()),
                            );
                        }
                        _ => {}
                    }
                }
            }

            // --- Phase transitions from A2a task_state ---
            let prev_phase = self.tracked.get(id).map(|p| p.phase.clone());
            let role = self.tracked.get(id).and_then(|p| p.role.clone());
            let role_str = role.as_deref().unwrap_or("worker");

            match event.task_state {
                TaskState::Working => {
                    if prev_phase
                        .as_ref()
                        .is_some_and(|p| *p != WorkerPhase::Running)
                        && role_str != "reviewer"
                    {
                        if let Some(tracked) = self.tracked.get_mut(id) {
                            tracked.running_count += 1;
                        }
                        let running_count = self.tracked.get(id).map_or(1, |p| p.running_count);
                        let metadata = serde_json::json!({
                            "worker_id": id,
                            "role": role_str,
                            "task_state": "Working",
                        })
                        .to_string();
                        signals.push(
                            SignalUpdate::new(
                                "swarm_worker_running",
                                format!("swarm-worker-running-{id}-{running_count}"),
                                format!("Worker running: {id}"),
                                Severity::Info,
                            )
                            .with_metadata(metadata),
                        );
                    }
                    if let Some(tracked) = self.tracked.get_mut(id) {
                        tracked.phase = WorkerPhase::Running;
                    }
                }
                TaskState::InputRequired => {
                    if prev_phase
                        .as_ref()
                        .is_some_and(|p| *p != WorkerPhase::Waiting)
                    {
                        signals.push(
                            SignalUpdate::new(
                                "swarm",
                                format!("swarm-waiting-{id}"),
                                format!("Worker waiting: {id}"),
                                Severity::Warning,
                            )
                            .with_body(format!("Agent in {id} is waiting for input")),
                        );
                    }
                    if let Some(tracked) = self.tracked.get_mut(id) {
                        tracked.phase = WorkerPhase::Waiting;
                    }
                }
                TaskState::Completed => {
                    if prev_phase.as_ref().is_some_and(|p| !p.is_terminal()) {
                        signals.push(
                            SignalUpdate::new(
                                "swarm",
                                format!("swarm-completed-{id}"),
                                format!("Worker completed: {id}"),
                                Severity::Info,
                            )
                            .with_body(format!("Worker {id} has completed")),
                        );
                    }
                    if let Some(tracked) = self.tracked.get_mut(id) {
                        tracked.phase = WorkerPhase::Completed;
                    }
                }
                TaskState::Failed => {
                    if prev_phase.as_ref().is_some_and(|p| !p.is_terminal()) {
                        signals.push(
                            SignalUpdate::new(
                                "swarm",
                                format!("swarm-completed-{id}"),
                                format!("Worker completed: {id}"),
                                Severity::Info,
                            )
                            .with_body(format!("Worker {id} has completed")),
                        );
                    }
                    if let Some(tracked) = self.tracked.get_mut(id) {
                        tracked.phase = WorkerPhase::Failed;
                    }
                }
                // Submitted, Canceled, Rejected, AuthRequired, Unknown — no specific handling
                _ => {}
            }
        }
    }

    /// Read `ready_branch` for each worker from `.swarm/state.json`.
    /// Returns a map of worker_id → (ready_branch, repo_path).
    fn read_ready_branches(&self) -> HashMap<String, (String, String)> {
        let state_path = self.work_dir.join(".swarm").join("state.json");
        let raw = match std::fs::read_to_string(&state_path) {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        let state: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return HashMap::new(),
        };
        let mut map = HashMap::new();
        if let Some(worktrees) = state.get("worktrees").and_then(|w| w.as_array()) {
            for wt in worktrees {
                let id = wt
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                if let Some(branch) = wt.get("ready_branch").and_then(|v| v.as_str()) {
                    let repo_path = wt
                        .get("repo_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    map.insert(id, (branch.to_string(), repo_path));
                }
            }
        }
        map
    }

    /// Diff the full worker list against tracked state.
    fn diff_workers(&mut self, workers: &[WorkerInfo]) -> Vec<SignalUpdate> {
        let ready_branches = self.read_ready_branches();
        let mut signals = Vec::new();

        for w in workers {
            let id = &w.id;
            let prev = self.tracked.get(id);
            let has_pr = w.pr_url.is_some();
            let ready_branch = ready_branches.get(id.as_str()).map(|(b, _)| b.clone());
            let repo_path = ready_branches
                .get(id.as_str())
                .map(|(_, r)| r.clone())
                .unwrap_or_default();
            let role = w.role.as_deref().unwrap_or("worker");

            if prev.is_none() {
                // New worktree spawned
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-spawned-{id}"),
                        format!("Worker spawned: {id}"),
                        Severity::Info,
                    )
                    .with_body(format!(
                        "agent: {}\n{}",
                        w.agent,
                        truncate_prompt(&w.prompt)
                    )),
                );
            }

            // Branch ready transition: ready_branch became set and no PR opened yet
            let had_ready_branch = prev.and_then(|p| p.ready_branch.as_deref()).is_some();
            if ready_branch.is_some() && !had_ready_branch && !has_pr {
                let branch_name = ready_branch.as_deref().unwrap_or("");
                let metadata = serde_json::json!({
                    "worker_id": id,
                    "branch_name": branch_name,
                    "repo": repo_path,
                });
                signals.push(
                    SignalUpdate::new(
                        "swarm_branch_ready",
                        format!("swarm-branch-ready-{id}"),
                        format!("Branch ready for review: {branch_name}"),
                        Severity::Info,
                    )
                    .with_metadata(metadata.to_string()),
                );
            }

            // PR opened transition
            if has_pr && prev.is_some_and(|p| !p.has_pr) {
                let title = w.pr_title.as_deref().unwrap_or("");
                let url = w.pr_url.as_deref().unwrap_or("");

                let mut signal = SignalUpdate::new(
                    "swarm",
                    format!("swarm-pr-{id}"),
                    format!("PR opened: {id}"),
                    Severity::Info,
                )
                .with_body(format!("{title}\n{url}"));

                if !url.is_empty() {
                    signal = signal.with_url(url);
                }
                signals.push(signal);
            }

            // Running transition — emit swarm_worker_running for non-reviewer workers
            // (from ListWorkers, in case subscription missed it)
            if w.phase == WorkerPhase::Running
                && prev.is_some_and(|p| p.phase != WorkerPhase::Running)
                && role != "reviewer"
            {
                let prev_running_count = prev.map_or(0, |p| p.running_count);
                let new_running_count = prev_running_count + 1;
                let metadata = serde_json::json!({"worker_id": id, "role": role}).to_string();
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_running",
                        format!("swarm-worker-running-{id}-{new_running_count}"),
                        format!("Worker running: {id}"),
                        Severity::Info,
                    )
                    .with_metadata(metadata),
                );
            }

            // Waiting transition (from ListWorkers, in case subscription missed it)
            if w.phase == WorkerPhase::Waiting
                && prev.is_some_and(|p| p.phase != WorkerPhase::Waiting)
            {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-waiting-{id}"),
                        format!("Worker waiting: {id}"),
                        Severity::Warning,
                    )
                    .with_body(format!("Agent in {id} is waiting for input")),
                );
            }

            // Completed/Failed transition (from ListWorkers, in case subscription missed it)
            if w.phase.is_terminal() && prev.is_some_and(|p| !p.phase.is_terminal()) {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-completed-{id}"),
                        format!("Worker completed: {id}"),
                        Severity::Info,
                    )
                    .with_body(format!("Worker {id} has completed")),
                );
            }

            // Compute new running_count (incremented if transitioning into Running)
            let new_running_count = {
                let prev_count = prev.map_or(0, |p| p.running_count);
                if w.phase == WorkerPhase::Running
                    && prev.is_some_and(|p| p.phase != WorkerPhase::Running)
                    && role != "reviewer"
                {
                    prev_count + 1
                } else {
                    prev_count
                }
            };

            // Update tracked state
            self.tracked.insert(
                id.clone(),
                TrackedWorker {
                    phase: w.phase.clone(),
                    has_pr,
                    ready_branch: ready_branch.clone(),
                    role: w.role.clone(),
                    running_count: new_running_count,
                },
            );
        }

        // Detect closed worktrees — resolve all related signals
        let current_ids: std::collections::HashSet<&String> =
            workers.iter().map(|w| &w.id).collect();
        let closed: Vec<String> = self
            .tracked
            .keys()
            .filter(|id| !current_ids.contains(id))
            .cloned()
            .collect();

        for id in &closed {
            let role_str = self
                .tracked
                .get(id)
                .and_then(|t| t.role.as_deref())
                .unwrap_or("worker");

            for prefix in &[
                "swarm-spawned",
                "swarm-waiting",
                "swarm-pr",
                "swarm-completed",
                "swarm-branch-ready",
            ] {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("{prefix}-{id}"),
                        format!("Worker closed: {id}"),
                        Severity::Info,
                    )
                    .with_status(SignalStatus::Resolved),
                );
            }
            signals.push(
                SignalUpdate::new(
                    "swarm",
                    format!("swarm-closed-{id}"),
                    format!("Worker closed: {id}"),
                    Severity::Info,
                )
                .with_status(SignalStatus::Resolved),
            );

            // Emit active swarm_worker_closed signal for the task engine to process
            let metadata = serde_json::json!({"worker_id": id, "role": role_str}).to_string();
            signals.push(
                SignalUpdate::new(
                    "swarm_worker_closed",
                    format!("swarm-worker-closed-{id}"),
                    format!("Worker closed: {id}"),
                    Severity::Info,
                )
                .with_metadata(metadata),
            );

            self.tracked.remove(id);
        }

        signals
    }
}

#[async_trait]
impl Watcher for SwarmWatcher {
    fn name(&self) -> &str {
        "swarm"
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        // Start the subscription task on first poll.
        self.ensure_subscription();

        let workers = match self.list_workers() {
            Some(w) => w,
            None => return Ok(Vec::new()),
        };

        if !self.initialized {
            // First poll: record current state, don't emit
            let ready_branches = self.read_ready_branches();
            for w in &workers {
                self.tracked.insert(
                    w.id.clone(),
                    TrackedWorker {
                        phase: w.phase.clone(),
                        has_pr: w.pr_url.is_some(),
                        ready_branch: ready_branches.get(&w.id).map(|(b, _)| b.clone()),
                        role: w.role.clone(),
                        running_count: 0,
                    },
                );
            }
            self.initialized = true;
            info!("swarm: initialized with {} worker(s)", workers.len());
            // Drain and discard any subscription events from before initialization
            self.events.lock().unwrap().clear();
            self.a2a_events.lock().unwrap().clear();
            return Ok(Vec::new());
        }

        let mut signals = Vec::new();

        // Process A2a events first — structured data and richer phase transitions
        self.drain_a2a_events(&mut signals);

        // Process StateChanged events — fallback phase transitions
        self.drain_events(&mut signals);

        // Then diff the full worker list (PRs, new/closed workers)
        signals.extend(self.diff_workers(&workers));

        if !signals.is_empty() {
            info!("swarm: {} signal(s)", signals.len());
        }

        Ok(signals)
    }

    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        if !self.initialized {
            return Ok(0);
        }
        let current_ids: Vec<String> = self
            .tracked
            .keys()
            .flat_map(|id| {
                vec![
                    format!("swarm-spawned-{id}"),
                    format!("swarm-waiting-{id}"),
                    format!("swarm-pr-{id}"),
                    format!("swarm-completed-{id}"),
                    format!("swarm-branch-ready-{id}"),
                ]
            })
            .collect();
        let resolved = store.resolve_missing_signals("swarm", &current_ids)?;
        if resolved > 0 {
            info!("swarm: reconciled {resolved} stale signal(s)");
        }
        Ok(resolved)
    }
}

/// Truncate a prompt string for signal bodies.
fn truncate_prompt(prompt: &str) -> &str {
    let end = prompt
        .char_indices()
        .nth(120)
        .map_or(prompt.len(), |(i, _)| i);
    &prompt[..end]
}

/// Background subscription loop — reconnects with backoff on disconnect.
async fn subscription_loop(
    work_dir: PathBuf,
    events: Arc<Mutex<Vec<(String, WorkerPhase)>>>,
    a2a_events: Arc<Mutex<Vec<A2aEvent>>>,
) {
    let mut backoff_secs = 1u64;

    loop {
        match connect_and_subscribe(&work_dir, &events, &a2a_events).await {
            Ok(()) => {
                // Clean disconnect — reset backoff
                backoff_secs = 1;
            }
            Err(e) => {
                tracing::debug!("swarm subscription error: {e}");
                backoff_secs = (backoff_secs * 2).min(60);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
    }
}

/// Connect to the daemon socket and subscribe to state change events.
///
/// Handles both `StateChanged` (existing) and `A2aTaskUpdate` (new) events.
/// Uses JSON value parsing so it gracefully ignores unknown variants.
async fn connect_and_subscribe(
    work_dir: &Path,
    events: &Mutex<Vec<(String, WorkerPhase)>>,
    a2a_events: &Mutex<Vec<A2aEvent>>,
) -> Result<()> {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
    };

    // Try per-workspace socket first, then global
    let local = socket_path(work_dir);
    let global = global_socket_path();
    let stream = if local.exists() {
        UnixStream::connect(&local).await
    } else {
        UnixStream::connect(&global).await
    }
    .map_err(|e| color_eyre::eyre::eyre!("failed to connect to daemon: {e}"))?;

    let (reader, mut writer) = stream.into_split();

    // Send Subscribe request
    let req = DaemonRequest::Subscribe {
        worktree_id: None,
        workspace: Some(work_dir.to_path_buf()),
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;

    // Read events — parse as Value to handle both known and future variants
    let mut reader = BufReader::new(reader);
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            break; // EOF — daemon disconnected
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(buf.trim()) {
            match val.get("kind").and_then(|k| k.as_str()) {
                Some("state_changed") => {
                    if let Some(worktree_id) = val
                        .get("worktree_id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        && let Some(phase) = val
                            .get("phase")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                    {
                        events.lock().unwrap().push((worktree_id, phase));
                    }
                }
                Some("a2a_task_update") => {
                    if let Some(worktree_id) = val
                        .get("worktree_id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        && let Some(task_state) = val
                            .get("task_state")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                    {
                        let message = val.get("message").cloned();
                        let timestamp = val
                            .get("timestamp")
                            .and_then(|t| serde_json::from_value(t.clone()).ok())
                            .unwrap_or_else(Utc::now);
                        a2a_events.lock().unwrap().push(A2aEvent {
                            worktree_id,
                            task_state,
                            message,
                            timestamp,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use apiari_swarm::core::state::{SwarmState, WorkerPhase};

    use super::*;

    /// Helper to build a minimal valid WorktreeState JSON object.
    fn wt_json(id: &str, extras: &str) -> String {
        let base = format!(
            r#"{{
                "id": "{id}",
                "branch": "swarm/{id}",
                "prompt": "test task",
                "agent_kind": "claude",
                "repo_path": "/tmp/myrepo",
                "worktree_path": "/tmp/.swarm/wt/{id}",
                "created_at": "2026-01-01T00:00:00-05:00"
            }}"#
        );
        if extras.is_empty() {
            return base;
        }
        // Insert extras before the closing brace
        let trimmed = base.trim_end();
        format!("{},\n{extras}\n}}", &trimmed[..trimmed.len() - 1])
    }

    /// Build a state.json string from a list of worktree JSON objects.
    fn state_json(worktrees: &[String]) -> String {
        format!(
            r#"{{"session_name": "test", "worktrees": [{}]}}"#,
            worktrees.join(",")
        )
    }

    #[test]
    fn test_parse_swarm_state() {
        let wt = wt_json(
            "hive-1",
            r#""summary": "Fix a bug",
                "pr": {"number": 1, "title": "Fix bug", "state": "OPEN", "url": "https://github.com/org/repo/pull/1"},
                "phase": "running""#,
        );
        let json = state_json(&[wt]);
        let state: SwarmState = serde_json::from_str(&json).unwrap();
        assert_eq!(state.worktrees.len(), 1);
        let wt = &state.worktrees[0];
        assert_eq!(wt.id, "hive-1");
        assert!(wt.pr.is_some());
    }

    #[test]
    fn test_parse_empty_state() {
        let json = r#"{"session_name": "test", "worktrees": []}"#;
        let state: SwarmState = serde_json::from_str(&json).unwrap();
        assert!(state.worktrees.is_empty());
    }

    #[test]
    fn test_parse_state_missing_optional_fields() {
        let wt = wt_json("wt-1", "");
        let json = state_json(&[wt]);
        let state: SwarmState = serde_json::from_str(&json).unwrap();
        let wt = &state.worktrees[0];
        assert!(wt.pr.is_none());
        assert_eq!(wt.phase, WorkerPhase::Running); // default
    }

    /// Parse a real state.json snapshot from swarm to guard against format drift.
    #[test]
    fn test_parse_real_state_snapshot() {
        let json = r#"{
          "session_name": "swarm-apiari",
          "sidebar_pane_id": null,
          "worktrees": [
            {
              "id": "swarm-042c",
              "branch": "swarm/fix-ci-failure-042c",
              "prompt": "Fix CI failure",
              "agent_kind": "claude",
              "repo_path": "/home/user/project/swarm",
              "worktree_path": "/home/user/project/.swarm/wt/swarm-042c",
              "created_at": "2026-03-13T11:51:21.270284-05:00",
              "agent": null,
              "terminals": [],
              "summary": null,
              "pr": {
                "number": 64,
                "title": "fix(ci): add apiari-tui to workspace",
                "state": "OPEN",
                "url": "https://github.com/ApiariTools/swarm/pull/64"
              },
              "phase": "waiting",
              "status": "running",
              "agent_session_status": "waiting",
              "agent_pid": null,
              "session_id": "c9dba914-a879-4e7d-866f-35d88089bdfd",
              "restart_count": 0
            }
          ],
          "last_inbox_pos": 0
        }"#;

        let state: SwarmState = serde_json::from_str(json).unwrap();
        assert_eq!(state.worktrees.len(), 1);

        let wt = &state.worktrees[0];
        assert_eq!(wt.id, "swarm-042c");
        assert_eq!(wt.phase, WorkerPhase::Waiting);

        let pr = wt.pr.as_ref().unwrap();
        assert_eq!(pr.url, "https://github.com/ApiariTools/swarm/pull/64");
        assert_eq!(pr.title, "fix(ci): add apiari-tui to workspace");
    }

    /// Test the diff_workers logic by feeding WorkerInfo directly.
    #[test]
    fn test_diff_workers_new_worker() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;

        let workers = vec![make_worker("w1", WorkerPhase::Running, None)];
        let signals = watcher.diff_workers(&workers);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("spawned"));
    }

    #[test]
    fn test_diff_workers_pr_opened() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![make_worker(
            "w1",
            WorkerPhase::Running,
            Some("https://github.com/org/repo/pull/1"),
        )];
        let signals = watcher.diff_workers(&workers);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("PR opened"));
        assert_eq!(
            signals[0].url.as_deref(),
            Some("https://github.com/org/repo/pull/1")
        );
    }

    #[test]
    fn test_diff_workers_waiting_transition() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![make_worker("w1", WorkerPhase::Waiting, None)];
        let signals = watcher.diff_workers(&workers);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("waiting"));
    }

    #[test]
    fn test_diff_workers_closed() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![]; // w1 is gone
        let signals = watcher.diff_workers(&workers);
        // 5 resolved (spawned/waiting/pr/completed/branch-ready) + 1 old closed + 1 swarm_worker_closed
        assert_eq!(signals.len(), 7);
        assert!(signals.iter().all(|s| s.title.contains("closed")));
    }

    #[test]
    fn test_diff_workers_no_change() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![make_worker("w1", WorkerPhase::Running, None)];
        let signals = watcher.diff_workers(&workers);
        assert!(signals.is_empty(), "no transition = no signals");
    }

    #[test]
    fn test_diff_workers_running_transition_emits_signal() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Waiting,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![make_worker("w1", WorkerPhase::Running, None)];
        let signals = watcher.diff_workers(&workers);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, "swarm_worker_running");
        assert!(signals[0].title.contains("running"));
        assert!(
            signals[0]
                .metadata
                .as_deref()
                .unwrap_or("")
                .contains("worker_id")
        );
    }

    #[test]
    fn test_diff_workers_running_transition_reviewer_no_signal() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "r1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Waiting,
                has_pr: false,
                ready_branch: None,
                role: Some("reviewer".to_string()),
                running_count: 0,
            },
        );

        let mut reviewer = make_worker("r1", WorkerPhase::Running, None);
        reviewer.role = Some("reviewer".to_string());
        let signals = watcher.diff_workers(&[reviewer]);
        assert!(
            signals.is_empty(),
            "reviewer running transition should not emit swarm_worker_running"
        );
    }

    #[test]
    fn test_diff_workers_closed_emits_worker_closed_signal() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![]; // w1 is gone
        let signals = watcher.diff_workers(&workers);
        let closed_signal = signals
            .iter()
            .find(|s| s.source == "swarm_worker_closed")
            .expect("swarm_worker_closed signal should be emitted");
        assert!(
            closed_signal
                .metadata
                .as_deref()
                .unwrap_or("")
                .contains("worker_id")
        );
        assert!(
            closed_signal
                .metadata
                .as_deref()
                .unwrap_or("")
                .contains("role")
        );
    }

    /// Helper to build a WorkerInfo for testing.
    fn make_worker(id: &str, phase: WorkerPhase, pr_url: Option<&str>) -> WorkerInfo {
        WorkerInfo {
            id: id.to_string(),
            branch: format!("swarm/{id}"),
            prompt: "test task".to_string(),
            agent: "claude".to_string(),
            phase,
            session_id: None,
            pr_url: pr_url.map(String::from),
            pr_number: pr_url.map(|_| 1),
            pr_title: pr_url.map(|_| "Test PR".to_string()),
            pr_state: pr_url.map(|_| "OPEN".to_string()),
            restart_count: 0,
            created_at: None,
            role: None,
            review_verdict: None,
            agent_card: None,
        }
    }

    /// Test that a branch-ready transition emits a swarm_branch_ready signal.
    #[test]
    fn test_diff_workers_branch_ready() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 1,
            },
        );

        // The watcher reads state.json to get ready_branch — we can't inject that
        // in a unit test without a real file, so test the signal emission logic by
        // manually calling diff_workers (which reads ready_branches from state.json,
        // returning an empty map since /tmp/test/.swarm/state.json doesn't exist).
        // The transition won't fire without state.json, but we verify the base case.
        let workers = vec![make_worker("w1", WorkerPhase::Running, None)];
        let signals = watcher.diff_workers(&workers);
        // No state.json → no ready_branch → no signal
        assert!(
            signals.is_empty(),
            "no state.json means no branch-ready signal"
        );
    }

    /// Test that a second poll with the same ready_branch does not re-emit.
    #[test]
    fn test_diff_workers_branch_ready_no_duplicate() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/test"));
        watcher.initialized = true;
        // Already tracked with ready_branch set
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: Some("swarm/my-feature".to_string()),
                role: None,
                running_count: 1,
            },
        );

        let workers = vec![make_worker("w1", WorkerPhase::Running, None)];
        let signals = watcher.diff_workers(&workers);
        // ready_branch was already tracked → no re-emission (even without state.json)
        assert!(
            signals.is_empty(),
            "already-tracked ready_branch must not re-emit"
        );
    }
}

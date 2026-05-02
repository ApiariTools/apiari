//! Swarm watcher — monitors worker state via daemon socket subscription.
//!
//! Pure state-fetcher: polls the daemon for worker state, compares to previous
//! state, and emits signals for changes. All business logic (notifications,
//! task transitions, PR creation, review dispatch) lives in the orchestrator.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use apiari_swarm::{
    WorkerPhase,
    client::{
        DaemonRequest, DaemonResponse, WorkerInfo, global_socket_path, send_daemon_request,
        socket_path,
    },
};
use async_trait::async_trait;
use color_eyre::Result;
use tracing::info;

use super::Watcher;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate, store::SignalStore};

/// Tracked state for a worker between polls.
#[derive(Debug, Clone)]
struct TrackedWorker {
    phase: WorkerPhase,
    has_pr: bool,
    ready_branch: Option<String>,
    role: Option<String>,
    running_count: u32,
}

/// Watches swarm daemon for worker state changes.
pub struct SwarmWatcher {
    work_dir: PathBuf,
    state_path: PathBuf,
    tracked: HashMap<String, TrackedWorker>,
    initialized: bool,
    /// Buffered phase transitions from the subscription task.
    events: Arc<Mutex<Vec<(String, WorkerPhase)>>>,
    subscription_started: Arc<AtomicBool>,
}

impl SwarmWatcher {
    pub fn new(work_dir: PathBuf, state_path: PathBuf) -> Self {
        Self {
            work_dir,
            state_path,
            tracked: HashMap::new(),
            initialized: false,
            events: Arc::new(Mutex::new(Vec::new())),
            subscription_started: Arc::new(AtomicBool::new(false)),
        }
    }

    fn list_workers(&self) -> Option<Vec<WorkerInfo>> {
        let req = DaemonRequest::ListWorkers {
            workspace: Some(self.work_dir.clone()),
        };
        match send_daemon_request(&self.work_dir, &req) {
            Ok(DaemonResponse::Workers { workers }) => Some(workers),
            Ok(DaemonResponse::Error { message }) => {
                tracing::warn!("swarm: list_workers error: {message}");
                None
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("swarm: daemon unreachable: {e}");
                None
            }
        }
    }

    fn ensure_subscription(&self) {
        if self
            .subscription_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let work_dir = self.work_dir.clone();
            let events = Arc::clone(&self.events);
            tokio::spawn(subscription_loop(work_dir, events));
        }
    }

    /// Apply buffered subscription events as phase transition signals.
    fn drain_events(&mut self, signals: &mut Vec<SignalUpdate>) {
        let events: Vec<(String, WorkerPhase)> = std::mem::take(&mut *self.events.lock().unwrap());
        for (id, phase) in events {
            let prev_phase = self.tracked.get(&id).map(|p| p.phase.clone());
            let role = self
                .tracked
                .get(&id)
                .and_then(|p| p.role.clone())
                .unwrap_or_else(|| "worker".to_string());

            if phase == WorkerPhase::Running
                && prev_phase
                    .as_ref()
                    .is_some_and(|p| *p != WorkerPhase::Running)
                && role != "reviewer"
            {
                if let Some(t) = self.tracked.get_mut(&id) {
                    t.running_count += 1;
                }
                let rc = self.tracked.get(&id).map_or(1, |p| p.running_count);
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_running",
                        format!("swarm-worker-running-{id}-{rc}"),
                        format!("Worker running: {id}"),
                        Severity::Info,
                    )
                    .with_metadata(serde_json::json!({"worker_id": id, "role": role}).to_string()),
                );
            }
            if phase == WorkerPhase::Waiting
                && prev_phase
                    .as_ref()
                    .is_some_and(|p| *p != WorkerPhase::Waiting)
            {
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_waiting",
                        format!("swarm-waiting-{id}"),
                        format!("Worker waiting: {id}"),
                        Severity::Warning,
                    )
                    .with_body(format!("Agent in {id} is waiting for input")),
                );
            }
            if phase.is_terminal() && prev_phase.as_ref().is_some_and(|p| !p.is_terminal()) {
                signals.push(SignalUpdate::new(
                    "swarm_worker_closed",
                    format!("swarm-completed-{id}"),
                    format!("Worker completed: {id}"),
                    Severity::Info,
                ));
            }
            if let Some(t) = self.tracked.get_mut(&id) {
                t.phase = phase;
            }
        }
    }

    /// Read `ready_branch` from `.swarm/state.json`.
    fn read_ready_branches(&self) -> HashMap<String, (String, String)> {
        let raw = match std::fs::read_to_string(&self.state_path) {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        let state: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return HashMap::new(),
        };
        let mut map = HashMap::new();
        if let Some(wts) = state.get("worktrees").and_then(|w| w.as_array()) {
            for wt in wts {
                let id = wt.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    continue;
                }
                if let Some(branch) = wt.get("ready_branch").and_then(|v| v.as_str()) {
                    let repo = wt
                        .get("repo_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    map.insert(id.to_string(), (branch.to_string(), repo));
                }
            }
        }
        map
    }

    /// Diff the full worker list against tracked state, emit signals.
    fn diff_workers(&mut self, workers: &[WorkerInfo]) -> Vec<SignalUpdate> {
        let ready_branches = self.read_ready_branches();
        let mut signals = Vec::new();

        for w in workers {
            let id = &w.id;
            let prev = self.tracked.get(id);
            let has_pr = w.pr_url.is_some();
            let ready_branch = ready_branches.get(id.as_str()).map(|(b, _)| b.clone());
            let role = w.role.as_deref().unwrap_or("worker");

            // New worker spawned
            if prev.is_none() {
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_spawned",
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

            // Branch ready transition
            let had_ready = prev.and_then(|p| p.ready_branch.as_deref()).is_some();
            if ready_branch.is_some() && !had_ready && !has_pr {
                let branch = ready_branch.as_deref().unwrap_or("");
                let repo = ready_branches
                    .get(id.as_str())
                    .map(|(_, r)| r.as_str())
                    .unwrap_or("");
                signals.push(
                    SignalUpdate::new(
                        "swarm_branch_ready",
                        format!("swarm-branch-ready-{id}"),
                        format!("Branch ready for review: {branch}"),
                        Severity::Info,
                    )
                    .with_metadata(
                        serde_json::json!({"worker_id": id, "branch_name": branch, "repo": repo})
                            .to_string(),
                    ),
                );
            }

            // PR opened transition
            if has_pr && prev.is_some_and(|p| !p.has_pr) {
                let url = w.pr_url.as_deref().unwrap_or("");
                let title = w.pr_title.as_deref().unwrap_or("");
                let pr_number = w.pr_number;
                let mut sig = SignalUpdate::new(
                    "swarm_pr_opened",
                    format!("swarm-pr-{id}"),
                    format!("PR opened: {id}"),
                    Severity::Info,
                )
                .with_body(format!("{title}\n{url}"))
                .with_metadata(
                    serde_json::json!({"worker_id": id, "pr_url": url, "pr_number": pr_number})
                        .to_string(),
                );
                if !url.is_empty() {
                    sig = sig.with_url(url);
                }
                signals.push(sig);
            }

            // Running transition (supplements subscription for reliability)
            if w.phase == WorkerPhase::Running
                && prev.is_some_and(|p| p.phase != WorkerPhase::Running)
                && role != "reviewer"
            {
                let rc = prev.map_or(0, |p| p.running_count) + 1;
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_running",
                        format!("swarm-worker-running-{id}-{rc}"),
                        format!("Worker running: {id}"),
                        Severity::Info,
                    )
                    .with_metadata(serde_json::json!({"worker_id": id, "role": role}).to_string()),
                );
            }

            // Waiting transition
            if w.phase == WorkerPhase::Waiting
                && prev.is_some_and(|p| p.phase != WorkerPhase::Waiting)
            {
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_waiting",
                        format!("swarm-waiting-{id}"),
                        format!("Worker waiting: {id}"),
                        Severity::Warning,
                    )
                    .with_body(format!("Agent in {id} is waiting for input")),
                );
            }

            // Completed/Failed transition
            if w.phase.is_terminal() && prev.is_some_and(|p| !p.phase.is_terminal()) {
                signals.push(SignalUpdate::new(
                    "swarm_worker_closed",
                    format!("swarm-completed-{id}"),
                    format!("Worker completed: {id}"),
                    Severity::Info,
                ));
            }

            // Update tracked state
            let running_count = {
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
            self.tracked.insert(
                id.clone(),
                TrackedWorker {
                    phase: w.phase.clone(),
                    has_pr,
                    ready_branch,
                    role: w.role.clone(),
                    running_count,
                },
            );
        }

        // Detect closed workers
        let current_ids: std::collections::HashSet<&String> =
            workers.iter().map(|w| &w.id).collect();
        let closed: Vec<String> = self
            .tracked
            .keys()
            .filter(|id| !current_ids.contains(id))
            .cloned()
            .collect();

        for id in &closed {
            let role = self
                .tracked
                .get(id)
                .and_then(|t| t.role.as_deref())
                .unwrap_or("worker");

            // Resolve related signals (new source names)
            for (source, prefix) in [
                ("swarm_worker_spawned", "swarm-spawned"),
                ("swarm_worker_waiting", "swarm-waiting"),
                ("swarm_pr_opened", "swarm-pr"),
                ("swarm_branch_ready", "swarm-branch-ready"),
            ] {
                signals.push(
                    SignalUpdate::new(
                        source,
                        format!("{prefix}-{id}"),
                        format!("Worker closed: {id}"),
                        Severity::Info,
                    )
                    .with_status(SignalStatus::Resolved),
                );
            }
            // Also resolve old "swarm" source signals for backward compat
            for prefix in [
                "swarm-spawned",
                "swarm-waiting",
                "swarm-pr",
                "swarm-completed",
                "swarm-branch-ready",
                "swarm-closed",
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

            // Emit active closed signal
            signals.push(
                SignalUpdate::new(
                    "swarm_worker_closed",
                    format!("swarm-worker-closed-{id}"),
                    format!("Worker closed: {id}"),
                    Severity::Info,
                )
                .with_metadata(serde_json::json!({"worker_id": id, "role": role}).to_string()),
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
        self.ensure_subscription();
        let workers = match self.list_workers() {
            Some(w) => w,
            None => return Ok(Vec::new()),
        };

        if !self.initialized {
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
            self.events.lock().unwrap().clear();
            info!("swarm: initialized with {} worker(s)", workers.len());
            return Ok(Vec::new());
        }

        let mut signals = Vec::new();
        self.drain_events(&mut signals);
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
        let mut total = 0;
        // Reconcile each signal source
        for (source, prefix) in [
            ("swarm_worker_spawned", "swarm-spawned"),
            ("swarm_worker_waiting", "swarm-waiting"),
            ("swarm_pr_opened", "swarm-pr"),
            ("swarm_branch_ready", "swarm-branch-ready"),
        ] {
            let ids: Vec<String> = self
                .tracked
                .keys()
                .map(|id| format!("{prefix}-{id}"))
                .collect();
            total += store.resolve_missing_signals(source, &ids)?;
        }
        // Also reconcile old "swarm" source for backward compat
        let old_ids: Vec<String> = self
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
        total += store.resolve_missing_signals("swarm", &old_ids)?;
        if total > 0 {
            info!("swarm: reconciled {total} stale signal(s)");
        }
        Ok(total)
    }
}

fn truncate_prompt(prompt: &str) -> &str {
    let end = prompt
        .char_indices()
        .nth(120)
        .map_or(prompt.len(), |(i, _)| i);
    &prompt[..end]
}

/// Background subscription loop — reconnects with backoff.
async fn subscription_loop(work_dir: PathBuf, events: Arc<Mutex<Vec<(String, WorkerPhase)>>>) {
    let mut backoff_secs = 1u64;
    loop {
        match connect_and_subscribe(&work_dir, &events).await {
            Ok(()) => backoff_secs = 1,
            Err(e) => {
                tracing::debug!("swarm subscription error: {e}");
                backoff_secs = (backoff_secs * 2).min(60);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
    }
}

/// Connect to the daemon socket and subscribe to state change events.
async fn connect_and_subscribe(
    work_dir: &Path,
    events: &Mutex<Vec<(String, WorkerPhase)>>,
) -> Result<()> {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
    };

    let local = socket_path(work_dir);
    let global = global_socket_path();
    let stream = if local.exists() {
        UnixStream::connect(&local).await
    } else {
        UnixStream::connect(&global).await
    }
    .map_err(|e| color_eyre::eyre::eyre!("failed to connect to daemon: {e}"))?;

    let (reader, mut writer) = stream.into_split();
    let req = DaemonRequest::Subscribe {
        worktree_id: None,
        workspace: Some(work_dir.to_path_buf()),
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;

    let mut reader = BufReader::new(reader);
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf).await? == 0 {
            break;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(buf.trim())
            && val.get("kind").and_then(|k| k.as_str()) == Some("state_changed")
            && let (Some(wt_id), Some(phase)) = (
                val.get("worktree_id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                val.get("phase")
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
            )
        {
            events.lock().unwrap().push((wt_id, phase));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use apiari_swarm::core::state::SwarmState;

    use super::*;

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

    fn tracked(phase: WorkerPhase, has_pr: bool, role: Option<&str>) -> TrackedWorker {
        TrackedWorker {
            phase,
            has_pr,
            ready_branch: None,
            role: role.map(String::from),
            running_count: 1,
        }
    }

    #[test]
    fn test_diff_new_worker_emits_spawned() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        let signals = w.diff_workers(&[make_worker("w1", WorkerPhase::Running, None)]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, "swarm_worker_spawned");
    }

    #[test]
    fn test_diff_pr_opened() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        w.tracked
            .insert("w1".into(), tracked(WorkerPhase::Running, false, None));
        let signals = w.diff_workers(&[make_worker(
            "w1",
            WorkerPhase::Running,
            Some("https://github.com/org/repo/pull/1"),
        )]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, "swarm_pr_opened");
        assert_eq!(
            signals[0].url.as_deref(),
            Some("https://github.com/org/repo/pull/1")
        );
    }

    #[test]
    fn test_diff_running_transition() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        w.tracked
            .insert("w1".into(), tracked(WorkerPhase::Waiting, false, None));
        let signals = w.diff_workers(&[make_worker("w1", WorkerPhase::Running, None)]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, "swarm_worker_running");
    }

    #[test]
    fn test_diff_waiting_transition() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        w.tracked
            .insert("w1".into(), tracked(WorkerPhase::Running, false, None));
        let signals = w.diff_workers(&[make_worker("w1", WorkerPhase::Waiting, None)]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, "swarm_worker_waiting");
    }

    #[test]
    fn test_diff_closed_worker() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        w.tracked
            .insert("w1".into(), tracked(WorkerPhase::Running, false, None));
        let signals = w.diff_workers(&[]); // w1 is gone
        let closed = signals
            .iter()
            .find(|s| s.source == "swarm_worker_closed" && s.status == SignalStatus::Open);
        assert!(closed.is_some(), "should emit swarm_worker_closed");
        assert!(
            closed
                .unwrap()
                .metadata
                .as_deref()
                .unwrap_or("")
                .contains("worker_id")
        );
    }

    #[test]
    fn test_diff_no_change() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        w.tracked
            .insert("w1".into(), tracked(WorkerPhase::Running, false, None));
        let signals = w.diff_workers(&[make_worker("w1", WorkerPhase::Running, None)]);
        assert!(signals.is_empty());
    }

    #[test]
    fn test_reviewer_running_no_signal() {
        let mut w = SwarmWatcher::new(
            PathBuf::from("/tmp/test"),
            PathBuf::from("/tmp/test/.swarm/state.json"),
        );
        w.initialized = true;
        w.tracked.insert(
            "r1".into(),
            tracked(WorkerPhase::Waiting, false, Some("reviewer")),
        );
        let mut reviewer = make_worker("r1", WorkerPhase::Running, None);
        reviewer.role = Some("reviewer".to_string());
        let signals = w.diff_workers(&[reviewer]);
        assert!(
            signals.is_empty(),
            "reviewer running transition should not emit swarm_worker_running"
        );
    }

    #[test]
    fn test_parse_swarm_state() {
        let json = r#"{"session_name": "test", "worktrees": [{
            "id": "w1", "branch": "swarm/w1", "prompt": "test",
            "agent_kind": "claude", "repo_path": "/tmp/repo",
            "worktree_path": "/tmp/.swarm/wt/w1",
            "created_at": "2026-01-01T00:00:00-05:00",
            "phase": "running",
            "pr": {"number": 1, "title": "Fix", "state": "OPEN", "url": "https://github.com/org/repo/pull/1"}
        }]}"#;
        let state: SwarmState = serde_json::from_str(json).unwrap();
        assert_eq!(state.worktrees.len(), 1);
        assert!(state.worktrees[0].pr.is_some());
    }

    #[test]
    fn test_read_ready_branches_uses_explicit_state_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let state_dir = temp.path().join("custom-swarm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            state_dir.join("state.json"),
            r#"{
                "worktrees": [{
                    "id": "w1",
                    "ready_branch": "common/fix-sdk",
                    "repo_path": "/tmp/common"
                }]
            }"#,
        )
        .unwrap();

        let watcher = SwarmWatcher::new(root, state_dir.join("state.json"));
        let ready = watcher.read_ready_branches();

        assert_eq!(
            ready.get("w1"),
            Some(&(String::from("common/fix-sdk"), String::from("/tmp/common")))
        );
    }
}

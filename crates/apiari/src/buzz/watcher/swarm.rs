//! Swarm watcher — monitors worker state via daemon socket subscription.
//!
//! Uses `apiari_swarm::daemon` IPC to subscribe to real-time `StateChanged`
//! events from the swarm daemon. Falls back to `ListWorkers` for full state
//! sync (PR detection, new/closed workers, reconnection).
//!
//! Replaces the previous approach of polling `.swarm/state.json` on disk.

use apiari_swarm::core::ipc::{global_socket_path, socket_path};
use apiari_swarm::core::state::WorkerPhase;
use apiari_swarm::daemon::ipc_client::send_daemon_request;
use apiari_swarm::daemon::protocol::{DaemonRequest, DaemonResponse, WorkerInfo};
use async_trait::async_trait;
use color_eyre::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate};

/// Tracked state for a worktree between polls.
#[derive(Debug, Clone)]
struct TrackedWorker {
    phase: WorkerPhase,
    has_pr: bool,
}

/// Watches swarm daemon for worker state changes via socket subscription.
pub struct SwarmWatcher {
    work_dir: PathBuf,
    /// Previous state of each worktree.
    tracked: HashMap<String, TrackedWorker>,
    initialized: bool,
    /// Buffered StateChanged events from the subscription task.
    events: Arc<Mutex<Vec<(String, WorkerPhase)>>>,
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
            tokio::spawn(subscription_loop(work_dir, events));
        }
    }

    /// Drain buffered subscription events and apply phase transitions.
    fn drain_events(&mut self, signals: &mut Vec<SignalUpdate>) {
        let events: Vec<(String, WorkerPhase)> = {
            let mut buf = self.events.lock().unwrap();
            std::mem::take(&mut *buf)
        };

        for (id, phase) in events {
            let prev = self.tracked.get(&id);

            // Waiting transition
            if phase == WorkerPhase::Waiting
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

            // Update tracked phase (preserve has_pr since StateChanged doesn't carry it)
            if let Some(tracked) = self.tracked.get_mut(&id) {
                tracked.phase = phase;
            }
        }
    }

    /// Diff the full worker list against tracked state.
    fn diff_workers(&mut self, workers: &[WorkerInfo]) -> Vec<SignalUpdate> {
        let mut signals = Vec::new();

        for w in workers {
            let id = &w.id;
            let prev = self.tracked.get(id);
            let has_pr = w.pr_url.is_some();

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

            // Update tracked state
            self.tracked.insert(
                id.clone(),
                TrackedWorker {
                    phase: w.phase.clone(),
                    has_pr,
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
            for prefix in &["swarm-spawned", "swarm-waiting", "swarm-pr"] {
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
            for w in &workers {
                self.tracked.insert(
                    w.id.clone(),
                    TrackedWorker {
                        phase: w.phase.clone(),
                        has_pr: w.pr_url.is_some(),
                    },
                );
            }
            self.initialized = true;
            info!("swarm: initialized with {} worker(s)", workers.len());
            // Drain and discard any subscription events from before initialization
            self.events.lock().unwrap().clear();
            return Ok(Vec::new());
        }

        let mut signals = Vec::new();

        // Process subscription events first (real-time phase changes)
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
async fn subscription_loop(work_dir: PathBuf, events: Arc<Mutex<Vec<(String, WorkerPhase)>>>) {
    let mut backoff_secs = 1u64;

    loop {
        match connect_and_subscribe(&work_dir, &events).await {
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
async fn connect_and_subscribe(
    work_dir: &Path,
    events: &Mutex<Vec<(String, WorkerPhase)>>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

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

    // Read events
    let mut reader = BufReader::new(reader);
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            break; // EOF — daemon disconnected
        }
        if let Ok(DaemonResponse::StateChanged { worktree_id, phase }) =
            serde_json::from_str::<DaemonResponse>(buf.trim())
        {
            events.lock().unwrap().push((worktree_id, phase));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apiari_swarm::core::state::{SwarmState, WorkerPhase};

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
            },
        );

        let workers = vec![]; // w1 is gone
        let signals = watcher.diff_workers(&workers);
        // 3 resolved (spawned/waiting/pr) + 1 closed
        assert_eq!(signals.len(), 4);
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
            },
        );

        let workers = vec![make_worker("w1", WorkerPhase::Running, None)];
        let signals = watcher.diff_workers(&workers);
        assert!(signals.is_empty(), "no transition = no signals");
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
        }
    }
}

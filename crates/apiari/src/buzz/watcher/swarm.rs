//! Swarm watcher — subscribes to the swarm daemon socket for real-time
//! worker state changes.
//!
//! Replaces the previous `.swarm/state.json` file-polling approach with a
//! persistent `DaemonRequest::Subscribe` connection that receives
//! `StateChanged` events instantly.
//!
//! Emits SignalUpdates for worker lifecycle events. The coordinator decides
//! what to notify about.

use apiari_swarm::{DaemonResponse, WorkerPhase};
use async_trait::async_trait;
use color_eyre::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::coordinator::swarm_client::SwarmClient;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate};

/// Tracked state for a worker between events.
#[derive(Debug, Clone)]
struct TrackedWorker {
    phase: WorkerPhase,
    has_pr: bool,
}

/// Pending state change event received from the subscription.
#[derive(Debug, Clone)]
struct StateEvent {
    worktree_id: String,
    phase: WorkerPhase,
}

/// Watches swarm worker state via daemon socket subscription.
///
/// A background thread maintains a persistent subscription connection.
/// State changes are buffered and consumed during `poll()`.
pub struct SwarmWatcher {
    client: SwarmClient,
    /// Previous state of each worker.
    tracked: HashMap<String, TrackedWorker>,
    initialized: bool,
    /// Buffered events from the subscription thread.
    event_buffer: Arc<Mutex<Vec<StateEvent>>>,
    /// Handle to the subscription background thread.
    _sub_handle: Option<std::thread::JoinHandle<()>>,
    /// Whether the initial worker list has been loaded.
    initial_sync_done: bool,
}

impl SwarmWatcher {
    pub fn new(work_dir: std::path::PathBuf) -> Self {
        let client = SwarmClient::new(work_dir.clone());
        let event_buffer: Arc<Mutex<Vec<StateEvent>>> = Arc::new(Mutex::new(Vec::new()));

        // Spawn subscription thread
        let buffer = event_buffer.clone();
        let sub_client = SwarmClient::new(work_dir);
        let handle = std::thread::spawn(move || {
            subscription_loop(sub_client, buffer);
        });

        Self {
            client,
            tracked: HashMap::new(),
            initialized: false,
            event_buffer,
            _sub_handle: Some(handle),
            initial_sync_done: false,
        }
    }

    /// Sync current worker state from the daemon (used on startup and reconnect).
    async fn sync_workers(&mut self) -> Result<()> {
        let workers = self.client.list_workers().await?;
        for w in &workers {
            self.tracked.insert(
                w.id.clone(),
                TrackedWorker {
                    phase: w.phase.clone(),
                    has_pr: w.pr_url.is_some(),
                },
            );
        }
        self.initial_sync_done = true;
        info!("swarm: synced {} worker(s) from daemon", workers.len());
        Ok(())
    }

    /// Drain buffered events from the subscription thread.
    fn drain_events(&self) -> Vec<StateEvent> {
        let mut buf = self.event_buffer.lock().unwrap();
        std::mem::take(&mut *buf)
    }
}

/// Background subscription loop with reconnect and exponential backoff.
fn subscription_loop(client: SwarmClient, buffer: Arc<Mutex<Vec<StateEvent>>>) {
    let mut backoff_ms = 1000u64;
    let max_backoff_ms = 30_000u64;

    loop {
        match client.subscribe_blocking() {
            Ok(events) => {
                backoff_ms = 1000; // Reset on successful connect
                for event_result in events {
                    match event_result {
                        Ok(DaemonResponse::StateChanged { worktree_id, phase }) => {
                            let mut buf = buffer.lock().unwrap();
                            buf.push(StateEvent { worktree_id, phase });
                        }
                        Ok(_) => {} // Ignore other events (AgentEvent, etc.)
                        Err(e) => {
                            warn!("swarm subscription read error: {e}");
                            break; // Reconnect
                        }
                    }
                }
            }
            Err(e) => {
                warn!("swarm subscription connect failed: {e}");
            }
        }

        // Backoff before reconnecting
        std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
    }
}

#[async_trait]
impl Watcher for SwarmWatcher {
    fn name(&self) -> &str {
        "swarm"
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        // On first poll, sync the current state from the daemon
        if !self.initialized {
            if let Err(e) = self.sync_workers().await {
                // Daemon might not be running yet — skip silently
                tracing::debug!("swarm: initial sync failed (daemon may not be running): {e}");
                return Ok(Vec::new());
            }
            self.initialized = true;
            return Ok(Vec::new());
        }

        // If initial sync hasn't completed, try again
        if !self.initial_sync_done {
            if let Err(e) = self.sync_workers().await {
                tracing::debug!("swarm: sync retry failed: {e}");
            }
            return Ok(Vec::new());
        }

        // Also periodically re-sync via ListWorkers to catch any missed events
        // (e.g., new workers created, PRs opened, workers closed).
        // This acts as a lightweight reconciliation pass.
        let mut signals = Vec::new();

        // Process buffered subscription events
        let events = self.drain_events();
        for event in &events {
            let id = &event.worktree_id;
            let prev = self.tracked.get(id);

            if prev.is_none() {
                // New worker appeared via subscription
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-spawned-{id}"),
                        format!("Worker spawned: {id}"),
                        Severity::Info,
                    )
                    .with_body(format!("phase: {:?}", event.phase).to_lowercase()),
                );
            }

            // Waiting transition
            if event.phase == WorkerPhase::Waiting
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

            // Update tracked state (preserve has_pr from previous state)
            let has_pr = prev.map_or(false, |p| p.has_pr);
            self.tracked.insert(
                id.clone(),
                TrackedWorker {
                    phase: event.phase.clone(),
                    has_pr,
                },
            );
        }

        // Periodically re-sync full worker list to catch PR opens and worker closes.
        // This is cheap (single IPC call) and catches state the subscription doesn't carry
        // (e.g., PR info, which only comes from ListWorkers).
        if let Ok(workers) = self.client.list_workers().await {
            let current_ids: std::collections::HashSet<String> =
                workers.iter().map(|w| w.id.clone()).collect();

            for w in &workers {
                let id = &w.id;
                let has_pr = w.pr_url.is_some();
                let prev = self.tracked.get(id);

                // PR opened transition
                if has_pr && prev.is_some_and(|p| !p.has_pr) {
                    let mut signal = SignalUpdate::new(
                        "swarm",
                        format!("swarm-pr-{id}"),
                        format!("PR opened: {id}"),
                        Severity::Info,
                    );
                    if let Some(ref title) = w.pr_title {
                        signal = signal.with_body(format!(
                            "{}\n{}",
                            title,
                            w.pr_url.as_deref().unwrap_or("")
                        ));
                    }
                    if let Some(ref url) = w.pr_url {
                        signal = signal.with_url(url);
                    }
                    signals.push(signal);
                }

                // New worker not yet tracked (missed subscription event)
                if prev.is_none() {
                    signals.push(
                        SignalUpdate::new(
                            "swarm",
                            format!("swarm-spawned-{id}"),
                            format!("Worker spawned: {id}"),
                            Severity::Info,
                        )
                        .with_body(
                            format!("agent: {}, phase: {:?}", w.agent, w.phase).to_lowercase(),
                        ),
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

            // Detect closed workers
            let closed: Vec<String> = self
                .tracked
                .keys()
                .filter(|id| !current_ids.contains(*id))
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
        }

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

    /// Test that StateEvent tracking works correctly.
    #[test]
    fn test_tracked_worker_state() {
        let mut tracked = HashMap::new();
        tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
            },
        );

        // Verify transition detection
        let prev = tracked.get("w1").unwrap();
        assert_ne!(prev.phase, WorkerPhase::Waiting);
        assert!(!prev.has_pr);

        // Simulate phase change
        tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Waiting,
                has_pr: false,
            },
        );
        let updated = tracked.get("w1").unwrap();
        assert_eq!(updated.phase, WorkerPhase::Waiting);
    }
}

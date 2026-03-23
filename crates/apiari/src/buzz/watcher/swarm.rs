//! Swarm watcher — monitors `.swarm/state.json` for worker state changes.
//!
//! Uses the real types from `apiari-swarm` to deserialize state.
//! Emits SignalUpdates for worker lifecycle events. The coordinator decides
//! what to notify about.

use apiari_swarm::core::state::{SwarmState, WorkerPhase};
use async_trait::async_trait;
use color_eyre::Result;
use std::collections::HashMap;
use tracing::info;

use super::Watcher;
use crate::buzz::config::SwarmWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate};

/// Tracked state for a worktree between polls.
#[derive(Debug, Clone)]
struct TrackedWorker {
    phase: WorkerPhase,
    has_pr: bool,
}

/// Watches swarm state.json for worker changes.
pub struct SwarmWatcher {
    config: SwarmWatcherConfig,
    /// Previous state of each worktree.
    tracked: HashMap<String, TrackedWorker>,
    initialized: bool,
}

impl SwarmWatcher {
    pub fn new(config: SwarmWatcherConfig) -> Self {
        Self {
            config,
            tracked: HashMap::new(),
            initialized: false,
        }
    }

    fn read_state(&self) -> Option<SwarmState> {
        let contents = std::fs::read_to_string(&self.config.state_path).ok()?;
        serde_json::from_str(&contents).ok()
    }
}

#[async_trait]
impl Watcher for SwarmWatcher {
    fn name(&self) -> &str {
        "swarm"
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        let state = match self.read_state() {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };

        let mut signals = Vec::new();

        if !self.initialized {
            // First poll: just record current state, don't emit
            for wt in &state.worktrees {
                self.tracked.insert(
                    wt.id.clone(),
                    TrackedWorker {
                        phase: wt.phase.clone(),
                        has_pr: wt.pr.is_some(),
                    },
                );
            }
            self.initialized = true;
            info!(
                "swarm: initialized with {} worktree(s)",
                state.worktrees.len()
            );
            return Ok(Vec::new());
        }

        // Detect new worktrees
        for wt in &state.worktrees {
            let id = &wt.id;
            let prev = self.tracked.get(id);
            let has_pr = wt.pr.is_some();
            let repo = wt
                .repo_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            let summary = wt.summary.as_deref().unwrap_or("");

            if prev.is_none() {
                // New worktree spawned
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-spawned-{id}"),
                        format!("Worker spawned: {id}"),
                        Severity::Info,
                    )
                    .with_body(format!("repo: {repo}\n{summary}")),
                );
            }

            // PR opened transition
            if has_pr && prev.is_some_and(|p| !p.has_pr) {
                let pr = wt.pr.as_ref().unwrap();

                let mut signal = SignalUpdate::new(
                    "swarm",
                    format!("swarm-pr-{id}"),
                    format!("PR opened: {id}"),
                    Severity::Info,
                )
                .with_body(format!("{}\n{}", pr.title, pr.url));

                if !pr.url.is_empty() {
                    signal = signal.with_url(&pr.url);
                }

                signals.push(signal);
            }

            // Agent waiting transition (using phase instead of agent_session_status)
            if wt.phase == WorkerPhase::Waiting
                && prev.is_some_and(|p| p.phase != WorkerPhase::Waiting)
            {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-waiting-{id}"),
                        format!("Worker waiting: {id}"),
                        Severity::Warning,
                    )
                    .with_body(format!("Agent in {id} is waiting for input\nrepo: {repo}")),
                );
            }

            // Update tracked state
            self.tracked.insert(
                id.clone(),
                TrackedWorker {
                    phase: wt.phase.clone(),
                    has_pr,
                },
            );
        }

        // Detect closed worktrees — resolve all related signals
        let current_ids: std::collections::HashSet<&String> =
            state.worktrees.iter().map(|wt| &wt.id).collect();
        let closed: Vec<String> = self
            .tracked
            .keys()
            .filter(|id| !current_ids.contains(id))
            .cloned()
            .collect();

        for id in &closed {
            // Resolve the lifecycle signals for this worker
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

        if !signals.is_empty() {
            info!("swarm: {} signal(s)", signals.len());
        }

        Ok(signals)
    }

    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        if !self.initialized {
            return Ok(0);
        }
        // Build the set of signal IDs that should remain open:
        // for each currently-tracked worker, its spawned/waiting/pr signals.
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
        // This is a verbatim snapshot from a real `.swarm/state.json` produced by swarm.
        // If swarm's serialization format changes, this test should break first.
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

    /// End-to-end: write state files, poll the watcher, verify signals emitted.
    #[tokio::test]
    async fn test_poll_detects_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let db_path = dir.path().join("test.db");

        let store = SignalStore::open(&db_path, "test").unwrap();
        let config = SwarmWatcherConfig {
            enabled: true,
            state_path: state_path.clone(),
            interval_secs: 15,
        };
        let mut watcher = SwarmWatcher::new(config);

        // Phase 1: worker running — init poll, no signals
        let wt = wt_json("w1", r#""phase": "running""#);
        std::fs::write(&state_path, state_json(&[wt])).unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert!(signals.is_empty(), "init poll should emit nothing");

        // Phase 2: same state — no change, no signals
        let signals = watcher.poll(&store).await.unwrap();
        assert!(signals.is_empty(), "no transition = no signals");

        // Phase 3: worker transitions to waiting — should emit
        let wt = wt_json("w1", r#""phase": "waiting""#);
        std::fs::write(&state_path, state_json(&[wt])).unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("waiting"));

        // Phase 4: PR opens — should emit
        let wt = wt_json(
            "w1",
            r#""phase": "waiting",
                "pr": {"number": 1, "title": "My PR", "state": "OPEN", "url": "https://github.com/org/repo/pull/1"}"#,
        );
        std::fs::write(&state_path, state_json(&[wt])).unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("PR opened"));
        assert_eq!(
            signals[0].url.as_deref(),
            Some("https://github.com/org/repo/pull/1")
        );

        // Phase 5: new worker spawns — should emit
        let wt1 = wt_json(
            "w1",
            r#""phase": "waiting",
                "pr": {"number": 1, "title": "My PR", "state": "OPEN", "url": "https://github.com/org/repo/pull/1"}"#,
        );
        let wt2 = wt_json("w2", r#""phase": "running""#);
        std::fs::write(&state_path, state_json(&[wt1, wt2])).unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("spawned"));

        // Phase 6: worker closed — should resolve spawned/waiting/pr + emit closed
        let wt1 = wt_json(
            "w1",
            r#""phase": "waiting",
                "pr": {"number": 1, "title": "My PR", "state": "OPEN", "url": "https://github.com/org/repo/pull/1"}"#,
        );
        std::fs::write(&state_path, state_json(&[wt1])).unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 4); // 3 resolved (spawned/waiting/pr) + 1 closed
        assert!(signals.iter().all(|s| s.title.contains("closed")));
    }
}

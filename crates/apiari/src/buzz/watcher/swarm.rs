//! Swarm watcher — monitors `.swarm/state.json` for worker state changes.
//!
//! Simplified from hive's SwarmWatcher: emits SignalUpdates for worker
//! lifecycle events. The coordinator decides what to notify about.

use async_trait::async_trait;
use color_eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::Watcher;
use crate::buzz::config::SwarmWatcherConfig;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate};

/// Minimal swarm state deserialization.
#[derive(Debug, Clone, Deserialize)]
struct SwarmState {
    #[serde(default)]
    worktrees: Vec<WorktreeEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct WorktreeEntry {
    id: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    pr: Option<PrInfo>,
    #[serde(default)]
    agent_kind: Option<String>,
    #[serde(default)]
    agent_session_status: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PrInfo {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Tracked state for a worktree between polls.
#[derive(Debug, Clone)]
struct TrackedWorker {
    status: Option<String>,
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
                        status: wt.agent_session_status.clone(),
                        has_pr: wt.pr.as_ref().and_then(|p| p.url.as_ref()).is_some(),
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
            let current_status = wt.agent_session_status.as_deref();
            let has_pr = wt.pr.as_ref().and_then(|p| p.url.as_ref()).is_some();
            let repo = wt.repo.as_deref().unwrap_or("unknown");
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
                let pr_url = wt.pr.as_ref().and_then(|p| p.url.as_deref()).unwrap_or("");
                let pr_title = wt
                    .pr
                    .as_ref()
                    .and_then(|p| p.title.as_deref())
                    .unwrap_or("PR opened");

                let mut signal = SignalUpdate::new(
                    "swarm",
                    format!("swarm-pr-{id}"),
                    format!("PR opened: {id}"),
                    Severity::Info,
                )
                .with_body(format!("{pr_title}\n{pr_url}"));

                if !pr_url.is_empty() {
                    signal = signal.with_url(pr_url);
                }

                signals.push(signal);
            }

            // Agent waiting transition
            if current_status == Some("waiting")
                && prev.is_some_and(|p| p.status.as_deref() != Some("waiting"))
            {
                let is_tui = wt.agent_kind.as_deref() == Some("claude-tui");
                if !is_tui {
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
            }

            // Update tracked state
            self.tracked.insert(
                id.clone(),
                TrackedWorker {
                    status: wt.agent_session_status.clone(),
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

    fn reconcile(&self, store: &SignalStore) -> Result<usize> {
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

    #[test]
    fn test_parse_swarm_state() {
        let json = r#"{
            "worktrees": [
                {
                    "id": "hive-1",
                    "repo": "hive",
                    "branch": "swarm/fix-bug",
                    "summary": "Fix a bug",
                    "pr": {"url": "https://github.com/org/repo/pull/1", "title": "Fix bug"},
                    "agent_kind": "claude",
                    "agent_session_status": "running"
                }
            ]
        }"#;
        let state: SwarmState = serde_json::from_str(json).unwrap();
        assert_eq!(state.worktrees.len(), 1);
        let wt = &state.worktrees[0];
        assert_eq!(wt.id, "hive-1");
        assert_eq!(wt.repo.as_deref(), Some("hive"));
        assert!(wt.pr.as_ref().unwrap().url.is_some());
    }

    #[test]
    fn test_parse_empty_state() {
        let json = r#"{"worktrees": []}"#;
        let state: SwarmState = serde_json::from_str(json).unwrap();
        assert!(state.worktrees.is_empty());
    }

    #[test]
    fn test_parse_state_missing_optional_fields() {
        let json = r#"{
            "worktrees": [
                {
                    "id": "wt-1",
                    "repo": "test"
                }
            ]
        }"#;
        let state: SwarmState = serde_json::from_str(json).unwrap();
        let wt = &state.worktrees[0];
        assert!(wt.pr.is_none());
        assert!(wt.agent_kind.is_none());
        assert!(wt.agent_session_status.is_none());
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
        assert_eq!(wt.repo.as_deref(), None); // swarm uses repo_path, not repo
        assert_eq!(wt.agent_kind.as_deref(), Some("claude"));
        assert_eq!(wt.agent_session_status.as_deref(), Some("waiting"));

        let pr = wt.pr.as_ref().unwrap();
        assert_eq!(
            pr.url.as_deref(),
            Some("https://github.com/ApiariTools/swarm/pull/64")
        );
        assert_eq!(
            pr.title.as_deref(),
            Some("fix(ci): add apiari-tui to workspace")
        );
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
        std::fs::write(
            &state_path,
            r#"{"worktrees": [{"id": "w1", "repo": "myrepo", "agent_session_status": "running"}]}"#,
        )
        .unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert!(signals.is_empty(), "init poll should emit nothing");

        // Phase 2: same state — no change, no signals
        let signals = watcher.poll(&store).await.unwrap();
        assert!(signals.is_empty(), "no transition = no signals");

        // Phase 3: worker transitions to waiting — should emit
        std::fs::write(
            &state_path,
            r#"{"worktrees": [{"id": "w1", "repo": "myrepo", "agent_session_status": "waiting"}]}"#,
        )
        .unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("waiting"));

        // Phase 4: PR opens — should emit
        std::fs::write(
            &state_path,
            r#"{"worktrees": [{"id": "w1", "repo": "myrepo", "agent_session_status": "waiting", "pr": {"url": "https://github.com/org/repo/pull/1", "title": "My PR"}}]}"#,
        )
        .unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("PR opened"));
        assert_eq!(
            signals[0].url.as_deref(),
            Some("https://github.com/org/repo/pull/1")
        );

        // Phase 5: new worker spawns — should emit
        std::fs::write(
            &state_path,
            r#"{"worktrees": [
                {"id": "w1", "repo": "myrepo", "agent_session_status": "waiting", "pr": {"url": "https://github.com/org/repo/pull/1", "title": "My PR"}},
                {"id": "w2", "repo": "other", "agent_session_status": "running"}
            ]}"#,
        )
        .unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].title.contains("spawned"));

        // Phase 6: worker closed — should resolve spawned/waiting/pr + emit closed
        std::fs::write(
            &state_path,
            r#"{"worktrees": [{"id": "w1", "repo": "myrepo", "agent_session_status": "waiting", "pr": {"url": "https://github.com/org/repo/pull/1", "title": "My PR"}}]}"#,
        )
        .unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert_eq!(signals.len(), 4); // 3 resolved (spawned/waiting/pr) + 1 closed
        assert!(signals.iter().all(|s| s.title.contains("closed")));
    }

    /// claude-tui workers should NOT emit waiting signals.
    #[tokio::test]
    async fn test_poll_skips_tui_waiting() {
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

        // Init: running
        std::fs::write(
            &state_path,
            r#"{"worktrees": [{"id": "w1", "agent_kind": "claude-tui", "agent_session_status": "running"}]}"#,
        )
        .unwrap();
        watcher.poll(&store).await.unwrap();

        // Transition to waiting — should NOT emit because claude-tui
        std::fs::write(
            &state_path,
            r#"{"worktrees": [{"id": "w1", "agent_kind": "claude-tui", "agent_session_status": "waiting"}]}"#,
        )
        .unwrap();
        let signals = watcher.poll(&store).await.unwrap();
        assert!(
            signals.is_empty(),
            "claude-tui waiting should be suppressed"
        );
    }
}

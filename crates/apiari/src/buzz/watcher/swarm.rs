use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use apiari_swarm::WorkerPhase;
use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::Deserialize;
use tracing::info;

use super::Watcher;
use crate::buzz::signal::{Severity, SignalStatus, SignalUpdate, store::SignalStore};

#[derive(Debug, Clone)]
struct TrackedWorker {
    phase: WorkerPhase,
    has_pr: bool,
    ready_branch: Option<String>,
    role: Option<String>,
    running_count: u32,
}

#[derive(Debug, Deserialize)]
struct SwarmStateFile {
    #[serde(default)]
    worktrees: Vec<WorktreeStateFile>,
}

#[derive(Debug, Deserialize)]
struct WorktreeStateFile {
    id: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_kind: Option<String>,
    #[serde(default)]
    phase: Option<WorkerPhase>,
    #[serde(default)]
    ready_branch: Option<String>,
    #[serde(default)]
    repo_path: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    pr: Option<PullRequestStateFile>,
}

#[derive(Debug, Deserialize)]
struct PullRequestStateFile {
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

pub struct SwarmWatcher {
    state_path: PathBuf,
    tracked: HashMap<String, TrackedWorker>,
    initialized: bool,
}

impl SwarmWatcher {
    pub fn new(state_path: PathBuf) -> Self {
        Self {
            state_path,
            tracked: HashMap::new(),
            initialized: false,
        }
    }

    fn load_state(&self) -> Result<SwarmStateFile> {
        let raw = std::fs::read_to_string(&self.state_path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    fn seed_tracked(&mut self, workers: &[WorktreeStateFile]) {
        self.tracked = workers
            .iter()
            .map(|worker| {
                (
                    worker.id.clone(),
                    TrackedWorker {
                        phase: worker.phase.clone().unwrap_or(WorkerPhase::Running),
                        has_pr: worker.pr.as_ref().and_then(|pr| pr.url.as_ref()).is_some(),
                        ready_branch: worker.ready_branch.clone(),
                        role: worker.role.clone(),
                        running_count: 0,
                    },
                )
            })
            .collect();
    }

    fn diff_workers(&mut self, workers: &[WorktreeStateFile]) -> Vec<SignalUpdate> {
        let mut signals = Vec::new();

        for worker in workers {
            let phase = worker.phase.clone().unwrap_or(WorkerPhase::Running);
            let has_pr = worker.pr.as_ref().and_then(|pr| pr.url.as_ref()).is_some();
            let prev = self.tracked.get(&worker.id).cloned();

            if prev.is_none() {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-spawned-{}", worker.id),
                        format!("Worker spawned: {}", worker.id),
                        Severity::Info,
                    )
                    .with_body(format!(
                        "agent: {}\n{}",
                        worker
                            .agent
                            .as_deref()
                            .or(worker.agent_kind.as_deref())
                            .unwrap_or("unknown"),
                        truncate_prompt(&worker.prompt)
                    )),
                );
            }

            if phase == WorkerPhase::Running
                && prev
                    .as_ref()
                    .is_some_and(|p| p.phase != WorkerPhase::Running)
                && worker.role.as_deref() != Some("reviewer")
            {
                let running_count = prev.as_ref().map_or(1, |p| p.running_count + 1);
                signals.push(
                    SignalUpdate::new(
                        "swarm_worker_running",
                        format!("swarm-worker-running-{}-{running_count}", worker.id),
                        format!("Worker running: {}", worker.id),
                        Severity::Info,
                    )
                    .with_metadata(
                        serde_json::json!({
                            "worker_id": worker.id,
                            "role": worker.role.as_deref().unwrap_or("worker"),
                        })
                        .to_string(),
                    ),
                );
            }

            if phase == WorkerPhase::Waiting
                && prev
                    .as_ref()
                    .is_some_and(|p| p.phase != WorkerPhase::Waiting)
            {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-waiting-{}", worker.id),
                        format!("Worker waiting: {}", worker.id),
                        Severity::Warning,
                    )
                    .with_body(format!("Agent in {} is waiting for input", worker.id)),
                );
            }

            if phase.is_terminal() && prev.as_ref().is_some_and(|p| !p.phase.is_terminal()) {
                signals.push(
                    SignalUpdate::new(
                        "swarm",
                        format!("swarm-completed-{}", worker.id),
                        format!("Worker completed: {}", worker.id),
                        Severity::Info,
                    )
                    .with_body(format!("Worker {} has completed", worker.id)),
                );
            }

            if worker.ready_branch.is_some()
                && !has_pr
                && prev
                    .as_ref()
                    .and_then(|p| p.ready_branch.as_deref())
                    .is_none()
            {
                let branch_name = worker.ready_branch.as_deref().unwrap_or_default();
                signals.push(
                    SignalUpdate::new(
                        "swarm_branch_ready",
                        format!("swarm-branch-ready-{}", worker.id),
                        format!("Branch ready for review: {branch_name}"),
                        Severity::Info,
                    )
                    .with_metadata(
                        serde_json::json!({
                            "worker_id": worker.id,
                            "branch_name": branch_name,
                            "repo": worker.repo_path.as_deref().unwrap_or_default(),
                        })
                        .to_string(),
                    ),
                );
            }

            if has_pr && prev.as_ref().is_some_and(|p| !p.has_pr) {
                let pr = worker.pr.as_ref().expect("checked has_pr");
                let url = pr.url.as_deref().unwrap_or_default();
                let title = pr.title.as_deref().unwrap_or_default();
                let mut signal = SignalUpdate::new(
                    "swarm",
                    format!("swarm-pr-{}", worker.id),
                    format!("PR opened: {}", worker.id),
                    Severity::Info,
                )
                .with_body(format!("{title}\n{url}"))
                .with_metadata(
                    serde_json::json!({
                        "worker_id": worker.id,
                        "pr_url": url,
                        "pr_number": pr.number,
                    })
                    .to_string(),
                );
                if !url.is_empty() {
                    signal = signal.with_url(url);
                }
                signals.push(signal);
            }

            self.tracked.insert(
                worker.id.clone(),
                TrackedWorker {
                    phase: phase.clone(),
                    has_pr,
                    ready_branch: worker.ready_branch.clone(),
                    role: worker.role.clone(),
                    running_count: if phase == WorkerPhase::Running
                        && prev
                            .as_ref()
                            .is_some_and(|p| p.phase != WorkerPhase::Running)
                        && worker.role.as_deref() != Some("reviewer")
                    {
                        prev.as_ref().map_or(1, |p| p.running_count + 1)
                    } else {
                        prev.as_ref().map_or(0, |p| p.running_count)
                    },
                },
            );
        }

        let current_ids: HashSet<&str> = workers.iter().map(|worker| worker.id.as_str()).collect();
        let closed_ids: Vec<String> = self
            .tracked
            .keys()
            .filter(|id| !current_ids.contains(id.as_str()))
            .cloned()
            .collect();

        for id in &closed_ids {
            for (source, external_id) in [
                ("swarm", format!("swarm-spawned-{id}")),
                ("swarm", format!("swarm-waiting-{id}")),
                ("swarm", format!("swarm-pr-{id}")),
                ("swarm", format!("swarm-completed-{id}")),
                ("swarm_branch_ready", format!("swarm-branch-ready-{id}")),
                (
                    "swarm_worker_running",
                    format!("swarm-worker-running-{id}-1"),
                ),
            ] {
                signals.push(
                    SignalUpdate::new(
                        source,
                        external_id,
                        format!("Worker closed: {id}"),
                        Severity::Info,
                    )
                    .with_status(SignalStatus::Resolved),
                );
            }

            let role = self
                .tracked
                .get(id)
                .and_then(|worker| worker.role.as_deref())
                .unwrap_or("worker");
            signals.push(
                SignalUpdate::new(
                    "swarm_worker_closed",
                    format!("swarm-worker-closed-{id}"),
                    format!("Worker closed: {id}"),
                    Severity::Info,
                )
                .with_metadata(
                    serde_json::json!({
                        "worker_id": id,
                        "role": role,
                    })
                    .to_string(),
                ),
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
        let state = match self.load_state() {
            Ok(state) => state,
            Err(_) => return Ok(Vec::new()),
        };

        if !self.initialized {
            self.seed_tracked(&state.worktrees);
            self.initialized = true;
            info!(
                "swarm: initialized with {} worker(s)",
                state.worktrees.len()
            );
            return Ok(Vec::new());
        }

        let signals = self.diff_workers(&state.worktrees);
        if !signals.is_empty() {
            info!("swarm: {} signal(s)", signals.len());
        }
        Ok(signals)
    }

    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        if !self.initialized {
            return Ok(0);
        }

        let mut resolved = 0;
        let running_ids: Vec<String> = self
            .tracked
            .iter()
            .filter(|(_, worker)| {
                worker.phase == WorkerPhase::Running && worker.role.as_deref() != Some("reviewer")
            })
            .map(|(id, worker)| {
                format!(
                    "swarm-worker-running-{}-{}",
                    id,
                    worker.running_count.max(1)
                )
            })
            .collect();
        let swarm_ids: Vec<String> = self
            .tracked
            .keys()
            .flat_map(|id| {
                [
                    format!("swarm-spawned-{id}"),
                    format!("swarm-waiting-{id}"),
                    format!("swarm-pr-{id}"),
                    format!("swarm-completed-{id}"),
                ]
            })
            .collect();
        let branch_ids: Vec<String> = self
            .tracked
            .iter()
            .filter(|(_, worker)| worker.ready_branch.is_some() && !worker.has_pr)
            .map(|(id, _)| format!("swarm-branch-ready-{id}"))
            .collect();
        let closed_ids: Vec<String> = self
            .tracked
            .keys()
            .map(|id| format!("swarm-worker-closed-{id}"))
            .collect();

        resolved += store.resolve_missing_signals("swarm", &swarm_ids)?;
        resolved += store.resolve_missing_signals("swarm_branch_ready", &branch_ids)?;
        resolved += store.resolve_missing_signals("swarm_worker_running", &running_ids)?;
        resolved += store.resolve_missing_signals("swarm_worker_closed", &closed_ids)?;
        Ok(resolved)
    }
}

fn truncate_prompt(prompt: &str) -> &str {
    let end = prompt
        .char_indices()
        .nth(120)
        .map_or(prompt.len(), |(index, _)| index);
    &prompt[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(
        id: &str,
        phase: &str,
        ready_branch: Option<&str>,
        pr_url: Option<&str>,
    ) -> WorktreeStateFile {
        WorktreeStateFile {
            id: id.to_string(),
            prompt: "test prompt".to_string(),
            agent: Some("claude".to_string()),
            agent_kind: None,
            phase: Some(match phase {
                "waiting" => WorkerPhase::Waiting,
                "completed" => WorkerPhase::Completed,
                _ => WorkerPhase::Running,
            }),
            ready_branch: ready_branch.map(str::to_string),
            repo_path: Some("/tmp/repo".to_string()),
            role: None,
            pr: pr_url.map(|url| PullRequestStateFile {
                number: Some(1),
                title: Some("PR".to_string()),
                url: Some(url.to_string()),
            }),
        }
    }

    #[test]
    fn emits_spawned_for_new_worker() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/state.json"));
        watcher.initialized = true;

        let signals = watcher.diff_workers(&[worker("w1", "running", None, None)]);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].external_id, "swarm-spawned-w1");
    }

    #[test]
    fn emits_pr_opened_transition() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/state.json"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: None,
                running_count: 0,
            },
        );

        let signals = watcher.diff_workers(&[worker(
            "w1",
            "running",
            None,
            Some("https://github.com/org/repo/pull/1"),
        )]);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].external_id, "swarm-pr-w1");
    }

    #[test]
    fn emits_closed_signal_when_worker_disappears() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/tmp/state.json"));
        watcher.initialized = true;
        watcher.tracked.insert(
            "w1".to_string(),
            TrackedWorker {
                phase: WorkerPhase::Running,
                has_pr: false,
                ready_branch: None,
                role: Some("worker".to_string()),
                running_count: 1,
            },
        );

        let signals = watcher.diff_workers(&[]);

        assert!(
            signals
                .iter()
                .any(|signal| signal.source == "swarm_worker_closed")
        );
    }
}

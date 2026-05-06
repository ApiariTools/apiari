//! In-process worker lifecycle manager.
//!
//! Creates git worktrees, spawns agents, and routes messages — all directly,
//! with no separate daemon process or IPC socket. One process total.

use apiari_swarm::core::agent::AgentKind;
use apiari_swarm::daemon::event_logger::{AgentEvent, EventLogger};
use apiari_swarm::daemon::managed_agent::{SpawnOptions, spawn_managed_agent};
use apiari_swarm::daemon::protocol::{AgentEventWire, TaskDirPayload};
use chrono::Utc;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

// ── Live worker handle ─────────────────────────────────────────────────

struct LiveWorker {
    msg_tx: mpsc::Sender<String>,
    repo_path: PathBuf,
    worktree_path: PathBuf,
    branch: String,
    work_dir: PathBuf,
}

// ── WorkerManager ──────────────────────────────────────────────────────

/// Manages all in-flight workers: git worktrees + agent processes.
pub struct WorkerManager {
    live: Arc<Mutex<HashMap<String, LiveWorker>>>,
}

impl WorkerManager {
    pub fn new() -> Self {
        Self {
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a git worktree and spawn an agent. Returns the worker ID.
    pub async fn create_worker(
        &self,
        work_dir: &Path,
        repo: &str,
        prompt: &str,
        agent: &str,
    ) -> Result<String> {
        self.create_worker_with_task_dir(work_dir, repo, prompt, agent, None)
            .await
    }

    /// Create a git worktree, optionally seed .task/ artifacts, and spawn an agent.
    /// Returns the worker ID.
    pub async fn create_worker_with_task_dir(
        &self,
        work_dir: &Path,
        repo: &str,
        prompt: &str,
        agent: &str,
        task_dir: Option<TaskDirPayload>,
    ) -> Result<String> {
        use apiari_swarm::core::git;

        let work_dir = work_dir.to_path_buf();
        let repo = repo.to_string();
        let prompt = prompt.to_string();
        let agent_str = agent.to_string();

        // Keep copies for use after the blocking closure consumes the originals.
        let work_dir_copy = work_dir.clone();
        let prompt_copy = prompt.clone();

        // All blocking git/fs work on a blocking thread.
        let (worker_id, branch, repo_path, worktree_path, effective_prompt) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let repo_path = resolve_repo(&work_dir, &repo)?;

                git::pull_main(&repo_path);

                let short_id = &uuid::Uuid::new_v4().to_string()[..4];
                let name = git::repo_name(&repo_path);
                let worker_id = format!("{}-{}", name, short_id);
                let branch = git::generate_branch_name(&prompt, short_id);
                let worktree_path = work_dir.join(".swarm").join("wt").join(&worker_id);

                git::create_worktree(&repo_path, &branch, &worktree_path, Some("origin/main"))?;
                git::symlink_worktree_files(&repo_path, &worktree_path);

                let cmds = git::read_worktree_setup_commands(&repo_path);
                if !cmds.is_empty() {
                    git::run_worktree_setup_commands(&worktree_path, &cmds)?;
                }

                // Seed .task/ artifacts if provided.
                if let Some(ref payload) = task_dir {
                    let task_path = worktree_path.join(".task");
                    let _ = std::fs::create_dir_all(&task_path);
                    if let Some(ref c) = payload.task_md {
                        let _ = std::fs::write(task_path.join("TASK.md"), c);
                    }
                    if let Some(ref c) = payload.context_md {
                        let _ = std::fs::write(task_path.join("CONTEXT.md"), c);
                    }
                    if let Some(ref c) = payload.plan_md {
                        let _ = std::fs::write(task_path.join("PLAN.md"), c);
                    }
                    if let Some(ref c) = payload.shaping_md {
                        let _ = std::fs::write(task_path.join("SHAPING.md"), c);
                    }
                }

                // Build effective prompt from workspace profile.
                let profile = apiari_swarm::core::profile::load_profile(&work_dir, "default");
                let effective_prompt =
                    apiari_swarm::core::profile::build_effective_prompt(&profile, &prompt);

                Ok((
                    worker_id,
                    branch,
                    repo_path,
                    worktree_path,
                    effective_prompt,
                ))
            })
            .await??;

        let kind = agent_str.parse::<AgentKind>().unwrap_or(AgentKind::Codex);

        // Write initial state.json so existing tooling / reconciler can see it.
        upsert_state_entry(
            &work_dir_copy,
            &worker_id,
            &branch,
            &prompt_copy,
            &kind,
            &repo_path,
            &worktree_path,
        );

        // Spawn agent + supervisor task.
        let agent_dir = work_dir_copy.join(".swarm").join("agents").join(&worker_id);
        std::fs::create_dir_all(&agent_dir)?;

        let mut agent = spawn_managed_agent(SpawnOptions {
            kind,
            prompt: effective_prompt,
            working_dir: worktree_path.clone(),
            dangerously_skip_permissions: true,
            resume_session_id: None,
            max_turns: None,
        })
        .await?;

        let (msg_tx, mut msg_rx) = mpsc::channel::<String>(8);

        let live = Arc::clone(&self.live);
        let wid = worker_id.clone();
        let wdir = work_dir_copy.clone();
        let wt_path = worktree_path.clone();
        let rpath = repo_path.clone();

        tokio::spawn(async move {
            let events_path = wdir
                .join(".swarm")
                .join("agents")
                .join(&wid)
                .join("events.jsonl");
            let mut logger = EventLogger::new(events_path);
            let _ = logger.log(&AgentEvent::Start {
                timestamp: Utc::now(),
                prompt: prompt_copy.clone(),
                model: None,
            });

            loop {
                if agent.accepts_input() {
                    // Agent is waiting for a follow-up message.
                    update_state_phase(&wdir, &wid, "waiting");
                    if let Some(msg) = msg_rx.recv().await {
                        let _ = logger.log(&AgentEvent::UserMessage {
                            timestamp: Utc::now(),
                            text: msg.clone(),
                        });
                        update_state_phase(&wdir, &wid, "running");
                        if let Err(e) = agent.send_message(&msg).await {
                            let _ = logger.log(&AgentEvent::Error {
                                timestamp: Utc::now(),
                                message: e.to_string(),
                            });
                            update_state_phase(&wdir, &wid, "failed");
                            break;
                        }
                    } else {
                        // Sender dropped — manager is shutting down.
                        break;
                    }
                } else {
                    // Drain the next agent event.
                    match agent.next_event().await {
                        Ok(Some(ev)) => log_event(&mut logger, &ev),
                        Ok(None) | Err(_) => {
                            update_state_phase(&wdir, &wid, "failed");
                            break;
                        }
                    }
                }
            }

            // Pull main and clean up worktree on agent exit.
            let _ = tokio::task::spawn_blocking(move || {
                apiari_swarm::core::git::pull_main(&rpath);
                let _ = apiari_swarm::core::git::remove_worktree(&rpath, &wt_path);
            })
            .await;

            live.lock().await.remove(&wid);
        });

        self.live.lock().await.insert(
            worker_id.clone(),
            LiveWorker {
                msg_tx,
                repo_path,
                worktree_path,
                branch,
                work_dir: work_dir_copy,
            },
        );

        Ok(worker_id)
    }

    /// Close a worker: disconnect the agent and remove its git worktree.
    pub async fn close_worker(&self, worker_id: &str) -> Result<()> {
        let mut live = self.live.lock().await;
        let worker = live
            .remove(worker_id)
            .ok_or_else(|| eyre!("worker {worker_id} not found or not running"))?;

        drop(worker.msg_tx); // signal the waiting loop to exit

        let repo_path = worker.repo_path;
        let worktree_path = worker.worktree_path;
        let branch = worker.branch;
        let work_dir = worker.work_dir;
        let wt_id = worker_id.to_string();

        tokio::task::spawn_blocking(move || {
            let _ = apiari_swarm::core::git::remove_worktree(&repo_path, &worktree_path);
            let _ = apiari_swarm::core::git::delete_branch(&repo_path, &branch);
            update_state_phase(&work_dir, &wt_id, "failed");
        });

        Ok(())
    }

    /// Send a message to a running worker.
    pub async fn send_message(&self, worker_id: &str, message: &str) -> Result<()> {
        let live = self.live.lock().await;
        let worker = live
            .get(worker_id)
            .ok_or_else(|| eyre!("worker {worker_id} not found or not running"))?;
        worker
            .msg_tx
            .send(message.to_string())
            .await
            .map_err(|_| eyre!("worker {worker_id} channel closed"))?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn is_live(&self, worker_id: &str) -> bool {
        self.live
            .try_lock()
            .map_or(false, |m| m.contains_key(worker_id))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn resolve_repo(work_dir: &Path, repo: &str) -> Result<PathBuf> {
    // Check configured repos in workspace config.
    if let Ok(configs) = apiari_swarm::core::git::detect_repos(work_dir) {
        if let Some(r) = configs
            .iter()
            .find(|r| apiari_swarm::core::git::repo_name(r) == repo || r.ends_with(repo))
        {
            return Ok(r.clone());
        }
    }
    // Fallback: treat as relative path from work_dir.
    let path = work_dir.join(repo);
    if path.exists() {
        return Ok(path);
    }
    Err(eyre!("repo '{repo}' not found in workspace"))
}

fn log_event(logger: &mut EventLogger, ev: &AgentEventWire) {
    let now = Utc::now();
    let event = match ev {
        AgentEventWire::TextDelta { text } => AgentEvent::AssistantText {
            timestamp: now,
            text: text.clone(),
        },
        AgentEventWire::ToolUse { tool, input } => AgentEvent::ToolUse {
            timestamp: now,
            tool: tool.clone(),
            input: input.clone(),
        },
        AgentEventWire::ToolResult { output, is_error } => AgentEvent::ToolResult {
            timestamp: now,
            tool: String::new(),
            output: output.clone(),
            is_error: *is_error,
        },
        AgentEventWire::SessionResult {
            turns,
            cost_usd,
            session_id,
        } => AgentEvent::SessionResult {
            timestamp: now,
            turns: *turns,
            cost_usd: *cost_usd,
            session_id: session_id.clone(),
        },
        AgentEventWire::Error { message } => AgentEvent::Error {
            timestamp: now,
            message: message.clone(),
        },
        _ => return,
    };
    let _ = logger.log(&event);
}

fn update_state_phase(work_dir: &Path, worker_id: &str, phase: &str) {
    let path = work_dir.join(".swarm").join("state.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut state) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    if let Some(worktrees) = state["worktrees"].as_array_mut() {
        for wt in worktrees.iter_mut() {
            if wt["id"].as_str() == Some(worker_id) {
                wt["phase"] = serde_json::Value::String(phase.to_string());
                wt["status"] =
                    serde_json::Value::String(if phase == "waiting" || phase == "running" {
                        "running".to_string()
                    } else {
                        "done".to_string()
                    });
                break;
            }
        }
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string(&state).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn upsert_state_entry(
    work_dir: &Path,
    worker_id: &str,
    branch: &str,
    prompt: &str,
    kind: &AgentKind,
    repo_path: &Path,
    worktree_path: &Path,
) {
    let path = work_dir.join(".swarm").join("state.json");
    let mut state: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "session_name": "apiari",
                "worktrees": [],
                "last_inbox_pos": 0
            })
        });

    let entry = serde_json::json!({
        "id": worker_id,
        "branch": branch,
        "prompt": prompt,
        "agent_kind": kind.label(),
        "repo_path": repo_path,
        "worktree_path": worktree_path,
        "phase": "starting",
        "status": "running",
        "created_at": Utc::now().to_rfc3339(),
    });

    let worktrees = state["worktrees"].as_array_mut().unwrap();
    if let Some(existing) = worktrees
        .iter_mut()
        .find(|w| w["id"].as_str() == Some(worker_id))
    {
        *existing = entry;
    } else {
        worktrees.push(entry);
    }

    let tmp = path.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_string(&state) {
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

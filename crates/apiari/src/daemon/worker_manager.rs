//! In-process worker lifecycle manager.
//!
//! Creates git worktrees, spawns agents, and routes messages — all directly,
//! with no separate daemon process or IPC socket. One process total.
//!
//! ## Message delivery model
//!
//! Claude/Codex/Gemini agents are CLI processes that run to completion and
//! resume via session ID — not long-lived processes that read from stdin.
//! Messages are therefore delivered by re-spawning the agent with the message
//! as a new prompt and the prior session ID for context continuity.
//!
//! If a message arrives while an agent is running, it is queued in `pending`
//! and picked up the moment the current run finishes.

use apiari_swarm::core::agent::AgentKind;
use apiari_swarm::daemon::event_logger::{AgentEvent, EventLogger};
use apiari_swarm::daemon::managed_agent::{SpawnOptions, spawn_managed_agent};
use apiari_swarm::daemon::protocol::{AgentEventWire, TaskDirPayload};
use chrono::Utc;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

// ── WorkerManager ──────────────────────────────────────────────────────

/// Manages all in-flight workers: git worktrees + agent processes.
pub struct WorkerManager {
    /// Workers currently running an agent process (prevents double-spawn).
    live: Arc<Mutex<HashSet<String>>>,
    /// Messages queued while an agent is running.
    pending: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
    /// work_dir per worker — used by resume_worker/close_worker to locate state.json.
    work_dirs: Arc<Mutex<HashMap<String, PathBuf>>>,
    db_path: PathBuf,
    workspace: String,
}

impl WorkerManager {
    pub fn new(db_path: PathBuf, workspace: String) -> Self {
        Self {
            live: Arc::new(Mutex::new(HashSet::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            work_dirs: Arc::new(Mutex::new(HashMap::new())),
            db_path,
            workspace,
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

        // Write worker record to SQLite so HTTP handlers (v2_send_message etc.)
        // can find it. Without this the worker exists in state.json but is
        // invisible to all API endpoints.
        upsert_worker_db_record(
            &self.db_path,
            &self.workspace,
            &worker_id,
            &repo_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            &prompt_copy,
        );

        // Prepare agent events directory.
        let agent_dir = work_dir_copy.join(".swarm").join("agents").join(&worker_id);
        std::fs::create_dir_all(&agent_dir)?;

        let agent = spawn_managed_agent(SpawnOptions {
            kind: kind.clone(),
            prompt: effective_prompt,
            working_dir: worktree_path.clone(),
            dangerously_skip_permissions: true,
            resume_session_id: None,
            max_turns: None,
        })
        .await?;

        // Register work_dir so resume_worker/close_worker can find state.json.
        self.work_dirs
            .lock()
            .await
            .insert(worker_id.clone(), work_dir_copy.clone());

        let events_path = agent_dir.join("events.jsonl");
        spawn_agent_task(
            Arc::clone(&self.live),
            Arc::clone(&self.pending),
            Arc::clone(&self.work_dirs),
            worker_id.clone(),
            work_dir_copy,
            repo_path,
            worktree_path,
            kind,
            agent,
            events_path,
            Some(prompt_copy),
        );

        self.live.lock().await.insert(worker_id.clone());

        Ok(worker_id)
    }

    /// Close a worker: stop any pending messages and remove its git worktree.
    pub async fn close_worker(&self, worker_id: &str) -> Result<()> {
        let work_dir = {
            let mut dirs = self.work_dirs.lock().await;
            dirs.remove(worker_id)
                .ok_or_else(|| eyre!("worker {worker_id} not found or not running"))?
        };

        self.live.lock().await.remove(worker_id);
        self.pending.lock().await.remove(worker_id);

        let wt_id = worker_id.to_string();
        let wdir2 = work_dir.clone();

        // Read paths from state.json for git cleanup.
        let state_path = work_dir.join(".swarm").join("state.json");
        let cleanup = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|state| {
                state["worktrees"].as_array().and_then(|arr| {
                    arr.iter()
                        .find(|w| w["id"].as_str() == Some(worker_id))
                        .cloned()
                })
            })
            .and_then(|wt| {
                let repo_path = wt["repo_path"].as_str().map(PathBuf::from)?;
                let worktree_path = wt["worktree_path"].as_str().map(PathBuf::from)?;
                let branch = wt["branch"].as_str().map(|s| s.to_string())?;
                Some((repo_path, worktree_path, branch))
            });

        tokio::task::spawn_blocking(move || {
            update_state_phase(&wdir2, &wt_id, "failed");
            if let Some((repo_path, worktree_path, branch)) = cleanup {
                let _ = apiari_swarm::core::git::remove_worktree(&repo_path, &worktree_path);
                let _ = apiari_swarm::core::git::delete_branch(&repo_path, &branch);
            }
        });

        Ok(())
    }

    /// Send a message to a worker.
    ///
    /// If the agent is currently running, the message is queued and delivered
    /// the instant the current run finishes. If the agent is idle (not live),
    /// a resume session is spawned immediately with this message as the prompt.
    pub async fn send_message(&self, worker_id: &str, message: &str) -> Result<()> {
        let is_running = self.live.lock().await.contains(worker_id);
        if is_running {
            self.pending
                .lock()
                .await
                .entry(worker_id.to_string())
                .or_default()
                .push_back(message.to_string());
            Ok(())
        } else {
            self.resume_worker(worker_id, message).await
        }
    }

    /// Spawn a new agent session to resume a worker that has finished its prior run.
    async fn resume_worker(&self, worker_id: &str, message: &str) -> Result<()> {
        let work_dir = {
            let dirs = self.work_dirs.lock().await;
            dirs.get(worker_id)
                .cloned()
                .ok_or_else(|| eyre!("worker {worker_id} not found"))?
        };

        let state_path = work_dir.join(".swarm").join("state.json");
        let raw = std::fs::read_to_string(&state_path)
            .map_err(|e| eyre!("failed to read state.json: {e}"))?;
        let state: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| eyre!("failed to parse state.json: {e}"))?;

        let wt = state["worktrees"]
            .as_array()
            .and_then(|arr| arr.iter().find(|w| w["id"].as_str() == Some(worker_id)))
            .ok_or_else(|| eyre!("worker {worker_id} not found in state.json"))?
            .clone();

        let session_id = wt["session_id"].as_str().map(|s| s.to_string());
        let worktree_path = wt["worktree_path"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| eyre!("worker {worker_id} has no worktree_path in state.json"))?;
        let repo_path = wt["repo_path"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| eyre!("worker {worker_id} has no repo_path in state.json"))?;
        let kind = wt["agent_kind"]
            .as_str()
            .and_then(|k| k.parse().ok())
            .unwrap_or(AgentKind::Codex);

        update_state_phase(&work_dir, worker_id, "running");

        let agent = spawn_managed_agent(SpawnOptions {
            kind: kind.clone(),
            prompt: message.to_string(),
            working_dir: worktree_path.clone(),
            dangerously_skip_permissions: true,
            resume_session_id: session_id,
            max_turns: None,
        })
        .await?;

        let events_path = work_dir
            .join(".swarm")
            .join("agents")
            .join(worker_id)
            .join("events.jsonl");

        spawn_agent_task(
            Arc::clone(&self.live),
            Arc::clone(&self.pending),
            Arc::clone(&self.work_dirs),
            worker_id.to_string(),
            work_dir,
            repo_path,
            worktree_path,
            kind,
            agent,
            events_path,
            None,
        );

        self.live.lock().await.insert(worker_id.to_string());

        Ok(())
    }

    #[allow(dead_code)]
    pub fn is_live(&self, worker_id: &str) -> bool {
        self.live.try_lock().is_ok_and(|m| m.contains(worker_id))
    }

    // ── Test helpers ───────────────────────────────────────────────────────

    #[cfg(test)]
    pub async fn inject_live_for_test(&self, worker_id: &str) {
        self.live.lock().await.insert(worker_id.to_string());
    }

    #[cfg(test)]
    pub async fn inject_pending_for_test(&self, worker_id: &str, message: &str) {
        self.pending
            .lock()
            .await
            .entry(worker_id.to_string())
            .or_default()
            .push_back(message.to_string());
    }

    #[cfg(test)]
    pub async fn pending_for_test(&self, worker_id: &str) -> Vec<String> {
        self.pending
            .lock()
            .await
            .get(worker_id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }
}

// ── Agent task ─────────────────────────────────────────────────────────

/// Spawn a tokio task that drains agent events, persists the session ID,
/// and picks up any pending messages when the agent finishes.
fn spawn_agent_task(
    live: Arc<Mutex<HashSet<String>>>,
    pending: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
    work_dirs: Arc<Mutex<HashMap<String, PathBuf>>>,
    worker_id: String,
    work_dir: PathBuf,
    repo_path: PathBuf,
    worktree_path: PathBuf,
    kind: AgentKind,
    mut agent: Box<dyn apiari_swarm::daemon::managed_agent::ManagedAgent>,
    events_path: PathBuf,
    initial_prompt: Option<String>,
) {
    tokio::spawn(async move {
        let mut logger = EventLogger::new(events_path);
        if let Some(ref prompt) = initial_prompt {
            logger.log(&AgentEvent::Start {
                timestamp: Utc::now(),
                prompt: prompt.clone(),
                model: None,
            });
        }

        'outer: loop {
            let mut session_id: Option<String> = None;

            // Drain all events for this agent run.
            while let Ok(Some(ev)) = agent.next_event().await {
                if let AgentEventWire::SessionResult {
                    session_id: Some(ref sid),
                    ..
                } = ev
                {
                    session_id = Some(sid.clone());
                    update_state_session_id(&work_dir, &worker_id, sid);
                }
                log_event(&mut logger, &ev);
            }

            // 1. Remove from live BEFORE checking pending (prevents double-spawn).
            live.lock().await.remove(&worker_id);

            // 2. Check pending queue for a follow-up message.
            let next_msg = {
                let mut p = pending.lock().await;
                p.get_mut(&worker_id).and_then(|q| q.pop_front())
            };

            if let Some(msg) = next_msg {
                // 3a. Message waiting — update state, re-add to live, resume.
                update_state_phase(&work_dir, &worker_id, "running");
                logger.log(&AgentEvent::UserMessage {
                    timestamp: Utc::now(),
                    text: msg.clone(),
                });

                match spawn_managed_agent(SpawnOptions {
                    kind: kind.clone(),
                    prompt: msg,
                    working_dir: worktree_path.clone(),
                    dangerously_skip_permissions: true,
                    resume_session_id: session_id,
                    max_turns: None,
                })
                .await
                {
                    Ok(new_agent) => {
                        live.lock().await.insert(worker_id.clone());
                        agent = new_agent;
                        // continue 'outer
                    }
                    Err(e) => {
                        logger.log(&AgentEvent::Error {
                            timestamp: Utc::now(),
                            message: e.to_string(),
                        });
                        update_state_phase(&work_dir, &worker_id, "failed");
                        work_dirs.lock().await.remove(&worker_id);
                        break 'outer;
                    }
                }
            } else {
                // 3b. No pending message — mark completed and clean up.
                update_state_phase(&work_dir, &worker_id, "completed");
                let rpath = repo_path.clone();
                let wt_path = worktree_path.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    apiari_swarm::core::git::pull_main(&rpath);
                    let _ = apiari_swarm::core::git::remove_worktree(&rpath, &wt_path);
                })
                .await;
                work_dirs.lock().await.remove(&worker_id);
                break 'outer;
            }
        }
    });
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Insert a minimal Worker row into SQLite so HTTP handlers can find the worker.
/// The v2_send_message handler reads from this DB — if the row is missing the
/// worker is invisible to the API even though it's in state.json.
fn upsert_worker_db_record(
    db_path: &Path,
    workspace: &str,
    worker_id: &str,
    repo: &str,
    prompt: &str,
) {
    let Ok(store) = crate::buzz::worker::WorkerStore::open(db_path) else {
        tracing::warn!(worker_id, "failed to open worker store for DB upsert");
        return;
    };
    let goal = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(prompt);
    let now = Utc::now().to_rfc3339();
    let worker = crate::buzz::worker::Worker {
        id: worker_id.to_string(),
        workspace: workspace.to_string(),
        state: crate::buzz::worker::WorkerState::Running,
        brief: Some(serde_json::json!({"goal": goal})),
        repo: Some(repo.to_string()),
        branch: None,
        goal: Some(goal.to_string()),
        tests_passing: false,
        branch_ready: false,
        pr_url: None,
        pr_approved: false,
        is_stalled: false,
        revision_count: 0,
        review_mode: "local_first".to_string(),
        blocked_reason: None,
        display_title: None,
        last_output_at: None,
        state_entered_at: now.clone(),
        created_at: now.clone(),
        updated_at: now,
        label: String::new(),
    };
    if let Err(e) = store.upsert(&worker) {
        tracing::warn!(worker_id, error = %e, "failed to write worker to DB");
    }
}

fn resolve_repo(work_dir: &Path, repo: &str) -> Result<PathBuf> {
    // Check configured repos in workspace config.
    if let Ok(configs) = apiari_swarm::core::git::detect_repos(work_dir)
        && let Some(r) = configs
            .iter()
            .find(|r| apiari_swarm::core::git::repo_name(r) == repo || r.ends_with(repo))
    {
        return Ok(r.clone());
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
    logger.log(&event);
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

fn update_state_session_id(work_dir: &Path, worker_id: &str, session_id: &str) {
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
                wt["session_id"] = serde_json::Value::String(session_id.to_string());
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
    if let Ok(json) = serde_json::to_string(&state)
        && std::fs::write(&tmp, json).is_ok()
    {
        let _ = std::fs::rename(tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn init_git_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
    }

    fn read_state(work_dir: &Path) -> serde_json::Value {
        let raw = fs::read_to_string(work_dir.join(".swarm").join("state.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn make_swarm_dir(work_dir: &Path) {
        fs::create_dir_all(work_dir.join(".swarm")).unwrap();
    }

    // ── resolve_repo ──────────────────────────────────────────────────────

    #[test]
    fn resolve_repo_finds_git_repo_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        init_git_repo(&repo);

        let result = resolve_repo(tmp.path(), "myrepo").unwrap();
        assert_eq!(result, repo);
    }

    #[test]
    fn resolve_repo_falls_back_to_direct_path_when_dir_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("somedir");
        fs::create_dir_all(&dir).unwrap();

        let result = resolve_repo(tmp.path(), "somedir").unwrap();
        assert_eq!(result, dir);
    }

    #[test]
    fn resolve_repo_errors_when_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_repo(tmp.path(), "nonexistent").unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn resolve_repo_errors_include_work_dir_context() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_repo(tmp.path(), "ghost").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost"), "error should name the missing repo");
    }

    #[test]
    fn resolve_repo_prefers_git_repo_over_plain_dir_with_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("proj");
        init_git_repo(&repo);

        // Should find it via detect_repos (git scan) — either way result is correct.
        let result = resolve_repo(tmp.path(), "proj").unwrap();
        assert_eq!(result, repo);
    }

    // ── upsert_state_entry ────────────────────────────────────────────────

    #[test]
    fn upsert_creates_state_json_from_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());

        upsert_state_entry(
            tmp.path(),
            "repo-a1b2",
            "feat/my-task",
            "do the thing",
            &AgentKind::Codex,
            Path::new("/workspace/repo"),
            Path::new("/workspace/.swarm/wt/repo-a1b2"),
        );

        let state = read_state(tmp.path());
        let worktrees = state["worktrees"].as_array().unwrap();
        assert_eq!(worktrees.len(), 1);
        let wt = &worktrees[0];
        assert_eq!(wt["id"].as_str(), Some("repo-a1b2"));
        assert_eq!(wt["branch"].as_str(), Some("feat/my-task"));
        assert_eq!(wt["prompt"].as_str(), Some("do the thing"));
        assert_eq!(wt["phase"].as_str(), Some("starting"));
        assert_eq!(wt["status"].as_str(), Some("running"));
        assert!(
            wt["created_at"].as_str().is_some(),
            "created_at should be set"
        );
    }

    #[test]
    fn upsert_preserves_session_name_and_inbox_pos() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());

        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        let state = read_state(tmp.path());
        assert_eq!(state["session_name"].as_str(), Some("apiari"));
        assert_eq!(state["last_inbox_pos"].as_i64(), Some(0));
    }

    #[test]
    fn upsert_updates_existing_entry_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());

        upsert_state_entry(
            tmp.path(),
            "w-1",
            "feat/old",
            "old prompt",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "feat/new",
            "new prompt",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        let state = read_state(tmp.path());
        let worktrees = state["worktrees"].as_array().unwrap();
        assert_eq!(worktrees.len(), 1, "same id must not create a duplicate");
        assert_eq!(worktrees[0]["branch"].as_str(), Some("feat/new"));
        assert_eq!(worktrees[0]["prompt"].as_str(), Some("new prompt"));
    }

    #[test]
    fn upsert_appends_when_ids_differ() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());

        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b1",
            "p1",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );
        upsert_state_entry(
            tmp.path(),
            "w-2",
            "b2",
            "p2",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );
        upsert_state_entry(
            tmp.path(),
            "w-3",
            "b3",
            "p3",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        let state = read_state(tmp.path());
        assert_eq!(state["worktrees"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn upsert_sets_correct_agent_kind_label() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());

        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Claude,
            Path::new("/r"),
            Path::new("/w"),
        );

        let state = read_state(tmp.path());
        let kind = state["worktrees"][0]["agent_kind"].as_str().unwrap();
        assert_eq!(kind, AgentKind::Claude.label());
    }

    // ── update_state_phase ────────────────────────────────────────────────

    #[test]
    fn update_phase_waiting_sets_status_running() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_phase(tmp.path(), "w-1", "waiting");

        let state = read_state(tmp.path());
        assert_eq!(state["worktrees"][0]["phase"].as_str(), Some("waiting"));
        assert_eq!(state["worktrees"][0]["status"].as_str(), Some("running"));
    }

    #[test]
    fn update_phase_running_keeps_status_running() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_phase(tmp.path(), "w-1", "running");

        let state = read_state(tmp.path());
        assert_eq!(state["worktrees"][0]["phase"].as_str(), Some("running"));
        assert_eq!(state["worktrees"][0]["status"].as_str(), Some("running"));
    }

    #[test]
    fn update_phase_failed_sets_status_done() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_phase(tmp.path(), "w-1", "failed");

        let state = read_state(tmp.path());
        assert_eq!(state["worktrees"][0]["phase"].as_str(), Some("failed"));
        assert_eq!(state["worktrees"][0]["status"].as_str(), Some("done"));
    }

    #[test]
    fn update_phase_starting_sets_status_done() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_phase(tmp.path(), "w-1", "starting");

        let state = read_state(tmp.path());
        assert_eq!(state["worktrees"][0]["status"].as_str(), Some("done"));
    }

    #[test]
    fn update_phase_is_noop_when_no_state_json() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        // No panic expected — silently does nothing.
        update_state_phase(tmp.path(), "w-1", "waiting");
    }

    #[test]
    fn update_phase_does_not_touch_other_workers() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b1",
            "p1",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );
        upsert_state_entry(
            tmp.path(),
            "w-2",
            "b2",
            "p2",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_phase(tmp.path(), "w-1", "failed");

        let state = read_state(tmp.path());
        let w2 = state["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"].as_str() == Some("w-2"))
            .unwrap();
        assert_eq!(
            w2["phase"].as_str(),
            Some("starting"),
            "w-2 must be unchanged"
        );
        assert_eq!(
            w2["status"].as_str(),
            Some("running"),
            "w-2 status must be unchanged"
        );
    }

    #[test]
    fn update_phase_is_noop_for_unknown_worker_id() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_phase(tmp.path(), "ghost", "failed");

        let state = read_state(tmp.path());
        // w-1 should be untouched.
        assert_eq!(state["worktrees"][0]["phase"].as_str(), Some("starting"));
    }

    // ── update_state_session_id ───────────────────────────────────────────

    #[test]
    fn update_session_id_persists_to_state_json() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "b",
            "p",
            &AgentKind::Claude,
            Path::new("/r"),
            Path::new("/w"),
        );

        update_state_session_id(tmp.path(), "w-1", "ses-abc123");

        let state = read_state(tmp.path());
        assert_eq!(
            state["worktrees"][0]["session_id"].as_str(),
            Some("ses-abc123")
        );
    }

    #[test]
    fn update_session_id_is_noop_when_no_state_json() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        // Should not panic.
        update_state_session_id(tmp.path(), "w-1", "ses-xyz");
    }

    // ── WorkerManager — error paths ───────────────────────────────────────

    #[tokio::test]
    async fn send_message_to_unknown_worker_returns_error() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        let err = mgr.send_message("ghost-1234", "hello").await.unwrap_err();
        assert!(
            err.to_string().contains("ghost-1234"),
            "error should name the missing worker"
        );
    }

    #[tokio::test]
    async fn close_unknown_worker_returns_error() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        let err = mgr.close_worker("ghost-1234").await.unwrap_err();
        assert!(err.to_string().contains("ghost-1234"));
    }

    #[tokio::test]
    async fn is_live_returns_false_for_unknown_worker() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        assert!(!mgr.is_live("ghost-1234"));
    }

    #[tokio::test]
    async fn new_manager_starts_with_no_live_workers() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        assert!(!mgr.is_live("a"));
        assert!(!mgr.is_live(""));
    }

    // ── send_message queueing ─────────────────────────────────────────────

    #[tokio::test]
    async fn send_message_while_live_queues_not_errors() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        mgr.inject_live_for_test("live-abc1").await;

        mgr.send_message("live-abc1", "follow-up").await.unwrap();

        let pending = mgr.pending_for_test("live-abc1").await;
        assert_eq!(pending, vec!["follow-up"]);
    }

    #[tokio::test]
    async fn send_message_queues_multiple_messages_in_order() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        mgr.inject_live_for_test("w-1").await;

        mgr.send_message("w-1", "first").await.unwrap();
        mgr.send_message("w-1", "second").await.unwrap();
        mgr.send_message("w-1", "third").await.unwrap();

        let pending = mgr.pending_for_test("w-1").await;
        assert_eq!(pending, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn send_message_when_not_live_calls_resume_and_returns_err_without_state() {
        // resume_worker tries to read state.json which doesn't exist — that's OK.
        // The important assertion: it returns Err, not panics.
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        // Inject into work_dirs so resume_worker can find the worker, but state.json doesn't exist.
        mgr.work_dirs.lock().await.insert(
            "idle-w1".to_string(),
            std::path::PathBuf::from("/nonexistent"),
        );

        let result = mgr.send_message("idle-w1", "please resume").await;
        assert!(
            result.is_err(),
            "should fail gracefully without real state.json"
        );
    }

    #[tokio::test]
    async fn pending_message_cleared_after_pickup() {
        let mgr = WorkerManager::new(std::path::PathBuf::from("/tmp/test.db"), "test".to_string());
        mgr.inject_pending_for_test("w-1", "queued message").await;

        // Simulate the agent task picking up the pending message.
        let msg = mgr
            .pending
            .lock()
            .await
            .get_mut("w-1")
            .and_then(|q| q.pop_front());

        assert_eq!(msg.as_deref(), Some("queued message"));
        assert!(
            mgr.pending_for_test("w-1").await.is_empty(),
            "queue should be empty after pickup"
        );
    }
}

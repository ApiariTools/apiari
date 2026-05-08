//! In-process worker lifecycle manager.
//!
//! Creates git worktrees, spawns agents, and routes messages — all directly,
//! with no separate daemon process or IPC socket. One process total.
//!
//! ## Message delivery
//!
//! Agents (Claude/Codex/Gemini) are CLI processes that run to completion and
//! resume via a session ID. They are NOT long-lived stdin-reading processes.
//! Messages are delivered by resuming the agent's session — not by writing to
//! a channel connected to a running process.
//!
//! - `live` tracks which workers have an agent process currently running.
//! - `pending` holds messages queued while an agent is running.
//! - `send_message`: if running → queue it; if idle → spawn a resume immediately.
//! - Agent task: when done → drain pending → resume if message waiting; else idle.

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
    /// Worker IDs whose agent process is currently running.
    live: Arc<Mutex<HashSet<String>>>,
    /// Messages queued while the agent is running. Drained on agent exit.
    pending: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
    db_path: PathBuf,
    workspace: String,
}

impl WorkerManager {
    pub fn new(db_path: PathBuf, workspace: String) -> Self {
        Self {
            live: Arc::new(Mutex::new(HashSet::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            db_path,
            workspace,
        }
    }

    /// Create a worker directory and spawn an agent. Returns the worker ID.
    pub async fn create_worker(
        &self,
        work_dir: &Path,
        repo: &str,
        prompt: &str,
        agent: &str,
    ) -> Result<String> {
        self.create_worker_with_task_dir(
            work_dir,
            repo,
            prompt,
            agent,
            None,
            crate::config::WorkerIsolation::Worktree,
        )
        .await
    }

    /// Create a worker directory, optionally seed .task/ artifacts, and spawn an agent.
    pub async fn create_worker_with_task_dir(
        &self,
        work_dir: &Path,
        repo: &str,
        prompt: &str,
        agent: &str,
        task_dir: Option<TaskDirPayload>,
        isolation: crate::config::WorkerIsolation,
    ) -> Result<String> {
        use apiari_swarm::core::git;

        let work_dir = work_dir.to_path_buf();
        let repo = repo.to_string();
        let prompt = prompt.to_string();
        let agent_str = agent.to_string();
        let work_dir_copy = work_dir.clone();
        let prompt_copy = prompt.clone();
        let isolation_clone = isolation.clone();

        let (worker_id, branch, repo_path, worktree_path, effective_prompt) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let short_id = &uuid::Uuid::new_v4().to_string()[..4];
                let worker_id;
                let branch;
                let repo_path;
                let worktree_path;

                // In e2e mode all git operations are skipped — CI uses a shallow
                // clone and worktree creation would hang or fail.  The mock agent
                // only needs the events directory to exist.
                if std::env::var("APIARI_E2E_AGENT").is_ok() {
                    repo_path = work_dir.clone();
                    worker_id =
                        format!("{}-{}", repo.rsplit('/').next().unwrap_or(&repo), short_id);
                    branch = git::generate_branch_name(&prompt, short_id);
                    worktree_path = work_dir.join(".swarm").join("wt").join(&worker_id);
                    std::fs::create_dir_all(&worktree_path)?;
                } else {
                    repo_path = resolve_repo(&work_dir, &repo)?;
                    git::pull_main(&repo_path);
                    let name = git::repo_name(&repo_path);
                    worker_id = format!("{}-{}", name, short_id);
                    branch = git::generate_branch_name(&prompt, short_id);
                    worktree_path = work_dir.join(".swarm").join("wt").join(&worker_id);

                    match isolation_clone {
                        crate::config::WorkerIsolation::Worktree => {
                            git::create_worktree(
                                &repo_path,
                                &branch,
                                &worktree_path,
                                Some("origin/main"),
                            )?;
                            git::symlink_worktree_files(&repo_path, &worktree_path);
                            let cmds = git::read_worktree_setup_commands(&repo_path);
                            if !cmds.is_empty() {
                                git::run_worktree_setup_commands(&worktree_path, &cmds)?;
                            }
                        }
                        crate::config::WorkerIsolation::Copy => {
                            create_repo_copy(&repo_path, &worktree_path, &branch)?;
                        }
                    }
                }

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

        upsert_state_entry(
            &work_dir_copy,
            &worker_id,
            &branch,
            &prompt_copy,
            &kind,
            &repo_path,
            &worktree_path,
        );

        upsert_worker_db_record(
            &self.db_path,
            &self.workspace,
            &worker_id,
            &repo_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            &prompt_copy,
            &worktree_path,
            isolation.as_str(),
            &agent_str,
            &repo_path,
        );

        let agent_dir = work_dir_copy.join(".swarm").join("agents").join(&worker_id);
        std::fs::create_dir_all(&agent_dir)?;

        let agent = spawn_managed_agent(SpawnOptions {
            kind,
            prompt: effective_prompt.clone(),
            working_dir: worktree_path.clone(),
            dangerously_skip_permissions: true,
            resume_session_id: None,
            max_turns: None,
        })
        .await?;

        // Mark live before spawning so send_message queues correctly.
        self.live.lock().await.insert(worker_id.clone());

        let live = Arc::clone(&self.live);
        let pending = Arc::clone(&self.pending);
        let wid = worker_id.clone();
        let wdir = work_dir_copy.clone();
        let wt_path = worktree_path.clone();
        let rpath = repo_path.clone();

        tokio::spawn(run_agent_task(
            agent,
            wid,
            effective_prompt,
            wdir,
            wt_path,
            rpath,
            live,
            pending,
        ));

        Ok(worker_id)
    }

    /// Send a message to a worker.
    ///
    /// - If the agent is currently running: queue the message. It will be
    ///   picked up when the current run finishes and the agent resumes.
    /// - If the agent is idle: spawn a resume session immediately.
    pub async fn send_message(&self, worker_id: &str, message: &str) -> Result<()> {
        let is_running = self.live.lock().await.contains(worker_id);
        if is_running {
            // Log the user message immediately so it appears in the timeline
            // before the current agent run finishes and dequeues it.
            if let Ok((work_dir, _, _, _)) =
                read_worker_paths(&self.db_path, &self.workspace, worker_id)
            {
                let events_path = work_dir
                    .join(".swarm")
                    .join("agents")
                    .join(worker_id)
                    .join("events.jsonl");
                let logger = EventLogger::new(events_path);
                logger.log(&AgentEvent::UserMessage {
                    timestamp: Utc::now(),
                    text: message.to_string(),
                });
            }
            self.pending
                .lock()
                .await
                .entry(worker_id.to_string())
                .or_default()
                .push_back(message.to_string());
            tracing::debug!(worker_id, "message queued (agent running)");
            Ok(())
        } else {
            // Agent idle — look up session_id from state.json and resume.
            self.resume_worker(worker_id, message).await
        }
    }

    /// Spawn a resume session for an idle worker.
    async fn resume_worker(&self, worker_id: &str, message: &str) -> Result<()> {
        let session_id = read_session_id(&self.db_path, &self.workspace, worker_id);

        let (work_dir, worktree_path, repo_path, kind) =
            read_worker_paths(&self.db_path, &self.workspace, worker_id)?;

        let wt_clone = worktree_path.clone();
        let msg_clone = message.to_string();

        let agent = spawn_managed_agent(SpawnOptions {
            kind,
            prompt: message.to_string(),
            working_dir: worktree_path.clone(),
            dangerously_skip_permissions: true,
            resume_session_id: session_id,
            max_turns: None,
        })
        .await
        .map_err(|e| eyre!("failed to resume worker {worker_id}: {e}"))?;

        // Log the user message synchronously so the UI reflects it before the
        // async task has a chance to write its own events.
        let events_path = work_dir
            .join(".swarm")
            .join("agents")
            .join(worker_id)
            .join("events.jsonl");
        let logger = EventLogger::new(events_path);
        logger.log(&AgentEvent::UserMessage {
            timestamp: Utc::now(),
            text: message.to_string(),
        });

        self.live.lock().await.insert(worker_id.to_string());

        let live = Arc::clone(&self.live);
        let pending = Arc::clone(&self.pending);
        let wid = worker_id.to_string();

        tokio::spawn(run_agent_task(
            agent, wid, msg_clone, work_dir, wt_clone, repo_path, live, pending,
        ));

        Ok(())
    }

    /// Close a worker: remove from tracking and clean up its directory.
    pub async fn close_worker(&self, worker_id: &str) -> Result<()> {
        let was_live = self.live.lock().await.remove(worker_id);
        self.pending.lock().await.remove(worker_id);

        if !was_live {
            return Err(eyre!("worker {worker_id} not found or not running"));
        }

        // Best-effort: look up paths and isolation mode from DB for cleanup.
        let paths = read_worker_paths(&self.db_path, &self.workspace, worker_id);
        let isolation_mode = crate::buzz::worker::WorkerStore::open(&self.db_path)
            .ok()
            .and_then(|s| s.get(&self.workspace, worker_id).ok().flatten())
            .and_then(|w| w.isolation_mode)
            .unwrap_or_else(|| "worktree".to_string());

        let wt_id = worker_id.to_string();
        tokio::task::spawn_blocking(move || {
            if let Ok((work_dir, worktree_path, repo_path, _)) = paths {
                match isolation_mode.as_str() {
                    "copy" => {
                        // Just delete the directory; branch lives only in the copy's .git
                        let _ = std::fs::remove_dir_all(&worktree_path);
                    }
                    _ => {
                        // git worktree remove + delete branch from main repo
                        let _ =
                            apiari_swarm::core::git::remove_worktree(&repo_path, &worktree_path);
                        let _ = apiari_swarm::core::git::delete_branch(&repo_path, &wt_id);
                    }
                }
                update_state_phase(&work_dir, &wt_id, "failed");
            }
        });

        Ok(())
    }

    #[cfg(test)]
    pub fn is_live(&self, worker_id: &str) -> bool {
        self.live.try_lock().is_ok_and(|m| m.contains(worker_id))
    }

    #[cfg(test)]
    pub async fn inject_live_for_test(&self, worker_id: &str) {
        self.live.lock().await.insert(worker_id.to_string());
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

/// Run an agent to completion, then drain pending messages via session resume.
/// Loops internally — never recurses — so the future stays Send.
#[allow(clippy::too_many_arguments)]
async fn run_agent_task(
    mut agent: Box<dyn apiari_swarm::daemon::managed_agent::ManagedAgent>,
    worker_id: String,
    prompt: String,
    work_dir: PathBuf,
    worktree_path: PathBuf,
    repo_path: PathBuf,
    live: Arc<Mutex<HashSet<String>>>,
    pending: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
) {
    let events_path = work_dir
        .join(".swarm")
        .join("agents")
        .join(&worker_id)
        .join("events.jsonl");
    let mut logger = EventLogger::new(events_path);
    logger.log(&AgentEvent::Start {
        timestamp: Utc::now(),
        prompt: prompt.clone(),
        model: None,
    });

    update_state_phase(&work_dir, &worker_id, "running");

    let mut accumulated_text = String::new();
    let mut pr_url_written = false;

    loop {
        // Drain all events from the current agent run.
        let mut last_session_id: Option<String> = None;
        while let Ok(Some(ev)) = agent.next_event().await {
            if let AgentEventWire::SessionResult {
                session_id: Some(ref sid),
                ..
            } = ev
            {
                last_session_id = Some(sid.clone());
            }
            // Fast path: capture PR_OPENED from streamed text immediately.
            // Covers mock agent and any real agent that outputs the marker.
            if let AgentEventWire::TextDelta { ref text } = ev {
                accumulated_text.push_str(text);
                if !pr_url_written
                    && let Some(url) = apiari_swarm::core::state::parse_pr_opened(&accumulated_text)
                {
                    update_state_pr(&work_dir, &worker_id, &url);
                    pr_url_written = true;
                }
            }
            log_event(&mut logger, &ev);
        }

        if let Some(ref sid) = last_session_id {
            persist_session_id(&work_dir, &worker_id, sid);
        }

        // Remove from live FIRST — any concurrent send_message will queue
        // into pending instead of hitting a dead path.
        live.lock().await.remove(&worker_id);

        // Check for a queued message.
        let next_msg = pending
            .lock()
            .await
            .get_mut(&worker_id)
            .and_then(|q| q.pop_front());

        match next_msg {
            Some(msg) => {
                logger.log(&AgentEvent::UserMessage {
                    timestamp: Utc::now(),
                    text: msg.clone(),
                });
                update_state_phase(&work_dir, &worker_id, "running");

                match spawn_managed_agent(SpawnOptions {
                    kind: AgentKind::Codex,
                    prompt: msg,
                    working_dir: worktree_path.clone(),
                    dangerously_skip_permissions: true,
                    resume_session_id: last_session_id,
                    max_turns: None,
                })
                .await
                {
                    Ok(resumed) => {
                        live.lock().await.insert(worker_id.clone());
                        agent = resumed;
                        // Loop: run the resumed agent.
                    }
                    Err(e) => {
                        logger.log(&AgentEvent::Error {
                            timestamp: Utc::now(),
                            message: e.to_string(),
                        });
                        update_state_phase(&work_dir, &worker_id, "failed");
                        break;
                    }
                }
            }
            None => {
                update_state_phase(&work_dir, &worker_id, "waiting");
                // Fallback: if text scan didn't find a PR URL, ask GitHub.
                if !pr_url_written && let Some(url) = lookup_pr_by_branch(&work_dir, &worker_id) {
                    update_state_pr(&work_dir, &worker_id, &url);
                    pr_url_written = true;
                }
                let _ = pr_url_written;
                break;
            }
        }
    }

    let _ = tokio::task::spawn_blocking(move || {
        apiari_swarm::core::git::pull_main(&repo_path);
    })
    .await;
}

// ── Session ID persistence ─────────────────────────────────────────────

fn persist_session_id(work_dir: &Path, worker_id: &str, session_id: &str) {
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

fn read_session_id(db_path: &Path, workspace: &str, worker_id: &str) -> Option<String> {
    let store = crate::buzz::worker::WorkerStore::open(db_path).ok()?;
    let worker = store.get(workspace, worker_id).ok()??;
    let worktree_path = PathBuf::from(worker.worktree_path?);
    // worktree_path is <work_dir>/.swarm/wt/<id> — climb three levels to work_dir
    let work_dir = worktree_path.parent()?.parent()?.parent()?;
    let raw = std::fs::read_to_string(work_dir.join(".swarm").join("state.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let worktrees = json
        .get("worktrees")
        .and_then(|v| v.as_array())
        .or_else(|| json.as_array())?;
    worktrees
        .iter()
        .find(|wt| wt.get("id").and_then(|v| v.as_str()) == Some(worker_id))
        .and_then(|wt| wt.get("session_id").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

// ── Worker path resolution ─────────────────────────────────────────────

/// Look up a worker's paths and agent kind.
///
/// SQLite is the authoritative source. state.json is consulted only when the
/// worker is not in the DB at all (e.g. created by an older version before the
/// worktree_path/repo_path columns existed).
///
/// If the worker IS in SQLite but its paths are missing, that is a data-integrity
/// error and we surface it loudly rather than silently masking it with a stale
/// state.json value.
fn read_worker_paths(
    db_path: &Path,
    workspace: &str,
    worker_id: &str,
) -> Result<(PathBuf, PathBuf, PathBuf, AgentKind)> {
    let store = crate::buzz::worker::WorkerStore::open(db_path)?;
    let db_worker = store.get(workspace, worker_id)?;

    let (worktree_path, repo_path, kind) = match db_worker {
        Some(w) => {
            // Worker is in SQLite — use its paths. Missing paths here is a bug,
            // not a legacy-compat case, so we return a hard error.
            let wt = w.worktree_path.map(PathBuf::from).ok_or_else(|| {
                eyre!(
                    "worker {worker_id} is in DB but missing worktree_path — data integrity error"
                )
            })?;
            let rp = w.repo_path.map(PathBuf::from).ok_or_else(|| {
                eyre!("worker {worker_id} is in DB but missing repo_path — data integrity error")
            })?;
            let kind = w
                .agent_kind
                .and_then(|k| k.parse::<AgentKind>().ok())
                .unwrap_or(AgentKind::Codex);
            (wt, rp, kind)
        }
        None => {
            // Worker not in SQLite at all — legitimate for workers created before
            // the DB columns were added. Fall back to state.json.
            let (wt, rp, k) = read_paths_from_state(db_path, workspace, worker_id)?;
            (wt, rp, k)
        }
    };

    // work_dir is three levels above worktree_path (.swarm/wt/<id>)
    let work_dir = worktree_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| eyre!("cannot derive work_dir from {}", worktree_path.display()))?
        .to_path_buf();

    Ok((work_dir, worktree_path, repo_path, kind))
}

/// Read worktree_path, repo_path, agent_kind from state.json for a given worker.
fn read_paths_from_state(
    _db_path: &Path,
    workspace: &str,
    worker_id: &str,
) -> Result<(PathBuf, PathBuf, AgentKind)> {
    let ws_root = crate::config::discover_workspaces()
        .map_err(|e| eyre!("could not discover workspaces: {e}"))?
        .into_iter()
        .find(|ws| ws.name == workspace)
        .ok_or_else(|| eyre!("workspace '{workspace}' not found in config"))?
        .config
        .root;

    let state_path = ws_root.join(".swarm").join("state.json");
    let raw = std::fs::read_to_string(&state_path)
        .map_err(|_| eyre!("state.json not found at {}", state_path.display()))?;
    let state: serde_json::Value = serde_json::from_str(&raw)?;

    let wt = state["worktrees"]
        .as_array()
        .and_then(|arr| arr.iter().find(|e| e["id"].as_str() == Some(worker_id)))
        .ok_or_else(|| eyre!("worker {worker_id} not found in state.json"))?;

    let worktree_path = wt["worktree_path"]
        .as_str()
        .ok_or_else(|| eyre!("worker {worker_id} has no worktree_path in state.json"))?;
    let repo_path = wt["repo_path"]
        .as_str()
        .ok_or_else(|| eyre!("worker {worker_id} has no repo_path in state.json"))?;
    let kind = wt["agent_kind"]
        .as_str()
        .and_then(|k| k.parse::<AgentKind>().ok())
        .unwrap_or(AgentKind::Codex);

    Ok((PathBuf::from(worktree_path), PathBuf::from(repo_path), kind))
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Create a worker directory by copying the repo (APFS CoW on macOS, plain cp elsewhere).
/// Returns the destination path on success.
fn create_repo_copy(repo_path: &Path, dest_path: &Path, branch: &str) -> Result<()> {
    use std::process::Command;

    // On macOS, `-c` enables clonefile (CoW) — instant on APFS, near-zero extra disk.
    // On other platforms, fall back to a plain recursive copy.
    #[cfg(target_os = "macos")]
    let status = Command::new("cp")
        .args([
            "-rc",
            &repo_path.to_string_lossy(),
            &dest_path.to_string_lossy(),
        ])
        .status()?;

    #[cfg(not(target_os = "macos"))]
    let status = Command::new("cp")
        .args([
            "-r",
            &repo_path.to_string_lossy(),
            &dest_path.to_string_lossy(),
        ])
        .status()?;

    if !status.success() {
        return Err(eyre!("failed to copy repo to {}", dest_path.display()));
    }

    // Create and switch to a new branch in the copy.
    let out = Command::new("git")
        .args(["checkout", "-b", branch])
        .current_dir(dest_path)
        .output()?;

    if !out.status.success() {
        return Err(eyre!(
            "failed to create branch {branch}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_worker_db_record(
    db_path: &Path,
    workspace: &str,
    worker_id: &str,
    repo: &str,
    prompt: &str,
    worktree_path: &Path,
    isolation_mode: &str,
    agent_kind: &str,
    repo_path: &Path,
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
        worktree_path: Some(worktree_path.to_string_lossy().to_string()),
        isolation_mode: Some(isolation_mode.to_string()),
        agent_kind: Some(agent_kind.to_string()),
        repo_path: Some(repo_path.to_string_lossy().to_string()),
        label: String::new(),
    };
    if let Err(e) = store.upsert(&worker) {
        tracing::warn!(worker_id, error = %e, "failed to write worker to DB");
    }
}

fn resolve_repo(work_dir: &Path, repo: &str) -> Result<PathBuf> {
    if let Ok(configs) = apiari_swarm::core::git::detect_repos(work_dir)
        && let Some(r) = configs
            .iter()
            .find(|r| apiari_swarm::core::git::repo_name(r) == repo || r.ends_with(repo))
    {
        return Ok(r.clone());
    }
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

fn lookup_pr_by_branch(work_dir: &Path, worker_id: &str) -> Option<String> {
    let path = work_dir.join(".swarm").join("state.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let state: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let branch = state["worktrees"]
        .as_array()?
        .iter()
        .find(|wt| wt["id"].as_str() == Some(worker_id))?
        .get("branch")
        .and_then(|v| v.as_str())?
        .to_string();

    let out = std::process::Command::new("gh")
        .args([
            "pr", "list", "--head", &branch, "--state", "all", "--json", "url", "--limit", "1",
        ])
        .current_dir(work_dir)
        .output()
        .ok()?;

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    json.as_array()?
        .first()?
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn update_state_pr(work_dir: &Path, worker_id: &str, pr_url: &str) {
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
                wt["pr"] = serde_json::json!({ "url": pr_url });
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
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn resolve_repo_prefers_git_repo_over_plain_dir_with_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("proj");
        init_git_repo(&repo);
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
        let wt = &state["worktrees"][0];
        assert_eq!(wt["id"].as_str(), Some("repo-a1b2"));
        assert_eq!(wt["branch"].as_str(), Some("feat/my-task"));
        assert_eq!(wt["phase"].as_str(), Some("starting"));
        assert_eq!(wt["status"].as_str(), Some("running"));
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
            "old",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );
        upsert_state_entry(
            tmp.path(),
            "w-1",
            "feat/new",
            "new",
            &AgentKind::Codex,
            Path::new("/r"),
            Path::new("/w"),
        );
        let state = read_state(tmp.path());
        let wts = state["worktrees"].as_array().unwrap();
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0]["branch"].as_str(), Some("feat/new"));
    }

    #[test]
    fn upsert_appends_when_ids_differ() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        for (id, b) in [("w-1", "b1"), ("w-2", "b2"), ("w-3", "b3")] {
            upsert_state_entry(
                tmp.path(),
                id,
                b,
                "p",
                &AgentKind::Codex,
                Path::new("/r"),
                Path::new("/w"),
            );
        }
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
        assert_eq!(
            state["worktrees"][0]["agent_kind"].as_str(),
            Some(AgentKind::Claude.label())
        );
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
        assert_eq!(state["worktrees"][0]["status"].as_str(), Some("done"));
    }

    #[test]
    fn update_phase_is_noop_when_no_state_json() {
        let tmp = tempfile::tempdir().unwrap();
        make_swarm_dir(tmp.path());
        update_state_phase(tmp.path(), "w-1", "waiting"); // no panic
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
        assert_eq!(w2["phase"].as_str(), Some("starting"));
    }

    // ── WorkerManager — pending queue ─────────────────────────────────────

    #[tokio::test]
    async fn send_message_while_live_queues_message() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorkerManager::new(tmp.path().join("test.db"), "ws".to_string());
        mgr.inject_live_for_test("w-abc1").await;

        mgr.send_message("w-abc1", "do more work").await.unwrap();

        let pending = mgr.pending_for_test("w-abc1").await;
        assert_eq!(pending, vec!["do more work"]);
    }

    #[tokio::test]
    async fn send_message_while_live_multiple_messages_all_queued() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorkerManager::new(tmp.path().join("test.db"), "ws".to_string());
        mgr.inject_live_for_test("w-abc1").await;

        mgr.send_message("w-abc1", "first").await.unwrap();
        mgr.send_message("w-abc1", "second").await.unwrap();
        mgr.send_message("w-abc1", "third").await.unwrap();

        let pending = mgr.pending_for_test("w-abc1").await;
        assert_eq!(pending, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn send_message_when_not_live_attempts_resume() {
        // Not live — should attempt resume (will fail without real DB/state.json).
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorkerManager::new(tmp.path().join("test.db"), "ws".to_string());
        let result = mgr.send_message("w-unknown", "hello").await;
        // Must fail cleanly (no real DB), not panic.
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.is_empty());
    }

    #[tokio::test]
    async fn is_live_false_when_not_inserted() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorkerManager::new(tmp.path().join("test.db"), "ws".to_string());
        assert!(!mgr.is_live("anything"));
    }

    #[tokio::test]
    async fn is_live_true_after_inject() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorkerManager::new(tmp.path().join("test.db"), "ws".to_string());
        mgr.inject_live_for_test("w-abc1").await;
        assert!(mgr.is_live("w-abc1"));
    }

    #[tokio::test]
    async fn close_worker_not_live_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorkerManager::new(tmp.path().join("test.db"), "ws".to_string());
        let err = mgr.close_worker("ghost-1234").await.unwrap_err();
        assert!(err.to_string().contains("ghost-1234"));
    }

    // ── upsert_worker_db_record ───────────────────────────────────────────

    #[test]
    fn upsert_worker_db_record_creates_findable_row() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let p = std::path::Path::new("/tmp");
        upsert_worker_db_record(
            &db_path,
            "myws",
            "w-abc1",
            "myrepo",
            "fix the bug",
            p,
            "worktree",
            "codex",
            p,
        );
        let store = crate::buzz::worker::WorkerStore::open(&db_path).unwrap();
        let worker = store.get("myws", "w-abc1").unwrap().unwrap();
        assert_eq!(worker.id, "w-abc1");
        assert_eq!(worker.workspace, "myws");
        assert_eq!(worker.repo.as_deref(), Some("myrepo"));
        assert_eq!(worker.goal.as_deref(), Some("fix the bug"));
        assert_eq!(worker.state, crate::buzz::worker::WorkerState::Running);
    }

    #[test]
    fn upsert_worker_db_record_trims_goal_from_multiline_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let p = std::path::Path::new("/tmp");
        upsert_worker_db_record(
            &db_path,
            "ws",
            "w-0001",
            "repo",
            "\n\nfix auth bug\n\ncontext",
            p,
            "worktree",
            "codex",
            p,
        );
        let store = crate::buzz::worker::WorkerStore::open(&db_path).unwrap();
        let worker = store.get("ws", "w-0001").unwrap().unwrap();
        assert_eq!(worker.goal.as_deref(), Some("fix auth bug"));
    }

    #[test]
    fn upsert_worker_db_record_appears_in_list() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let p = std::path::Path::new("/tmp");
        upsert_worker_db_record(
            &db_path, "ws", "w-list1", "repo", "task one", p, "worktree", "codex", p,
        );
        upsert_worker_db_record(
            &db_path, "ws", "w-list2", "repo", "task two", p, "worktree", "codex", p,
        );
        let store = crate::buzz::worker::WorkerStore::open(&db_path).unwrap();
        let workers = store.list("ws").unwrap();
        let ids: Vec<&str> = workers.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&"w-list1"));
        assert!(ids.contains(&"w-list2"));
    }

    #[test]
    fn upsert_worker_db_record_workspace_isolation() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let p = std::path::Path::new("/tmp");
        upsert_worker_db_record(
            &db_path, "ws-a", "w-0001", "repo", "task", p, "worktree", "codex", p,
        );
        upsert_worker_db_record(
            &db_path, "ws-b", "w-0002", "repo", "task", p, "worktree", "codex", p,
        );
        let store = crate::buzz::worker::WorkerStore::open(&db_path).unwrap();
        assert_eq!(store.list("ws-a").unwrap().len(), 1);
        assert_eq!(store.list("ws-b").unwrap().len(), 1);
        assert!(store.get("ws-a", "w-0002").unwrap().is_none());
    }

    #[test]
    fn upsert_worker_db_record_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let p = std::path::Path::new("/tmp");
        upsert_worker_db_record(
            &db_path, "ws", "w-idem", "repo", "task", p, "worktree", "codex", p,
        );
        upsert_worker_db_record(
            &db_path, "ws", "w-idem", "repo", "task", p, "worktree", "codex", p,
        );
        let store = crate::buzz::worker::WorkerStore::open(&db_path).unwrap();
        assert_eq!(
            store
                .list("ws")
                .unwrap()
                .iter()
                .filter(|w| w.id == "w-idem")
                .count(),
            1
        );
    }

    #[test]
    fn upsert_worker_db_record_survives_missing_db_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("nonexistent").join("test.db");
        let p = std::path::Path::new("/tmp");
        upsert_worker_db_record(
            &db_path, "ws", "w-0001", "repo", "task", p, "worktree", "codex", p,
        ); // no panic
    }
}

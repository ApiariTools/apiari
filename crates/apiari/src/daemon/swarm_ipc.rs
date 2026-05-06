//! Thin wrappers around the embedded swarm daemon IPC.
//!
//! Instead of shelling out to the `swarm` CLI, apiari embeds the swarm daemon
//! as an in-process tokio task and communicates via its Unix socket. This
//! eliminates the two-daemon problem: one process, one lifecycle.

use apiari_swarm::client::{DaemonRequest, DaemonResponse, send_daemon_request};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;

/// Ensure the embedded swarm daemon is running. Safe to call repeatedly.
pub async fn ensure_swarm_running(work_dir: &Path) {
    if let Err(e) = apiari_swarm::daemon::lifecycle::ensure_daemon_running(work_dir).await {
        tracing::warn!("[swarm-embed] failed to start swarm daemon: {e}");
    } else {
        tracing::info!("[swarm-embed] daemon ready");
    }
}

/// Create a new swarm worker. Returns the assigned worktree_id (e.g. "apiari-a1b2").
pub async fn swarm_create(work_dir: &Path, repo: &str, prompt: &str) -> Result<String> {
    let req = DaemonRequest::CreateWorker {
        prompt: prompt.to_string(),
        agent: "codex".to_string(),
        repo: Some(repo.to_string()),
        start_point: None,
        workspace: Some(work_dir.to_path_buf()),
        profile: None,
        task_dir: None,
        role: None,
        review_pr: None,
        base_branch: None,
    };
    let work_dir = work_dir.to_path_buf();
    let resp = tokio::task::spawn_blocking(move || send_daemon_request(&work_dir, &req)).await??;
    match resp {
        DaemonResponse::Ok { data: Some(d) } => d["worktree_id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| eyre!("no worktree_id in swarm create response")),
        DaemonResponse::Error { message } => Err(eyre!("swarm create failed: {message}")),
        _ => Err(eyre!("unexpected response from swarm daemon")),
    }
}

/// Send a message to a running swarm worker.
pub async fn swarm_send(work_dir: &Path, worktree_id: &str, message: &str) -> Result<()> {
    let req = DaemonRequest::SendMessage {
        worktree_id: worktree_id.to_string(),
        message: message.to_string(),
    };
    let work_dir = work_dir.to_path_buf();
    let resp = tokio::task::spawn_blocking(move || send_daemon_request(&work_dir, &req)).await??;
    match resp {
        DaemonResponse::Ok { .. } => Ok(()),
        DaemonResponse::Error { message } => Err(eyre!("swarm send failed: {message}")),
        _ => Err(eyre!("unexpected response from swarm daemon")),
    }
}

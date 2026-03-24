//! Thin async wrapper around `apiari_swarm::daemon::ipc_client::send_daemon_request`.
//!
//! All daemon calls are synchronous (Unix socket I/O), so we use
//! `tokio::task::spawn_blocking` to avoid blocking the async runtime.

use std::path::{Path, PathBuf};

use apiari_swarm::daemon::ipc_client::send_daemon_request;
use apiari_swarm::daemon::protocol::{DaemonRequest, DaemonResponse, WorkerInfo};
use color_eyre::eyre::{Result, bail};

/// Async client for the swarm daemon.
pub struct SwarmClient {
    work_dir: PathBuf,
}

impl SwarmClient {
    pub fn new(work_dir: PathBuf) -> Self {
        Self { work_dir }
    }

    /// Send a request to the daemon, offloading blocking I/O to a thread.
    async fn request(&self, req: DaemonRequest) -> Result<DaemonResponse> {
        let dir = self.work_dir.clone();
        tokio::task::spawn_blocking(move || send_daemon_request(&dir, &req)).await?
    }

    /// Create a new worker. Returns the worktree ID.
    pub async fn create_worker(&self, repo: &str, prompt: &str, agent: &str) -> Result<String> {
        let resp = self
            .request(DaemonRequest::CreateWorker {
                prompt: prompt.to_string(),
                agent: agent.to_string(),
                repo: Some(repo.to_string()),
                start_point: None,
                workspace: Some(self.work_dir.clone()),
                profile: None,
                task_dir: None,
            })
            .await?;

        match resp {
            DaemonResponse::Ok { data } => {
                // The daemon returns the worktree ID in the data field.
                let id = data
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                Ok(id)
            }
            DaemonResponse::Error { message } => bail!("create_worker failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Send a follow-up message to a running worker.
    pub async fn send_message(&self, worktree_id: &str, message: &str) -> Result<()> {
        let resp = self
            .request(DaemonRequest::SendMessage {
                worktree_id: worktree_id.to_string(),
                message: message.to_string(),
            })
            .await?;

        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => bail!("send_message failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Close (tear down) a worker.
    pub async fn close_worker(&self, worktree_id: &str) -> Result<()> {
        let resp = self
            .request(DaemonRequest::CloseWorker {
                worktree_id: worktree_id.to_string(),
            })
            .await?;

        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => bail!("close_worker failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// List all workers in this workspace.
    pub async fn list_workers(&self) -> Result<Vec<WorkerInfo>> {
        let resp = self
            .request(DaemonRequest::ListWorkers {
                workspace: Some(self.work_dir.clone()),
            })
            .await?;

        match resp {
            DaemonResponse::Workers { workers } => Ok(workers),
            DaemonResponse::Error { message } => bail!("list_workers failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Health check — returns true if the daemon responds to ping.
    pub async fn ping(&self) -> bool {
        self.request(DaemonRequest::Ping).await.is_ok()
    }

    /// Ping the swarm daemon synchronously (for use in non-async contexts).
    pub fn ping_sync(work_dir: &Path) -> bool {
        send_daemon_request(work_dir, &DaemonRequest::Ping).is_ok()
    }
}

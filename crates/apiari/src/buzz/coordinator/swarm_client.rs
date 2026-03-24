//! SwarmClient — async wrapper around the swarm daemon's Unix socket IPC.
//!
//! Uses `apiari_swarm::send_daemon_request` (sync) wrapped in
//! `tokio::task::spawn_blocking` for async compatibility.

use std::path::PathBuf;

use apiari_swarm::{DaemonRequest, DaemonResponse, WorkerInfo, WorkerPhase};
use color_eyre::eyre::{Result, eyre};

/// Async client for the swarm daemon's Unix socket IPC.
pub struct SwarmClient {
    work_dir: PathBuf,
}

impl SwarmClient {
    pub fn new(work_dir: PathBuf) -> Self {
        Self { work_dir }
    }

    /// Send a request to the daemon, returning the raw response.
    async fn send(&self, req: DaemonRequest) -> Result<DaemonResponse> {
        let work_dir = self.work_dir.clone();
        tokio::task::spawn_blocking(move || apiari_swarm::send_daemon_request(&work_dir, &req))
            .await?
    }

    /// Ping the daemon to check if it's running.
    pub async fn ping(&self) -> Result<()> {
        let resp = self.send(DaemonRequest::Ping).await?;
        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => Err(eyre!("daemon error: {message}")),
            _ => Ok(()),
        }
    }

    /// Dispatch a new worker. Returns the worktree ID on success.
    pub async fn create_worker(
        &self,
        prompt: &str,
        agent: &str,
        repo: Option<&str>,
    ) -> Result<String> {
        let resp = self
            .send(DaemonRequest::CreateWorker {
                prompt: prompt.to_string(),
                agent: agent.to_string(),
                repo: repo.map(|s| s.to_string()),
                start_point: None,
                workspace: Some(self.work_dir.clone()),
                profile: None,
                task_dir: None,
            })
            .await?;
        match resp {
            DaemonResponse::Ok { data } => {
                // The daemon returns the worktree ID in the data field
                let id = data
                    .and_then(|v| {
                        v.get("worktree_id")
                            .and_then(|id| id.as_str().map(String::from))
                    })
                    .unwrap_or_default();
                Ok(id)
            }
            DaemonResponse::Error { message } => Err(eyre!("create_worker failed: {message}")),
            other => Err(eyre!("unexpected response: {other:?}")),
        }
    }

    /// Send a message to a waiting worker.
    pub async fn send_message(&self, worktree_id: &str, message: &str) -> Result<()> {
        let resp = self
            .send(DaemonRequest::SendMessage {
                worktree_id: worktree_id.to_string(),
                message: message.to_string(),
            })
            .await?;
        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => Err(eyre!("send_message failed: {message}")),
            other => Err(eyre!("unexpected response: {other:?}")),
        }
    }

    /// Close a worker.
    pub async fn close_worker(&self, worktree_id: &str) -> Result<()> {
        let resp = self
            .send(DaemonRequest::CloseWorker {
                worktree_id: worktree_id.to_string(),
            })
            .await?;
        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => Err(eyre!("close_worker failed: {message}")),
            other => Err(eyre!("unexpected response: {other:?}")),
        }
    }

    /// List workers, optionally filtered to this workspace.
    pub async fn list_workers(&self) -> Result<Vec<WorkerInfo>> {
        let resp = self
            .send(DaemonRequest::ListWorkers {
                workspace: Some(self.work_dir.clone()),
            })
            .await?;
        match resp {
            DaemonResponse::Workers { workers } => Ok(workers),
            DaemonResponse::Error { message } => Err(eyre!("list_workers failed: {message}")),
            other => Err(eyre!("unexpected response: {other:?}")),
        }
    }

    /// Subscribe to state changes. Returns a blocking reader that yields events.
    ///
    /// This opens a persistent connection to the daemon socket.
    /// Each line read from the connection is a `DaemonResponse` — either
    /// `StateChanged` or `AgentEvent`. The connection stays open until the
    /// daemon disconnects or the reader is dropped.
    pub fn subscribe_blocking(
        &self,
    ) -> Result<impl Iterator<Item = Result<DaemonResponse>> + Send> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let local = apiari_swarm::socket_path(&self.work_dir);
        let sock = if local.exists() {
            &local
        } else {
            &apiari_swarm::global_socket_path()
        };

        let stream =
            UnixStream::connect(sock).map_err(|e| eyre!("subscribe connect failed: {e}"))?;
        // No read timeout for subscriptions — they're long-lived.
        stream.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;

        let req = DaemonRequest::Subscribe {
            worktree_id: None,
            workspace: Some(self.work_dir.clone()),
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        // Write the subscribe request, then drop the writer before creating reader
        {
            let mut writer = std::io::BufWriter::new(&stream);
            writer.write_all(line.as_bytes())?;
            writer.flush()?;
        }

        let reader = BufReader::new(stream);
        Ok(reader.lines().map(|line_result| {
            let line = line_result.map_err(|e| eyre!("subscribe read error: {e}"))?;
            let resp: DaemonResponse = serde_json::from_str(line.trim())
                .map_err(|e| eyre!("subscribe parse error: {e}"))?;
            Ok(resp)
        }))
    }
}

/// Extract phase from a WorkerInfo for signal emission.
pub fn worker_has_pr(worker: &WorkerInfo) -> bool {
    worker.pr_url.is_some()
}

/// Check if a phase represents a "waiting" state.
pub fn is_waiting(phase: &WorkerPhase) -> bool {
    *phase == WorkerPhase::Waiting
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swarm_client_new() {
        let client = SwarmClient::new(PathBuf::from("/tmp/test"));
        assert_eq!(client.work_dir, PathBuf::from("/tmp/test"));
    }

    #[test]
    fn test_worker_has_pr() {
        let worker = WorkerInfo {
            id: "test-1".into(),
            branch: "swarm/test".into(),
            prompt: "test".into(),
            agent: "claude".into(),
            phase: WorkerPhase::Running,
            session_id: None,
            pr_url: Some("https://github.com/org/repo/pull/1".into()),
            pr_number: Some(1),
            pr_title: Some("Test PR".into()),
            pr_state: Some("OPEN".into()),
            restart_count: 0,
            created_at: None,
        };
        assert!(worker_has_pr(&worker));

        let worker_no_pr = WorkerInfo {
            pr_url: None,
            pr_number: None,
            pr_title: None,
            pr_state: None,
            ..worker
        };
        assert!(!worker_has_pr(&worker_no_pr));
    }
}

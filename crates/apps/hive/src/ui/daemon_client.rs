//! Socket client for connecting `hive ui` to the daemon.
//!
//! Connects to `.hive/daemon.sock` and communicates via JSONL.

use crate::daemon::tui_socket::{TuiRequest, TuiResponse};
use color_eyre::Result;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Client connection to the daemon's TUI socket.
pub struct DaemonClient {
    writer: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
}

impl DaemonClient {
    /// Connect to the daemon socket at `{workspace_root}/.hive/daemon.sock`.
    pub async fn connect(workspace_root: &Path) -> Result<Self> {
        let socket_path = workspace_root.join(".hive/daemon.sock");
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("failed to connect to daemon socket: {e}"))?;

        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            writer: write_half,
            reader: BufReader::new(read_half),
        })
    }

    /// Send a message to the coordinator.
    pub async fn send_message(&mut self, text: &str) -> Result<()> {
        let req = TuiRequest::Message {
            text: text.to_owned(),
        };
        self.send_request(&req).await
    }

    /// Send a dispatch request.
    #[allow(dead_code)]
    pub async fn send_dispatch(&mut self, repo: &str, description: &str) -> Result<()> {
        let req = TuiRequest::Dispatch {
            repo: repo.to_owned(),
            description: description.to_owned(),
        };
        self.send_request(&req).await
    }

    /// Submit a multi-repo dispatch to the coordinator.
    pub async fn send_submit_dispatch(&mut self, repos: &[String], task: &str) -> Result<()> {
        let req = TuiRequest::SubmitDispatch {
            repos: repos.to_vec(),
            task: task.to_owned(),
        };
        self.send_request(&req).await
    }

    /// Confirm the pending dispatch.
    pub async fn send_confirm_dispatch(&mut self) -> Result<()> {
        self.send_request(&TuiRequest::ConfirmDispatch).await
    }

    /// Cancel the pending dispatch.
    pub async fn send_cancel_dispatch(&mut self) -> Result<()> {
        self.send_request(&TuiRequest::CancelDispatch).await
    }

    /// Request current dispatch state from the daemon.
    pub async fn send_get_dispatch_state(&mut self) -> Result<()> {
        self.send_request(&TuiRequest::GetDispatchState).await
    }

    /// Read the next response from the daemon.
    ///
    /// Returns `None` if the connection is closed.
    pub async fn next_response(&mut self) -> Result<Option<TuiResponse>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let resp: TuiResponse = serde_json::from_str(line.trim())
            .map_err(|e| color_eyre::eyre::eyre!("invalid response from daemon: {e}"))?;
        Ok(Some(resp))
    }

    /// Send a raw request.
    async fn send_request(&mut self, req: &TuiRequest) -> Result<()> {
        let mut json = serde_json::to_string(req)?;
        json.push('\n');
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

/// Check if the daemon socket exists.
pub fn daemon_sock_exists(workspace_root: &Path) -> bool {
    workspace_root.join(".hive/daemon.sock").exists()
}

/// Try to connect to the daemon socket (non-blocking probe).
pub async fn can_connect(workspace_root: &Path) -> bool {
    DaemonClient::connect(workspace_root).await.is_ok()
}

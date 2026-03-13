//! TUI-side daemon client — connects to daemon Unix socket.

use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::daemon::socket::{DaemonRequest, DaemonResponse};

pub struct DaemonClient {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl DaemonClient {
    /// Connect to the daemon socket.
    pub async fn connect(socket_path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (reader, writer) = stream.into_split();
        let lines = BufReader::new(reader).lines();
        Ok(Self { lines, writer })
    }

    /// Send a chat message to the daemon.
    pub async fn send_chat(&mut self, workspace: &str, text: &str) -> std::io::Result<()> {
        let req = DaemonRequest::Chat {
            workspace: workspace.to_string(),
            text: text.to_string(),
        };
        let json = serde_json::to_string(&req).map_err(std::io::Error::other)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await
    }

    /// Read the next response from the daemon.
    pub async fn next_response(&mut self) -> std::io::Result<Option<DaemonResponse>> {
        match self.lines.next_line().await? {
            Some(line) => {
                let resp: DaemonResponse =
                    serde_json::from_str(&line).map_err(std::io::Error::other)?;
                Ok(Some(resp))
            }
            None => Ok(None),
        }
    }
}

/// Check if the daemon socket file exists.
pub fn socket_exists() -> bool {
    crate::config::socket_path().exists()
}

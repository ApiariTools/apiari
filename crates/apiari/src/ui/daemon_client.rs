//! TUI-side daemon client — connects to daemon via Unix socket or TCP.

use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::{TcpStream, UnixStream};
use tracing::info;

use crate::daemon::socket::{DaemonRequest, DaemonResponse};

/// Transport-agnostic line reader + writer.
enum Transport {
    Unix {
        lines: Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
        writer: tokio::net::unix::OwnedWriteHalf,
    },
    Tcp {
        lines: Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
        writer: tokio::net::tcp::OwnedWriteHalf,
    },
}

pub struct DaemonClient {
    transport: Transport,
    /// Whether this client is connected via TCP (remote).
    #[allow(dead_code)]
    pub is_remote: bool,
}

impl DaemonClient {
    /// Connect to the daemon via Unix socket.
    pub async fn connect(socket_path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (reader, writer) = stream.into_split();
        let lines = BufReader::new(reader).lines();
        info!("[tui] connected to daemon via Unix socket");
        Ok(Self {
            transport: Transport::Unix { lines, writer },
            is_remote: false,
        })
    }

    /// Connect to a remote daemon via TCP with a 2-second timeout.
    pub async fn connect_tcp(host: &str, port: u16) -> std::io::Result<Self> {
        let addr = format!("{host}:{port}");
        let stream =
            tokio::time::timeout(std::time::Duration::from_secs(2), TcpStream::connect(&addr))
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "TCP connect timed out")
                })??;
        let (reader, writer) = stream.into_split();
        let lines = BufReader::new(reader).lines();
        info!("[tui] connected to daemon via TCP at {addr}");
        Ok(Self {
            transport: Transport::Tcp { lines, writer },
            is_remote: true,
        })
    }

    /// Send a chat message to the daemon.
    pub async fn send_chat(&mut self, workspace: &str, text: &str) -> std::io::Result<()> {
        let req = DaemonRequest::Chat {
            workspace: workspace.to_string(),
            text: text.to_string(),
        };
        let json = serde_json::to_string(&req).map_err(std::io::Error::other)?;
        match &mut self.transport {
            Transport::Unix { writer, .. } => {
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            }
            Transport::Tcp { writer, .. } => {
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            }
        }
    }

    /// Read the next response from the daemon.
    pub async fn next_response(&mut self) -> std::io::Result<Option<DaemonResponse>> {
        let line = match &mut self.transport {
            Transport::Unix { lines, .. } => lines.next_line().await?,
            Transport::Tcp { lines, .. } => lines.next_line().await?,
        };
        match line {
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

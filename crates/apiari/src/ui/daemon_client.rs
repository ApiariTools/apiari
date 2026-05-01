//! TUI-side daemon client — connects to daemon via Unix socket or TCP.

use std::path::Path;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{TcpStream, UnixStream},
};
use tracing::{debug, info};

use crate::{
    config::DaemonEndpoint,
    daemon::socket::{DaemonRequest, DaemonResponse},
};

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
    /// The host we connected to (for status bar display).
    pub connected_host: Option<String>,
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
            connected_host: None,
        })
    }

    /// Try connecting to each endpoint in order with a 500ms timeout per endpoint.
    /// Returns the first successful connection.
    pub async fn connect_tcp_fallback(endpoints: &[DaemonEndpoint]) -> std::io::Result<Self> {
        let mut last_err = std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "no daemon endpoints configured",
        );
        for ep in endpoints {
            debug!("[ui] trying endpoint {}:{}...", ep.host, ep.port);
            let addr = format!("{}:{}", ep.host, ep.port);
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                TcpStream::connect(&addr),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    let (reader, writer) = stream.into_split();
                    let lines = BufReader::new(reader).lines();
                    info!("[ui] connected to {}:{}", ep.host, ep.port);
                    return Ok(Self {
                        transport: Transport::Tcp { lines, writer },
                        is_remote: true,
                        connected_host: Some(ep.host.clone()),
                    });
                }
                Ok(Err(e)) => {
                    debug!("[ui] endpoint {}:{} failed: {e}", ep.host, ep.port);
                    last_err = e;
                }
                Err(_) => {
                    debug!("[ui] endpoint {}:{} timed out", ep.host, ep.port);
                    last_err = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("{}:{} timed out", ep.host, ep.port),
                    );
                }
            }
        }
        Err(last_err)
    }

    /// Send a chat message to the daemon, optionally targeting a specific bee.
    pub async fn send_chat(
        &mut self,
        workspace: &str,
        text: &str,
        bee: Option<&str>,
    ) -> std::io::Result<()> {
        let req = DaemonRequest::Chat {
            workspace: workspace.to_string(),
            text: text.to_string(),
            bee: bee.map(|s| s.to_string()),
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

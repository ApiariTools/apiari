//! Unix socket IPC between daemon and TUI clients.
//!
//! Protocol: JSONL over `~/.config/apiari/daemon.sock`.
//!
//! Two channels per client:
//! - Per-client `mpsc::unbounded_channel` for unicast (Token/Done/Error for the requesting client)
//! - `tokio::sync::broadcast` for Activity events pushed to ALL connected clients

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

// ── Protocol types ──────────────────────────────────────

/// TUI → Daemon request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonRequest {
    Chat { workspace: String, text: String },
}

/// Daemon → TUI response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonResponse {
    /// Unicast: streaming token for YOUR request.
    Token { text: String },
    /// Unicast: your request completed.
    Done,
    /// Unicast: error on your request.
    Error { text: String },
    /// Broadcast: activity from any source.
    Activity {
        source: String,
        workspace: String,
        kind: String,
        text: String,
    },
}

/// A request from a connected client, tagged with a responder channel.
pub struct ClientRequest {
    pub request: DaemonRequest,
    pub responder: mpsc::UnboundedSender<DaemonResponse>,
}

// ── Server ──────────────────────────────────────────────

pub struct DaemonSocketServer {
    socket_path: PathBuf,
    activity_tx: broadcast::Sender<DaemonResponse>,
}

impl DaemonSocketServer {
    /// Start the socket server, returning a receiver for client requests.
    pub fn start(
        socket_path: &Path,
    ) -> std::io::Result<(mpsc::UnboundedReceiver<ClientRequest>, Arc<Self>)> {
        // Clean up stale socket
        let _ = std::fs::remove_file(socket_path);
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        let (req_tx, req_rx) = mpsc::unbounded_channel::<ClientRequest>();
        let (activity_tx, _) = broadcast::channel::<DaemonResponse>(256);

        let server = Arc::new(Self {
            socket_path: socket_path.to_path_buf(),
            activity_tx: activity_tx.clone(),
        });

        let server_clone = server.clone();
        let req_tx_clone = req_tx.clone();
        tokio::spawn(async move {
            server_clone.accept_loop(listener, req_tx_clone).await;
        });

        info!("socket server listening on {}", socket_path.display());
        Ok((req_rx, server))
    }

    /// Accept loop — spawns a task per client.
    async fn accept_loop(
        &self,
        listener: UnixListener,
        req_tx: mpsc::UnboundedSender<ClientRequest>,
    ) {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    info!("TUI client connected");
                    let req_tx = req_tx.clone();
                    let activity_rx = self.activity_tx.subscribe();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, req_tx, activity_rx).await {
                            warn!("TUI client disconnected: {e}");
                        } else {
                            info!("TUI client disconnected");
                        }
                    });
                }
                Err(e) => {
                    error!("socket accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Broadcast an activity event to all connected TUI clients.
    pub fn broadcast_activity(
        &self,
        source: &str,
        workspace: &str,
        kind: &str,
        text: &str,
    ) {
        let msg = DaemonResponse::Activity {
            source: source.to_string(),
            workspace: workspace.to_string(),
            kind: kind.to_string(),
            text: text.to_string(),
        };
        // Ignore error — no subscribers means no TUI clients connected
        let _ = self.activity_tx.send(msg);
    }

    /// Check if any TUI clients are connected.
    pub fn has_clients(&self) -> bool {
        self.activity_tx.receiver_count() > 0
    }
}

impl Drop for DaemonSocketServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Handle a single TUI client connection.
async fn handle_client(
    stream: UnixStream,
    req_tx: mpsc::UnboundedSender<ClientRequest>,
    mut activity_rx: broadcast::Receiver<DaemonResponse>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Per-client unicast channel (daemon sends Token/Done/Error here)
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<DaemonResponse>();

    loop {
        tokio::select! {
            // Read requests from client
            line = lines.next_line() => {
                match line? {
                    Some(line) => {
                        match serde_json::from_str::<DaemonRequest>(&line) {
                            Ok(request) => {
                                let client_req = ClientRequest {
                                    request,
                                    responder: resp_tx.clone(),
                                };
                                if req_tx.send(client_req).is_err() {
                                    break; // daemon shutting down
                                }
                            }
                            Err(e) => {
                                let err = DaemonResponse::Error {
                                    text: format!("invalid request: {e}"),
                                };
                                let json = serde_json::to_string(&err).unwrap();
                                writer.write_all(json.as_bytes()).await?;
                                writer.write_all(b"\n").await?;
                                writer.flush().await?;
                            }
                        }
                    }
                    None => break, // client disconnected
                }
            }

            // Forward unicast responses to client
            Some(resp) = resp_rx.recv() => {
                let json = serde_json::to_string(&resp).unwrap();
                if writer.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }

            // Forward broadcast activity events to client
            Ok(activity) = activity_rx.recv() => {
                let json = serde_json::to_string(&activity).unwrap();
                if writer.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_request_serde() {
        let req = DaemonRequest::Chat {
            workspace: "apiari".into(),
            text: "hello".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"chat\""));
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonRequest::Chat { workspace, text } => {
                assert_eq!(workspace, "apiari");
                assert_eq!(text, "hello");
            }
        }
    }

    #[test]
    fn test_daemon_response_token_serde() {
        let resp = DaemonResponse::Token {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"token\""));
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DaemonResponse::Token { text } if text == "hello"));
    }

    #[test]
    fn test_daemon_response_done_serde() {
        let resp = DaemonResponse::Done;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"done\""));
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DaemonResponse::Done));
    }

    #[test]
    fn test_daemon_response_error_serde() {
        let resp = DaemonResponse::Error {
            text: "oops".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DaemonResponse::Error { text } if text == "oops"));
    }

    #[test]
    fn test_daemon_response_activity_serde() {
        let resp = DaemonResponse::Activity {
            source: "telegram".into(),
            workspace: "apiari".into(),
            kind: "user_message".into(),
            text: "check PR status".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"activity\""));
        assert!(json.contains("\"source\":\"telegram\""));
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonResponse::Activity {
                source,
                workspace,
                kind,
                text,
            } => {
                assert_eq!(source, "telegram");
                assert_eq!(workspace, "apiari");
                assert_eq!(kind, "user_message");
                assert_eq!(text, "check PR status");
            }
            _ => panic!("expected Activity"),
        }
    }
}

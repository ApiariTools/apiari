//! Unix socket IPC between daemon and TUI clients.
//!
//! Protocol: JSONL over `~/.config/apiari/daemon.sock`.
//!
//! Two channels per client:
//! - Per-client `mpsc::unbounded_channel` for unicast (Token/Done/Error for the requesting client)
//! - `tokio::sync::broadcast` for Activity events pushed to ALL connected clients

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixListener},
    sync::{broadcast, mpsc},
};
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
    Token {
        #[serde(default)]
        workspace: String,
        text: String,
    },
    /// Unicast: your request completed.
    Done {
        #[serde(default)]
        workspace: String,
    },
    /// Unicast: error on your request.
    Error {
        #[serde(default)]
        workspace: String,
        text: String,
    },
    /// Broadcast: activity from any source.
    Activity {
        source: String,
        workspace: String,
        kind: String,
        text: String,
    },
    /// Unicast: token usage stats for the completed turn.
    Usage {
        #[serde(default)]
        workspace: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        total_cost_usd: Option<f64>,
        context_window: u64,
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
    ///
    /// Also returns the request sender so callers can pass it to `start_tcp()`.
    pub fn start(
        socket_path: &Path,
    ) -> std::io::Result<(
        mpsc::UnboundedReceiver<ClientRequest>,
        mpsc::UnboundedSender<ClientRequest>,
        Arc<Self>,
    )> {
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

        info!(
            "[daemon] Unix socket listening on {}",
            socket_path.display()
        );
        Ok((req_rx, req_tx, server))
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
                    info!("TUI client connected (unix)");
                    let req_tx = req_tx.clone();
                    let activity_rx = self.activity_tx.subscribe();
                    tokio::spawn(async move {
                        let (reader, writer) = stream.into_split();
                        if let Err(e) = handle_client(reader, writer, req_tx, activity_rx).await {
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

    /// Start a TCP listener on the given port, reusing the same protocol and request channel.
    ///
    /// `bind_addr` defaults to `127.0.0.1` (loopback only) for safety.
    /// Set to `0.0.0.0` to listen on all interfaces.
    /// Each TCP client gets the same per-client handler as Unix socket clients.
    pub fn start_tcp(
        self: &Arc<Self>,
        port: u16,
        bind_addr: &str,
        req_tx: mpsc::UnboundedSender<ClientRequest>,
    ) -> std::io::Result<()> {
        let addr: std::net::SocketAddr = format!("{bind_addr}:{port}")
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(listener)?;

        let server = self.clone();
        tokio::spawn(async move {
            server.accept_tcp_loop(listener, req_tx).await;
        });

        info!("[daemon] TCP listener bound on {bind_addr}:{port}");
        Ok(())
    }

    /// Accept loop for TCP clients.
    async fn accept_tcp_loop(
        &self,
        listener: TcpListener,
        req_tx: mpsc::UnboundedSender<ClientRequest>,
    ) {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("TUI client connected (tcp: {addr})");
                    let req_tx = req_tx.clone();
                    let activity_rx = self.activity_tx.subscribe();
                    tokio::spawn(async move {
                        let (reader, writer) = stream.into_split();
                        if let Err(e) = handle_client(reader, writer, req_tx, activity_rx).await {
                            warn!("TUI TCP client disconnected ({addr}): {e}");
                        } else {
                            info!("TUI TCP client disconnected ({addr})");
                        }
                    });
                }
                Err(e) => {
                    error!("TCP accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Broadcast an activity event to all connected TUI clients.
    /// Returns the number of receivers that got the message (0 = nobody listening).
    pub fn broadcast_activity(
        &self,
        source: &str,
        workspace: &str,
        kind: &str,
        text: &str,
    ) -> usize {
        let msg = DaemonResponse::Activity {
            source: source.to_string(),
            workspace: workspace.to_string(),
            kind: kind.to_string(),
            text: text.to_string(),
        };
        // send() returns Ok(num_receivers) or Err if zero subscribers
        self.activity_tx.send(msg).unwrap_or(0)
    }

    /// Check if any TUI clients are connected.
    #[allow(dead_code)]
    pub fn has_clients(&self) -> bool {
        self.activity_tx.receiver_count() > 0
    }
}

impl Drop for DaemonSocketServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Handle a single TUI client connection (Unix or TCP).
async fn handle_client<R, W>(
    reader: R,
    mut writer: W,
    req_tx: mpsc::UnboundedSender<ClientRequest>,
    mut activity_rx: broadcast::Receiver<DaemonResponse>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
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
                                    workspace: String::new(),
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
            workspace: "ws1".into(),
            text: "hello".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"token\""));
        assert!(json.contains("\"workspace\":\"ws1\""));
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(parsed, DaemonResponse::Token { workspace, text } if workspace == "ws1" && text == "hello")
        );
    }

    #[test]
    fn test_daemon_response_done_serde() {
        let resp = DaemonResponse::Done {
            workspace: "ws1".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"done\""));
        assert!(json.contains("\"workspace\":\"ws1\""));
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DaemonResponse::Done { workspace } if workspace == "ws1"));
    }

    #[test]
    fn test_daemon_response_error_serde() {
        let resp = DaemonResponse::Error {
            workspace: "ws1".into(),
            text: "oops".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(parsed, DaemonResponse::Error { workspace, text } if workspace == "ws1" && text == "oops")
        );
    }

    #[tokio::test]
    async fn test_handle_client_request_response() {
        // Simulate a client connection with in-memory pipes
        let (client_reader, mut server_writer) = tokio::io::duplex(1024);
        let (mut server_reader, client_writer) = tokio::io::duplex(1024);

        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<ClientRequest>();
        let (_activity_tx, activity_rx) = broadcast::channel::<DaemonResponse>(16);

        // Spawn the client handler
        let handle = tokio::spawn(async move {
            handle_client(client_reader, client_writer, req_tx, activity_rx).await
        });

        // Write a chat request from the "client" side
        let req = DaemonRequest::Chat {
            workspace: "test".into(),
            text: "hello".into(),
        };
        let mut json = serde_json::to_string(&req).unwrap();
        json.push('\n');
        tokio::io::AsyncWriteExt::write_all(&mut server_writer, json.as_bytes())
            .await
            .unwrap();

        // Read it from the request channel
        let client_req = req_rx.recv().await.unwrap();
        match &client_req.request {
            DaemonRequest::Chat { workspace, text } => {
                assert_eq!(workspace, "test");
                assert_eq!(text, "hello");
            }
        }

        // Send a response back via the responder
        client_req
            .responder
            .send(DaemonResponse::Token {
                workspace: "test".into(),
                text: "world".into(),
            })
            .unwrap();

        // Read the response from the "client" side
        let mut buf = String::new();
        let mut reader = tokio::io::BufReader::new(&mut server_reader);
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut buf)
            .await
            .unwrap();
        let resp: DaemonResponse = serde_json::from_str(buf.trim()).unwrap();
        assert!(
            matches!(resp, DaemonResponse::Token { workspace, text } if workspace == "test" && text == "world")
        );

        // Drop the writer to close the connection
        drop(server_writer);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_handle_client_invalid_json() {
        let (client_reader, mut server_writer) = tokio::io::duplex(1024);
        let (mut server_reader, client_writer) = tokio::io::duplex(1024);

        let (req_tx, _req_rx) = mpsc::unbounded_channel::<ClientRequest>();
        let (_activity_tx, activity_rx) = broadcast::channel::<DaemonResponse>(16);

        let handle = tokio::spawn(async move {
            handle_client(client_reader, client_writer, req_tx, activity_rx).await
        });

        // Send invalid JSON
        tokio::io::AsyncWriteExt::write_all(&mut server_writer, b"not json\n")
            .await
            .unwrap();

        // Should get an error response back
        let mut buf = String::new();
        let mut reader = tokio::io::BufReader::new(&mut server_reader);
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut buf)
            .await
            .unwrap();
        let resp: DaemonResponse = serde_json::from_str(buf.trim()).unwrap();
        match resp {
            DaemonResponse::Error { text, .. } => {
                assert!(text.contains("invalid request"));
            }
            _ => panic!("expected Error response"),
        }

        drop(server_writer);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_handle_client_broadcast_activity() {
        let (client_reader, server_writer) = tokio::io::duplex(1024);
        let (mut server_reader, client_writer) = tokio::io::duplex(1024);

        let (req_tx, _req_rx) = mpsc::unbounded_channel::<ClientRequest>();
        let (activity_tx, activity_rx) = broadcast::channel::<DaemonResponse>(16);

        let handle = tokio::spawn(async move {
            handle_client(client_reader, client_writer, req_tx, activity_rx).await
        });

        // Broadcast an activity event
        let _ = activity_tx.send(DaemonResponse::Activity {
            source: "telegram".into(),
            workspace: "ws".into(),
            kind: "user_message".into(),
            text: "hello from tg".into(),
        });

        // Read it from the client side
        let mut buf = String::new();
        let mut reader = tokio::io::BufReader::new(&mut server_reader);
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut buf)
            .await
            .unwrap();
        let resp: DaemonResponse = serde_json::from_str(buf.trim()).unwrap();
        match resp {
            DaemonResponse::Activity {
                source, workspace, ..
            } => {
                assert_eq!(source, "telegram");
                assert_eq!(workspace, "ws");
            }
            _ => panic!("expected Activity"),
        }

        drop(server_writer);
        let _ = handle.await;
    }

    #[test]
    fn test_daemon_response_backward_compat_no_workspace() {
        // Old daemons may send Token/Done/Error without a workspace field.
        // Verify #[serde(default)] allows deserialization with workspace = "".
        let token_json = r#"{"type":"token","text":"hi"}"#;
        let parsed: DaemonResponse = serde_json::from_str(token_json).unwrap();
        assert!(
            matches!(parsed, DaemonResponse::Token { workspace, text } if workspace.is_empty() && text == "hi")
        );

        let done_json = r#"{"type":"done"}"#;
        let parsed: DaemonResponse = serde_json::from_str(done_json).unwrap();
        assert!(matches!(parsed, DaemonResponse::Done { workspace } if workspace.is_empty()));

        let error_json = r#"{"type":"error","text":"fail"}"#;
        let parsed: DaemonResponse = serde_json::from_str(error_json).unwrap();
        assert!(
            matches!(parsed, DaemonResponse::Error { workspace, text } if workspace.is_empty() && text == "fail")
        );
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

    #[test]
    fn test_daemon_response_usage_serde() {
        let resp = DaemonResponse::Usage {
            workspace: "apiari".into(),
            input_tokens: 1500,
            output_tokens: 300,
            cache_read_tokens: 800,
            total_cost_usd: Some(0.042),
            context_window: 200_000,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"usage\""));
        assert!(json.contains("\"input_tokens\":1500"));
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonResponse::Usage {
                workspace,
                input_tokens,
                output_tokens,
                context_window,
                ..
            } => {
                assert_eq!(workspace, "apiari");
                assert_eq!(input_tokens, 1500);
                assert_eq!(output_tokens, 300);
                assert_eq!(context_window, 200_000);
            }
            _ => panic!("expected Usage"),
        }
    }
}

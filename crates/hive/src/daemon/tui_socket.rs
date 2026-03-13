//! Unix socket server for TUI ↔ daemon IPC.
//!
//! The daemon listens on `.hive/daemon.sock` for connections from `hive ui`.
//! Messages are JSONL (one JSON object per line).
//!
//! Protocol:
//! - Client → Server: `TuiRequest` (message, dispatch)
//! - Server → Client: `TuiResponse` (token, done, error, notification)

use crate::ui::inbox::UiEvent;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Messages from TUI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TuiRequest {
    /// Send a message to the coordinator.
    Message { text: String },
    /// Dispatch a task to a swarm worker.
    Dispatch { repo: String, description: String },
    /// Submit a multi-repo dispatch (coordinator decomposes into per-repo prompts).
    SubmitDispatch { repos: Vec<String>, task: String },
    /// Confirm the pending dispatch — daemon executes swarm creates.
    ConfirmDispatch,
    /// Cancel the pending dispatch.
    CancelDispatch,
    /// Request current dispatch state (sent on connect for sync).
    GetDispatchState,
}

/// Per-repo pipeline artifacts sent to the TUI for review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoDispatchInfo {
    pub repo: String,
    /// Pipeline stage: "pending" | "context" | "planning" | "done"
    #[serde(default = "default_stage")]
    pub stage: String,
    pub context_md: Option<String>,
    pub plan_md: Option<String>,
    #[serde(default)]
    pub file_count: Option<usize>,
    #[serde(default)]
    pub step_count: Option<usize>,
}

fn default_stage() -> String {
    "pending".into()
}

/// Messages from daemon to TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TuiResponse {
    /// Streaming text chunk from coordinator.
    Token { text: String },
    /// Coordinator finished responding.
    Done,
    /// Error from coordinator or daemon.
    Error { text: String },
    /// Swarm/buzz notification pushed to TUI.
    Notification { event: UiEvent },
    /// Current dispatch state snapshot (sent on connect + on state changes).
    DispatchUpdate {
        task: String,
        repos: Vec<String>,
        status: String,
        streaming_text: String,
        task_md: String,
        title: String,
        per_repo: Vec<RepoDispatchInfo>,
    },
    /// Dispatch was executed or cancelled — no longer active.
    DispatchCleared,
}

/// A handle to a connected TUI client's write channel.
pub type ResponseSender = mpsc::UnboundedSender<TuiResponse>;

/// Incoming request paired with the sender to respond on.
pub struct TuiClientRequest {
    pub request: TuiRequest,
    pub responder: ResponseSender,
}

/// Handle to the socket server — dropping it cleans up the socket file.
pub struct TuiSocketServer {
    socket_path: PathBuf,
    _listener_handle: JoinHandle<()>,
    /// Broadcast channel for pushing notifications to all connected TUI clients.
    notify_tx: broadcast::Sender<TuiResponse>,
}

impl TuiSocketServer {
    /// Start listening on `{workspace_root}/.hive/daemon.sock`.
    ///
    /// Returns a receiver for incoming requests and the server handle.
    pub fn start(
        workspace_root: &Path,
    ) -> std::io::Result<(mpsc::UnboundedReceiver<TuiClientRequest>, Self)> {
        let socket_path = workspace_root.join(".hive/daemon.sock");

        // Remove stale socket file if it exists.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        // Ensure the directory exists.
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!(path = %socket_path.display(), "TUI socket listening");

        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (notify_tx, _) = broadcast::channel::<TuiResponse>(64);

        let notify_tx_clone = notify_tx.clone();
        let handle = tokio::spawn(accept_loop(listener, req_tx, notify_tx_clone));

        Ok((
            req_rx,
            Self {
                socket_path,
                _listener_handle: handle,
                notify_tx,
            },
        ))
    }

    /// Push a notification to all connected TUI clients.
    pub fn push_notification(&self, event: UiEvent) {
        // Ignore send errors — no subscribers is fine.
        let _ = self.notify_tx.send(TuiResponse::Notification { event });
    }

    /// Push an arbitrary response to all connected TUI clients.
    pub fn push_response(&self, response: TuiResponse) {
        let _ = self.notify_tx.send(response);
    }

    /// Check if any TUI clients are connected.
    pub fn has_clients(&self) -> bool {
        self.notify_tx.receiver_count() > 0
    }
}

impl Drop for TuiSocketServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        tracing::debug!("TUI socket cleaned up");
    }
}

/// Accept loop — spawns a handler task per connected client.
async fn accept_loop(
    listener: UnixListener,
    req_tx: mpsc::UnboundedSender<TuiClientRequest>,
    notify_tx: broadcast::Sender<TuiResponse>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tracing::debug!("TUI client connected");
                let req_tx = req_tx.clone();
                let notify_rx = notify_tx.subscribe();
                tokio::spawn(handle_client(stream, req_tx, notify_rx));
            }
            Err(e) => {
                tracing::error!(error = %e, "TUI socket accept error");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Handle a single TUI client connection.
///
/// Reads JSONL requests from the client, forwards them to the daemon via `req_tx`,
/// and writes responses + notifications back to the client.
async fn handle_client(
    stream: tokio::net::UnixStream,
    req_tx: mpsc::UnboundedSender<TuiClientRequest>,
    mut notify_rx: broadcast::Receiver<TuiResponse>,
) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;

    // Per-client response channel: the daemon sends responses here,
    // and we forward them to the socket.
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<TuiResponse>();

    let mut line = String::new();
    loop {
        tokio::select! {
            // Read requests from the client.
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        tracing::debug!("TUI client disconnected");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            match serde_json::from_str::<TuiRequest>(trimmed) {
                                Ok(request) => {
                                    if req_tx.send(TuiClientRequest {
                                        request,
                                        responder: resp_tx.clone(),
                                    }).is_err() {
                                        tracing::warn!("TUI request channel closed");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let err = TuiResponse::Error {
                                        text: format!("invalid request: {e}"),
                                    };
                                    if write_response(&mut writer, &err).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "TUI read error");
                        break;
                    }
                }
            }

            // Forward responses from the daemon to the client.
            Some(resp) = resp_rx.recv() => {
                if write_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }

            // Forward broadcast notifications to the client.
            Ok(notif) = notify_rx.recv() => {
                if write_response(&mut writer, &notif).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Write a single JSONL response to the client socket.
async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &TuiResponse,
) -> Result<(), std::io::Error> {
    let mut json = serde_json::to_string(response).map_err(std::io::Error::other)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserialize_message() {
        let json = r#"{"type":"message","text":"hello"}"#;
        let req: TuiRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TuiRequest::Message { text } if text == "hello"));
    }

    #[test]
    fn request_deserialize_dispatch() {
        let json = r#"{"type":"dispatch","repo":"hive","description":"fix bug"}"#;
        let req: TuiRequest = serde_json::from_str(json).unwrap();
        assert!(
            matches!(req, TuiRequest::Dispatch { repo, description } if repo == "hive" && description == "fix bug")
        );
    }

    #[test]
    fn response_serialize_token() {
        let resp = TuiResponse::Token {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"token""#));
        assert!(json.contains(r#""text":"hello""#));
    }

    #[test]
    fn response_serialize_done() {
        let resp = TuiResponse::Done;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"done""#));
    }

    #[test]
    fn response_serialize_error() {
        let resp = TuiResponse::Error {
            text: "oops".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"error""#));
    }

    #[test]
    fn request_deserialize_submit_dispatch() {
        let json = r#"{"type":"submit_dispatch","repos":["hive","swarm"],"task":"add feature"}"#;
        let req: TuiRequest = serde_json::from_str(json).unwrap();
        assert!(
            matches!(req, TuiRequest::SubmitDispatch { repos, task } if repos == vec!["hive", "swarm"] && task == "add feature")
        );
    }

    #[test]
    fn request_deserialize_confirm_dispatch() {
        let json = r#"{"type":"confirm_dispatch"}"#;
        let req: TuiRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TuiRequest::ConfirmDispatch));
    }

    #[test]
    fn request_deserialize_cancel_dispatch() {
        let json = r#"{"type":"cancel_dispatch"}"#;
        let req: TuiRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TuiRequest::CancelDispatch));
    }

    #[test]
    fn request_deserialize_get_dispatch_state() {
        let json = r#"{"type":"get_dispatch_state"}"#;
        let req: TuiRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TuiRequest::GetDispatchState));
    }

    #[test]
    fn response_serialize_dispatch_update() {
        let resp = TuiResponse::DispatchUpdate {
            task: "add auth".into(),
            repos: vec!["hive".into()],
            status: "refining".into(),
            streaming_text: String::new(),
            task_md: String::new(),
            title: String::new(),
            per_repo: vec![RepoDispatchInfo {
                repo: "hive".into(),
                stage: "context".into(),
                context_md: None,
                plan_md: None,
                file_count: Some(12),
                step_count: None,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"dispatch_update""#));
        assert!(json.contains(r#""task":"add auth""#));
        assert!(json.contains(r#""stage":"context""#));
        assert!(json.contains(r#""file_count":12"#));
    }

    #[test]
    fn repo_dispatch_info_backward_compat() {
        // Old format without stage/file_count/step_count should deserialize fine.
        let json = r#"{"repo":"hive","context_md":null,"plan_md":null}"#;
        let info: RepoDispatchInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.repo, "hive");
        assert_eq!(info.stage, "pending");
        assert!(info.file_count.is_none());
        assert!(info.step_count.is_none());
    }

    #[test]
    fn response_serialize_dispatch_cleared() {
        let resp = TuiResponse::DispatchCleared;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"dispatch_cleared""#));
    }

    #[test]
    fn response_serialize_notification() {
        let resp = TuiResponse::Notification {
            event: UiEvent::PrOpened {
                worktree_id: "hive-1".into(),
                pr_url: "https://github.com/test/1".into(),
                pr_title: "Fix".into(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"notification""#));
        assert!(json.contains("PrOpened"));
    }

    #[tokio::test]
    async fn socket_server_start_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hive")).unwrap();

        let sock_path = root.join(".hive/daemon.sock");

        {
            let (_rx, server) = TuiSocketServer::start(root).unwrap();
            assert!(sock_path.exists(), "socket file should exist");
            assert!(!server.has_clients(), "no clients yet");
            drop(server);
        }

        // Socket file should be cleaned up after drop.
        // Give a moment for the drop to run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!sock_path.exists(), "socket file should be cleaned up");
    }
}

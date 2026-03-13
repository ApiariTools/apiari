//! Webhook receiver — accepts inbound POST requests and converts them to signals.
//!
//! Starts an HTTP server on the configured port. External systems can push
//! signals via `POST /signal` with a JSON body conforming to the Signal schema.
//! Signals are buffered and drained on each `poll()` call.

use std::any::Any;

use crate::signal::Signal;
use async_trait::async_trait;
use color_eyre::Result;
use std::sync::{Arc, Mutex};

use super::Watcher;
use crate::buzz::config::WebhookConfig;

/// A webhook receiver that listens for inbound HTTP POSTs.
///
/// On construction, spawns a background HTTP server task. Incoming signals
/// are buffered in memory and returned when `poll()` is called.
pub struct WebhookWatcher {
    buffer: Arc<Mutex<Vec<Signal>>>,
    port: u16,
    started: bool,
}

impl WebhookWatcher {
    pub fn new(config: WebhookConfig) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            port: config.port,
            started: false,
        }
    }

    /// Start the background HTTP server. Called lazily on first poll.
    fn start_server(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        let buffer = self.buffer.clone();
        let port = self.port;

        tokio::spawn(async move {
            if let Err(e) = run_server(port, buffer).await {
                tracing::error!(error = %e, "server error");
            }
        });

        tracing::info!(port = port, "HTTP server started");
    }
}

#[async_trait]
impl Watcher for WebhookWatcher {
    fn name(&self) -> &str {
        "webhook"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn poll(&mut self) -> Result<Vec<Signal>> {
        // Start server lazily on first poll (needs tokio runtime to be active).
        self.start_server();

        let mut buf = self.buffer.lock().unwrap();
        let signals = buf.drain(..).collect();
        Ok(signals)
    }
}

/// Run the HTTP server that accepts `POST /signal`.
async fn run_server(port: u16, buffer: Arc<Mutex<Vec<Signal>>>) -> Result<()> {
    use hyper::Request;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let buffer = buffer.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let buffer = buffer.clone();
                async move { handle_request(req, buffer).await }
            });

            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                tracing::error!(error = %e, "connection error");
            }
        });
    }
}

async fn handle_request(
    req: hyper::Request<hyper::body::Incoming>,
    buffer: Arc<Mutex<Vec<Signal>>>,
) -> std::result::Result<
    hyper::Response<http_body_util::Full<hyper::body::Bytes>>,
    std::convert::Infallible,
> {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper::{Method, StatusCode};

    let response = match (req.method(), req.uri().path()) {
        (&Method::POST, "/signal") => {
            let body = match req.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(e) => {
                    return Ok(hyper::Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Full::new(Bytes::from(format!("failed to read body: {e}"))))
                        .unwrap());
                }
            };

            match serde_json::from_slice::<Signal>(&body) {
                Ok(signal) => {
                    tracing::debug!(
                        title = signal.title.as_str(),
                        severity = %signal.severity,
                        "received signal"
                    );
                    buffer.lock().unwrap().push(signal);
                    hyper::Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(r#"{"status":"ok"}"#)))
                        .unwrap()
                }
                Err(e) => hyper::Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Full::new(Bytes::from(format!(
                        r#"{{"error":"invalid signal JSON: {e}"}}"#
                    ))))
                    .unwrap(),
            }
        }
        (&Method::GET, "/health") => hyper::Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from(r#"{"status":"ok"}"#)))
            .unwrap(),
        _ => hyper::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from(r#"{"error":"not found"}"#)))
            .unwrap(),
    };

    Ok(response)
}

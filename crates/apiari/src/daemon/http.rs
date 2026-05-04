//! HTTP + WebSocket server for the web UI.
//!
//! Serves the workflow graph definition, task state, and live updates over WebSocket.
//! In production, static assets are embedded via rust-embed; in dev mode, Vite proxies here.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast, mpsc};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use crate::buzz::{
    orchestrator::graph::{WorkflowGraph, walker::GraphCursor},
    task::{Task, store::TaskStore},
};

// ── Shared state ───────────────────────────────────────────────────────

/// Shared state for the HTTP server.
#[derive(Clone)]
pub struct HttpState {
    /// The current workflow graph (mutable for editor).
    graph: Arc<RwLock<WorkflowGraph>>,
    /// Path to save YAML back to (None = in-memory only).
    yaml_path: Arc<Option<std::path::PathBuf>>,
    /// Access to the task store.
    db_path: Arc<std::path::PathBuf>,
    /// Workspace name.
    workspace: Arc<String>,
    /// Broadcast channel for live updates to WebSocket clients.
    updates_tx: broadcast::Sender<WsUpdate>,
    /// Channel for injecting test signals (dev mode).
    signal_tx: mpsc::UnboundedSender<InjectSignal>,
    /// Channel for chat messages from web UI to daemon coordinator.
    chat_tx: mpsc::UnboundedSender<WebChatRequest>,
    /// Channel for cancellation requests from the web UI.
    cancel_tx: mpsc::UnboundedSender<WebCancelRequest>,
}

/// A WebSocket update message sent to all connected clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsUpdate {
    /// Full state snapshot (sent on initial connect).
    Snapshot {
        tasks: Vec<TaskView>,
        graph: GraphView,
    },
    /// A task was updated.
    TaskUpdated { task: TaskView },
    /// A signal was processed.
    SignalProcessed { source: String, title: String },
    /// The graph was edited and saved.
    GraphUpdated { graph: GraphView },
    /// A new signal arrived (for the Briefing feed).
    Signal {
        id: i64,
        workspace: String,
        source: String,
        title: String,
        severity: String,
        url: Option<String>,
        created_at: String,
    },
    /// A chat message changed for a bot.
    Message {
        id: i64,
        workspace: String,
        bot: String,
        role: String,
        content: String,
        attachments: Option<String>,
        created_at: String,
    },
    /// A bot's live status changed.
    BotStatus {
        workspace: String,
        bot: String,
        status: String,
        streaming_content: String,
        tool_name: Option<String>,
    },
    /// A followup was created.
    FollowupCreated {
        id: String,
        workspace: String,
        bot: String,
        action: String,
        fires_at: String,
        status: String,
    },
    /// A followup fired.
    FollowupFired {
        id: String,
        workspace: String,
        bot: String,
        action: String,
        fires_at: String,
        status: String,
    },
    /// A followup was cancelled.
    FollowupCancelled {
        id: String,
        workspace: String,
        bot: String,
        action: String,
        fires_at: String,
        status: String,
    },
}

/// A test signal to inject (dev mode).
#[derive(Debug, Deserialize)]
pub struct InjectSignal {
    pub source: String,
    pub title: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// A chat message from the web UI to a Bee.
#[derive(Debug)]
pub struct WebChatRequest {
    pub workspace: String,
    pub bee: Option<String>,
    pub text: String,
    pub attachments_json: Option<String>,
    pub attachments: Vec<WebChatAttachment>,
    pub response_tx: mpsc::UnboundedSender<WebChatEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebChatAttachment {
    pub name: String,
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(rename = "dataUrl")]
    pub data_url: String,
}

#[derive(Debug)]
pub struct WebCancelRequest {
    pub workspace: String,
    pub bee: Option<String>,
}

/// Streaming chat response events.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebChatEvent {
    Token {
        text: String,
    },
    Done,
    Error {
        text: String,
    },
    /// A workflow step is starting.
    StepStart {
        step: String,
        label: String,
    },
    /// A workflow step completed.
    StepDone {
        step: String,
    },
}

// ── API view types ─────────────────────────────────────────────────────

/// Task as seen by the web UI.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    pub stage: String,
    pub worker_id: Option<String>,
    pub pr_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub cursor: Option<CursorView>,
}

/// Cursor state for a task.
#[derive(Debug, Clone, Serialize)]
pub struct CursorView {
    pub current_node: String,
    pub counters: HashMap<String, u32>,
    pub history: Vec<StepView>,
}

/// A step in the cursor history.
#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub from_node: String,
    pub to_node: String,
    pub trigger: String,
    pub timestamp: String,
}

/// Graph definition for the web UI (read + write).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphView {
    pub name: String,
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub stage: Option<String>,
    /// Action config (for action nodes). Round-tripped as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<serde_json::Value>,
    /// Wait-for config (for wait nodes). Round-tripped as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<serde_json::Value>,
    /// Notification tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<String>,
    /// Human-readable description of what this node does (derived for UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeView {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub has_condition: bool,
    /// Full condition for round-tripping edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<serde_json::Value>,
    /// Full guard for round-tripping edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<serde_json::Value>,
    #[serde(default)]
    pub priority: u32,
}

/// Bee (coordinator) configuration as seen by the web UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeeConfigView {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub execution_policy: crate::config::BeeExecutionPolicy,
    pub provider: String,
    pub model: String,
    pub max_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub max_session_turns: u32,
    #[serde(default)]
    pub signal_hooks: Vec<SignalHookView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<i64>,
}

/// Signal hook as seen by the web UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalHookView {
    pub source: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default)]
    pub ttl_secs: u64,
}

/// Full workspace bee configuration response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeesConfigResponse {
    pub workspace: String,
    pub bees: Vec<BeeConfigView>,
}

// ── Conversions ────────────────────────────────────────────────────────

pub fn task_to_view(task: &Task) -> TaskView {
    let cursor: Option<CursorView> = task
        .metadata
        .get("graph_cursor")
        .and_then(|v| serde_json::from_value::<GraphCursor>(v.clone()).ok())
        .map(|c| CursorView {
            current_node: c.current_node,
            counters: c.counters,
            history: c
                .history
                .iter()
                .map(|s| StepView {
                    from_node: s.from_node.clone(),
                    to_node: s.to_node.clone(),
                    trigger: s.trigger.clone(),
                    timestamp: s.timestamp.to_rfc3339(),
                })
                .collect(),
        });

    TaskView {
        id: task.id.clone(),
        title: task.title.clone(),
        stage: task.stage.as_str().to_string(),
        worker_id: task.worker_id.clone(),
        pr_url: task.pr_url.clone(),
        created_at: task.created_at.to_rfc3339(),
        updated_at: task.updated_at.to_rfc3339(),
        cursor,
    }
}

fn graph_to_view(graph: &WorkflowGraph) -> GraphView {
    let nodes = graph
        .nodes
        .iter()
        .map(|(id, node)| {
            // Build human-readable description from node config
            let description = node
                .action
                .as_ref()
                .map(|a| format!("{:?}", a.kind).to_lowercase());

            NodeView {
                id: id.clone(),
                label: node.label.clone(),
                node_type: format!("{:?}", node.node_type).to_lowercase(),
                stage: node.stage.clone(),
                action: node
                    .action
                    .as_ref()
                    .and_then(|a| serde_json::to_value(a).ok()),
                wait_for: None,
                notify: None,
                description,
            }
        })
        .collect();

    let edges = graph
        .edges
        .iter()
        .map(|edge| {
            let label = edge.condition.as_ref().map(|c| match c {
                crate::buzz::orchestrator::graph::Condition::Signal { source, .. } => {
                    format!("signal: {source}")
                }
                crate::buzz::orchestrator::graph::Condition::AgentOutput {
                    source,
                    match_fields,
                    ..
                } => {
                    let fields: Vec<String> = match_fields
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect();
                    if fields.is_empty() {
                        format!("agent: {source}")
                    } else {
                        format!("agent: {} ({})", source, fields.join(", "))
                    }
                }
                crate::buzz::orchestrator::graph::Condition::StateCheck { check, expect } => {
                    format!("check: {check}={expect}")
                }
            });

            // Serialize condition and guard for round-trip editing
            let condition_json = edge
                .condition
                .as_ref()
                .and_then(|c| serde_json::to_value(c).ok());
            let guard_json = edge
                .guard
                .as_ref()
                .and_then(|g| serde_json::to_value(g).ok());

            EdgeView {
                from: edge.from.clone(),
                to: edge.to.clone(),
                label,
                has_condition: edge.condition.is_some(),
                condition: condition_json,
                guard: guard_json,
                priority: edge.priority as u32,
            }
        })
        .collect();

    GraphView {
        name: "workflow".to_string(),
        nodes,
        edges,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

/// Convert a GraphView (from the editor) back into a WorkflowGraph.
fn view_to_graph(view: &GraphView) -> color_eyre::Result<WorkflowGraph> {
    use crate::buzz::orchestrator::graph::{Condition, Edge, Guard, Node, NodeType};

    let mut nodes = HashMap::new();
    for nv in &view.nodes {
        let node_type = match nv.node_type.as_str() {
            "entry" => NodeType::Entry,
            "action" => NodeType::Action,
            "wait" => NodeType::Wait,
            "terminal" => NodeType::Terminal,
            other => {
                return Err(color_eyre::eyre::eyre!("unknown node type: {other}"));
            }
        };
        nodes.insert(
            nv.id.clone(),
            Node {
                node_type,
                label: nv.label.clone(),
                stage: nv.stage.clone(),
                action: nv
                    .action
                    .as_ref()
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| color_eyre::eyre::eyre!("invalid action: {e}"))?,
            },
        );
    }

    let mut edges = Vec::new();
    for ev in &view.edges {
        let condition: Option<Condition> = ev
            .condition
            .as_ref()
            .map(|c| serde_json::from_value(c.clone()))
            .transpose()
            .map_err(|e| color_eyre::eyre::eyre!("invalid condition: {e}"))?;

        let guard: Option<Guard> = ev
            .guard
            .as_ref()
            .map(|g| serde_json::from_value(g.clone()))
            .transpose()
            .map_err(|e| color_eyre::eyre::eyre!("invalid guard: {e}"))?;

        edges.push(Edge {
            from: ev.from.clone(),
            to: ev.to.clone(),
            condition,
            guard,
            priority: ev.priority as i32,
        });
    }

    Ok(WorkflowGraph {
        name: view.name.clone(),
        nodes,
        edges,
    })
}

// ── Route handlers ─────────────────────────────────────────────────────

/// GET /api/graph — return the workflow graph definition.
/// Supports `?workspace=mgm` to load a different workspace's graph from disk.
async fn get_graph(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
) -> Json<GraphView> {
    // If a different workspace is requested, load its graph from disk
    if let Some(ref ws_name) = q.workspace
        && ws_name.as_str() != state.workspace.as_str()
    {
        if let Ok(workspaces) = crate::config::discover_workspaces()
            && let Some(ws) = workspaces.iter().find(|w| &w.name == ws_name)
        {
            let yaml_path = ws.config.root.join(".apiari/workflow.yaml");
            if let Ok(g) =
                crate::buzz::orchestrator::graph::builtin::load_workflow(Some(&yaml_path))
            {
                return Json(graph_to_view(&g));
            }
        }
        // Fall back to builtin if workspace not found or no custom workflow
        let builtin = crate::buzz::orchestrator::graph::builtin::builtin_workflow();
        return Json(graph_to_view(&builtin));
    }
    let graph = state.graph.read().await;
    Json(graph_to_view(&graph))
}

/// PUT /api/graph — save an edited graph. Validates, updates in memory, writes YAML.
async fn save_graph(
    State(state): State<HttpState>,
    Json(view): Json<GraphView>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Convert view back to a WorkflowGraph and validate
    let new_graph = view_to_graph(&view)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid graph: {e}")))?;

    // Write YAML file if we have a path
    if let Some(ref path) = *state.yaml_path {
        let yaml = serde_yaml::to_string(&new_graph).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to serialize YAML: {e}"),
            )
        })?;
        std::fs::write(path, &yaml).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to write {}: {e}", path.display()),
            )
        })?;
        info!("[http] saved graph to {}", path.display());
    }

    // Update in-memory graph
    let view_for_broadcast = graph_to_view(&new_graph);
    {
        let mut graph = state.graph.write().await;
        *graph = new_graph;
    }

    // Broadcast to all WebSocket clients
    let _ = state.updates_tx.send(WsUpdate::GraphUpdated {
        graph: view_for_broadcast,
    });

    Ok(Json(serde_json::json!({"ok": true})))
}

/// GET /api/graph/yaml — return the raw YAML source.
async fn get_yaml(State(state): State<HttpState>) -> Result<String, StatusCode> {
    let graph = state.graph.read().await;
    serde_yaml::to_string(&*graph).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// GET /api/tasks — return all active tasks with cursor state.
async fn get_tasks(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
) -> Json<Vec<TaskView>> {
    let workspace = q.workspace.as_deref().unwrap_or(state.workspace.as_str());
    let store = match TaskStore::open(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to open task store: {e}");
            return Json(vec![]);
        }
    };

    let tasks = store
        .get_all_tasks(workspace)
        .unwrap_or_default()
        .iter()
        .map(task_to_view)
        .collect();

    Json(tasks)
}

/// DELETE /api/tasks — clear all tasks.
async fn clear_tasks(State(state): State<HttpState>) -> Json<serde_json::Value> {
    let store = match TaskStore::open(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to open task store: {e}");
            return Json(serde_json::json!({"ok": false, "error": e.to_string()}));
        }
    };
    let count = store.clear_tasks(&state.workspace).unwrap_or(0);
    info!("[http] cleared {count} tasks");
    Json(serde_json::json!({"ok": true, "deleted": count}))
}

/// POST /api/signal — inject a test signal (dev mode).
async fn inject_signal(
    State(state): State<HttpState>,
    Json(sig): Json<InjectSignal>,
) -> impl IntoResponse {
    info!(
        "[http] injecting test signal: {} ({})",
        sig.title, sig.source
    );
    let _ = state.signal_tx.send(sig);
    Json(serde_json::json!({"ok": true}))
}

/// WebSocket endpoint for live updates.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<HttpState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: HttpState) {
    info!("[http] WebSocket client connected");

    // Send initial snapshot
    let tasks: Vec<TaskView> = TaskStore::open(&state.db_path)
        .ok()
        .and_then(|store| store.get_all_tasks(&state.workspace).ok())
        .unwrap_or_default()
        .iter()
        .map(task_to_view)
        .collect();

    let graph = state.graph.read().await;
    let snapshot = WsUpdate::Snapshot {
        tasks,
        graph: graph_to_view(&graph),
    };
    drop(graph);

    if let Ok(json) = serde_json::to_string(&snapshot) {
        let _ = socket.send(Message::Text(json.into())).await;
    }

    // Forward broadcast updates to this client
    let mut rx = state.updates_tx.subscribe();
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(update) => {
                        if let Ok(json) = serde_json::to_string(&update)
                            && socket.send(Message::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    info!("[http] WebSocket client disconnected");
}

// ── Query params ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WorkspaceQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    history: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WorkspaceListItem {
    name: String,
}

#[derive(Debug, Serialize)]
struct BotListItem {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    watch: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RepoListItem {
    name: String,
    path: String,
    has_swarm: bool,
    is_clean: bool,
    branch: String,
    workers: Vec<WorkerView>,
}

#[derive(Debug, Serialize)]
struct BotStatusView {
    status: String,
    streaming_content: String,
    tool_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct BotTurnFailureView {
    id: i64,
    bot: String,
    provider: Option<String>,
    source: String,
    error_text: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct BotTurnDecisionView {
    id: i64,
    bot: String,
    provider: Option<String>,
    decision_type: String,
    detail: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct ProviderCapabilityView {
    name: String,
    installed: bool,
    binary_path: Option<String>,
    sandbox_flag_supported: Option<bool>,
    approval_flag_supported: Option<bool>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BotDebugView {
    workspace: String,
    bot: String,
    provider: Option<String>,
    status: Option<BotStatusView>,
    recent_failures: Vec<BotTurnFailureView>,
    recent_decisions: Vec<BotTurnDecisionView>,
    recent_messages: Vec<ConversationMessageView>,
}

#[derive(Debug, Serialize)]
struct ConversationMessageView {
    id: i64,
    workspace: String,
    bot: String,
    role: String,
    content: String,
    attachments: Option<String>,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct WorkerConversationMessageView {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkerDetailView {
    #[serde(flatten)]
    worker: WorkerView,
    prompt: Option<String>,
    output: Option<String>,
    conversation: Vec<WorkerConversationMessageView>,
}

#[derive(Debug, Serialize)]
struct WorkerDiffView {
    diff: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceChatBody {
    message: String,
    #[serde(default)]
    attachments: Option<Vec<WebChatAttachment>>,
}

async fn probe_provider_capability(name: &str, bin: &str) -> ProviderCapabilityView {
    let which = tokio::process::Command::new("which")
        .arg(bin)
        .output()
        .await;
    let installed = which.as_ref().is_ok_and(|output| output.status.success());
    let binary_path = which.ok().and_then(|output| {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!value.is_empty()).then_some(value)
        } else {
            None
        }
    });

    let mut notes = Vec::new();
    let mut sandbox_flag_supported = None;
    let mut approval_flag_supported = None;

    if installed {
        match tokio::process::Command::new(bin)
            .arg("exec")
            .arg("--help")
            .output()
            .await
        {
            Ok(output) => {
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                if name == "codex" {
                    sandbox_flag_supported = Some(combined.contains("--sandbox"));
                    approval_flag_supported = Some(combined.contains("--approval-policy"));
                    if approval_flag_supported == Some(false) {
                        notes.push(
                            "Current codex exec CLI does not support --approval-policy."
                                .to_string(),
                        );
                    }
                }
            }
            Err(err) => {
                notes.push(format!("Failed to inspect CLI help: {err}"));
            }
        }
    } else {
        notes.push("Provider CLI not found on PATH.".to_string());
    }

    ProviderCapabilityView {
        name: name.to_string(),
        installed,
        binary_path,
        sandbox_flag_supported,
        approval_flag_supported,
        notes,
    }
}

#[derive(Debug, Serialize)]
struct UsageProviderView {
    name: String,
    status: String,
    usage_percent: Option<f64>,
    remaining: Option<String>,
    limit: Option<String>,
    resets_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct UsageView {
    installed: bool,
    providers: Vec<UsageProviderView>,
    updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct FollowupView {
    id: String,
    workspace: String,
    bot: String,
    action: String,
    created_at: String,
    fires_at: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct ResearchTaskView {
    id: String,
    workspace: String,
    topic: String,
    status: String,
    error: Option<String>,
    started_at: String,
    completed_at: Option<String>,
    output_file: Option<String>,
}

#[derive(Debug, Serialize)]
struct DocView {
    name: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct ResearchRequestBody {
    topic: String,
}

#[derive(Debug, Deserialize)]
struct SaveDocBody {
    content: String,
}

fn load_workspace_by_name(workspace: &str) -> Option<crate::config::Workspace> {
    crate::config::discover_workspaces()
        .ok()?
        .into_iter()
        .find(|ws| ws.name == workspace)
}

fn workspace_docs_dirs(config: &crate::config::WorkspaceConfig) -> Vec<std::path::PathBuf> {
    vec![config.root.join(".apiari/docs"), config.root.join("docs")]
}

fn workspace_docs_dir(config: &crate::config::WorkspaceConfig) -> std::path::PathBuf {
    workspace_docs_dirs(config)
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or_else(|| config.root.join(".apiari/docs"))
}

fn resolve_workspace_doc_path(
    config: &crate::config::WorkspaceConfig,
    filename: &str,
) -> std::path::PathBuf {
    for dir in workspace_docs_dirs(config) {
        let path = dir.join(filename);
        if path.exists() {
            return path;
        }
    }
    workspace_docs_dir(config).join(filename)
}

fn doc_title_from_filename(name: &str) -> String {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    stem.split(['-', '_', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn doc_title_from_content(name: &str, content: &str) -> String {
    content
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| doc_title_from_filename(name))
}

fn doc_updated_at(metadata: &std::fs::Metadata) -> String {
    metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

fn load_workspace_docs(config: &crate::config::WorkspaceConfig) -> Vec<DocView> {
    let mut docs = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for docs_dir in workspace_docs_dirs(config) {
        let entries = match std::fs::read_dir(docs_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|file| file.to_str()) else {
                continue;
            };
            if !seen.insert(name.to_string()) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            docs.push(DocView {
                name: name.to_string(),
                title: doc_title_from_content(name, &content),
                content: None,
                updated_at: doc_updated_at(&metadata),
            });
        }
    }

    docs.sort_by(|a, b| a.name.cmp(&b.name));
    docs
}

fn sanitize_doc_name(name: &str) -> Option<String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    Some(name.to_string())
}

fn display_bee_name(_bees: &[crate::config::BeeConfig], bee: &crate::config::BeeConfig) -> String {
    if bee.name == "Bee" {
        "Main".to_string()
    } else {
        bee.name.clone()
    }
}

fn resolve_bee_name_for_api(
    config: &crate::config::WorkspaceConfig,
    api_name: &str,
) -> Option<String> {
    let bees = config.resolved_bees();
    if api_name == "Main" {
        if let Some(bee) = bees.iter().find(|bee| bee.name == "Bee") {
            return Some(bee.name.clone());
        }
        if let Some(first) = bees.first() {
            return Some(first.name.clone());
        }
    }

    bees.into_iter()
        .find(|bee| bee.name == api_name)
        .map(|bee| bee.name)
}

fn bot_items_for_workspace(config: &crate::config::WorkspaceConfig) -> Vec<BotListItem> {
    let bees = config.resolved_bees();
    bees.iter()
        .map(|bee| BotListItem {
            name: display_bee_name(&bees, bee),
            color: bee.color.clone(),
            role: bee.role.clone(),
            description: bee.prompt.clone(),
            provider: Some(bee.provider.clone()),
            model: (!bee.model.trim().is_empty()).then(|| bee.model.clone()),
            watch: bee
                .signal_hooks
                .iter()
                .map(|hook| hook.source.clone())
                .collect(),
        })
        .collect()
}

fn conversation_scopes_for_bot(
    workspace: &str,
    config: &crate::config::WorkspaceConfig,
    bot: &str,
) -> Vec<String> {
    let actual_bot = resolve_bee_name_for_api(config, bot).unwrap_or_else(|| bot.to_string());
    let mut scopes = vec![format!("{workspace}/{actual_bot}")];
    if bot == "Main" {
        scopes.push(format!("{workspace}/Main"));
        scopes.push(format!("{workspace}/Bee"));
        scopes.push(workspace.to_string());
    } else if actual_bot != bot {
        scopes.push(format!("{workspace}/{bot}"));
    }
    scopes
}

fn latest_message_id_for_scopes(conn: &rusqlite::Connection, scopes: &[String]) -> Option<i64> {
    scopes
        .iter()
        .filter_map(|scope| {
            conn.query_row(
                "SELECT MAX(id) FROM conversations WHERE workspace = ?1",
                params![scope],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
        })
        .max()
}

fn unread_assistant_count_for_scopes(
    conn: &rusqlite::Connection,
    scopes: &[String],
    seen_id: i64,
) -> usize {
    let mut seen_ids = std::collections::HashSet::new();
    for scope in scopes {
        let mut stmt = match conn.prepare(
            "SELECT id
             FROM conversations
             WHERE workspace = ?1 AND role = 'assistant' AND id > ?2",
        ) {
            Ok(stmt) => stmt,
            Err(_) => continue,
        };
        let rows = match stmt.query_map(params![scope, seen_id], |row| row.get::<_, i64>(0)) {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        for id in rows.flatten() {
            seen_ids.insert(id);
        }
    }
    seen_ids.len()
}

fn repo_slug_to_local_path(root: &std::path::Path, repo: &str) -> std::path::PathBuf {
    let direct = root.join(repo);
    if direct.exists() {
        return direct;
    }

    let basename = repo.rsplit('/').next().unwrap_or(repo);
    let by_name = root.join(basename);
    if by_name.exists() {
        return by_name;
    }

    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) || !path.join(".git").exists() {
                continue;
            }
            let Some(origin_url) = git_output(&path, &["remote", "get-url", "origin"]) else {
                continue;
            };
            let slug = if let Some(rest) = origin_url.strip_prefix("https://github.com/") {
                rest
            } else if let Some(rest) = origin_url.strip_prefix("git@github.com:") {
                rest
            } else {
                continue;
            };
            let slug = slug.strip_suffix(".git").unwrap_or(slug);
            if slug == repo {
                return path;
            }
        }
    }

    root.to_path_buf()
}

fn swarm_state_path(config: &crate::config::WorkspaceConfig) -> std::path::PathBuf {
    config.resolved_swarm_state_path()
}

fn git_output(path: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_git_clean(path: &std::path::Path) -> bool {
    git_output(path, &["status", "--porcelain"])
        .map(|out| out.trim().is_empty())
        .unwrap_or(true)
}

fn current_git_branch(path: &std::path::Path) -> String {
    git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default()
}

fn build_repo_list_items(
    config: &crate::config::WorkspaceConfig,
    workspace_workers: &[WorkerView],
) -> Vec<RepoListItem> {
    crate::config::resolve_repos(config)
        .into_iter()
        .map(|repo| {
            let local_path = repo_slug_to_local_path(&config.root, &repo);
            let basename = local_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| repo.rsplit('/').next().unwrap_or(&repo).to_string());

            RepoListItem {
                name: basename.clone(),
                path: local_path.display().to_string(),
                has_swarm: local_path.join(".swarm").exists(),
                is_clean: is_git_clean(&local_path),
                branch: current_git_branch(&local_path),
                workers: workspace_workers
                    .iter()
                    .filter(|worker| worker.branch.starts_with(&basename))
                    .cloned()
                    .collect(),
            }
        })
        .collect()
}

// ── Bee config handlers ────────────────────────────────────────────────

/// GET /api/workspaces — list all configured workspaces.
async fn list_workspaces() -> Json<Vec<WorkspaceListItem>> {
    let names = crate::config::discover_workspaces()
        .map(|ws| {
            ws.into_iter()
                .map(|w| WorkspaceListItem { name: w.name })
                .collect()
        })
        .unwrap_or_default();
    Json(names)
}

/// GET /api/workspaces/:workspace/bots — list UI bots for a workspace.
async fn list_workspace_bots(Path(workspace): Path<String>) -> Json<Vec<BotListItem>> {
    let bots = load_workspace_by_name(&workspace)
        .map(|ws| bot_items_for_workspace(&ws.config))
        .unwrap_or_default();
    Json(bots)
}

/// GET /api/workspaces/:workspace/repos — list repos for a workspace.
async fn list_workspace_repos(Path(workspace): Path<String>) -> Json<Vec<RepoListItem>> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(vec![]);
    };

    let all_workers = get_workers().await.0;
    let workspace_workers: Vec<WorkerView> = all_workers
        .into_iter()
        .filter(|worker| worker.workspace == workspace)
        .collect();

    let repos = build_repo_list_items(&ws.config, &workspace_workers);

    Json(repos)
}

/// GET /api/workspaces/:workspace/conversations/:bot — load history for one bot.
async fn get_workspace_conversations(
    Path((workspace, bot)): Path<(String, String)>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<ConversationMessageView>> {
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(30);

    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(vec![]);
    };

    let store = match crate::buzz::signal::store::SignalStore::open(
        &crate::config::db_path(),
        &workspace,
    ) {
        Ok(store) => store,
        Err(_) => return Json(vec![]),
    };

    let mut rows = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    let scopes = conversation_scopes_for_bot(&workspace, &ws.config, &bot);

    for scope in scopes {
        let scoped = crate::buzz::conversation::ConversationStore::new(store.conn(), &scope);
        if let Ok(history) = scoped.load_history(limit) {
            for row in history {
                if seen_ids.insert(row.id) {
                    rows.push(row);
                }
            }
        }
    }

    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if rows.len() > limit {
        rows = rows.split_off(rows.len() - limit);
    }

    Json(
        rows.into_iter()
            .map(|row| ConversationMessageView {
                id: row.id,
                workspace: workspace.clone(),
                bot: bot.clone(),
                role: row.role,
                content: row.content,
                attachments: row.attachments,
                created_at: row.created_at,
            })
            .collect(),
    )
}

/// GET /api/workspaces/:workspace/workers — list workers for one workspace.
async fn get_workspace_workers(Path(workspace): Path<String>) -> Json<Vec<WorkerView>> {
    Json(
        get_workers()
            .await
            .0
            .into_iter()
            .filter(|worker| worker.workspace == workspace)
            .collect(),
    )
}

/// GET /api/workspaces/:workspace/workers/:worker_id — detailed worker state.
async fn get_workspace_worker_detail(
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Result<Json<WorkerDetailView>, StatusCode> {
    let ws = load_workspace_by_name(&workspace).ok_or(StatusCode::NOT_FOUND)?;
    let worker = find_worker_state(&ws.config, &worker_id).ok_or(StatusCode::NOT_FOUND)?;
    let worker_view = worker_view_from_state(&workspace, &worker);
    let conversation = worker_conversation_messages(&ws.config.resolved_swarm_dir(), &worker_id);
    let output = worker_output_from_conversation(&conversation).or_else(|| worker.summary.clone());

    Ok(Json(WorkerDetailView {
        worker: worker_view,
        prompt: (!worker.prompt.trim().is_empty()).then_some(worker.prompt),
        output,
        conversation,
    }))
}

#[derive(Debug, Deserialize)]
struct WorkspaceWorkerMessageBody {
    message: String,
}

/// POST /api/workspaces/:workspace/workers/:worker_id/send — send a message to a worker.
async fn send_workspace_worker_message(
    Path((workspace, worker_id)): Path<(String, String)>,
    Json(body): Json<WorkspaceWorkerMessageBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ws = load_workspace_by_name(&workspace).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("workspace '{workspace}' not found"),
        )
    })?;

    let output = tokio::process::Command::new("swarm")
        .arg("--dir")
        .arg(&ws.config.root)
        .arg("send")
        .arg(&worker_id)
        .arg(&body.message)
        .output()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to run swarm: {e}"),
            )
        })?;

    if output.status.success() {
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("swarm send failed: {stderr}"),
        ))
    }
}

/// GET /api/workspaces/:workspace/workers/:worker_id/diff — current worker worktree diff.
async fn get_workspace_worker_diff(
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Json<WorkerDiffView> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(WorkerDiffView { diff: None });
    };

    let worktree = ws.config.resolved_swarm_dir().join("wt").join(worker_id);
    let output = std::process::Command::new("git")
        .args(["diff", "--no-ext-diff", "--unified=3", "HEAD"])
        .current_dir(worktree)
        .output();

    let diff = output.ok().and_then(|out| {
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        (!text.trim().is_empty()).then_some(text)
    });

    Json(WorkerDiffView { diff })
}

/// GET /api/workspaces/:workspace/bots/:bot/status — current bot status.
async fn get_workspace_bot_status(
    Path((workspace, bot)): Path<(String, String)>,
) -> Json<BotStatusView> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(BotStatusView {
            status: "idle".to_string(),
            streaming_content: String::new(),
            tool_name: None,
        });
    };
    let actual_bot = resolve_bee_name_for_api(&ws.config, &bot).unwrap_or(bot);
    let status =
        crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), &workspace)
            .ok()
            .and_then(|store| store.get_bot_status(&actual_bot).ok().flatten());

    match status {
        Some(status) => Json(BotStatusView {
            status: status.status,
            streaming_content: status.streaming_content,
            tool_name: status.tool_name,
        }),
        None => Json(BotStatusView {
            status: "idle".to_string(),
            streaming_content: String::new(),
            tool_name: None,
        }),
    }
}

/// GET /api/workspaces/:workspace/unread — unread message counts by bot.
async fn get_workspace_unread(
    Path(workspace): Path<String>,
) -> Json<serde_json::Map<String, serde_json::Value>> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(serde_json::Map::new());
    };
    let store = match crate::buzz::signal::store::SignalStore::open(
        &crate::config::db_path(),
        &workspace,
    ) {
        Ok(store) => store,
        Err(_) => return Json(serde_json::Map::new()),
    };

    let mut unread = serde_json::Map::new();
    for bot in bot_items_for_workspace(&ws.config) {
        let seen_id = store
            .get_bot_seen_message_id(&bot.name)
            .ok()
            .flatten()
            .unwrap_or(0);
        let scopes = conversation_scopes_for_bot(&workspace, &ws.config, &bot.name);
        let count = unread_assistant_count_for_scopes(store.conn(), &scopes, seen_id);
        if count > 0 {
            unread.insert(bot.name, serde_json::json!(count));
        }
    }

    Json(unread)
}

/// POST /api/workspaces/:workspace/seen/:bot — mark a bot as seen.
async fn mark_workspace_seen(
    Path((workspace, bot)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(serde_json::json!({ "ok": false, "error": "workspace not found" }));
    };
    let store = match crate::buzz::signal::store::SignalStore::open(
        &crate::config::db_path(),
        &workspace,
    ) {
        Ok(store) => store,
        Err(e) => {
            return Json(serde_json::json!({ "ok": false, "error": e.to_string() }));
        }
    };
    let scopes = conversation_scopes_for_bot(&workspace, &ws.config, &bot);
    let latest_id = latest_message_id_for_scopes(store.conn(), &scopes).unwrap_or(0);
    match store.mark_bot_seen(&bot, latest_id) {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// POST /api/workspaces/:workspace/bots/:bot/cancel — cancel a running bot.
async fn cancel_workspace_bot(
    Path((workspace, bot)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> Json<serde_json::Value> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(serde_json::json!({ "ok": false, "error": "workspace not found" }));
    };
    let bee = resolve_bee_name_for_api(&ws.config, &bot).unwrap_or(bot);
    if state
        .cancel_tx
        .send(WebCancelRequest {
            workspace,
            bee: Some(bee),
        })
        .is_err()
    {
        return Json(serde_json::json!({ "ok": false, "error": "cancel channel unavailable" }));
    }
    Json(serde_json::json!({ "ok": true }))
}

async fn get_usage() -> Json<UsageView> {
    Json(UsageView {
        installed: false,
        providers: vec![],
        updated_at: None,
    })
}

async fn list_workspace_docs(
    Path(workspace): Path<String>,
) -> Result<Json<Vec<DocView>>, StatusCode> {
    let ws = load_workspace_by_name(&workspace).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(load_workspace_docs(&ws.config)))
}

async fn get_workspace_doc(
    Path((workspace, filename)): Path<(String, String)>,
) -> Result<Json<DocView>, StatusCode> {
    let ws = load_workspace_by_name(&workspace).ok_or(StatusCode::NOT_FOUND)?;
    let filename = sanitize_doc_name(&filename).ok_or(StatusCode::BAD_REQUEST)?;
    let path = resolve_workspace_doc_path(&ws.config, &filename);
    let content = std::fs::read_to_string(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    let metadata = std::fs::metadata(&path).map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(Json(DocView {
        name: filename.clone(),
        title: doc_title_from_content(&filename, &content),
        content: Some(content),
        updated_at: doc_updated_at(&metadata),
    }))
}

async fn save_workspace_doc(
    Path((workspace, filename)): Path<(String, String)>,
    Json(body): Json<SaveDocBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let ws = load_workspace_by_name(&workspace).ok_or(StatusCode::NOT_FOUND)?;
    let filename = sanitize_doc_name(&filename).ok_or(StatusCode::BAD_REQUEST)?;
    let docs_dir = workspace_docs_dir(&ws.config);
    std::fs::create_dir_all(&docs_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let path = docs_dir.join(filename);
    std::fs::write(path, body.content).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_workspace_doc(
    Path((workspace, filename)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let ws = load_workspace_by_name(&workspace).ok_or(StatusCode::NOT_FOUND)?;
    let filename = sanitize_doc_name(&filename).ok_or(StatusCode::BAD_REQUEST)?;
    let path = resolve_workspace_doc_path(&ws.config, &filename);
    std::fs::remove_file(path).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_workspace_followups(Path(workspace): Path<String>) -> Json<Vec<FollowupView>> {
    let store = match crate::buzz::signal::store::SignalStore::open(
        &crate::config::db_path(),
        &workspace,
    ) {
        Ok(store) => store,
        Err(_) => return Json(vec![]),
    };
    let followups = store
        .list_followups()
        .unwrap_or_default()
        .into_iter()
        .map(|followup| FollowupView {
            id: followup.id,
            workspace: workspace.clone(),
            bot: followup.bot,
            action: followup.action,
            created_at: followup.created_at,
            fires_at: followup.fires_at,
            status: followup.status,
        })
        .collect();
    Json(followups)
}

async fn cancel_workspace_followup(
    Path((workspace, followup_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> Json<serde_json::Value> {
    let store = match crate::buzz::signal::store::SignalStore::open(
        &crate::config::db_path(),
        &workspace,
    ) {
        Ok(store) => store,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    };
    let followup = match store.get_followup(&followup_id) {
        Ok(record) => record,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    };
    match store.cancel_followup(&followup_id) {
        Ok(changed) => {
            if changed && let Some(followup) = followup {
                let _ = state.updates_tx.send(WsUpdate::FollowupCancelled {
                    id: followup.id,
                    workspace: workspace.clone(),
                    bot: followup.bot,
                    action: followup.action,
                    fires_at: followup.fires_at,
                    status: "cancelled".to_string(),
                });
            }
            Json(serde_json::json!({ "ok": changed }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn list_workspace_research(Path(_workspace): Path<String>) -> Json<Vec<ResearchTaskView>> {
    Json(vec![])
}

async fn start_workspace_research(
    Path(workspace): Path<String>,
    Json(body): Json<ResearchRequestBody>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": format!("research-{}", chrono::Utc::now().timestamp_millis()),
        "topic": body.topic,
        "status": "running",
        "workspace": workspace,
    }))
}

/// POST /api/workspaces/:workspace/chat/:bot — enqueue a user message for a bot.
async fn send_workspace_chat(
    Path((workspace, bot)): Path<(String, String)>,
    State(state): State<HttpState>,
    Json(body): Json<WorkspaceChatBody>,
) -> Json<serde_json::Value> {
    let Some(ws) = load_workspace_by_name(&workspace) else {
        return Json(serde_json::json!({ "ok": false, "error": "workspace not found" }));
    };
    let actual_bot = resolve_bee_name_for_api(&ws.config, &bot).unwrap_or(bot.clone());

    let attachments = body.attachments.unwrap_or_default();
    let attachments_json = if attachments.is_empty() {
        None
    } else {
        serde_json::to_string(&attachments).ok()
    };

    let (response_tx, _response_rx) = mpsc::unbounded_channel::<WebChatEvent>();
    let req = WebChatRequest {
        workspace,
        bee: Some(actual_bot),
        text: body.message,
        attachments_json,
        attachments,
        response_tx,
    };

    let ok = state.chat_tx.send(req).is_ok();
    Json(serde_json::json!({ "ok": ok }))
}

/// GET /api/bees — returns the resolved bee config for a workspace.
/// Supports `?workspace=mgm` query param (defaults to daemon's primary workspace).
async fn get_bees(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
) -> Json<BeesConfigResponse> {
    let workspace = q.workspace.as_deref().unwrap_or(state.workspace.as_str());
    let bees = match crate::config::discover_workspaces() {
        Ok(workspaces) => {
            if let Some(ws) = workspaces.iter().find(|w| w.name == workspace) {
                ws.config
                    .resolved_bees()
                    .into_iter()
                    .map(|b| BeeConfigView {
                        name: b.name,
                        role: b.role,
                        color: b.color,
                        execution_policy: b.execution_policy,
                        provider: b.provider,
                        model: b.model,
                        max_turns: b.max_turns,
                        prompt: b.prompt,
                        max_session_turns: b.max_session_turns,
                        signal_hooks: b
                            .signal_hooks
                            .into_iter()
                            .map(|h| SignalHookView {
                                source: h.source,
                                prompt: h.prompt,
                                action: h.action,
                                ttl_secs: h.ttl_secs,
                            })
                            .collect(),
                        topic_id: b.topic_id,
                    })
                    .collect()
            } else {
                vec![]
            }
        }
        Err(_) => vec![],
    };
    Json(BeesConfigResponse {
        workspace: workspace.to_string(),
        bees,
    })
}

/// PUT /api/bees — save the bee config array to a workspace TOML.
/// Supports `?workspace=mgm` query param.
async fn save_bees(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
    Json(body): Json<Vec<BeeConfigView>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let workspace = q.workspace.as_deref().unwrap_or(state.workspace.as_str());
    let config_path = crate::config::workspaces_dir().join(format!("{workspace}.toml"));

    // Read existing TOML
    let contents = std::fs::read_to_string(&config_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read config: {e}"),
        )
    })?;
    let mut doc = contents.parse::<toml_edit::DocumentMut>().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to parse TOML: {e}"),
        )
    })?;

    // Build the [[bees]] array
    let mut bees_array = toml_edit::ArrayOfTables::new();
    for bee in &body {
        let mut table = toml_edit::Table::new();
        table["name"] = toml_edit::value(&bee.name);
        if let Some(ref role) = bee.role {
            table["role"] = toml_edit::value(role);
        }
        if let Some(ref color) = bee.color {
            table["color"] = toml_edit::value(color);
        }
        table["execution_policy"] = toml_edit::value(match bee.execution_policy {
            crate::config::BeeExecutionPolicy::Observe => "observe",
            crate::config::BeeExecutionPolicy::DispatchOnly => "dispatch_only",
            crate::config::BeeExecutionPolicy::Autonomous => "autonomous",
        });
        table["provider"] = toml_edit::value(&bee.provider);
        table["model"] = toml_edit::value(&bee.model);
        table["max_turns"] = toml_edit::value(bee.max_turns as i64);
        if let Some(ref prompt) = bee.prompt {
            table["prompt"] = toml_edit::value(prompt);
        }
        table["max_session_turns"] = toml_edit::value(bee.max_session_turns as i64);
        if let Some(topic_id) = bee.topic_id {
            table["topic_id"] = toml_edit::value(topic_id);
        }
        if !bee.signal_hooks.is_empty() {
            let mut hooks = toml_edit::ArrayOfTables::new();
            for hook in &bee.signal_hooks {
                let mut h = toml_edit::Table::new();
                h["source"] = toml_edit::value(&hook.source);
                if !hook.prompt.is_empty() {
                    h["prompt"] = toml_edit::value(&hook.prompt);
                }
                if let Some(ref a) = hook.action {
                    h["action"] = toml_edit::value(a);
                }
                if hook.ttl_secs > 0 {
                    h["ttl_secs"] = toml_edit::value(hook.ttl_secs as i64);
                }
                hooks.push(h);
            }
            table.insert("signal_hooks", toml_edit::Item::ArrayOfTables(hooks));
        }
        bees_array.push(table);
    }

    // Replace/add the [[bees]] section
    doc.as_table_mut()
        .insert("bees", toml_edit::Item::ArrayOfTables(bees_array));

    // Write back
    std::fs::write(&config_path, doc.to_string()).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write config: {e}"),
        )
    })?;

    Ok(Json(serde_json::json!({ "ok": true, "count": body.len() })))
}

// ── Briefing endpoint ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct BriefingItem {
    id: String,
    priority: String,
    icon: String,
    title: String,
    body: Option<String>,
    workspace: String,
    source: String,
    url: Option<String>,
    actions: Vec<BriefingAction>,
    timestamp: String,
}

#[derive(Debug, Serialize)]
struct BriefingAction {
    label: String,
    style: String,
}

/// GET /api/briefing — aggregated, prioritized briefing items across all workspaces.
async fn get_briefing(State(state): State<HttpState>) -> Json<Vec<BriefingItem>> {
    let mut items = Vec::new();
    let workspaces = crate::config::discover_workspaces().unwrap_or_default();

    for ws in &workspaces {
        let store = match crate::buzz::signal::store::SignalStore::open(&state.db_path, &ws.name) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let signals = store.get_open_signals().unwrap_or_default();

        // Filter to last 48 hours only, and exclude currently snoozed signals
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(48);
        let now_utc = chrono::Utc::now();
        let signals: Vec<_> = signals
            .into_iter()
            .filter(|s| {
                s.created_at > cutoff && s.snoozed_until.is_none_or(|until| now_utc > until)
            })
            .collect();

        let mut by_source: std::collections::HashMap<
            &str,
            Vec<&crate::buzz::signal::SignalRecord>,
        > = std::collections::HashMap::new();
        for sig in &signals {
            by_source.entry(sig.source.as_str()).or_default().push(sig);
        }

        // Workers waiting → action (deduplicated by worker_id)
        {
            let mut seen_workers: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for sig in by_source.get("swarm_worker_waiting").unwrap_or(&vec![]) {
                let worker_id = sig
                    .metadata
                    .as_ref()
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                    .and_then(|v| {
                        v.get("worker_id")
                            .and_then(|w| w.as_str().map(|s| s.to_string()))
                    })
                    .unwrap_or_default();
                // Skip duplicate worker IDs
                if !worker_id.is_empty() && !seen_workers.insert(worker_id.clone()) {
                    continue;
                }
                let pr_url = sig
                    .metadata
                    .as_ref()
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                    .and_then(|v| {
                        v.get("pr_url")
                            .and_then(|w| w.as_str().map(|s| s.to_string()))
                    });
                let body_text = if let Some(ref pr) = pr_url {
                    format!("Worker: {worker_id} · PR: {pr}")
                } else {
                    format!("Worker: {worker_id} · no PR yet")
                };
                items.push(BriefingItem {
                    id: format!("wait-{}-{}", sig.id, worker_id),
                    priority: "action".into(),
                    icon: "⏸".into(),
                    title: sig.title.clone(),
                    body: Some(body_text),
                    workspace: ws.name.clone(),
                    source: format!("swarm:{worker_id}"),
                    url: pr_url.or_else(|| sig.url.clone()),
                    actions: vec![
                        BriefingAction {
                            label: "View PR".into(),
                            style: "primary".into(),
                        },
                        BriefingAction {
                            label: "Snooze".into(),
                            style: "default".into(),
                        },
                    ],
                    timestamp: sig.created_at.to_rfc3339(),
                });
            }
        }

        // PRs with CI pass → check if bot reviewed too → action (deduplicate by URL)
        for sig in by_source.get("github_ci_pass").unwrap_or(&vec![]) {
            let has_review = by_source.get("github_bot_review").is_some_and(|reviews| {
                reviews
                    .iter()
                    .any(|r| sig.url.is_some() && sig.url == r.url)
            });
            if has_review {
                items.push(BriefingItem {
                    id: format!("pr-ready-{}", sig.id),
                    priority: "action".into(),
                    icon: "✅".into(),
                    title: format!("PR ready: {}", sig.title),
                    body: Some("CI green + Copilot reviewed".into()),
                    workspace: ws.name.clone(),
                    source: "github".into(),
                    url: sig.url.clone(),
                    actions: vec![
                        BriefingAction {
                            label: "Review".into(),
                            style: "primary".into(),
                        },
                        BriefingAction {
                            label: "Dismiss".into(),
                            style: "danger".into(),
                        },
                    ],
                    timestamp: sig.created_at.to_rfc3339(),
                });
            }
        }

        // Sentry: group if 3+, otherwise individual notices
        let sentry = by_source.get("sentry").cloned().unwrap_or_default();
        if sentry.len() >= 3 {
            let newest = sentry.iter().max_by_key(|s| s.created_at).unwrap();
            items.push(BriefingItem {
                id: format!("sentry-{}", ws.name),
                priority: "action".into(),
                icon: "⚡".into(),
                title: format!("{} Sentry errors", sentry.len()),
                body: Some(format!(
                    "Latest: {}",
                    &newest.title[..newest.title.len().min(80)]
                )),
                workspace: ws.name.clone(),
                source: "sentry".into(),
                url: newest.url.clone(),
                actions: vec![
                    BriefingAction {
                        label: "Investigate".into(),
                        style: "primary".into(),
                    },
                    BriefingAction {
                        label: "Dismiss".into(),
                        style: "danger".into(),
                    },
                ],
                timestamp: newest.created_at.to_rfc3339(),
            });
        } else {
            for sig in &sentry {
                items.push(BriefingItem {
                    id: format!("sentry-{}", sig.id),
                    priority: "notice".into(),
                    icon: "⚡".into(),
                    title: sig.title.clone(),
                    body: None,
                    workspace: ws.name.clone(),
                    source: "sentry".into(),
                    url: sig.url.clone(),
                    actions: vec![],
                    timestamp: sig.created_at.to_rfc3339(),
                });
            }
        }

        // Bee escalations → action (highest priority)
        for sig in by_source.get("escalation").unwrap_or(&vec![]) {
            items.push(BriefingItem {
                id: format!("escalation-{}", sig.id),
                priority: "action".into(),
                icon: "🚨".into(),
                title: sig.title.clone(),
                body: Some("Escalated by a Bee — needs your attention".into()),
                workspace: ws.name.clone(),
                source: "escalation".into(),
                url: sig.url.clone(),
                actions: vec![
                    BriefingAction {
                        label: "Acknowledge".into(),
                        style: "primary".into(),
                    },
                    BriefingAction {
                        label: "Dismiss".into(),
                        style: "danger".into(),
                    },
                ],
                timestamp: sig.created_at.to_rfc3339(),
            });
        }

        // Bee fix requests → notice (CodeBee is handling it)
        for (source, sigs) in &by_source {
            if source.starts_with("bee_") {
                for sig in sigs {
                    let bee_name = source.strip_prefix("bee_").unwrap_or("Bee");
                    items.push(BriefingItem {
                        id: format!("bee-fix-{}", sig.id),
                        priority: "notice".into(),
                        icon: "🔧".into(),
                        title: format!("{bee_name} requested fix: {}", sig.title),
                        body: Some("CodeBee is dispatching a worker".into()),
                        workspace: ws.name.clone(),
                        source: source.to_string(),
                        url: None,
                        actions: vec![],
                        timestamp: sig.created_at.to_rfc3339(),
                    });
                }
            }
        }

        // Merged PRs → quiet
        for sig in by_source
            .get("github_merged_pr")
            .unwrap_or(&vec![])
            .iter()
            .take(5)
        {
            items.push(BriefingItem {
                id: format!("merged-{}", sig.id),
                priority: "quiet".into(),
                icon: "🔀".into(),
                title: format!("Merged: {}", sig.title),
                body: None,
                workspace: ws.name.clone(),
                source: "github".into(),
                url: sig.url.clone(),
                actions: vec![],
                timestamp: sig.created_at.to_rfc3339(),
            });
        }

        // Active workers → quiet
        for sig in by_source.get("swarm_worker_spawned").unwrap_or(&vec![]) {
            items.push(BriefingItem {
                id: format!("worker-{}", sig.id),
                priority: "quiet".into(),
                icon: "🐝".into(),
                title: sig.title.clone(),
                body: None,
                workspace: ws.name.clone(),
                source: "swarm".into(),
                url: None,
                actions: vec![],
                timestamp: sig.created_at.to_rfc3339(),
            });
        }
    }

    // Deduplicate: group similar items
    {
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| {
            // Extract PR number if present (e.g. "PR #375 ..." → "PR#375")
            let pr_key = item.title.find("#").and_then(|pos| {
                let rest = &item.title[pos + 1..];
                let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if num.is_empty() {
                    None
                } else {
                    Some(format!("PR#{num}"))
                }
            });
            let dedup_key = pr_key.unwrap_or_else(|| item.title.chars().take(25).collect());
            let key = format!("{}:{}:{}", item.workspace, item.source, dedup_key);
            seen.insert(key)
        });
    }

    // Sort: action > notice > quiet, then newest first
    items.sort_by(|a, b| {
        let tier = |p: &str| match p {
            "action" => 0,
            "notice" => 1,
            _ => 2,
        };
        tier(&a.priority)
            .cmp(&tier(&b.priority))
            .then(b.timestamp.cmp(&a.timestamp))
    });

    Json(items)
}

// ── Workers endpoint ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct WorkerView {
    id: String,
    workspace: String,
    branch: String,
    agent: String,
    status: String,
    pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ci_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_comments: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_comments: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_comments: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SwarmStateFile {
    #[serde(default)]
    worktrees: Vec<SwarmWorktreeState>,
}

#[derive(Debug, Deserialize)]
struct SwarmWorktreeState {
    id: String,
    branch: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    agent_kind: String,
    #[serde(default)]
    created_at: Option<chrono::DateTime<chrono::Local>>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    pr: Option<SwarmWorkerPrState>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    agent_session_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SwarmWorkerPrState {
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

fn load_swarm_state(config: &crate::config::WorkspaceConfig) -> Option<SwarmStateFile> {
    let path = swarm_state_path(config);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn worker_status_for_state(worker: &SwarmWorktreeState) -> String {
    match worker.phase.as_deref() {
        Some("running") if worker.agent_session_status.as_deref() == Some("waiting") => {
            "waiting".to_string()
        }
        Some(phase) => phase.to_string(),
        None => worker
            .agent_session_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

fn elapsed_secs(created_at: Option<chrono::DateTime<chrono::Local>>) -> Option<i64> {
    created_at.map(|ts| {
        chrono::Local::now()
            .signed_duration_since(ts)
            .num_seconds()
            .max(0)
    })
}

fn worker_view_from_state(workspace: &str, worker: &SwarmWorktreeState) -> WorkerView {
    WorkerView {
        id: worker.id.clone(),
        workspace: workspace.to_string(),
        branch: worker.branch.clone(),
        agent: if worker.agent_kind.is_empty() {
            "claude".to_string()
        } else {
            worker.agent_kind.clone()
        },
        status: worker_status_for_state(worker),
        pr_url: worker
            .pr
            .as_ref()
            .and_then(|pr| pr.url.clone())
            .filter(|url| !url.is_empty()),
        pr_title: worker.pr.as_ref().and_then(|pr| pr.title.clone()),
        description: worker
            .summary
            .clone()
            .or_else(|| (!worker.prompt.trim().is_empty()).then(|| worker.prompt.clone())),
        elapsed_secs: elapsed_secs(worker.created_at),
        dispatched_by: None,
        review_state: worker.pr.as_ref().and_then(|pr| pr.state.clone()),
        ci_status: None,
        total_comments: None,
        open_comments: None,
        resolved_comments: None,
    }
}

fn load_worker_views(workspace: &str, config: &crate::config::WorkspaceConfig) -> Vec<WorkerView> {
    load_swarm_state(config)
        .map(|state| {
            state
                .worktrees
                .iter()
                .map(|worker| worker_view_from_state(workspace, worker))
                .collect()
        })
        .unwrap_or_default()
}

fn find_worker_state(
    config: &crate::config::WorkspaceConfig,
    worker_id: &str,
) -> Option<SwarmWorktreeState> {
    load_swarm_state(config)?
        .worktrees
        .into_iter()
        .find(|worker| worker.id == worker_id)
}

fn worker_conversation_messages(
    swarm_dir: &std::path::Path,
    worker_id: &str,
) -> Vec<WorkerConversationMessageView> {
    let events_path = swarm_dir
        .join("agents")
        .join(worker_id)
        .join("events.jsonl");
    apiari_tui::events_parser::parse_events(&events_path)
        .into_iter()
        .map(|entry| match entry {
            apiari_tui::conversation::ConversationEntry::User { text, timestamp } => {
                WorkerConversationMessageView {
                    role: "user".to_string(),
                    content: text,
                    timestamp: Some(timestamp),
                }
            }
            apiari_tui::conversation::ConversationEntry::AssistantText { text, timestamp }
            | apiari_tui::conversation::ConversationEntry::Question { text, timestamp } => {
                WorkerConversationMessageView {
                    role: "assistant".to_string(),
                    content: text,
                    timestamp: Some(timestamp),
                }
            }
            apiari_tui::conversation::ConversationEntry::ToolCall { tool, .. } => {
                WorkerConversationMessageView {
                    role: "tool".to_string(),
                    content: tool,
                    timestamp: None,
                }
            }
            apiari_tui::conversation::ConversationEntry::Status { text } => {
                WorkerConversationMessageView {
                    role: "assistant".to_string(),
                    content: text,
                    timestamp: None,
                }
            }
        })
        .collect()
}

fn worker_output_from_conversation(
    conversation: &[WorkerConversationMessageView],
) -> Option<String> {
    conversation
        .iter()
        .rev()
        .find(|entry| entry.role == "assistant")
        .map(|entry| entry.content.clone())
}

/// GET /api/workers — all swarm workers across all workspaces.
async fn get_workers() -> Json<Vec<WorkerView>> {
    let mut workers = Vec::new();
    let workspaces = crate::config::discover_workspaces().unwrap_or_default();

    for ws in &workspaces {
        workers.extend(load_worker_views(&ws.name, &ws.config));
    }

    Json(workers)
}

// ── Worker message endpoint ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WorkerMessageBody {
    workspace: String,
    worker_id: String,
    text: String,
}

/// POST /api/worker/send — send a message to a swarm worker.
async fn send_worker_message(
    Json(body): Json<WorkerMessageBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Find the workspace root
    let workspaces = crate::config::discover_workspaces().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to discover workspaces: {e}"),
        )
    })?;
    let ws = workspaces
        .iter()
        .find(|w| w.name == body.workspace)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("workspace '{}' not found", body.workspace),
            )
        })?;

    // Run swarm send command
    let output = tokio::process::Command::new("swarm")
        .arg("--dir")
        .arg(&ws.config.root)
        .arg("send")
        .arg(&body.worker_id)
        .arg(&body.text)
        .output()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to run swarm: {e}"),
            )
        })?;

    if output.status.success() {
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("swarm send failed: {stderr}"),
        ))
    }
}

// ── Worker activity endpoint ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WorkerActivityQuery {
    workspace: String,
    worker_id: String,
}

#[derive(Debug, Serialize)]
struct WorkerActivityEntry {
    role: String,
    text: String,
}

/// GET /api/worker/activity — worker conversation/tool history from events.jsonl.
async fn get_worker_activity(
    axum::extract::Query(q): axum::extract::Query<WorkerActivityQuery>,
) -> Json<Vec<WorkerActivityEntry>> {
    let workspaces = crate::config::discover_workspaces().unwrap_or_default();
    let ws = match workspaces.iter().find(|w| w.name == q.workspace) {
        Some(w) => w,
        None => return Json(vec![]),
    };

    let state_path = swarm_state_path(&ws.config);
    let state_dir = std::path::Path::new(&state_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));

    let events_path = state_dir.join(format!("agents/{}/events.jsonl", q.worker_id));
    let content = match std::fs::read_to_string(&events_path) {
        Ok(c) => c,
        Err(_) => return Json(vec![]),
    };

    let mut entries = Vec::new();
    for line in content.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let event_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "start" => {
                if let Some(prompt) = val.get("prompt").and_then(|p| p.as_str()) {
                    let preview: String = prompt.chars().take(200).collect();
                    entries.push(WorkerActivityEntry {
                        role: "user".into(),
                        text: format!("{preview}…"),
                    });
                }
            }
            "user_message" => {
                if let Some(text) = val.get("text").and_then(|t| t.as_str())
                    && !text.trim().is_empty()
                {
                    entries.push(WorkerActivityEntry {
                        role: "user".into(),
                        text: text.to_string(),
                    });
                }
            }
            "assistant_text" => {
                if let Some(text) = val.get("text").and_then(|t| t.as_str())
                    && !text.trim().is_empty()
                {
                    entries.push(WorkerActivityEntry {
                        role: "assistant".into(),
                        text: text.to_string(),
                    });
                }
            }
            "tool_use" => {
                let tool = val.get("tool").and_then(|t| t.as_str()).unwrap_or("tool");
                let input = val
                    .get("input")
                    .map(|i| {
                        let s = i.to_string();
                        if s.len() > 200 {
                            format!("{}…", &s[..200])
                        } else {
                            s
                        }
                    })
                    .unwrap_or_default();
                entries.push(WorkerActivityEntry {
                    role: "tool".into(),
                    text: format!("**{tool}** `{input}`"),
                });
            }
            _ => {}
        }
    }

    if entries.len() > 50 {
        entries = entries.split_off(entries.len() - 50);
    }

    Json(entries)
}

// ── Canvas endpoint ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CanvasQuery {
    workspace: String,
    bee: String,
}

/// GET /api/canvas?workspace=apiari&bee=ResearchBee — get a Bee's canvas content.
async fn get_canvas(
    axum::extract::Query(q): axum::extract::Query<CanvasQuery>,
) -> Json<serde_json::Value> {
    let workspaces = crate::config::discover_workspaces().unwrap_or_default();
    let ws = workspaces.iter().find(|w| w.name == q.workspace);
    let content = ws
        .map(|w| w.config.root.join(format!(".apiari/canvas/{}.md", q.bee)))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    Json(serde_json::json!({
        "workspace": q.workspace,
        "bee": q.bee,
        "content": content,
    }))
}

// ── Bee activity endpoint ─────────────────────────────────────────────

/// GET /api/bee-activity — recent autonomous Bee actions (last 24h).
async fn get_bee_activity(State(state): State<HttpState>) -> Json<Vec<BriefingItem>> {
    let mut items = Vec::new();
    let workspaces = crate::config::discover_workspaces().unwrap_or_default();
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();

    for ws in &workspaces {
        let store = match crate::buzz::signal::store::SignalStore::open(&state.db_path, &ws.name) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Resolved signals = Bee auto-dismissed them
        let mut stmt = match store.conn().prepare(
            "SELECT id, source, title, resolved_at FROM signals \
             WHERE workspace = ?1 AND status = 'resolved' AND resolved_at > ?2 \
             ORDER BY resolved_at DESC LIMIT 20",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let rows = stmt
            .query_map(rusqlite::params![&ws.name, &cutoff], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .ok()
            .into_iter()
            .flatten()
            .flatten();

        for (id, source, title, resolved_at) in rows {
            items.push(BriefingItem {
                id: format!("dismissed-{id}"),
                priority: "quiet".into(),
                icon: "✓".into(),
                title: format!(
                    "Auto-dismissed: {}",
                    title.chars().take(60).collect::<String>()
                ),
                body: Some(format!("Source: {source}")),
                workspace: ws.name.clone(),
                source: "bee_activity".into(),
                url: None,
                actions: vec![],
                timestamp: resolved_at,
            });
        }
    }

    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Json(items)
}

// ── Signals endpoint ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SignalView {
    id: i64,
    workspace: String,
    source: String,
    title: String,
    severity: String,
    status: String,
    url: Option<String>,
    created_at: String,
    updated_at: String,
    resolved_at: Option<String>,
}

/// GET /api/signals?workspace=mgm&limit=50 — recent signals for the Briefing feed.
async fn get_signals(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
) -> Json<Vec<SignalView>> {
    let workspace = q.workspace.as_deref().unwrap_or(state.workspace.as_str());
    let limit = q.limit.unwrap_or(50).min(200);
    let include_history = q.history.unwrap_or(false);

    let store = match crate::buzz::signal::store::SignalStore::open(&state.db_path, workspace) {
        Ok(s) => s,
        Err(_) => return Json(vec![]),
    };

    let signals = if include_history {
        store.get_signal_history(limit).unwrap_or_default()
    } else {
        store.get_open_signals().unwrap_or_default()
    };
    let views: Vec<SignalView> = signals
        .iter()
        .take(limit)
        .map(|s| SignalView {
            id: s.id,
            workspace: workspace.to_string(),
            source: s.source.clone(),
            title: s.title.clone(),
            severity: format!("{:?}", s.severity),
            status: format!("{:?}", s.status),
            url: s.url.clone(),
            created_at: s.created_at.to_rfc3339(),
            updated_at: s.updated_at.to_rfc3339(),
            resolved_at: s.resolved_at.map(|value| value.to_rfc3339()),
        })
        .collect();

    Json(views)
}

/// GET /api/providers/capabilities — inspect the locally installed provider CLIs.
async fn get_provider_capabilities() -> Json<Vec<ProviderCapabilityView>> {
    let mut items = Vec::with_capacity(3);
    items.push(probe_provider_capability("claude", "claude").await);
    items.push(probe_provider_capability("codex", "codex").await);
    items.push(probe_provider_capability("gemini", "gemini").await);
    Json(items)
}

/// GET /api/workspaces/:workspace/bots/:bot/debug — recent failures + status + recent messages.
async fn get_workspace_bot_debug(
    Path((workspace, bot)): Path<(String, String)>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<BotDebugView> {
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .min(100);

    let provider = load_workspace_by_name(&workspace).and_then(|ws| {
        let resolved = resolve_bee_name_for_api(&ws.config, &bot).unwrap_or(bot.clone());
        ws.config
            .resolved_bees()
            .into_iter()
            .find(|bee| bee.name == resolved)
            .map(|bee| bee.provider)
    });

    let store = match crate::buzz::signal::store::SignalStore::open(
        &crate::config::db_path(),
        &workspace,
    ) {
        Ok(store) => store,
        Err(_) => {
            return Json(BotDebugView {
                workspace,
                bot,
                provider,
                status: None,
                recent_failures: vec![],
                recent_decisions: vec![],
                recent_messages: vec![],
            });
        }
    };

    let status = store
        .get_bot_status(&bot)
        .ok()
        .flatten()
        .map(|status| BotStatusView {
            status: status.status,
            streaming_content: status.streaming_content,
            tool_name: status.tool_name,
        });

    let recent_failures = store
        .list_bot_turn_failures(&bot, limit)
        .unwrap_or_default()
        .into_iter()
        .map(|failure| BotTurnFailureView {
            id: failure.id,
            bot: failure.bot,
            provider: failure.provider,
            source: failure.source,
            error_text: failure.error_text,
            created_at: failure.created_at,
        })
        .collect();

    let recent_decisions = store
        .list_bot_turn_decisions(&bot, limit)
        .unwrap_or_default()
        .into_iter()
        .map(|decision| BotTurnDecisionView {
            id: decision.id,
            bot: decision.bot,
            provider: decision.provider,
            decision_type: decision.decision_type,
            detail: decision.detail,
            created_at: decision.created_at,
        })
        .collect();

    let recent_messages = get_workspace_conversations(
        Path((workspace.clone(), bot.clone())),
        axum::extract::Query(std::collections::HashMap::from([(
            "limit".to_string(),
            limit.to_string(),
        )])),
    )
    .await
    .0;

    Json(BotDebugView {
        workspace,
        bot,
        provider,
        status,
        recent_failures,
        recent_decisions,
        recent_messages,
    })
}

// ── Conversation history ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ConversationMessage {
    role: String,
    content: String,
    source: Option<String>,
    bee: String,
    workspace: String,
    created_at: String,
}

/// GET /api/conversations?workspace=apiari&limit=50 — load recent chat history.
async fn get_conversations(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
) -> Json<Vec<ConversationMessage>> {
    let workspace = q.workspace.as_deref().unwrap_or(state.workspace.as_str());

    let db_path = &*state.db_path;
    let store = match crate::buzz::signal::store::SignalStore::open(db_path, workspace) {
        Ok(s) => s,
        Err(_) => return Json(vec![]),
    };

    // Load conversations for all bees in this workspace
    // Keys are either "workspace" (legacy) or "workspace/BeeName"
    let mut messages = Vec::new();

    // Try legacy key first
    let conv = crate::buzz::conversation::ConversationStore::new(store.conn(), workspace);
    if let Ok(rows) = conv.load_history(50) {
        for row in rows {
            messages.push(ConversationMessage {
                role: row.role,
                content: row.content,
                source: row.source,
                bee: "Bee".to_string(),
                workspace: workspace.to_string(),
                created_at: row.created_at,
            });
        }
    }

    // Try per-bee keys
    if let Ok(workspaces) = crate::config::discover_workspaces()
        && let Some(ws) = workspaces.iter().find(|w| w.name == workspace)
    {
        for bee in ws.config.resolved_bees() {
            let key = format!("{workspace}/{}", bee.name);
            let conv = crate::buzz::conversation::ConversationStore::new(store.conn(), &key);
            if let Ok(rows) = conv.load_history(50) {
                for row in rows {
                    messages.push(ConversationMessage {
                        role: row.role,
                        content: row.content,
                        source: row.source,
                        bee: bee.name.clone(),
                        workspace: workspace.to_string(),
                        created_at: row.created_at,
                    });
                }
            }
        }
    }

    // Sort by timestamp
    messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    // Keep last 50
    if messages.len() > 50 {
        messages = messages.split_off(messages.len() - 50);
    }

    Json(messages)
}

// ── Chat handler ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatBody {
    workspace: String,
    #[serde(default)]
    bee: Option<String>,
    text: String,
}

/// POST /api/chat — send a message to a Bee, stream the response as SSE.
async fn chat_handler(
    State(state): State<HttpState>,
    Json(body): Json<ChatBody>,
) -> Sse<impl futures::stream::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<WebChatEvent>();

    let req = WebChatRequest {
        workspace: body.workspace,
        bee: body.bee,
        text: body.text,
        attachments_json: None,
        attachments: vec![],
        response_tx,
    };

    let send_failed = state.chat_tx.send(req).is_err();

    let stream = async_stream::stream! {
        if send_failed {
            let err_event = WebChatEvent::Error { text: "daemon chat channel closed".into() };
            let data = serde_json::to_string(&err_event).unwrap_or_default();
            yield Ok(SseEvent::default().data(data));
            return;
        }

        while let Some(event) = response_rx.recv().await {
            let is_terminal = matches!(event, WebChatEvent::Done | WebChatEvent::Error { .. });
            let data = serde_json::to_string(&event).unwrap_or_default();
            yield Ok(SseEvent::default().data(data));
            if is_terminal {
                break;
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Workflow run handler ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WorkflowRunBody {
    workspace: String,
    #[serde(default)]
    bee: Option<String>,
    topic: String,
    #[serde(default)]
    lane: Option<String>,
}

/// POST /api/workflow/run — execute a workflow lane step-by-step via a Bee.
///
/// Walks the workflow graph from entry, following the specified lane.
/// For each action node, sends the node's prompt + user's topic to the Bee
/// and streams the response. Accumulates context across steps.
async fn workflow_run_handler(
    State(state): State<HttpState>,
    Json(body): Json<WorkflowRunBody>,
) -> Sse<impl futures::stream::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let graph = state.graph.read().await.clone();
    let chat_tx = state.chat_tx.clone();
    let db_path = state.db_path.clone();
    let updates_tx = state.updates_tx.clone();

    let stream = async_stream::stream! {
        // Find the entry node
        let entry_id = graph.nodes.iter()
            .find(|(_, n)| n.node_type == crate::buzz::orchestrator::graph::NodeType::Entry)
            .map(|(id, _)| id.clone());

        let Some(entry_id) = entry_id else {
            let data = serde_json::to_string(&WebChatEvent::Error {
                text: "no entry node in workflow".into(),
            }).unwrap_or_default();
            yield Ok(SseEvent::default().data(data));
            return;
        };

        // Walk from entry, find action nodes in the target lane.
        // Follow edges linearly, collecting action nodes with prompts.
        let mut steps: Vec<(String, String, String)> = Vec::new(); // (node_id, label, prompt)
        let mut current = entry_id;
        let mut visited = std::collections::HashSet::new();

        loop {
            if visited.contains(&current) { break; }
            visited.insert(current.clone());

            let Some(node) = graph.nodes.get(&current) else { break; };

            if let Some(ref action) = node.action {
                let description = format!("{:?}", action.kind).to_lowercase();
                let in_lane = body.lane.as_ref().is_none_or(|lane| {
                    action.role.as_deref() == Some(lane.as_str())
                        || node.label.to_lowercase().contains(&lane.to_lowercase())
                });
                if in_lane {
                    steps.push((current.clone(), node.label.clone(), description));
                }
            }

            if node.node_type == crate::buzz::orchestrator::graph::NodeType::Terminal {
                break;
            }

            // Follow edges — when a lane is specified, prefer edges leading
            // to nodes with matching role (so we branch into the right lane
            // from the entry node instead of following the default path).
            let mut outgoing: Vec<&crate::buzz::orchestrator::graph::Edge> = graph.edges.iter()
                .filter(|e| e.from == current)
                .collect();
            outgoing.sort_by_key(|e| e.priority);

            let next = if let Some(ref lane) = body.lane {
                // Try to find an edge leading to a node whose action.role matches the lane
                outgoing.iter().find(|e| {
                    graph.nodes.get(&e.to).is_some_and(|n| {
                        n.action.as_ref().is_some_and(|a| {
                            a.role.as_deref() == Some(lane.as_str())
                        })
                    })
                }).or_else(|| {
                    // Fallback: unconditional, then any
                    outgoing.iter().find(|e| e.condition.is_none()).or(outgoing.first())
                })
            } else {
                outgoing.iter().find(|e| e.condition.is_none()).or(outgoing.first())
            };

            if let Some(edge) = next {
                current = edge.to.clone();
            } else {
                break;
            }
        }

        if steps.is_empty() {
            let data = serde_json::to_string(&WebChatEvent::Error {
                text: "no action steps found in workflow lane".into(),
            }).unwrap_or_default();
            yield Ok(SseEvent::default().data(data));
            return;
        }

        // Create a task to track this workflow run
        let task_id = uuid::Uuid::new_v4().to_string();
        let first_step = &steps[0].0;
        let cursor = crate::buzz::orchestrator::graph::walker::GraphCursor {
            current_node: first_step.clone(),
            counters: std::collections::HashMap::new(),
            artifacts: std::collections::HashMap::new(),
            history: Vec::new(),
        };
        let task = crate::buzz::task::Task {
            id: task_id.clone(),
            workspace: body.workspace.clone(),
            title: body.topic.clone(),
            stage: crate::buzz::task::TaskStage::InProgress,
            source: Some("workflow".to_string()),
            source_url: None,
            worker_id: body.bee.clone(),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            resolved_at: None,
            metadata: serde_json::json!({ "graph_cursor": cursor }),
        };
        if let Ok(store) = crate::buzz::task::store::TaskStore::open(&db_path) {
            let _ = store.create_task(&task);
            // Broadcast task creation to web UI
            let _ = updates_tx.send(WsUpdate::TaskUpdated {
                task: task_to_view(&task),
            });
        }

        let mut accumulated = String::new();

        for (i, (node_id, label, prompt)) in steps.iter().enumerate() {
            // Update cursor position
            let cursor = crate::buzz::orchestrator::graph::walker::GraphCursor {
                current_node: node_id.clone(),
                counters: std::collections::HashMap::new(),
                artifacts: std::collections::HashMap::new(),
                history: steps[..=i].iter().enumerate().filter(|(j, _)| *j > 0).map(|(_, (nid, _, _))| {
                    crate::buzz::orchestrator::graph::walker::CursorStep {
                        from_node: steps[i.saturating_sub(1)].0.clone(),
                        to_node: nid.clone(),
                        timestamp: chrono::Utc::now(),
                        trigger: "workflow_run".to_string(),
                    }
                }).collect(),
            };
            if let Ok(store) = crate::buzz::task::store::TaskStore::open(&db_path) {
                let metadata = serde_json::json!({ "graph_cursor": cursor });
                let _ = store.update_task_metadata(&task_id, &metadata);
                // Broadcast updated cursor position
                let mut updated_task = task.clone();
                updated_task.metadata = metadata;
                let _ = updates_tx.send(WsUpdate::TaskUpdated {
                    task: task_to_view(&updated_task),
                });
            }

            // Emit step start
            let start = serde_json::to_string(&WebChatEvent::StepStart {
                step: node_id.clone(),
                label: label.clone(),
            }).unwrap_or_default();
            yield Ok(SseEvent::default().data(start));

            // Build the message for this step
            let step_message = if accumulated.is_empty() {
                format!("[Step: {label}]\n{prompt}\n\nTopic: {}", body.topic)
            } else {
                format!(
                    "[Step: {label}]\n{prompt}\n\nOriginal topic: {}\n\nFindings so far:\n{accumulated}",
                    body.topic
                )
            };

            // Send to coordinator via chat channel
            let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<WebChatEvent>();
            let req = WebChatRequest {
                workspace: body.workspace.clone(),
                bee: body.bee.clone(),
                text: step_message,
                attachments_json: None,
                attachments: vec![],
                response_tx: resp_tx,
            };

            if chat_tx.send(req).is_err() {
                let data = serde_json::to_string(&WebChatEvent::Error {
                    text: "coordinator unavailable".into(),
                }).unwrap_or_default();
                yield Ok(SseEvent::default().data(data));
                return;
            }

            // Stream tokens for this step
            let mut step_text = String::new();
            while let Some(event) = resp_rx.recv().await {
                match &event {
                    WebChatEvent::Token { text } => {
                        step_text.push_str(text);
                        let data = serde_json::to_string(&event).unwrap_or_default();
                        yield Ok(SseEvent::default().data(data));
                    }
                    WebChatEvent::Done => break,
                    WebChatEvent::Error { .. } => {
                        let data = serde_json::to_string(&event).unwrap_or_default();
                        yield Ok(SseEvent::default().data(data));
                        return;
                    }
                    _ => {}
                }
            }

            accumulated.push_str(&format!("\n## {label}\n{step_text}\n"));

            // Emit step done
            let done = serde_json::to_string(&WebChatEvent::StepDone {
                step: node_id.clone(),
            }).unwrap_or_default();
            yield Ok(SseEvent::default().data(done));
        }

        // All steps complete — mark task as done
        if let Ok(store) = crate::buzz::task::store::TaskStore::open(&db_path) {
            let _ = store.update_task_stage(&task_id, &crate::buzz::task::TaskStage::Merged);
            let mut final_task = task.clone();
            final_task.stage = crate::buzz::task::TaskStage::Merged;
            let _ = updates_tx.send(WsUpdate::TaskUpdated {
                task: task_to_view(&final_task),
            });
        }

        // Ask the Bee to update its canvas with the results
        {
            let canvas_prompt = format!(
                "You just completed a workflow. Here are your accumulated findings:\n\n\
                 {accumulated}\n\n\
                 Write a clean, well-organized summary of these findings for your canvas. \
                 Put your summary inside [CANVAS] and [/CANVAS] tags. Make it useful — \
                 something the user can reference later. Use markdown headers, bullet points, \
                 and links. Be concise but thorough."
            );
            let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<WebChatEvent>();
            let req = WebChatRequest {
                workspace: body.workspace.clone(),
                bee: body.bee.clone(),
                text: canvas_prompt,
                attachments_json: None,
                attachments: vec![],
                response_tx: resp_tx,
            };
            if chat_tx.send(req).is_ok() {
                // Collect the canvas response (don't stream it — it's a system action)
                let mut canvas_response = String::new();
                while let Some(event) = resp_rx.recv().await {
                    match event {
                        WebChatEvent::Token { text } => canvas_response.push_str(&text),
                        WebChatEvent::Done | WebChatEvent::Error { .. } => break,
                        _ => {}
                    }
                }
                // Parse and execute canvas action
                let actions = crate::buzz::coordinator::actions::parse_actions(&canvas_response);
                for action in &actions {
                    if let crate::buzz::coordinator::actions::BeeAction::Canvas { content } = action {
                        // Write canvas file
                        if let Ok(workspaces) = crate::config::discover_workspaces()
                            && let Some(ws) = workspaces.iter().find(|w| w.name == body.workspace)
                        {
                                let canvas_dir = ws.config.root.join(".apiari/canvas");
                                let _ = std::fs::create_dir_all(&canvas_dir);
                                let bee_name = body.bee.as_deref().unwrap_or("Bee");
                                let path = canvas_dir.join(format!("{bee_name}.md"));
                                // Prepend new content with date header (don't overwrite)
                                let date = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
                                let new_section = format!("## {date} — {}\n\n{content}\n\n---\n\n", body.topic);
                                let existing = std::fs::read_to_string(&path).unwrap_or_default();
                                let _ = std::fs::write(&path, format!("{new_section}{existing}"));
                                tracing::info!("[{}/{bee_name}] canvas updated after workflow", body.workspace);
                        }
                    }
                }
            }
        }

        let data = serde_json::to_string(&WebChatEvent::Done).unwrap_or_default();
        yield Ok(SseEvent::default().data(data));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Briefing action handlers ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DismissBody {
    signal_id: i64,
    workspace: String,
}

#[derive(Debug, Deserialize)]
struct SnoozeBody {
    signal_id: i64,
    workspace: String,
    #[serde(default = "default_snooze_hours")]
    hours: u64,
}

fn default_snooze_hours() -> u64 {
    1
}

/// POST /api/briefing/dismiss — resolve a signal so it disappears from briefing.
async fn dismiss_signal(
    State(state): State<HttpState>,
    Json(body): Json<DismissBody>,
) -> impl IntoResponse {
    let store = match crate::buzz::signal::store::SignalStore::open(&state.db_path, &body.workspace)
    {
        Ok(s) => s,
        Err(e) => {
            return Json(
                serde_json::json!({"ok": false, "error": format!("failed to open store: {e}")}),
            );
        }
    };
    if let Err(e) = store.resolve_signal(body.signal_id) {
        return Json(serde_json::json!({"ok": false, "error": e.to_string()}));
    }
    info!(
        "[http] dismissed signal {} in workspace {}",
        body.signal_id, body.workspace
    );
    Json(serde_json::json!({"ok": true}))
}

/// POST /api/briefing/snooze — snooze a signal for N hours.
async fn snooze_signal(
    State(state): State<HttpState>,
    Json(body): Json<SnoozeBody>,
) -> impl IntoResponse {
    let store = match crate::buzz::signal::store::SignalStore::open(&state.db_path, &body.workspace)
    {
        Ok(s) => s,
        Err(e) => {
            return Json(
                serde_json::json!({"ok": false, "error": format!("failed to open store: {e}")}),
            );
        }
    };
    let until = chrono::Utc::now() + chrono::Duration::hours(body.hours as i64);
    if let Err(e) = store.snooze_signal(body.signal_id, until) {
        return Json(serde_json::json!({"ok": false, "error": e.to_string()}));
    }
    info!(
        "[http] snoozed signal {} for {}h in workspace {}",
        body.signal_id, body.hours, body.workspace
    );
    Json(serde_json::json!({"ok": true}))
}

// ── Server setup ───────────────────────────────────────────────────────

/// Start the HTTP server. Returns channels for the daemon to consume.
pub async fn start_http_server(
    graph: WorkflowGraph,
    yaml_path: Option<std::path::PathBuf>,
    db_path: std::path::PathBuf,
    workspace: String,
    port: u16,
) -> color_eyre::Result<(
    broadcast::Sender<WsUpdate>,
    mpsc::UnboundedReceiver<InjectSignal>,
    mpsc::UnboundedReceiver<WebChatRequest>,
    mpsc::UnboundedReceiver<WebCancelRequest>,
)> {
    let (updates_tx, _) = broadcast::channel(256);
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let (chat_tx, chat_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = mpsc::unbounded_channel();

    let state = HttpState {
        graph: Arc::new(RwLock::new(graph)),
        yaml_path: Arc::new(yaml_path),
        db_path: Arc::new(db_path),
        workspace: Arc::new(workspace),
        updates_tx: updates_tx.clone(),
        signal_tx,
        chat_tx,
        cancel_tx,
    };

    let app = Router::new()
        .route("/api/graph", get(get_graph).put(save_graph))
        .route("/api/graph/yaml", get(get_yaml))
        .route("/api/tasks", get(get_tasks).delete(clear_tasks))
        .route("/api/signal", post(inject_signal))
        .route("/api/workspaces", get(list_workspaces))
        .route("/api/workspaces/{workspace}/bots", get(list_workspace_bots))
        .route(
            "/api/workspaces/{workspace}/repos",
            get(list_workspace_repos),
        )
        .route(
            "/api/workspaces/{workspace}/workers",
            get(get_workspace_workers),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}",
            get(get_workspace_worker_detail),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/send",
            post(send_workspace_worker_message),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/diff",
            get(get_workspace_worker_diff),
        )
        .route("/api/workspaces/{workspace}/docs", get(list_workspace_docs))
        .route(
            "/api/workspaces/{workspace}/docs/{filename}",
            get(get_workspace_doc)
                .put(save_workspace_doc)
                .delete(delete_workspace_doc),
        )
        .route(
            "/api/workspaces/{workspace}/conversations/{bot}",
            get(get_workspace_conversations),
        )
        .route(
            "/api/workspaces/{workspace}/bots/{bot}/status",
            get(get_workspace_bot_status),
        )
        .route(
            "/api/workspaces/{workspace}/bots/{bot}/debug",
            get(get_workspace_bot_debug),
        )
        .route(
            "/api/workspaces/{workspace}/bots/{bot}/cancel",
            post(cancel_workspace_bot),
        )
        .route(
            "/api/workspaces/{workspace}/unread",
            get(get_workspace_unread),
        )
        .route(
            "/api/workspaces/{workspace}/seen/{bot}",
            post(mark_workspace_seen),
        )
        .route(
            "/api/workspaces/{workspace}/followups",
            get(list_workspace_followups),
        )
        .route(
            "/api/workspaces/{workspace}/followups/{followup_id}",
            axum::routing::delete(cancel_workspace_followup),
        )
        .route(
            "/api/workspaces/{workspace}/research",
            get(list_workspace_research).post(start_workspace_research),
        )
        .route(
            "/api/workspaces/{workspace}/chat/{bot}",
            post(send_workspace_chat),
        )
        .route("/api/usage", get(get_usage))
        .route("/api/chat", post(chat_handler))
        .route("/api/workflow/run", post(workflow_run_handler))
        .route("/api/briefing", get(get_briefing))
        .route("/api/bee-activity", get(get_bee_activity))
        .route("/api/canvas", get(get_canvas))
        .route("/api/workers", get(get_workers))
        .route("/api/worker/activity", get(get_worker_activity))
        .route("/api/worker/send", post(send_worker_message))
        .route("/api/briefing/dismiss", post(dismiss_signal))
        .route("/api/briefing/snooze", post(snooze_signal))
        .route(
            "/api/providers/capabilities",
            get(get_provider_capabilities),
        )
        .route("/api/signals", get(get_signals))
        .route("/api/conversations", get(get_conversations))
        .route("/api/bees", get(get_bees).put(save_bees))
        .route("/api/ws", get(ws_handler))
        .route("/ws", get(ws_handler))
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("[http] listening on http://0.0.0.0:{port}");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("[http] server error: {e}");
        }
    });

    Ok((updates_tx, signal_rx, chat_rx, cancel_rx))
}

/// Run a standalone dev server — no daemon needed.
pub async fn run_dev_server(port: u16) -> color_eyre::Result<()> {
    use chrono::Utc;

    use crate::buzz::{
        orchestrator::{Orchestrator, OrchestratorConfig, graph::builtin::builtin_workflow},
        signal::{self, Severity, SignalStatus},
        task::store::TaskStore,
    };

    let graph = builtin_workflow();
    let config = OrchestratorConfig::default();
    let orchestrator = Orchestrator::with_graph(&config, graph.clone());
    let workspace = "dev";

    // Create a temp dir for DB + YAML
    let tmp_dir = std::env::temp_dir().join("apiari-web-dev");
    std::fs::create_dir_all(&tmp_dir)?;
    let db_path = tmp_dir.join("dev.db");
    let yaml_path = tmp_dir.join("workflow.yaml");

    // Write initial YAML so it can be edited
    let initial_yaml = serde_yaml::to_string(&graph)?;
    std::fs::write(&yaml_path, &initial_yaml)?;

    // Ensure schema exists
    let _ = TaskStore::open(&db_path)?;

    eprintln!("apiari daemon API dev server");
    eprintln!("  API:      http://127.0.0.1:{port}");
    eprintln!("  DB:       {}", db_path.display());
    eprintln!("  YAML:     {}", yaml_path.display());
    eprintln!(
        "  Graph:    {} ({} nodes, {} edges)",
        graph.name,
        graph.nodes.len(),
        graph.edges.len()
    );
    eprintln!();
    eprintln!("Run `cd web && npm run dev` for the React UI on http://localhost:5173");
    eprintln!("Press Ctrl+C to stop.");
    eprintln!();

    let (updates_tx, mut signal_rx, _chat_rx, _cancel_rx) = start_http_server(
        graph,
        Some(yaml_path),
        db_path.clone(),
        workspace.to_string(),
        port,
    )
    .await?;

    // Process injected signals through the real orchestrator
    loop {
        tokio::select! {
            Some(sig) = signal_rx.recv() => {
                let store = match TaskStore::open(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("failed to open task store: {e}");
                        continue;
                    }
                };

                let now = Utc::now();
                let signal = signal::SignalRecord {
                    id: now.timestamp_millis(),
                    source: sig.source.clone(),
                    external_id: format!("dev-{}", now.timestamp_millis()),
                    title: sig.title.clone(),
                    body: None,
                    severity: Severity::Info,
                    status: SignalStatus::Open,
                    url: None,
                    created_at: now,
                    updated_at: now,
                    resolved_at: None,
                    metadata: sig.metadata.map(|m| m.to_string()),
                    snoozed_until: None,
                };

                match orchestrator.process_signal(&store, workspace, &signal) {
                    Ok(result) => {
                        info!(
                            "[dev] processed signal '{}': transitioned={}, workflow_actions={}",
                            sig.source,
                            result.engine_result.transitioned,
                            result.workflow_actions.len(),
                        );

                        if let Some(task) = &result.engine_result.task {
                            let _ = updates_tx.send(WsUpdate::TaskUpdated {
                                task: task_to_view(task),
                            });
                        }
                        let _ = updates_tx.send(WsUpdate::SignalProcessed {
                            source: sig.source,
                            title: sig.title,
                        });
                    }
                    Err(e) => {
                        error!("[dev] failed to process signal: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nshutting down");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        process::Command,
        sync::{Mutex, OnceLock},
    };

    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct HomeGuard {
        previous_home: Option<std::ffi::OsString>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous_home.take() {
                Some(home) => unsafe { std::env::set_var("HOME", home) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    fn install_temp_home(home: &Path) -> HomeGuard {
        let previous_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", home) };
        HomeGuard { previous_home }
    }

    fn write_workspace_file(home: &Path, name: &str, body: &str) -> PathBuf {
        let workspaces_dir = home.join(".config/hive/workspaces");
        fs::create_dir_all(&workspaces_dir).unwrap();
        let path = workspaces_dir.join(format!("{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn write_minimal_workspace(home: &Path, name: &str, root: &Path) -> PathBuf {
        write_workspace_file(
            home,
            name,
            &format!("root = {:?}\n", root.display().to_string()),
        )
    }

    fn write_swarm_state(path: &Path, body: &serde_json::Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, serde_json::to_vec_pretty(body).unwrap()).unwrap();
    }

    fn write_doc(root: &Path, name: &str, content: &str) {
        let docs_dir = root.join("docs");
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(docs_dir.join(name), content).unwrap();
    }

    fn write_apiari_doc(root: &Path, name: &str, content: &str) {
        let docs_dir = root.join(".apiari/docs");
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(docs_dir.join(name), content).unwrap();
    }

    fn init_git_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
    }

    fn add_origin(path: &Path, origin: &str) {
        Command::new("git")
            .args(["remote", "add", "origin", origin])
            .current_dir(path)
            .status()
            .unwrap();
    }

    fn test_workspace_config(
        root: std::path::PathBuf,
        repos: Vec<&str>,
    ) -> crate::config::WorkspaceConfig {
        crate::config::WorkspaceConfig {
            config_version: None,
            root,
            repos: repos.into_iter().map(str::to_string).collect(),
            authority: crate::config::WorkspaceAuthority::default(),
            capabilities: crate::config::WorkspaceCapabilities::default(),
            telegram: None,
            coordinator: crate::config::CoordinatorConfig::default(),
            bees: None,
            watchers: crate::config::WatchersConfig::default(),
            swarm: crate::config::SwarmConfig::default(),
            orchestrator: Default::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
            shells: crate::config::ShellsConfig::default(),
            schedule: None,
            activity: crate::config::ActivityConfig::default(),
        }
    }

    #[test]
    fn repo_slug_to_local_path_prefers_matching_origin_over_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("apiari");
        let common = root.join("common");

        init_git_repo(&root);
        add_origin(&root, "git@github.com:ApiariTools/apiari.git");

        init_git_repo(&common);
        add_origin(&common, "git@github.com:ApiariTools/apiari-common.git");

        let resolved = repo_slug_to_local_path(&root, "ApiariTools/apiari-common");
        assert_eq!(resolved, common);
    }

    #[test]
    fn display_bee_name_maps_default_bee_to_main() {
        let bees = vec![crate::config::BeeConfig {
            name: "Bee".to_string(),
            role: None,
            color: None,
            execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            max_turns: 20,
            prompt: None,
            max_session_turns: 50,
            signal_hooks: vec![],
            topic_id: None,
            heartbeat: None,
            heartbeat_prompt: None,
        }];

        assert_eq!(display_bee_name(&bees, &bees[0]), "Main");
    }

    #[test]
    fn resolve_bee_name_for_api_prefers_default_bee_for_main() {
        let mut config = test_workspace_config(std::path::PathBuf::from("/tmp/apiari"), vec![]);
        config.bees = Some(vec![
            crate::config::BeeConfig {
                name: "Bee".to_string(),
                role: None,
                color: None,
                execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
                provider: "claude".to_string(),
                model: "sonnet".to_string(),
                max_turns: 20,
                prompt: None,
                max_session_turns: 50,
                signal_hooks: vec![],
                topic_id: None,
                heartbeat: None,
                heartbeat_prompt: None,
            },
            crate::config::BeeConfig {
                name: "Codex".to_string(),
                role: None,
                color: None,
                execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
                provider: "codex".to_string(),
                model: "gpt-5.3-codex".to_string(),
                max_turns: 20,
                prompt: None,
                max_session_turns: 50,
                signal_hooks: vec![],
                topic_id: None,
                heartbeat: None,
                heartbeat_prompt: None,
            },
        ]);

        assert_eq!(
            resolve_bee_name_for_api(&config, "Main").as_deref(),
            Some("Bee")
        );
        assert_eq!(
            resolve_bee_name_for_api(&config, "Codex").as_deref(),
            Some("Codex")
        );
    }

    #[test]
    fn resolve_bee_name_for_api_falls_back_to_first_bee_when_default_missing() {
        let mut config = test_workspace_config(std::path::PathBuf::from("/tmp/apiari"), vec![]);
        config.bees = Some(vec![crate::config::BeeConfig {
            name: "Claude".to_string(),
            role: None,
            color: None,
            execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            max_turns: 20,
            prompt: None,
            max_session_turns: 50,
            signal_hooks: vec![],
            topic_id: None,
            heartbeat: None,
            heartbeat_prompt: None,
        }]);

        assert_eq!(
            resolve_bee_name_for_api(&config, "Main").as_deref(),
            Some("Claude")
        );
    }

    #[test]
    fn bot_items_for_workspace_exposes_main_name_for_default_bee() {
        let mut config = test_workspace_config(std::path::PathBuf::from("/tmp/apiari"), vec![]);
        config.bees = Some(vec![
            crate::config::BeeConfig {
                name: "Bee".to_string(),
                role: Some("Coordinator".to_string()),
                color: Some("#f5c542".to_string()),
                execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
                provider: "claude".to_string(),
                model: "sonnet".to_string(),
                max_turns: 20,
                prompt: Some("Default coordinator".to_string()),
                max_session_turns: 50,
                signal_hooks: vec![],
                topic_id: None,
                heartbeat: None,
                heartbeat_prompt: None,
            },
            crate::config::BeeConfig {
                name: "Codex".to_string(),
                role: Some("Code specialist".to_string()),
                color: Some("#5b9bd5".to_string()),
                execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
                provider: "codex".to_string(),
                model: "gpt-5.3-codex".to_string(),
                max_turns: 20,
                prompt: Some("Code specialist".to_string()),
                max_session_turns: 50,
                signal_hooks: vec![],
                topic_id: None,
                heartbeat: None,
                heartbeat_prompt: None,
            },
        ]);

        let items = bot_items_for_workspace(&config);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Main");
        assert_eq!(items[0].role.as_deref(), Some("Coordinator"));
        assert_eq!(items[0].color.as_deref(), Some("#f5c542"));
        assert_eq!(items[0].provider.as_deref(), Some("claude"));
        assert_eq!(items[1].name, "Codex");
        assert_eq!(items[1].role.as_deref(), Some("Code specialist"));
        assert_eq!(items[1].provider.as_deref(), Some("codex"));
    }

    #[test]
    fn build_repo_list_items_uses_local_dir_name_without_duplicate_root_repo() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("apiari");
        let common = root.join("common");

        init_git_repo(&root);
        add_origin(&root, "git@github.com:ApiariTools/apiari.git");

        init_git_repo(&common);
        add_origin(&common, "git@github.com:ApiariTools/apiari-common.git");

        let config = test_workspace_config(
            root.clone(),
            vec!["ApiariTools/apiari", "ApiariTools/apiari-common"],
        );
        let workers = vec![
            WorkerView {
                id: "worker-apiari".to_string(),
                workspace: "ws".to_string(),
                branch: "apiari/fix-http".to_string(),
                agent: "claude".to_string(),
                status: "running".to_string(),
                pr_url: None,
                pr_title: None,
                description: None,
                elapsed_secs: None,
                dispatched_by: None,
                review_state: None,
                ci_status: None,
                total_comments: None,
                open_comments: None,
                resolved_comments: None,
            },
            WorkerView {
                id: "worker-common".to_string(),
                workspace: "ws".to_string(),
                branch: "common/fix-sdk".to_string(),
                agent: "gemini".to_string(),
                status: "running".to_string(),
                pr_url: None,
                pr_title: None,
                description: None,
                elapsed_secs: None,
                dispatched_by: None,
                review_state: None,
                ci_status: None,
                total_comments: None,
                open_comments: None,
                resolved_comments: None,
            },
        ];

        let repos = build_repo_list_items(&config, &workers);
        let names: Vec<_> = repos.iter().map(|repo| repo.name.as_str()).collect();

        assert_eq!(names, vec!["apiari", "common"]);
        assert_eq!(repos[0].path, root.display().to_string());
        assert_eq!(repos[1].path, common.display().to_string());
        assert_eq!(repos[0].workers.len(), 1);
        assert_eq!(repos[0].workers[0].id, "worker-apiari");
        assert_eq!(repos[1].workers.len(), 1);
        assert_eq!(repos[1].workers[0].id, "worker-common");
    }

    #[test]
    fn swarm_state_path_defaults_to_workspace_root_when_watcher_config_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("apiari");
        let config = test_workspace_config(root.clone(), vec!["ApiariTools/apiari"]);

        assert_eq!(swarm_state_path(&config), root.join(".swarm/state.json"));
    }

    #[test]
    fn swarm_state_path_prefers_explicit_watcher_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("apiari");
        let explicit = temp.path().join("custom/state.json");
        let mut config = test_workspace_config(root, vec!["ApiariTools/apiari"]);
        config.watchers.swarm = Some(crate::config::SwarmWatcherConfig {
            state_path: explicit.clone(),
            interval_secs: 30,
            active_hours: None,
        });

        assert_eq!(swarm_state_path(&config), explicit);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn list_workspaces_reads_workspace_contract_from_discovered_configs() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());

        write_minimal_workspace(temp.path(), "apiari", &temp.path().join("apiari"));
        write_minimal_workspace(temp.path(), "mgm", &temp.path().join("mgm"));

        let workspaces = list_workspaces().await.0;
        let names: Vec<_> = workspaces.into_iter().map(|ws| ws.name).collect();
        assert_eq!(names, vec!["apiari", "mgm"]);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn list_workspace_bots_exposes_main_for_default_bee() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());

        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"

[[bees]]
name = "Bee"
provider = "claude"
model = "sonnet"

[[bees]]
name = "Codex"
provider = "codex"
model = "gpt-5.3-codex"
"#,
                temp.path().join("apiari").display()
            ),
        );

        let bots = list_workspace_bots(Path("apiari".to_string())).await.0;
        assert_eq!(bots.len(), 2);
        assert_eq!(bots[0].name, "Main");
        assert_eq!(bots[0].provider.as_deref(), Some("claude"));
        assert_eq!(bots[1].name, "Codex");
        assert_eq!(bots[1].provider.as_deref(), Some("codex"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_conversations_merges_main_legacy_scopes() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();

        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"
"#,
                root.display()
            ),
        );

        let store =
            crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), "apiari")
                .unwrap();
        let bee_scope =
            crate::buzz::conversation::ConversationStore::new(store.conn(), "apiari/Bee");
        bee_scope
            .save_message(
                "assistant",
                "from bee scope",
                None,
                Some("system"),
                None,
                None,
            )
            .unwrap();
        let root_scope = crate::buzz::conversation::ConversationStore::new(store.conn(), "apiari");
        root_scope
            .save_message(
                "assistant",
                "from workspace scope",
                None,
                Some("system"),
                None,
                None,
            )
            .unwrap();

        let messages = get_workspace_conversations(
            Path(("apiari".to_string(), "Main".to_string())),
            axum::extract::Query(std::collections::HashMap::new()),
        )
        .await
        .0;

        let contents: Vec<_> = messages.into_iter().map(|msg| msg.content).collect();
        assert_eq!(contents, vec!["from bee scope", "from workspace scope"]);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_bot_status_uses_resolved_main_bee_name() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();

        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"
"#,
                root.display()
            ),
        );

        let store =
            crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), "apiari")
                .unwrap();
        store
            .set_bot_status("Bee", "streaming", "working", Some("Bash"))
            .unwrap();

        let status = get_workspace_bot_status(Path(("apiari".to_string(), "Main".to_string())))
            .await
            .0;
        assert_eq!(status.status, "streaming");
        assert_eq!(status.streaming_content, "working");
        assert_eq!(status.tool_name.as_deref(), Some("Bash"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_workspace_chat_enqueues_request_for_resolved_main_bee() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();

        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"
"#,
                root.display()
            ),
        );

        let (updates_tx, _) = broadcast::channel(4);
        let (signal_tx, _signal_rx) = mpsc::unbounded_channel();
        let (chat_tx, mut chat_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        let state = HttpState {
            graph: Arc::new(RwLock::new(
                crate::buzz::orchestrator::graph::builtin::builtin_workflow(),
            )),
            yaml_path: Arc::new(None),
            db_path: Arc::new(crate::config::db_path()),
            workspace: Arc::new("apiari".to_string()),
            updates_tx,
            signal_tx,
            chat_tx,
            cancel_tx,
        };

        let response = send_workspace_chat(
            Path(("apiari".to_string(), "Main".to_string())),
            State(state),
            Json(WorkspaceChatBody {
                message: "hello".to_string(),
                attachments: Some(vec![WebChatAttachment {
                    name: "spec.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    data_url: "data:text/plain;base64,c3BlYw==".to_string(),
                }]),
            }),
        )
        .await
        .0;

        assert_eq!(response.get("ok").and_then(|v| v.as_bool()), Some(true));

        let request = chat_rx.recv().await.expect("chat request should be queued");
        assert_eq!(request.workspace, "apiari");
        assert_eq!(request.bee.as_deref(), Some("Bee"));
        assert_eq!(request.text, "hello");
        assert_eq!(request.attachments.len(), 1);
        assert!(request.attachments_json.is_some());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_workers_uses_resolved_state_path_and_filters_workspace() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());

        let apiari_root = temp.path().join("apiari");
        let apiari_state = temp.path().join("runtime/apiari-workers.json");
        fs::create_dir_all(&apiari_root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[watchers.swarm]
state_path = "{}"
"#,
                apiari_root.display(),
                apiari_state.display()
            ),
        );
        write_swarm_state(
            &apiari_state,
            &serde_json::json!({
                "worktrees": [
                    {
                        "id": "common-sdk-fix",
                        "branch": "common/fix-sdk",
                        "agent_kind": "codex",
                        "phase": "running",
                        "agent_session_status": "waiting",
                        "prompt": "Fix shared repo mapping",
                        "summary": "Repair local slug resolution",
                        "pr": {
                            "url": "https://example.com/pr/1",
                            "title": "Fix SDK mapping",
                            "state": "OPEN"
                        }
                    }
                ]
            }),
        );

        let mgm_root = temp.path().join("mgm");
        fs::create_dir_all(&mgm_root).unwrap();
        write_minimal_workspace(temp.path(), "mgm", &mgm_root);
        write_swarm_state(
            &mgm_root.join(".swarm/state.json"),
            &serde_json::json!({
                "worktrees": [
                    {
                        "id": "mgm-worker",
                        "branch": "mgm/landing-page",
                        "agent_kind": "claude",
                        "phase": "running",
                        "prompt": "Ship landing page"
                    }
                ]
            }),
        );

        let workers = get_workspace_workers(Path("apiari".to_string())).await.0;
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "common-sdk-fix");
        assert_eq!(workers[0].workspace, "apiari");
        assert_eq!(workers[0].agent, "codex");
        assert_eq!(workers[0].status, "waiting");
        assert_eq!(workers[0].pr_title.as_deref(), Some("Fix SDK mapping"));
        assert_eq!(
            workers[0].description.as_deref(),
            Some("Repair local slug resolution")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_worker_detail_returns_prompt_output_and_conversation() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        let swarm_dir = root.join(".swarm");
        fs::create_dir_all(&swarm_dir).unwrap();

        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(r#"root = "{}""#, root.display()),
        );
        write_swarm_state(
            &swarm_dir.join("state.json"),
            &serde_json::json!({
                "worktrees": [
                    {
                        "id": "common-sdk-fix",
                        "branch": "common/fix-sdk",
                        "agent_kind": "codex",
                        "phase": "running",
                        "prompt": "Investigate repo slug resolution",
                        "summary": "Working through daemon/http.rs",
                        "pr": {
                            "url": "https://example.com/pr/1",
                            "title": "Fix SDK mapping",
                            "state": "OPEN"
                        }
                    }
                ]
            }),
        );

        let agent_dir = swarm_dir.join("agents/common-sdk-fix");
        fs::create_dir_all(&agent_dir).unwrap();
        let mut events = fs::File::create(agent_dir.join("events.jsonl")).unwrap();
        writeln!(
            events,
            "{}",
            serde_json::to_string(&apiari_tui::events_parser::AgentEvent::Start {
                timestamp: chrono::Utc::now(),
                prompt: "Investigate repo slug resolution".to_string(),
                model: Some("gpt-5.3-codex".to_string()),
            })
            .unwrap()
        )
        .unwrap();
        writeln!(
            events,
            "{}",
            serde_json::to_string(&apiari_tui::events_parser::AgentEvent::AssistantText {
                timestamp: chrono::Utc::now(),
                text: "Found fallback to workspace root.".to_string(),
            })
            .unwrap()
        )
        .unwrap();

        let detail =
            get_workspace_worker_detail(Path(("apiari".to_string(), "common-sdk-fix".to_string())))
                .await
                .expect("worker detail should resolve")
                .0;

        assert_eq!(detail.worker.id, "common-sdk-fix");
        assert_eq!(detail.worker.status, "running");
        assert_eq!(
            detail.prompt.as_deref(),
            Some("Investigate repo slug resolution")
        );
        assert_eq!(
            detail.output.as_deref(),
            Some("Found fallback to workspace root.")
        );
        assert_eq!(detail.conversation.len(), 2);
        assert_eq!(detail.conversation[0].role, "user");
        assert_eq!(detail.conversation[1].role, "assistant");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_worker_diff_returns_current_worktree_diff() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        let worktree = root.join(".swarm/wt/common-sdk-fix");
        fs::create_dir_all(&worktree).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(r#"root = "{}""#, root.display()),
        );

        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&worktree)
            .status()
            .unwrap();
        fs::write(worktree.join("file.txt"), "before\n").unwrap();
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(&worktree)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Apiari Tests",
                "-c",
                "user.email=tests@apiari.dev",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(&worktree)
            .status()
            .unwrap();
        fs::write(worktree.join("file.txt"), "after\n").unwrap();

        let diff =
            get_workspace_worker_diff(Path(("apiari".to_string(), "common-sdk-fix".to_string())))
                .await
                .0;

        let text = diff.diff.expect("diff should be present");
        assert!(text.contains("diff --git"));
        assert!(text.contains("-before"));
        assert!(text.contains("+after"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn docs_endpoints_list_get_save_and_delete_workspace_docs() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(r#"root = "{}""#, root.display()),
        );
        write_doc(&root, "architecture.md", "# Architecture\n\nDetails");
        write_doc(&root, "setup-guide.md", "Getting started");

        let docs = list_workspace_docs(Path("apiari".to_string()))
            .await
            .expect("docs list should resolve")
            .0;
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].name, "architecture.md");
        assert_eq!(docs[0].title, "Architecture");
        assert_eq!(docs[1].title, "Setup Guide");

        let architecture =
            get_workspace_doc(Path(("apiari".to_string(), "architecture.md".to_string())))
                .await
                .expect("doc get should resolve")
                .0;
        assert_eq!(
            architecture.content.as_deref(),
            Some("# Architecture\n\nDetails")
        );

        let save = save_workspace_doc(
            Path(("apiari".to_string(), "notes.md".to_string())),
            Json(SaveDocBody {
                content: "# Notes\n\nTodo".to_string(),
            }),
        )
        .await
        .expect("doc save should resolve")
        .0;
        assert_eq!(save.get("ok").and_then(|value| value.as_bool()), Some(true));
        assert_eq!(
            fs::read_to_string(root.join("docs/notes.md")).unwrap(),
            "# Notes\n\nTodo"
        );

        let delete = delete_workspace_doc(Path(("apiari".to_string(), "notes.md".to_string())))
            .await
            .expect("doc delete should resolve")
            .0;
        assert_eq!(
            delete.get("ok").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(!root.join("docs/notes.md").exists());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn docs_endpoints_prefer_apiari_docs_directory_when_present() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(r#"root = "{}""#, root.display()),
        );

        write_apiari_doc(&root, "architecture.md", "# Apiari Architecture\n\nDetails");
        write_doc(
            &root,
            "architecture.md",
            "# Legacy Architecture\n\nOld details",
        );

        let docs = list_workspace_docs(Path("apiari".to_string()))
            .await
            .expect("docs list should resolve")
            .0;
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "Apiari Architecture");

        let architecture =
            get_workspace_doc(Path(("apiari".to_string(), "architecture.md".to_string())))
                .await
                .expect("doc get should resolve")
                .0;
        assert_eq!(
            architecture.content.as_deref(),
            Some("# Apiari Architecture\n\nDetails")
        );

        let save = save_workspace_doc(
            Path(("apiari".to_string(), "notes.md".to_string())),
            Json(SaveDocBody {
                content: "# Notes\n\nTodo".to_string(),
            }),
        )
        .await
        .expect("doc save should resolve")
        .0;
        assert_eq!(save.get("ok").and_then(|value| value.as_bool()), Some(true));
        assert_eq!(
            fs::read_to_string(root.join(".apiari/docs/notes.md")).unwrap(),
            "# Notes\n\nTodo"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn docs_endpoints_reject_path_traversal() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(r#"root = "{}""#, root.display()),
        );

        let result =
            get_workspace_doc(Path(("apiari".to_string(), "../secret.md".to_string()))).await;
        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn unread_and_seen_endpoints_track_assistant_messages_per_bot() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"

[[bees]]
name = "Bee"
provider = "claude"
model = "sonnet"

[[bees]]
name = "Codex"
provider = "codex"
model = "gpt-5.3-codex"
"#,
                root.display()
            ),
        );

        let store =
            crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), "apiari")
                .unwrap();
        let main_scope =
            crate::buzz::conversation::ConversationStore::new(store.conn(), "apiari/Bee");
        main_scope
            .save_message("assistant", "main reply", None, Some("system"), None, None)
            .unwrap();
        let codex_scope =
            crate::buzz::conversation::ConversationStore::new(store.conn(), "apiari/Codex");
        codex_scope
            .save_message("assistant", "codex reply", None, Some("system"), None, None)
            .unwrap();

        let unread_before = get_workspace_unread(Path("apiari".to_string())).await.0;
        assert_eq!(unread_before.get("Main").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(unread_before.get("Codex").and_then(|v| v.as_u64()), Some(1));

        let seen = mark_workspace_seen(Path(("apiari".to_string(), "Main".to_string())))
            .await
            .0;
        assert_eq!(seen.get("ok").and_then(|v| v.as_bool()), Some(true));

        let unread_after = get_workspace_unread(Path("apiari".to_string())).await.0;
        assert!(unread_after.get("Main").is_none());
        assert_eq!(unread_after.get("Codex").and_then(|v| v.as_u64()), Some(1));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn unread_and_seen_endpoints_resume_correctly_after_store_reopen() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(
                r#"
root = "{}"

[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"

[[bees]]
name = "Bee"
provider = "claude"
model = "sonnet"
"#,
                root.display()
            ),
        );

        {
            let store =
                crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), "apiari")
                    .unwrap();
            let main_scope =
                crate::buzz::conversation::ConversationStore::new(store.conn(), "apiari/Bee");
            main_scope
                .save_message("assistant", "first reply", None, Some("system"), None, None)
                .unwrap();
        }

        let seen = mark_workspace_seen(Path(("apiari".to_string(), "Main".to_string())))
            .await
            .0;
        assert_eq!(seen.get("ok").and_then(|v| v.as_bool()), Some(true));

        {
            let store =
                crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), "apiari")
                    .unwrap();
            assert!(store.get_bot_seen_message_id("Main").unwrap().is_some());
            let main_scope =
                crate::buzz::conversation::ConversationStore::new(store.conn(), "apiari/Bee");
            main_scope
                .save_message(
                    "assistant",
                    "second reply",
                    None,
                    Some("system"),
                    None,
                    None,
                )
                .unwrap();
        }

        let unread = get_workspace_unread(Path("apiari".to_string())).await.0;
        assert_eq!(unread.get("Main").and_then(|v| v.as_u64()), Some(1));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn followups_endpoint_lists_and_cancels_persisted_followups() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            temp.path(),
            "apiari",
            &format!(r#"root = "{}""#, root.display()),
        );

        let store =
            crate::buzz::signal::store::SignalStore::open(&crate::config::db_path(), "apiari")
                .unwrap();
        store
            .create_followup(
                "f1",
                "Main",
                "Check CI status",
                "2026-05-02T00:00:00Z",
                "2026-05-02T01:00:00Z",
                "pending",
            )
            .unwrap();

        let (updates_tx, mut updates_rx) = broadcast::channel(4);
        let (signal_tx, _signal_rx) = mpsc::unbounded_channel();
        let (chat_tx, _chat_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        let state = HttpState {
            graph: Arc::new(RwLock::new(
                crate::buzz::orchestrator::graph::builtin::builtin_workflow(),
            )),
            yaml_path: Arc::new(None),
            db_path: Arc::new(crate::config::db_path()),
            workspace: Arc::new("apiari".to_string()),
            updates_tx,
            signal_tx,
            chat_tx,
            cancel_tx,
        };

        let followups = list_workspace_followups(Path("apiari".to_string())).await.0;
        assert_eq!(followups.len(), 1);
        assert_eq!(followups[0].id, "f1");
        assert_eq!(followups[0].status, "pending");

        let cancelled =
            cancel_workspace_followup(Path(("apiari".to_string(), "f1".to_string())), State(state))
                .await
                .0;
        assert_eq!(
            cancelled.get("ok").and_then(|value| value.as_bool()),
            Some(true)
        );

        let followups = list_workspace_followups(Path("apiari".to_string())).await.0;
        assert_eq!(followups[0].status, "cancelled");
        let update = updates_rx.recv().await.expect("followup cancel update");
        assert!(matches!(
            update,
            WsUpdate::FollowupCancelled { id, bot, status, .. }
                if id == "f1" && bot == "Main" && status == "cancelled"
        ));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cancel_bot_endpoint_enqueues_cancel_request() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let _env_guard = env_lock();
        let _home_guard = install_temp_home(home);
        let root = home.join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_workspace_file(
            home,
            "apiari",
            &format!(
                r#"
root = "{}"
[coordinator]
name = "Bee"
provider = "claude"
model = "sonnet"
"#,
                root.display()
            ),
        );

        let (updates_tx, _) = broadcast::channel(4);
        let (signal_tx, _signal_rx) = mpsc::unbounded_channel();
        let (chat_tx, _chat_rx) = mpsc::unbounded_channel();
        let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
        let state = HttpState {
            graph: Arc::new(RwLock::new(
                crate::buzz::orchestrator::graph::builtin::builtin_workflow(),
            )),
            yaml_path: Arc::new(None),
            db_path: Arc::new(crate::config::db_path()),
            workspace: Arc::new("apiari".to_string()),
            updates_tx,
            signal_tx,
            chat_tx,
            cancel_tx,
        };

        let response = cancel_workspace_bot(
            Path(("apiari".to_string(), "Main".to_string())),
            State(state),
        )
        .await
        .0;
        assert_eq!(
            response.get("ok").and_then(|value| value.as_bool()),
            Some(true)
        );

        let request = cancel_rx.recv().await.expect("cancel should be queued");
        assert_eq!(request.workspace, "apiari");
        assert_eq!(request.bee.as_deref(), Some("Bee"));
    }

    #[tokio::test]
    async fn research_endpoint_returns_empty_list_and_running_shape() {
        let research = list_workspace_research(Path("apiari".to_string())).await.0;
        assert!(research.is_empty());

        let started = start_workspace_research(
            Path("apiari".to_string()),
            Json(ResearchRequestBody {
                topic: "monorepo cleanup".to_string(),
            }),
        )
        .await
        .0;
        assert_eq!(
            started.get("workspace").and_then(|value| value.as_str()),
            Some("apiari")
        );
        assert_eq!(
            started.get("topic").and_then(|value| value.as_str()),
            Some("monorepo cleanup")
        );
        assert_eq!(
            started.get("status").and_then(|value| value.as_str()),
            Some("running")
        );
        assert!(
            started
                .get("id")
                .and_then(|value| value.as_str())
                .is_some_and(|id| id.starts_with("research-"))
        );
    }
}

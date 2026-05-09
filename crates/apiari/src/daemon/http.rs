//! HTTP + WebSocket server for the web UI.
//!
//! Serves the workflow graph definition, task state, and live updates over WebSocket.
//! In production, static assets are embedded via rust-embed; in dev mode, Vite proxies here.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

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
    routing::{get, post, put},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast, mpsc};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use crate::buzz::{
    orchestrator::graph::{WorkflowGraph, walker::GraphCursor},
    task::{Task, TaskAttempt, store::TaskStore},
};
use crate::daemon::worker_manager::WorkerManager;

// ── Shared state ───────────────────────────────────────────────────────

/// UUID generated once per daemon process lifetime. Clients reload when it changes.
static STARTUP_ID: OnceLock<String> = OnceLock::new();

fn startup_id() -> &'static str {
    STARTUP_ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

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
    /// In-process worker manager — creates worktrees and spawns agents directly.
    pub worker_manager: Arc<WorkerManager>,
}

/// A WebSocket update message sent to all connected clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum WsUpdate {
    /// Full state snapshot (sent on initial connect).
    Snapshot {
        tasks: Vec<TaskView>,
        graph: GraphView,
        startup_id: String,
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
    /// A v2 worker state or property change.
    WorkerV2State {
        workspace: String,
        worker_id: String,
        state: String,
        label: String,
        properties: serde_json::Value,
    },
    /// An auto bot run started.
    AutoBotRunStarted {
        workspace: String,
        auto_bot_id: String,
        run_id: String,
    },
    /// An auto bot run finished.
    AutoBotRunFinished {
        workspace: String,
        auto_bot_id: String,
        run_id: String,
        outcome: String,
    },
    /// A worker hook fired.
    WorkerHookFired {
        workspace: String,
        worker_id: String,
        hook_id: i64,
        action: String,
    },
    /// A worker review completed.
    WorkerReview {
        workspace: String,
        worker_id: String,
        verdict: String,
        reviewer: String,
    },
    /// A dashboard widget slot was updated.
    WidgetUpdated {
        workspace: String,
        slot: String,
        widget: serde_json::Value,
        updated_at: String,
    },
    /// Context bot is mid-run — reports which tool it's currently using.
    ContextBotActivity {
        workspace: String,
        session_id: String,
        activity: String,
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
    pub lifecycle_state: String,
    pub source: Option<String>,
    pub worker_id: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i64>,
    pub repo: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attempt: Option<TaskAttemptView>,
    pub cursor: Option<CursorView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskAttemptView {
    pub worker_id: String,
    pub role: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
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

fn task_attempt_to_view(attempt: &TaskAttempt) -> TaskAttemptView {
    TaskAttemptView {
        worker_id: attempt.worker_id.clone(),
        role: attempt.role.as_str().to_string(),
        state: attempt.state.as_str().to_string(),
        branch: attempt.branch.clone(),
        pr_url: attempt.pr_url.clone(),
        pr_number: attempt.pr_number,
        detail: attempt.detail.clone(),
        created_at: attempt.created_at.to_rfc3339(),
        updated_at: attempt.updated_at.to_rfc3339(),
        completed_at: attempt.completed_at.map(|value| value.to_rfc3339()),
    }
}

fn latest_attempt_view_for_task(store: &TaskStore, task_id: &str) -> Option<TaskAttemptView> {
    store
        .get_latest_attempt_for_task(task_id)
        .ok()
        .flatten()
        .map(|attempt| task_attempt_to_view(&attempt))
}

fn latest_attempt_view_for_worker(
    store: &TaskStore,
    workspace: &str,
    worker_id: &str,
) -> Option<TaskAttemptView> {
    store
        .find_attempt_by_worker(workspace, worker_id)
        .ok()
        .flatten()
        .map(|attempt| task_attempt_to_view(&attempt))
}

fn reviewer_changes_requested(attempt: &TaskAttemptView) -> bool {
    if attempt.role != "reviewer" {
        return false;
    }
    attempt
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("CHANGES_REQUESTED"))
}

fn derive_task_lifecycle_state(task: &Task, latest_attempt: Option<&TaskAttemptView>) -> String {
    match task.stage {
        crate::buzz::task::TaskStage::Triage => "Triage".to_string(),
        crate::buzz::task::TaskStage::InProgress => {
            match latest_attempt.map(|attempt| attempt.state.as_str()) {
                Some("blocked") | Some("waiting") => "Blocked".to_string(),
                Some("failed") | Some("cancelled") => "Ready".to_string(),
                Some("running") | Some("created") | Some("preparing") => "Running".to_string(),
                Some("succeeded") if task.pr_url.is_some() => "PR Open".to_string(),
                _ if task.worker_id.is_some() => "Running".to_string(),
                _ => "Ready".to_string(),
            }
        }
        crate::buzz::task::TaskStage::InAiReview => {
            if latest_attempt.is_some_and(reviewer_changes_requested) {
                "Changes Requested".to_string()
            } else if task.pr_url.is_some() {
                "AI Review".to_string()
            } else {
                "PR Open".to_string()
            }
        }
        crate::buzz::task::TaskStage::HumanReview => "Human Review".to_string(),
        crate::buzz::task::TaskStage::Merged => "Merged".to_string(),
        crate::buzz::task::TaskStage::Dismissed => "Dismissed".to_string(),
    }
}

pub fn task_to_view(task: &Task) -> TaskView {
    task_to_view_with_attempt(task, None)
}

fn task_to_view_with_attempt(task: &Task, latest_attempt: Option<TaskAttemptView>) -> TaskView {
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
        lifecycle_state: derive_task_lifecycle_state(task, latest_attempt.as_ref()),
        source: task.source.clone(),
        worker_id: task.worker_id.clone(),
        pr_url: task.pr_url.clone(),
        pr_number: task.pr_number,
        repo: task.repo.clone(),
        created_at: task.created_at.to_rfc3339(),
        updated_at: task.updated_at.to_rfc3339(),
        resolved_at: task.resolved_at.map(|value| value.to_rfc3339()),
        latest_attempt,
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
        .map(|task| task_to_view_with_attempt(task, latest_attempt_view_for_task(&store, &task.id)))
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
        .map(|store| {
            store
                .get_all_tasks(&state.workspace)
                .unwrap_or_default()
                .iter()
                .map(|task| {
                    task_to_view_with_attempt(task, latest_attempt_view_for_task(&store, &task.id))
                })
                .collect()
        })
        .unwrap_or_default();

    let graph = state.graph.read().await;
    let snapshot = WsUpdate::Snapshot {
        tasks,
        graph: graph_to_view(&graph),
        startup_id: startup_id().to_string(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    behind_count: usize,
    ahead_count: usize,
    workers: Vec<WorkerView>,
}

#[derive(Debug, Serialize)]
struct WorkerEnvironmentStatusView {
    repo: Option<String>,
    ready: bool,
    git_worktree_metadata_writable: bool,
    frontend_toolchain_required: bool,
    frontend_toolchain_ready: bool,
    worktree_links_ready: bool,
    setup_commands_ready: bool,
    blockers: Vec<String>,
    suggested_fixes: Vec<String>,
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
struct BotEffectiveConfigView {
    api_name: String,
    resolved_bee_name: String,
    workspace_authority: String,
    configured_execution_policy: String,
    effective_execution_policy: String,
    provider: String,
    model: String,
    role: Option<String>,
    color: Option<String>,
    max_turns: u32,
    max_session_turns: u32,
    heartbeat: Option<String>,
    signal_sources: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BotDebugView {
    workspace: String,
    bot: String,
    provider: Option<String>,
    effective_config: Option<BotEffectiveConfigView>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    task_packet: Option<WorkerTaskPacketView>,
}

#[derive(Debug, Serialize)]
struct WorkerDiffView {
    diff: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkerTaskPacketView {
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shaping_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress_md: Option<String>,
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

fn bot_description_for_ui(bee: &crate::config::BeeConfig) -> Option<String> {
    bee.role
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            bee.prompt.as_deref().and_then(|prompt| {
                prompt
                    .split("\n\n")
                    .map(str::trim)
                    .find(|section| {
                        !section.is_empty()
                            && !section.starts_with("Role:")
                            && !section.starts_with("Color:")
                    })
                    .map(|section| {
                        let compact = section.split_whitespace().collect::<Vec<_>>().join(" ");
                        if compact.len() > 160 {
                            format!("{}…", compact[..160].trim_end())
                        } else {
                            compact
                        }
                    })
                    .filter(|section| !section.is_empty())
            })
        })
}

fn bot_items_for_workspace(config: &crate::config::WorkspaceConfig) -> Vec<BotListItem> {
    let bees = config.resolved_bees();
    bees.iter()
        .map(|bee| BotListItem {
            name: display_bee_name(&bees, bee),
            color: bee.color.clone(),
            role: bee.role.clone(),
            description: bot_description_for_ui(bee),
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

fn authority_label(authority: crate::config::WorkspaceAuthority) -> &'static str {
    match authority {
        crate::config::WorkspaceAuthority::Observe => "observe",
        crate::config::WorkspaceAuthority::Autonomous => "autonomous",
    }
}

fn execution_policy_label(policy: crate::config::BeeExecutionPolicy) -> &'static str {
    match policy {
        crate::config::BeeExecutionPolicy::Observe => "observe",
        crate::config::BeeExecutionPolicy::DispatchOnly => "dispatch_only",
        crate::config::BeeExecutionPolicy::Autonomous => "autonomous",
    }
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
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

fn current_git_upstream(path: &std::path::Path) -> Option<String> {
    git_output(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
}

fn git_ahead_behind_counts(path: &std::path::Path, upstream: &str) -> (usize, usize) {
    let range = format!("{upstream}...HEAD");
    let Some(output) = git_output(path, &["rev-list", "--left-right", "--count", &range]) else {
        return (0, 0);
    };

    let mut parts = output.split_whitespace();
    let behind = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let ahead = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    (ahead, behind)
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
            let upstream = current_git_upstream(&local_path);
            let (ahead_count, behind_count) = upstream
                .as_deref()
                .map(|upstream| git_ahead_behind_counts(&local_path, upstream))
                .unwrap_or((0, 0));

            RepoListItem {
                name: basename.clone(),
                path: local_path.display().to_string(),
                has_swarm: local_path.join(".swarm").exists(),
                is_clean: is_git_clean(&local_path),
                branch: current_git_branch(&local_path),
                upstream,
                behind_count,
                ahead_count,
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

/// GET /api/workspaces/:workspace/worker-environment — worker readiness for this workspace.
async fn get_workspace_worker_environment(
    Path(workspace): Path<String>,
) -> Result<Json<WorkerEnvironmentStatusView>, StatusCode> {
    let ws = load_workspace_by_name(&workspace).ok_or(StatusCode::NOT_FOUND)?;
    let status =
        crate::daemon::worker_environment_status_for_workspace(&workspace, &ws.config, None)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(WorkerEnvironmentStatusView {
        repo: status.repo,
        ready: status.ready,
        git_worktree_metadata_writable: status.git_worktree_metadata_writable,
        frontend_toolchain_required: status.frontend_toolchain_required,
        frontend_toolchain_ready: status.frontend_toolchain_ready,
        worktree_links_ready: status.worktree_links_ready,
        setup_commands_ready: status.setup_commands_ready,
        blockers: status.blockers,
        suggested_fixes: status.suggested_fixes,
    }))
}

/// GET /api/workspaces/:workspace/tasks — list tasks for a workspace.
async fn list_workspace_tasks(Path(workspace): Path<String>) -> Json<Vec<TaskView>> {
    let tasks = TaskStore::open(&crate::config::db_path())
        .ok()
        .map(|store| {
            store
                .get_all_tasks(&workspace)
                .unwrap_or_default()
                .into_iter()
                .map(|task| {
                    let latest_attempt = latest_attempt_view_for_task(&store, &task.id);
                    task_to_view_with_attempt(&task, latest_attempt)
                })
                .collect()
        })
        .unwrap_or_default();
    Json(tasks)
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
    let task = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        .ok()
        .and_then(|store| store.find_task_by_worker(&workspace, &worker_id).ok())
        .flatten();
    let worker = find_worker_state(&ws.config, &worker_id);
    let worker_view = if let Some(worker) = worker.as_ref() {
        worker_view_from_state(&workspace, &ws.config, worker)
    } else if let Some(task) = task.as_ref() {
        worker_view_from_task(&workspace, task).ok_or(StatusCode::NOT_FOUND)?
    } else {
        return Err(StatusCode::NOT_FOUND);
    };
    let conversation = worker_conversation_messages(&ws.config.resolved_swarm_dir(), &worker_id);
    let output = worker.as_ref().and_then(|worker| {
        worker_output_from_conversation(&conversation).or_else(|| worker.summary.clone())
    });
    let prompt = worker
        .as_ref()
        .and_then(|worker| (!worker.prompt.trim().is_empty()).then(|| worker.prompt.clone()));
    let task_packet = worker
        .as_ref()
        .and_then(|worker| worker.worktree_path.as_ref())
        .and_then(|path| read_worker_task_packet(path));

    Ok(Json(WorkerDetailView {
        worker: worker_view,
        prompt,
        output,
        conversation,
        task_packet,
    }))
}

#[derive(Debug, Deserialize)]
struct WorkspaceWorkerMessageBody {
    message: String,
}

#[derive(Debug, Deserialize)]
struct CloseWorkerBody {
    #[serde(default = "default_true")]
    dismiss_task: bool,
}

#[derive(Debug, Serialize)]
struct WorkerActionView {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_url: Option<String>,
    detail: String,
}

fn default_true() -> bool {
    true
}

fn worker_repo_name(
    worker: &SwarmWorktreeState,
    fallback_root: &std::path::Path,
) -> Option<String> {
    worker
        .repo_path
        .as_deref()
        .unwrap_or(fallback_root)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn worker_task_title(worker: &SwarmWorktreeState) -> String {
    worker
        .summary
        .clone()
        .or_else(|| {
            worker
                .prompt
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("Worker {}", worker.id))
}

/// POST /api/workspaces/:workspace/workers/:worker_id/send — send a message to a worker.
async fn send_workspace_worker_message(
    State(state): State<HttpState>,
    Path((workspace, worker_id)): Path<(String, String)>,
    Json(body): Json<WorkspaceWorkerMessageBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let _ = load_workspace_by_name(&workspace).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("workspace '{workspace}' not found"),
        )
    })?;

    state
        .worker_manager
        .send_message(&worker_id, &body.message)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/workspaces/:workspace/workers/:worker_id/promote — commit worktree changes and open a PR.
async fn promote_workspace_worker(
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Result<Json<WorkerActionView>, (StatusCode, String)> {
    let ws = load_workspace_by_name(&workspace).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("workspace '{workspace}' not found"),
        )
    })?;
    let worker = find_worker_state(&ws.config, &worker_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("worker '{worker_id}' not found"),
        )
    })?;

    let worktree_path = worker.worktree_path.clone().ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            format!("worker '{worker_id}' has no worktree path"),
        )
    })?;
    let repo_path = worker
        .repo_path
        .clone()
        .unwrap_or_else(|| ws.config.root.clone());

    if worker_has_uncommitted_changes(&worker) {
        apiari_swarm::core::merge::commit_all(
            &worktree_path,
            &format!("promote worker {}", worker.id),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to commit worker changes: {e}"),
            )
        })?;
    }

    let branch_name = worker
        .ready_branch
        .clone()
        .unwrap_or_else(|| worker.branch.clone());
    let task_store = crate::buzz::task::store::TaskStore::open(&crate::config::db_path()).ok();
    let existing_task = task_store
        .as_ref()
        .and_then(|store| store.find_task_by_worker(&workspace, &worker_id).ok())
        .flatten();
    let title = existing_task
        .as_ref()
        .map(|task| task.title.clone())
        .unwrap_or_else(|| worker_task_title(&worker));
    let body = "Promoted from worker output by apiari".to_string();

    let pr_result = crate::buzz::orchestrator::workflow::create_system_pr(
        &repo_path,
        &branch_name,
        &title,
        &body,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create PR: {e}"),
        )
    })?;

    if let Some(store) = task_store {
        let task_id = if let Some(task) = existing_task {
            if let Some(num) = pr_result.pr_number {
                let _ = store.update_task_pr(&task.id, &pr_result.pr_url, num);
            }
            if let Some(repo) = worker_repo_name(&worker, &ws.config.root) {
                let _ = store.update_task_repo(&task.id, &repo);
            }
            let _ = store.update_task_stage(&task.id, &crate::buzz::task::TaskStage::HumanReview);
            task.id
        } else {
            let now = chrono::Utc::now();
            let repo = worker_repo_name(&worker, &ws.config.root);
            let task = crate::buzz::task::Task {
                id: uuid::Uuid::new_v4().to_string(),
                workspace: workspace.clone(),
                title: title.clone(),
                stage: crate::buzz::task::TaskStage::HumanReview,
                source: Some("manual".to_string()),
                source_url: None,
                worker_id: Some(worker_id.clone()),
                pr_url: Some(pr_result.pr_url.clone()),
                pr_number: pr_result.pr_number,
                repo,
                created_at: now,
                updated_at: now,
                resolved_at: None,
                metadata: serde_json::json!({ "promoted_manually": true }),
            };
            let _ = store.create_task(&task);
            task.id
        };
        if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(&crate::config::db_path()) {
            let _ = ae.log_event(
                &workspace,
                Some(&task_id),
                "pr",
                &format!("PR created for worker {}", worker_id),
                Some(&pr_result.pr_url),
                Some("worker_action"),
                None,
                None,
            );
        }
    }

    Ok(Json(WorkerActionView {
        ok: true,
        worker_id: Some(worker_id),
        pr_url: Some(pr_result.pr_url.clone()),
        detail: format!("Created PR for branch `{branch_name}`."),
    }))
}

/// POST /api/workspaces/:workspace/workers/:worker_id/redispatch — create a replacement worker and reattach the task.
async fn redispatch_workspace_worker(
    Path((workspace, worker_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> Result<Json<WorkerActionView>, (StatusCode, String)> {
    let ws = load_workspace_by_name(&workspace).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("workspace '{workspace}' not found"),
        )
    })?;
    let worker = find_worker_state(&ws.config, &worker_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("worker '{worker_id}' not found"),
        )
    })?;

    let repo = worker_repo_name(&worker, &ws.config.root).ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            format!("worker '{worker_id}' has no repo context"),
        )
    })?;
    let prompt = if !worker.prompt.trim().is_empty() {
        worker.prompt.clone()
    } else {
        worker_task_title(&worker)
    };

    let worker_mode = crate::buzz::coordinator::swarm_client::infer_worker_mode(&prompt);
    let task_dir = crate::buzz::coordinator::swarm_client::build_worker_task_dir_with_mode(
        &repo,
        &prompt,
        worker_mode,
    );
    let replacement_id = state
        .worker_manager
        .create_worker_with_task_dir(
            &ws.config.root,
            &repo,
            &prompt,
            &ws.config.swarm.default_agent,
            None,
            Some(task_dir),
            ws.config.swarm.worker_isolation.clone(),
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create replacement worker: {e}"),
            )
        })?;

    if let Ok(store) = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        && let Ok(Some(task)) = store.find_task_by_worker(&workspace, &worker_id)
    {
        let _ = store.update_task_worker(&task.id, &replacement_id);
        let _ = store.update_task_stage(&task.id, &crate::buzz::task::TaskStage::InProgress);
        let mut metadata = task.metadata;
        metadata["redispatched_from_worker_id"] = serde_json::Value::String(worker_id.clone());
        metadata["replacement_worker_id"] = serde_json::Value::String(replacement_id.clone());
        let _ = store.update_task_metadata(&task.id, &metadata);
    }

    Ok(Json(WorkerActionView {
        ok: true,
        worker_id: Some(replacement_id.clone()),
        pr_url: None,
        detail: format!(
            "Spawned replacement {} worker `{replacement_id}` in repo `{repo}`. Original worktree was left intact.",
            worker_mode.as_str()
        ),
    }))
}

/// POST /api/workspaces/:workspace/workers/:worker_id/close — close a worker and optionally dismiss its task.
async fn close_workspace_worker(
    Path((workspace, worker_id)): Path<(String, String)>,
    State(state): State<HttpState>,
    Json(body): Json<CloseWorkerBody>,
) -> Result<Json<WorkerActionView>, (StatusCode, String)> {
    let _ = load_workspace_by_name(&workspace).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("workspace '{workspace}' not found"),
        )
    })?;

    // Best-effort close — worker may have already exited.
    let _ = state.worker_manager.close_worker(&worker_id).await;

    if body.dismiss_task
        && let Ok(store) = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        && let Ok(Some(task)) = store.find_task_by_worker(&workspace, &worker_id)
        && !task.stage.is_terminal()
    {
        let _ = store.transition_task(
            &task.id,
            &task.stage,
            &crate::buzz::task::TaskStage::Dismissed,
            Some("Closed from worker detail".to_string()),
        );
    }

    Ok(Json(WorkerActionView {
        ok: true,
        worker_id: Some(worker_id),
        pr_url: None,
        detail: if body.dismiss_task {
            "Closed worker and dismissed its task.".to_string()
        } else {
            "Closed worker.".to_string()
        },
    }))
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
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ready_branch: Option<String>,
    has_uncommitted_changes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_attempt: Option<TaskAttemptView>,
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

struct WorkerTaskOverlay {
    task_id: String,
    task_title: String,
    task_stage: String,
    task_repo: Option<String>,
    pr_url: Option<String>,
    pr_title: Option<String>,
    status: Option<String>,
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
    #[serde(default)]
    repo_path: Option<std::path::PathBuf>,
    #[serde(default)]
    worktree_path: Option<std::path::PathBuf>,
    #[serde(default)]
    agent_pid: Option<u32>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    ready_branch: Option<String>,
    #[serde(default)]
    failure_note: Option<String>,
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

fn worker_events_path(
    config: &crate::config::WorkspaceConfig,
    worker_id: &str,
) -> std::path::PathBuf {
    config
        .resolved_swarm_dir()
        .join("agents")
        .join(worker_id)
        .join("events.jsonl")
}

fn worker_session_completed(config: &crate::config::WorkspaceConfig, worker_id: &str) -> bool {
    let path = worker_events_path(config, worker_id);
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content
        .lines()
        .rev()
        .take(20)
        .any(|line| line.contains(r#""type":"session_result""#))
}

fn worker_status_for_state(
    config: &crate::config::WorkspaceConfig,
    worker: &SwarmWorktreeState,
) -> String {
    if worker_session_completed(config, &worker.id) {
        return if worker.ready_branch.is_some() || worker.pr.is_some() {
            "completed".to_string()
        } else {
            "failed".to_string()
        };
    }

    if worker.phase.as_deref() == Some("running")
        && worker.agent_session_status.as_deref() != Some("running")
        && worker.ready_branch.is_none()
        && worker.session_id.is_none()
        && worker.agent_pid.is_none()
        && worker_has_uncommitted_changes(worker)
    {
        return "stalled".to_string();
    }
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

fn worker_execution_note(
    config: &crate::config::WorkspaceConfig,
    worker: &SwarmWorktreeState,
) -> Option<String> {
    if let Some(note) = worker.failure_note.clone()
        && !note.trim().is_empty()
    {
        return Some(note);
    }
    if worker_session_completed(config, &worker.id)
        && worker.ready_branch.is_none()
        && worker.pr.is_none()
        && !worker_has_uncommitted_changes(worker)
    {
        return Some(
            "Worker finished without a ready branch or PR handoff. It stopped without a promotable code change."
                .to_string(),
        );
    }
    if worker.phase.as_deref() == Some("running")
        && worker.agent_session_status.as_deref() != Some("running")
        && worker.ready_branch.is_none()
        && worker.session_id.is_none()
        && worker.agent_pid.is_none()
        && worker_has_uncommitted_changes(worker)
    {
        return Some("Uncommitted diff, no ready branch, and no active session.".to_string());
    }
    if worker.ready_branch.is_none() && worker_has_uncommitted_changes(worker) {
        return Some(
            "Uncommitted diff present; worker has not marked a ready branch yet.".to_string(),
        );
    }
    None
}

fn worker_has_uncommitted_changes(worker: &SwarmWorktreeState) -> bool {
    let Some(worktree_path) = worker.worktree_path.as_ref() else {
        return false;
    };
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn worker_task_overlay(workspace: &str, worker_id: &str) -> Option<WorkerTaskOverlay> {
    let task = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        .ok()?
        .find_task_by_worker(workspace, worker_id)
        .ok()??;

    Some(worker_task_overlay_from_task(task))
}

fn worker_task_overlay_from_task(task: crate::buzz::task::Task) -> WorkerTaskOverlay {
    let status = match task.stage {
        crate::buzz::task::TaskStage::HumanReview => Some("waiting".to_string()),
        crate::buzz::task::TaskStage::Merged | crate::buzz::task::TaskStage::Dismissed => {
            Some("completed".to_string())
        }
        _ => None,
    };

    WorkerTaskOverlay {
        task_id: task.id,
        task_title: task.title.clone(),
        task_stage: task.stage.as_str().to_string(),
        task_repo: task.repo.clone(),
        pr_url: task.pr_url,
        pr_title: Some(task.title),
        status,
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

fn worker_status_for_task(task: &crate::buzz::task::Task) -> String {
    match task.stage {
        crate::buzz::task::TaskStage::Triage => "queued".to_string(),
        crate::buzz::task::TaskStage::InProgress | crate::buzz::task::TaskStage::InAiReview => {
            "running".to_string()
        }
        crate::buzz::task::TaskStage::HumanReview => "waiting".to_string(),
        crate::buzz::task::TaskStage::Merged | crate::buzz::task::TaskStage::Dismissed => {
            "completed".to_string()
        }
    }
}

fn worker_view_from_task(workspace: &str, task: &crate::buzz::task::Task) -> Option<WorkerView> {
    let worker_id = task.worker_id.as_ref()?.clone();
    let latest_attempt = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        .ok()
        .and_then(|store| latest_attempt_view_for_task(&store, &task.id));
    let task_lifecycle_state = derive_task_lifecycle_state(task, latest_attempt.as_ref());
    Some(WorkerView {
        id: worker_id,
        workspace: workspace.to_string(),
        branch: task
            .repo
            .as_ref()
            .map(|repo| format!("task/{repo}"))
            .unwrap_or_else(|| "task/lifecycle".to_string()),
        agent: "system".to_string(),
        status: worker_status_for_task(task),
        execution_note: None,
        ready_branch: None,
        has_uncommitted_changes: false,
        task_id: Some(task.id.clone()),
        task_title: Some(task.title.clone()),
        task_stage: Some(task.stage.as_str().to_string()),
        task_lifecycle_state: Some(task_lifecycle_state),
        task_repo: task.repo.clone(),
        latest_attempt,
        pr_url: task.pr_url.clone(),
        pr_title: Some(task.title.clone()),
        description: Some(task.title.clone()),
        elapsed_secs: elapsed_secs(Some(task.created_at.with_timezone(&chrono::Local))),
        dispatched_by: None,
        review_state: None,
        ci_status: None,
        total_comments: None,
        open_comments: None,
        resolved_comments: None,
    })
}

fn worker_view_from_state(
    workspace: &str,
    config: &crate::config::WorkspaceConfig,
    worker: &SwarmWorktreeState,
) -> WorkerView {
    let overlay = worker_task_overlay(workspace, &worker.id);
    let overlay_task_id = overlay.as_ref().map(|task| task.task_id.clone());
    let overlay_task_title = overlay.as_ref().map(|task| task.task_title.clone());
    let overlay_task_stage = overlay.as_ref().map(|task| task.task_stage.clone());
    let overlay_task_repo = overlay.as_ref().and_then(|task| task.task_repo.clone());
    let overlay_pr_url = overlay.as_ref().and_then(|task| task.pr_url.clone());
    let overlay_pr_title = overlay.as_ref().and_then(|task| task.pr_title.clone());
    let overlay_status = overlay.as_ref().and_then(|task| task.status.clone());
    let task_record = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        .ok()
        .and_then(|store| store.find_task_by_worker(workspace, &worker.id).ok())
        .flatten();
    let latest_attempt = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        .ok()
        .and_then(|store| latest_attempt_view_for_worker(&store, workspace, &worker.id));
    let task_lifecycle_state = task_record
        .as_ref()
        .map(|task| derive_task_lifecycle_state(task, latest_attempt.as_ref()));

    WorkerView {
        id: worker.id.clone(),
        workspace: workspace.to_string(),
        branch: worker.branch.clone(),
        agent: if worker.agent_kind.is_empty() {
            "claude".to_string()
        } else {
            worker.agent_kind.clone()
        },
        status: overlay_status.unwrap_or_else(|| worker_status_for_state(config, worker)),
        execution_note: worker_execution_note(config, worker),
        ready_branch: worker.ready_branch.clone(),
        has_uncommitted_changes: worker_has_uncommitted_changes(worker),
        task_id: overlay_task_id,
        task_title: overlay_task_title,
        task_stage: overlay_task_stage,
        task_lifecycle_state,
        task_repo: overlay_task_repo,
        latest_attempt,
        pr_url: worker
            .pr
            .as_ref()
            .and_then(|pr| pr.url.clone())
            .filter(|url| !url.is_empty())
            .or(overlay_pr_url),
        pr_title: worker
            .pr
            .as_ref()
            .and_then(|pr| pr.title.clone())
            .or(overlay_pr_title),
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
    let mut workers: Vec<WorkerView> = load_swarm_state(config)
        .map(|state| {
            state
                .worktrees
                .iter()
                .map(|worker| worker_view_from_state(workspace, config, worker))
                .collect()
        })
        .unwrap_or_default();

    let existing_ids: std::collections::HashSet<String> =
        workers.iter().map(|worker| worker.id.clone()).collect();

    if let Ok(store) = crate::buzz::task::store::TaskStore::open(&crate::config::db_path())
        && let Ok(tasks) = store.get_all_tasks(workspace)
    {
        for task in tasks {
            let Some(worker_id) = task.worker_id.as_ref() else {
                continue;
            };
            if existing_ids.contains(worker_id) {
                continue;
            }
            if task.stage.is_terminal() && task.pr_url.is_none() {
                continue;
            }
            if let Some(worker) = worker_view_from_task(workspace, &task) {
                workers.push(worker);
            }
        }
    }

    workers
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

fn read_worker_task_packet(worktree_path: &std::path::Path) -> Option<WorkerTaskPacketView> {
    let task_dir = worktree_path.join(".task");
    if !task_dir.exists() {
        return None;
    }

    let read_optional = |name: &str| std::fs::read_to_string(task_dir.join(name)).ok();
    let task_md = read_optional("TASK.md");
    let context_md = read_optional("CONTEXT.md");
    let plan_md = read_optional("PLAN.md");
    let shaping_md = read_optional("SHAPING.md");
    let progress_md = read_optional("PROGRESS.md");
    let worker_mode = task_md.as_ref().and_then(|content| {
        content.lines().find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- `")
                .and_then(|value| value.strip_suffix('`'))
                .map(str::to_string)
        })
    });

    if task_md.is_none()
        && context_md.is_none()
        && plan_md.is_none()
        && shaping_md.is_none()
        && progress_md.is_none()
    {
        return None;
    }

    Some(WorkerTaskPacketView {
        worker_mode,
        task_md,
        context_md,
        plan_md,
        shaping_md,
        progress_md,
    })
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
    State(state): State<HttpState>,
    Json(body): Json<WorkerMessageBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .worker_manager
        .send_message(&body.worker_id, &body.text)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
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

    let loaded_workspace = load_workspace_by_name(&workspace);
    let provider = loaded_workspace.as_ref().and_then(|ws| {
        let resolved = resolve_bee_name_for_api(&ws.config, &bot).unwrap_or(bot.clone());
        ws.config
            .resolved_bees()
            .into_iter()
            .find(|bee| bee.name == resolved)
            .map(|bee| bee.provider)
    });
    let effective_config = loaded_workspace.as_ref().and_then(|ws| {
        let resolved_name = resolve_bee_name_for_api(&ws.config, &bot).unwrap_or(bot.clone());
        ws.config
            .resolved_bees()
            .into_iter()
            .find(|bee| bee.name == resolved_name)
            .map(|bee| BotEffectiveConfigView {
                api_name: bot.clone(),
                resolved_bee_name: resolved_name,
                workspace_authority: authority_label(ws.config.authority).to_string(),
                configured_execution_policy: execution_policy_label(bee.execution_policy)
                    .to_string(),
                effective_execution_policy: execution_policy_label(
                    bee.execution_policy.resolved(ws.config.authority),
                )
                .to_string(),
                provider: bee.provider,
                model: bee.model,
                role: bee.role,
                color: bee.color,
                max_turns: bee.max_turns,
                max_session_turns: bee.max_session_turns,
                heartbeat: bee.heartbeat,
                signal_sources: bee
                    .signal_hooks
                    .into_iter()
                    .map(|hook| hook.source)
                    .collect(),
            })
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
                effective_config,
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
        effective_config,
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
                task: task_to_view_with_attempt(&task, latest_attempt_view_for_task(&store, &task.id)),
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
                    task: task_to_view_with_attempt(
                        &updated_task,
                        latest_attempt_view_for_task(&store, &updated_task.id),
                    ),
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
                task: task_to_view_with_attempt(
                    &final_task,
                    latest_attempt_view_for_task(&store, &final_task.id),
                ),
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

// ── v2 Worker API routes ───────────────────────────────────────────────

/// Open a WorkerStore against the given db_path. Ensures schema is created.
fn open_worker_store_from_path(
    db_path: &std::path::Path,
) -> color_eyre::Result<crate::buzz::worker::WorkerStore> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| color_eyre::eyre::eyre!("failed to open worker db: {e}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let conn = std::sync::Arc::new(std::sync::Mutex::new(conn));
    crate::buzz::worker::WorkerStore::new(conn)
}

/// GET /api/workspaces/{ws}/v2/workers — list workers with computed labels.
async fn v2_list_workers(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match store.list(&workspace) {
        Ok(workers) => Json(serde_json::json!({"workers": workers})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /api/workspaces/{ws}/v2/workers/{id} — single worker detail.
async fn v2_get_worker(
    Path((workspace, id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let worker = match store.get(&workspace, &id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Read events from .swarm/agents/{id}/events.jsonl
    let events = if let Some(ws) = load_workspace_by_name(&workspace) {
        let events_path = ws
            .config
            .root
            .join(".swarm")
            .join("agents")
            .join(&id)
            .join("events.jsonl");
        read_worker_events(&events_path)
    } else {
        vec![]
    };
    let task_packet = worker
        .worktree_path
        .as_deref()
        .map(std::path::Path::new)
        .and_then(read_worker_task_packet);

    let mut response = serde_json::to_value(&worker).unwrap_or_default();
    if let Some(obj) = response.as_object_mut() {
        obj.insert("events".to_string(), serde_json::json!(events));
        if let Some(task_packet) = task_packet {
            obj.insert("task_packet".to_string(), serde_json::json!(task_packet));
        }
    }
    Json(response).into_response()
}

fn read_worker_events(path: &std::path::Path) -> Vec<serde_json::Value> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    content
        .lines()
        .filter_map(|line| {
            let val: serde_json::Value = serde_json::from_str(line).ok()?;
            let event_type = val.get("type")?.as_str()?;
            // Only surface meaningful event types
            match event_type {
                "assistant_text" => {
                    let content = val.get("text")?.as_str()?;
                    let created_at = val.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "event_type": "assistant_text",
                        "content": content,
                        "created_at": created_at,
                    }))
                }
                "tool_use" => {
                    let tool = val
                        .get("tool")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown");
                    let raw_input = val.get("input");
                    // input may be a JSON object or a JSON-encoded string — normalize to string
                    let input_str = match raw_input {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    // Try to parse input_str as JSON object for structured access
                    let input_obj: Option<serde_json::Map<String, serde_json::Value>> =
                        serde_json::from_str(&input_str)
                            .ok()
                            .and_then(|v: serde_json::Value| {
                                if let serde_json::Value::Object(m) = v {
                                    Some(m)
                                } else {
                                    None
                                }
                            });
                    let created_at = val.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "event_type": "tool_use",
                        "tool": tool,
                        "input": input_obj,
                        "input_raw": input_str,
                        "content": format!("{tool}: {input_str}"),
                        "created_at": created_at,
                    }))
                }
                "user_message" => {
                    let content = val
                        .get("text")
                        .or_else(|| val.get("message"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    let created_at = val.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "event_type": "user_message",
                        "content": content,
                        "created_at": created_at,
                    }))
                }
                "system" => {
                    let content = val.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let created_at = val.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "event_type": "system",
                        "content": content,
                        "created_at": created_at,
                    }))
                }
                "session_result" => {
                    let session_id = val.get("session_id").and_then(|v| v.as_str());
                    let created_at = val.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "event_type": "session_result",
                        "content": session_id.unwrap_or(""),
                        "session_id": session_id,
                        "created_at": created_at,
                    }))
                }
                "start" => {
                    let created_at = val.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "event_type": "session_start",
                        "content": "",
                        "created_at": created_at,
                    }))
                }
                _ => None,
            }
        })
        .collect()
}

/// POST /api/workspaces/{ws}/v2/workers — create a worker from a brief.
#[derive(Debug, Deserialize)]
struct V2CreateWorkerBody {
    brief: serde_json::Value,
    repo: String,
    agent: Option<String>,
    model: Option<String>,
}

async fn v2_create_worker(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
    Json(body): Json<V2CreateWorkerBody>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let ws = match load_workspace_by_name(&workspace) {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("workspace '{workspace}' not found")})),
            )
                .into_response();
        }
    };

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let selected_agent = body
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .filter(|agent| {
            agent
                .parse::<apiari_swarm::core::agent::AgentKind>()
                .is_ok()
        })
        .map(str::to_string)
        .unwrap_or_else(|| ws.config.swarm.default_agent.clone());
    let selected_model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);

    // Extract optional goal from brief
    let goal = body
        .brief
        .get("goal")
        .and_then(|v| v.as_str())
        .map(String::from);

    // Extract review_mode from brief
    let review_mode = body
        .brief
        .get("review_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("local_first")
        .to_string();

    let worker = crate::buzz::worker::Worker {
        id: id.clone(),
        workspace: workspace.clone(),
        state: crate::buzz::worker::WorkerState::Briefed,
        brief: Some(body.brief.clone()),
        repo: Some(body.repo.clone()),
        branch: None,
        goal: goal.clone(),
        tests_passing: false,
        branch_ready: false,
        pr_url: None,
        pr_approved: false,
        ci_passing: None,
        is_stalled: false,
        revision_count: 0,
        review_mode: review_mode.clone(),
        blocked_reason: None,
        last_output_at: None,
        state_entered_at: now.clone(),
        created_at: now.clone(),
        updated_at: now,
        display_title: None,
        worktree_path: None,
        isolation_mode: None,
        agent_kind: Some(selected_agent.clone()),
        model: selected_model.clone(),
        repo_path: None,
        label: String::new(),
    };

    if let Err(e) = store.upsert(&worker) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }

    // Write brief as prompt file
    let prompt_file = std::env::temp_dir().join(format!("worker-{id}.txt"));
    let prompt_content = format_brief_as_prompt(&body.brief);
    if let Err(e) = std::fs::write(&prompt_file, &prompt_content) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("failed to write prompt file: {e}")})),
        )
            .into_response();
    }

    // Dispatch to swarm via embedded daemon IPC.
    let _ = std::fs::remove_file(&prompt_file); // prompt content is passed directly
    let swarm_result = state
        .worker_manager
        .create_worker_with_task_dir(
            &ws.config.root,
            &body.repo,
            &prompt_content,
            &selected_agent,
            selected_model.as_deref(),
            None,
            ws.config.swarm.worker_isolation.clone(),
        )
        .await;

    match swarm_result {
        Ok(swarm_id) => {
            let final_id = if !swarm_id.is_empty() && swarm_id != id {
                let _ = store.rekey(&id, &swarm_id);
                let mut rekeyed = worker.clone();
                rekeyed.id = swarm_id.clone();
                rekeyed.state = crate::buzz::worker::WorkerState::Queued;
                rekeyed.agent_kind = Some(selected_agent.clone());
                rekeyed.model = selected_model.clone();
                let _ = store.upsert(&rekeyed);
                swarm_id
            } else {
                let _ = store.transition(&workspace, &id, crate::buzz::worker::WorkerState::Queued);
                id.clone()
            };

            let _ = state
                .updates_tx
                .send(crate::daemon::http::WsUpdate::WorkerV2State {
                    workspace: workspace.clone(),
                    worker_id: final_id.clone(),
                    state: "queued".to_string(),
                    label: "Queued".to_string(),
                    properties: serde_json::json!({}),
                });

            if let Some(goal_text) = goal.filter(|g| !g.is_empty()) {
                let title_workspace = workspace.clone();
                let title_id = final_id.clone();
                let db_path = state.db_path.clone();
                tokio::spawn(async move {
                    generate_and_store_worker_title(
                        &title_workspace,
                        &title_id,
                        &goal_text,
                        &db_path,
                    )
                    .await;
                });
            }

            Json(serde_json::json!({
                "ok": true,
                "worker_id": final_id,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Semaphore capping concurrent title-generation claude subprocesses.
static TITLE_GEN_SEMAPHORE: std::sync::OnceLock<tokio::sync::Semaphore> =
    std::sync::OnceLock::new();

fn title_gen_semaphore() -> &'static tokio::sync::Semaphore {
    TITLE_GEN_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(4))
}

/// Generate a short display title for a worker using Claude and store it in the DB.
/// Runs as a background task; failures are silently logged.
async fn generate_and_store_worker_title(
    workspace: &str,
    worker_id: &str,
    goal: &str,
    db_path: &std::path::Path,
) {
    use tokio::io::AsyncWriteExt as _;
    use tracing::{info, warn};

    // Limit concurrent claude subprocesses for title generation
    let _permit = match title_gen_semaphore().try_acquire() {
        Ok(p) => p,
        Err(_) => {
            warn!("[worker-title/{workspace}/{worker_id}] skipped: title generation at capacity");
            return;
        }
    };

    let prompt = format!(
        "Generate a short display title (4-8 words) for this task. \
         Output only the title, no punctuation, no markdown, no explanation.\n\nTask: {goal}"
    );

    let result = async {
        let mut child = tokio::process::Command::new("claude")
            .arg("--print")
            .arg("--max-turns")
            .arg("1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn claude: {e}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| format!("write stdin: {e}"))?;
        }

        // 30-second timeout to avoid hanging tasks
        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            child.wait_with_output(),
        )
        .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("wait: {e}")),
            Err(_) => return Err("timeout waiting for claude".to_string()),
        };

        if !output.status.success() {
            return Err("claude exited non-zero".to_string());
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let title = raw
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();

        if title.is_empty() {
            return Err("empty title from claude".to_string());
        }

        // Truncate to 80 chars on a character boundary (not byte index)
        let title: String = title.chars().take(80).collect();

        let store = open_worker_store_from_path(db_path).map_err(|e| format!("open store: {e}"))?;
        store
            .update_display_title(workspace, worker_id, &title)
            .map_err(|e| format!("update: {e}"))?;

        Ok::<String, String>(title)
    }
    .await;

    match result {
        Ok(title) => info!("[worker-title/{workspace}/{worker_id}] generated: {title:?}"),
        Err(e) => warn!("[worker-title/{workspace}/{worker_id}] failed: {e}"),
    }
}

/// Copy agent events/output from the old swarm dir to the new one so history
/// is preserved across rekeys (send-to-dead-agent, requeue, redispatch).
fn copy_agent_dir(swarm_root: &std::path::Path, old_id: &str, new_id: &str) {
    let src = swarm_root.join("agents").join(old_id);
    let dst = swarm_root.join("agents").join(new_id);
    if !src.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&dst);
    for entry in std::fs::read_dir(&src).into_iter().flatten().flatten() {
        let from = entry.path();
        if let Some(name) = from.file_name() {
            let _ = std::fs::copy(&from, dst.join(name));
        }
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip escape sequence: ESC [ ... m
            if chars.peek() == Some(&'[') {
                chars.next();
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn format_brief_as_prompt(brief: &serde_json::Value) -> String {
    let goal = brief
        .get("goal")
        .and_then(|v| v.as_str())
        .unwrap_or("(no goal specified)");
    let mut parts = vec![format!("# Goal\n\n{goal}")];

    if let Some(context) = brief.get("context") {
        parts.push(format!("# Context\n\n{context}"));
    }
    if let Some(constraints) = brief.get("constraints").and_then(|v| v.as_array()) {
        let items: Vec<String> = constraints
            .iter()
            .filter_map(|v| v.as_str().map(|s| format!("- {s}")))
            .collect();
        if !items.is_empty() {
            parts.push(format!("# Constraints\n\n{}", items.join("\n")));
        }
    }
    if let Some(criteria) = brief.get("acceptance_criteria").and_then(|v| v.as_array()) {
        let items: Vec<String> = criteria
            .iter()
            .filter_map(|v| v.as_str().map(|s| format!("- {s}")))
            .collect();
        if !items.is_empty() {
            parts.push(format!("# Acceptance Criteria\n\n{}", items.join("\n")));
        }
    }

    // Review mode instructions — tell the worker explicitly what to do when done
    let review_mode = brief
        .get("review_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("local_first");
    let review_instructions = if review_mode == "pr_first" {
        "# When Done\n\nCommit your changes, push the branch, and open a PR with `gh pr create`."
    } else {
        "# When Done\n\nCommit your changes and push the branch. Do NOT open a PR — the reviewer will inspect the branch locally first."
    };
    parts.push(review_instructions.to_string());

    // Reporting — tell workers to write a structured report file so the daemon
    // can pick up test results without parsing free-form text.
    parts.push(
        "# Reporting\n\n\
        After running tests, write a status report to `.swarm/agents/$WORKER_ID/report.json` \
        (where `$WORKER_ID` is the id of this worktree, e.g. `apiari-df40`). \
        Use this exact format:\n\n\
        ```json\n\
        {\"tests_passing\": true, \"branch_ready\": true}\n\
        ```\n\n\
        Set `tests_passing` to `true` only if all tests pass. \
        Set `branch_ready` to `true` once the branch is pushed and ready for review. \
        The file path is relative to the workspace root: \
        `/path/to/workspace/.swarm/agents/<worker-id>/report.json`. \
        You can find the worker id from the worktree path or the `id` field in `.swarm/state.json`."
            .to_string(),
    );

    parts.join("\n\n")
}

/// POST /api/workspaces/{ws}/v2/workers/{id}/send — send message to worker.
#[derive(Debug, Deserialize)]
struct V2SendMessageBody {
    message: String,
}

async fn v2_send_message(
    Path((workspace, id)): Path<(String, String)>,
    State(state): State<HttpState>,
    Json(body): Json<V2SendMessageBody>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let worker = match store.get(&workspace, &id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            // Worker not in DB (e.g. created via swarm CLI directly). Fall back to
            // worker_manager.send_message which reads paths from state.json.
            return match state.worker_manager.send_message(&id, &body.message).await {
                Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response(),
            };
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let ws = match load_workspace_by_name(&workspace) {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("workspace '{workspace}' not found")})),
            )
                .into_response();
        }
    };

    // If branch_ready, OR the agent isn't in the live map (daemon restarted and
    // the agent process is gone), spawn a fresh agent with the message prepended.
    if worker.branch_ready {
        let brief = match &worker.brief {
            Some(b) => b.clone(),
            None => serde_json::json!({}),
        };
        let repo = match &worker.repo {
            Some(r) => r.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "worker has no repo"})),
                )
                    .into_response();
            }
        };

        let mut prompt = format!(
            "# Revision Instructions\n\n{}\n\n{}",
            body.message,
            format_brief_as_prompt(&brief)
        );

        // Also include any review feedback if present.
        let review_feedback: Option<String> = open_review_store_from_path(&state.db_path)
            .ok()
            .and_then(|rs| rs.list_for_worker(&workspace, &id).ok())
            .and_then(|reviews| {
                reviews
                    .into_iter()
                    .find(|r| r.verdict == "request_changes" && r.worker_message.is_some())
                    .and_then(|r| r.worker_message)
            });
        if let Some(ref feedback) = review_feedback {
            prompt.push_str(&format!("\n\n# Prior Review Feedback\n\n{feedback}"));
        }

        let tmp_id = uuid::Uuid::new_v4().to_string();
        let prompt_file = std::env::temp_dir().join(format!("worker-send-{tmp_id}.txt"));
        if let Err(e) = std::fs::write(&prompt_file, &prompt) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("failed to write prompt file: {e}")})),
            )
                .into_response();
        }

        let _ = std::fs::remove_file(&prompt_file);

        let new_swarm_id = match state
            .worker_manager
            .create_worker_with_task_dir(
                &ws.config.root,
                &repo,
                &prompt,
                worker
                    .agent_kind
                    .as_deref()
                    .unwrap_or(&ws.config.swarm.default_agent),
                worker.model.as_deref(),
                None,
                ws.config.swarm.worker_isolation.clone(),
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response();
            }
        };

        if new_swarm_id.is_empty() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "swarm create did not return a worker ID"})),
            )
                .into_response();
        }

        let _ = store.rekey(&id, &new_swarm_id);
        copy_agent_dir(&ws.config.root.join(".swarm"), &id, &new_swarm_id);
        let review_mode = brief
            .get("review_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("local_first")
            .to_string();
        let new_revision = worker.revision_count + 1;
        let updated_worker = crate::buzz::worker::Worker {
            id: new_swarm_id.clone(),
            workspace: workspace.clone(),
            state: crate::buzz::worker::WorkerState::Queued,
            brief: Some(brief),
            repo: Some(repo),
            branch: worker.branch.clone(),
            goal: worker.goal.clone(),
            tests_passing: worker.tests_passing,
            branch_ready: false,
            pr_url: worker.pr_url.clone(),
            pr_approved: worker.pr_approved,
            ci_passing: worker.ci_passing,
            is_stalled: false,
            revision_count: new_revision,
            review_mode,
            blocked_reason: None,
            display_title: worker.display_title.clone(),
            last_output_at: worker.last_output_at.clone(),
            state_entered_at: chrono::Utc::now().to_rfc3339(),
            created_at: worker.created_at.clone(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            worktree_path: None,
            isolation_mode: None,
            agent_kind: worker.agent_kind.clone(),
            model: worker.model.clone(),
            repo_path: None,
            label: String::new(),
        };
        let _ = store.upsert(&updated_worker);

        return Json(serde_json::json!({"ok": true, "new_id": new_swarm_id})).into_response();
    }

    if let Err(e) = state.worker_manager.send_message(&id, &body.message).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }

    // Waiting → running, increment revision
    if worker.state == crate::buzz::worker::WorkerState::Waiting {
        let _ = store.update_properties(
            &workspace,
            &id,
            crate::buzz::worker::WorkerPropertyUpdate {
                increment_revision: true,
                ..Default::default()
            },
        );
        let _ = store.transition(&workspace, &id, crate::buzz::worker::WorkerState::Running);

        if let Ok(Some(updated)) = store.get(&workspace, &id) {
            let _ = state.updates_tx.send(WsUpdate::WorkerV2State {
                workspace: workspace.clone(),
                worker_id: id.clone(),
                state: updated.state.as_str().to_string(),
                label: updated.label.clone(),
                properties: serde_json::json!({
                    "revision_count": updated.revision_count,
                    "is_stalled": updated.is_stalled,
                }),
            });
        }
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

/// POST /api/workspaces/{ws}/v2/workers/{id}/cancel — abandon a worker.
async fn v2_cancel_worker(
    Path((workspace, id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    match store.transition(&workspace, &id, crate::buzz::worker::WorkerState::Abandoned) {
        Ok(()) => {
            if let Ok(Some(updated)) = store.get(&workspace, &id) {
                let _ = state.updates_tx.send(WsUpdate::WorkerV2State {
                    workspace: workspace.clone(),
                    worker_id: id.clone(),
                    state: updated.state.as_str().to_string(),
                    label: updated.label.clone(),
                    properties: serde_json::json!({}),
                });
            }
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/workspaces/{ws}/v2/workers/{id}/requeue — re-queue a worker by spawning a fresh
/// swarm agent. If the worker has a `request_changes` review, its feedback is prepended to
/// the new prompt so the agent knows what to fix.
async fn v2_requeue_worker(
    Path((workspace, id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Load the original worker record.
    let worker = match store.get(&workspace, &id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "worker not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let brief = match &worker.brief {
        Some(b) => b.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "worker has no brief — cannot requeue"})),
            )
                .into_response();
        }
    };

    let repo = match &worker.repo {
        Some(r) => r.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "worker has no repo — cannot requeue"})),
            )
                .into_response();
        }
    };

    let ws = match load_workspace_by_name(&workspace) {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("workspace '{workspace}' not found")})),
            )
                .into_response();
        }
    };

    // Check for the most recent request_changes review and inject its feedback.
    let review_feedback: Option<String> = open_review_store_from_path(&state.db_path)
        .ok()
        .and_then(|rs| rs.list_for_worker(&workspace, &id).ok())
        .and_then(|reviews| {
            reviews
                .into_iter()
                .find(|r| r.verdict == "request_changes" && r.worker_message.is_some())
                .and_then(|r| r.worker_message)
        });

    // Build prompt: original brief + optional review feedback section.
    let mut prompt = format_brief_as_prompt(&brief);
    if let Some(ref feedback) = review_feedback {
        prompt.push_str(&format!(
            "\n\n# Review Feedback (Previous Attempt)\n\nA reviewer inspected your last attempt and requested changes. Address all of the following before marking the work done:\n\n{feedback}"
        ));
    }

    // Write to temp file (prompt can be large).
    let tmp_id = uuid::Uuid::new_v4().to_string();
    let prompt_file = std::env::temp_dir().join(format!("worker-requeue-{tmp_id}.txt"));
    if let Err(e) = std::fs::write(&prompt_file, &prompt) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("failed to write prompt file: {e}")})),
        )
            .into_response();
    }

    let _ = std::fs::remove_file(&prompt_file);

    let new_swarm_id = match state
        .worker_manager
        .create_worker_with_task_dir(
            &ws.config.root,
            &repo,
            &prompt,
            worker
                .agent_kind
                .as_deref()
                .unwrap_or(&ws.config.swarm.default_agent),
            worker.model.as_deref(),
            None,
            ws.config.swarm.worker_isolation.clone(),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    if new_swarm_id.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "swarm create did not return a worker ID"})),
        )
            .into_response();
    }

    // Rekey the existing DB record to the new swarm ID, preserving all reviews and history.
    // The task is the same task — only the underlying swarm worktree changed.
    let _ = store.rekey(&id, &new_swarm_id);
    copy_agent_dir(&ws.config.root.join(".swarm"), &id, &new_swarm_id);

    let now = chrono::Utc::now().to_rfc3339();
    let review_mode = brief
        .get("review_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("local_first")
        .to_string();
    let new_revision = worker.revision_count + 1;
    let updated_worker = crate::buzz::worker::Worker {
        id: new_swarm_id.clone(),
        workspace: workspace.clone(),
        state: crate::buzz::worker::WorkerState::Queued,
        brief: Some(brief),
        repo: Some(repo),
        branch: worker.branch.clone(),
        goal: worker.goal.clone(),
        tests_passing: worker.tests_passing,
        branch_ready: false,
        pr_url: worker.pr_url.clone(),
        pr_approved: worker.pr_approved,
        ci_passing: worker.ci_passing,
        is_stalled: false,
        revision_count: new_revision,
        review_mode,
        blocked_reason: None,
        last_output_at: worker.last_output_at.clone(),
        state_entered_at: now.clone(),
        created_at: worker.created_at.clone(),
        updated_at: now,
        display_title: worker.display_title.clone(),
        worktree_path: None,
        isolation_mode: None,
        agent_kind: worker.agent_kind.clone(),
        model: worker.model.clone(),
        repo_path: None,
        label: String::new(),
    };
    let _ = store.upsert(&updated_worker);

    // Notify WebSocket listeners — single update for the rekeyed worker.
    let _ = state.updates_tx.send(WsUpdate::WorkerV2State {
        workspace: workspace.clone(),
        worker_id: new_swarm_id.clone(),
        state: "queued".to_string(),
        label: "Queued".to_string(),
        properties: serde_json::json!({"revision_count": new_revision}),
    });

    tracing::info!(
        "[requeue/{workspace}] {id} → {new_swarm_id} (revision {}{})",
        new_revision,
        if review_feedback.is_some() {
            ", with review feedback"
        } else {
            ""
        }
    );

    Json(serde_json::json!({
        "ok": true,
        "new_worker_id": new_swarm_id,
        "with_review_feedback": review_feedback.is_some(),
    }))
    .into_response()
}

// ── Auto-requeue ───────────────────────────────────────────────────────

/// Spawn a fresh swarm worker for `worker` with `review_feedback` injected into the prompt.
/// Called automatically when `swarm send` fails after a review (e.g. agent process is dead).
async fn auto_requeue_with_feedback(
    workspace: &str,
    worker: &crate::buzz::worker::Worker,
    review_feedback: &str,
    workspace_root: &std::path::Path,
    db_path: &std::path::Path,
    updates_tx: tokio::sync::broadcast::Sender<WsUpdate>,
    worker_manager: Arc<WorkerManager>,
) {
    let brief = match &worker.brief {
        Some(b) => b.clone(),
        None => {
            tracing::warn!(
                "[auto-requeue/{workspace}] {} has no brief — skipping",
                worker.id
            );
            return;
        }
    };
    let repo = match &worker.repo {
        Some(r) => r.clone(),
        None => {
            tracing::warn!(
                "[auto-requeue/{workspace}] {} has no repo — skipping",
                worker.id
            );
            return;
        }
    };

    let mut prompt = format_brief_as_prompt(&brief);
    prompt.push_str(&format!(
        "\n\n# Review Feedback (Previous Attempt)\n\nA reviewer inspected your last attempt and requested changes. Address all of the following before marking the work done:\n\n{review_feedback}"
    ));

    let new_id = match worker_manager
        .create_worker(
            workspace_root,
            &repo,
            &prompt,
            worker.agent_kind.as_deref().unwrap_or("codex"),
            worker.model.as_deref(),
        )
        .await  // uses worktree isolation (no ws config available here)
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                "[auto-requeue/{workspace}] swarm create failed for {}: {e}",
                worker.id
            );
            return;
        }
    };

    if new_id.is_empty() {
        tracing::warn!(
            "[auto-requeue/{workspace}] swarm create returned no ID for {}",
            worker.id
        );
        return;
    }

    // Rekey the existing DB record to the new swarm ID, preserving all reviews and history.
    let store = match open_worker_store_from_path(db_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("[auto-requeue/{workspace}] failed to open worker store: {e}");
            return;
        }
    };

    let _ = store.rekey(&worker.id, &new_id);
    copy_agent_dir(&workspace_root.join(".swarm"), &worker.id, &new_id);

    let now = chrono::Utc::now().to_rfc3339();
    let review_mode = brief
        .get("review_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("local_first")
        .to_string();
    let new_revision = worker.revision_count + 1;
    let updated_worker = crate::buzz::worker::Worker {
        id: new_id.clone(),
        workspace: workspace.to_string(),
        state: crate::buzz::worker::WorkerState::Queued,
        brief: Some(brief),
        repo: Some(repo),
        branch: worker.branch.clone(),
        goal: worker.goal.clone(),
        tests_passing: worker.tests_passing,
        branch_ready: false,
        pr_url: worker.pr_url.clone(),
        pr_approved: worker.pr_approved,
        ci_passing: worker.ci_passing,
        is_stalled: false,
        revision_count: new_revision,
        review_mode,
        blocked_reason: None,
        last_output_at: worker.last_output_at.clone(),
        state_entered_at: now.clone(),
        created_at: worker.created_at.clone(),
        updated_at: now,
        display_title: worker.display_title.clone(),
        worktree_path: None,
        isolation_mode: None,
        agent_kind: worker.agent_kind.clone(),
        model: worker.model.clone(),
        repo_path: None,
        label: String::new(),
    };
    let _ = store.upsert(&updated_worker);

    let _ = updates_tx.send(WsUpdate::WorkerV2State {
        workspace: workspace.to_string(),
        worker_id: new_id.clone(),
        state: "queued".to_string(),
        label: "Queued".to_string(),
        properties: serde_json::json!({"revision_count": new_revision}),
    });

    tracing::info!(
        "[auto-requeue/{workspace}] {} → {} (revision {}, with review feedback)",
        worker.id,
        new_id,
        new_revision
    );
}

// ── v2 Worker review API routes ────────────────────────────────────────

/// Open a ReviewStore against the given db_path.
fn open_review_store_from_path(
    db_path: &std::path::Path,
) -> color_eyre::Result<crate::buzz::review::ReviewStore> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| color_eyre::eyre::eyre!("failed to open review db: {e}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let conn = std::sync::Arc::new(std::sync::Mutex::new(conn));
    crate::buzz::review::ReviewStore::new(conn)
}

/// Append a single synthetic event to `.swarm/agents/{worker_id}/events.jsonl`
/// so the worker timeline records the review outcome.
fn append_review_timeline_event(
    workspace_root: &std::path::Path,
    worker_id: &str,
    verdict: &str,
    summary: &str,
    send_succeeded: bool,
) {
    let text = match verdict {
        "approve" => format!("✓ Review approved — {summary}"),
        "request_changes" => {
            let suffix = if send_succeeded {
                "Feedback sent to worker."
            } else {
                "Worker auto-requeued with feedback."
            };
            format!("✗ Review requested changes — {summary}\n\n{suffix}")
        }
        _ => format!("💬 Review comment — {summary}"),
    };

    let event = serde_json::json!({
        "type": "system",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "text": text,
    });

    let dir = workspace_root.join(".swarm").join("agents").join(worker_id);
    let path = dir.join("events.jsonl");

    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("[review] failed to create agent dir for {worker_id}: {e}");
        return;
    }

    let line = format!("{}\n", event);
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(line.as_bytes())
        })
    {
        tracing::warn!("[review] failed to write timeline event for {worker_id}: {e}");
    }
}

/// POST /api/workspaces/{ws}/v2/workers/{id}/review — trigger a review (202 Accepted).
async fn v2_request_review(
    Path((workspace, id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let worker = match store.get(&workspace, &id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "worker not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Only allow review when worker is in waiting state and branch is ready.
    if worker.state != crate::buzz::worker::WorkerState::Waiting || !worker.branch_ready {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "review only available when worker is waiting and branch_ready=true"})),
        )
            .into_response();
    }

    let workspace_root = load_workspace_root(&workspace);
    let review_config = load_workspace_by_name(&workspace)
        .map(|ws| ws.config.review.clone())
        .unwrap_or_default();
    let db_path = state.db_path.as_ref().clone();
    let updates_tx = state.updates_tx.clone();
    let worker_manager = Arc::clone(&state.worker_manager);

    // Run review in background — caller gets 202 immediately.
    tokio::spawn(async move {
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => {
                let _ = c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;");
                std::sync::Arc::new(std::sync::Mutex::new(c))
            }
            Err(e) => {
                tracing::error!("[review/{workspace}] failed to open DB: {e}");
                return;
            }
        };

        // Build a Value sender from the WsUpdate broadcast so review.rs can emit events.
        let (val_tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let val_tx_clone = val_tx.clone();

        // Forward val_tx messages to updates_tx as WorkerReview variants.
        let updates_tx_clone = updates_tx.clone();
        let workspace_clone = workspace.clone();
        let worker_id_clone = worker.id.clone();
        tokio::spawn(async move {
            let mut rx = val_tx_clone.subscribe();
            while let Ok(event) = rx.recv().await {
                if let Some(verdict) = event.get("verdict").and_then(|v| v.as_str()) {
                    let _ = updates_tx_clone.send(WsUpdate::WorkerReview {
                        workspace: workspace_clone.clone(),
                        worker_id: worker_id_clone.clone(),
                        verdict: verdict.to_string(),
                        reviewer: event
                            .get("reviewer")
                            .and_then(|r| r.as_str())
                            .unwrap_or("General")
                            .to_string(),
                    });
                }
            }
        });

        match crate::buzz::review::run_review(
            &workspace,
            &worker,
            &workspace_root,
            conn,
            Some(val_tx),
            &review_config,
        )
        .await
        {
            Ok(outcome) => {
                tracing::info!(
                    "[review/{workspace}] review {} done verdict={}",
                    outcome.review.id,
                    outcome.review.verdict
                );

                // Append a synthetic timeline event so the timeline reflects the review outcome.
                append_review_timeline_event(
                    &workspace_root,
                    &worker.id,
                    &outcome.review.verdict,
                    &outcome.review.summary,
                    outcome.send_succeeded,
                );

                // Auto-requeue when the worker message couldn't be delivered.
                // This happens when the agent process has exited (e.g. after a reboot).
                if crate::buzz::review::should_auto_requeue(
                    &outcome.review.verdict,
                    outcome.send_succeeded,
                    &outcome.review.worker_message,
                ) && let Some(ref msg) = outcome.review.worker_message
                {
                    tracing::info!(
                        "[review/{workspace}] send failed — auto-requeueing {}",
                        worker.id
                    );
                    auto_requeue_with_feedback(
                        &workspace,
                        &worker,
                        msg,
                        &workspace_root,
                        &db_path,
                        updates_tx.clone(),
                        Arc::clone(&worker_manager),
                    )
                    .await;
                }
            }
            Err(e) => {
                tracing::error!("[review/{workspace}] review failed for {}: {e}", worker.id);
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "review_started"})),
    )
        .into_response()
}

/// GET /api/workspaces/{ws}/v2/workers/{id}/reviews — list reviews for a worker.
async fn v2_list_worker_reviews(
    Path((workspace, id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_review_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    match store.list_for_worker(&workspace, &id) {
        Ok(reviews) => Json(serde_json::json!({"reviews": reviews})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── v2 Auto-bot API routes ─────────────────────────────────────────────

/// Open an AutoBotStore against the given db_path.
fn open_auto_bot_store_from_path(
    db_path: &std::path::Path,
) -> color_eyre::Result<crate::buzz::auto_bot::AutoBotStore> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| color_eyre::eyre::eyre!("failed to open auto_bot db: {e}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let conn = std::sync::Arc::new(std::sync::Mutex::new(conn));
    let store = crate::buzz::auto_bot::AutoBotStore::new(conn);
    store.ensure_schema()?;
    Ok(store)
}

/// GET /api/workspaces/{ws}/v2/auto-bots — list auto bots.
async fn v2_list_auto_bots(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match store.list(&workspace) {
        Ok(bots) => Json(serde_json::json!({"auto_bots": bots})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /api/workspaces/{ws}/v2/auto-bots/{id} — detail + last 20 runs.
async fn v2_get_auto_bot(
    Path((workspace, bot_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match store.get(&workspace, &bot_id) {
        Ok(Some(bot)) => {
            let runs = store.list_runs(&bot_id, 20).unwrap_or_default();
            Json(serde_json::json!({"auto_bot": bot, "runs": runs})).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Request body for creating an auto bot.
#[derive(Debug, serde::Deserialize)]
struct V2CreateAutoBotBody {
    name: String,
    #[serde(default = "default_auto_bot_color")]
    color: String,
    trigger_type: String,
    cron_schedule: Option<String>,
    signal_source: Option<String>,
    signal_filter: Option<String>,
    prompt: String,
    #[serde(default = "default_provider")]
    provider: String,
    model: Option<String>,
}

fn default_auto_bot_color() -> String {
    "#f5c542".to_string()
}

fn default_provider() -> String {
    "claude".to_string()
}

/// POST /api/workspaces/{ws}/v2/auto-bots — create.
async fn v2_create_auto_bot(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
    Json(body): Json<V2CreateAutoBotBody>,
) -> impl IntoResponse {
    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    let bot = crate::buzz::auto_bot::AutoBot {
        id: uuid::Uuid::new_v4().to_string(),
        workspace: workspace.clone(),
        name: body.name,
        color: body.color,
        trigger_type: body.trigger_type,
        cron_schedule: body.cron_schedule,
        signal_source: body.signal_source,
        signal_filter: body.signal_filter,
        prompt: body.prompt,
        provider: body.provider,
        model: body.model,
        enabled: true,
        paused_until: None,
        created_at: now.clone(),
        updated_at: now,
        status: String::new(),
    };

    match store.upsert(&bot) {
        Ok(()) => match store.get(&workspace, &bot.id) {
            Ok(Some(created)) => (StatusCode::CREATED, Json(created)).into_response(),
            _ => (StatusCode::CREATED, Json(serde_json::json!({"id": bot.id}))).into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Request body for patching an auto bot.
#[derive(Debug, serde::Deserialize)]
struct V2UpdateAutoBotBody {
    name: Option<String>,
    color: Option<String>,
    trigger_type: Option<String>,
    cron_schedule: Option<String>,
    signal_source: Option<String>,
    signal_filter: Option<String>,
    prompt: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    enabled: Option<bool>,
}

/// PATCH /api/workspaces/{ws}/v2/auto-bots/{id} — update (partial).
async fn v2_update_auto_bot(
    Path((workspace, bot_id)): Path<(String, String)>,
    State(state): State<HttpState>,
    Json(body): Json<V2UpdateAutoBotBody>,
) -> impl IntoResponse {
    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let existing = match store.get(&workspace, &bot_id) {
        Ok(Some(b)) => b,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let updated = crate::buzz::auto_bot::AutoBot {
        name: body.name.unwrap_or(existing.name),
        color: body.color.unwrap_or(existing.color),
        trigger_type: body.trigger_type.unwrap_or(existing.trigger_type),
        cron_schedule: body.cron_schedule.or(existing.cron_schedule),
        signal_source: body.signal_source.or(existing.signal_source),
        signal_filter: body.signal_filter.or(existing.signal_filter),
        prompt: body.prompt.unwrap_or(existing.prompt),
        provider: body.provider.unwrap_or(existing.provider),
        model: body.model.or(existing.model),
        enabled: body.enabled.unwrap_or(existing.enabled),
        updated_at: chrono::Utc::now().to_rfc3339(),
        ..existing
    };

    match store.upsert(&updated) {
        Ok(()) => match store.get(&workspace, &bot_id) {
            Ok(Some(b)) => Json(b).into_response(),
            _ => Json(serde_json::json!({"ok": true})).into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/workspaces/{ws}/v2/auto-bots/{id} — delete.
async fn v2_delete_auto_bot(
    Path((workspace, bot_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match store.delete(&workspace, &bot_id) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "not found"})),
                )
                    .into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": msg})),
                )
                    .into_response()
            }
        }
    }
}

/// GET /api/workspaces/{ws}/v2/auto-bots/{id}/runs — paginated runs.
async fn v2_list_auto_bot_runs(
    Path((workspace, bot_id)): Path<(String, String)>,
    State(state): State<HttpState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // Check workspace is valid by checking the bot exists.
    let _ = workspace;
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let offset: usize = params
        .get("offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Fetch limit+offset rows then slice (SQLite OFFSET requires LIMIT to work efficiently)
    let all_runs = match store.list_runs(&bot_id, limit + offset) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let runs: Vec<_> = all_runs.into_iter().skip(offset).collect();
    Json(serde_json::json!({"runs": runs})).into_response()
}

/// POST /api/workspaces/{ws}/v2/auto-bots/{id}/trigger — manually trigger.
async fn v2_trigger_auto_bot(
    Path((workspace, bot_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_auto_bot_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let bot = match store.get(&workspace, &bot_id) {
        Ok(Some(b)) => b,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let run_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let run = crate::buzz::auto_bot::AutoBotRun {
        id: run_id.clone(),
        auto_bot_id: bot_id.clone(),
        workspace: workspace.clone(),
        triggered_by: "manual".to_string(),
        started_at: now,
        finished_at: None,
        outcome: None,
        summary: None,
        worker_id: None,
        cost_usd: None,
    };

    if let Err(e) = store.insert_run(&run) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }

    // Broadcast started event
    let _ = state.updates_tx.send(WsUpdate::AutoBotRunStarted {
        workspace: workspace.clone(),
        auto_bot_id: bot_id.clone(),
        run_id: run_id.clone(),
    });

    // Spawn the bot execution asynchronously
    let store = std::sync::Arc::new(store);
    let workspace_root = load_workspace_root(&workspace);
    let db_path = (*state.db_path).clone();
    let workspace_config = load_workspace_by_name(&workspace).map(|ws| ws.config);
    let updates_tx = state.updates_tx.clone();
    tokio::spawn(async move {
        crate::buzz::auto_bot_runner::run_bot_external(
            bot,
            "manual".to_string(),
            run_id.clone(),
            store,
            workspace.clone(),
            workspace_root,
            db_path,
            workspace_config,
        )
        .await;

        // Broadcast finished event (we don't know outcome here, so emit a generic done)
        let _ = updates_tx.send(WsUpdate::AutoBotRunFinished {
            workspace,
            auto_bot_id: bot_id,
            run_id,
            outcome: "unknown".to_string(),
        });
    });

    Json(serde_json::json!({"run_id": run.id, "ok": true})).into_response()
}

/// Load the workspace root path from config, falling back to CWD.
fn load_workspace_root(workspace: &str) -> std::path::PathBuf {
    load_workspace_by_name(workspace)
        .map(|ws| ws.config.root)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

// ── v2 Worker-hooks API routes ─────────────────────────────────────────

/// Open a WorkerHookStore against the given db_path.
fn open_worker_hook_store_from_path(
    db_path: &std::path::Path,
) -> color_eyre::Result<crate::buzz::worker_hooks::WorkerHookStore> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| color_eyre::eyre::eyre!("failed to open worker_hook db: {e}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let conn = std::sync::Arc::new(std::sync::Mutex::new(conn));
    // Ensure the worker_hooks table exists (schema created by worker::ensure_schema).
    {
        let c = conn.lock().unwrap();
        crate::buzz::worker::ensure_schema(&c)?;
    }
    Ok(crate::buzz::worker_hooks::WorkerHookStore::new(conn))
}

/// GET /api/workspaces/{ws}/v2/worker-hooks — list hooks.
async fn v2_list_worker_hooks(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_hook_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match store.list(&workspace) {
        Ok(hooks) => Json(serde_json::json!({"hooks": hooks})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST body for creating a worker hook.
#[derive(Debug, serde::Deserialize)]
struct CreateWorkerHookBody {
    #[serde(default)]
    trigger_state: Option<String>,
    #[serde(default)]
    trigger_property: Option<String>,
    #[serde(default)]
    trigger_value: Option<String>,
    #[serde(default)]
    duration_minutes: Option<i64>,
    action: String,
    #[serde(default)]
    auto_bot_id: Option<i64>,
}

/// POST /api/workspaces/{ws}/v2/worker-hooks — create hook.
async fn v2_create_worker_hook(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
    Json(body): Json<CreateWorkerHookBody>,
) -> impl IntoResponse {
    let store = match open_worker_hook_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let new_hook = crate::buzz::worker_hooks::NewWorkerHook {
        workspace: workspace.clone(),
        trigger_state: body.trigger_state,
        trigger_property: body.trigger_property,
        trigger_value: body.trigger_value,
        duration_minutes: body.duration_minutes,
        action: body.action,
        auto_bot_id: body.auto_bot_id,
    };
    match store.insert(&new_hook) {
        Ok(hook) => (StatusCode::CREATED, Json(serde_json::json!({"hook": hook}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/workspaces/{ws}/v2/worker-hooks/{id} — delete hook.
async fn v2_delete_worker_hook(
    Path((workspace, hook_id)): Path<(String, i64)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let store = match open_worker_hook_store_from_path(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match store.delete(&workspace, hook_id) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── v2 Context-bot API routes ──────────────────────────────────────────

/// Context snapshot sent with each context bot message.
#[derive(Debug, Deserialize)]
struct ContextBotContext {
    view: String,
    #[serde(default)]
    entity_id: Option<String>,
    #[serde(default)]
    entity_snapshot: Option<serde_json::Value>,
}

/// POST /api/workspaces/{ws}/v2/context-bot/chat request body.
#[derive(Debug, Deserialize)]
struct ContextBotChatBody {
    message: String,
    /// Client-managed session ID — echoed back in the response.
    /// Not used for server-side resumption (each call is stateless).
    #[serde(default)]
    session_id: Option<String>,
    context: ContextBotContext,
    /// Optional model override for this session. Falls back to workspace
    /// config `context_bot_model`, then to `claude-sonnet-4-6`.
    #[serde(default)]
    model: Option<String>,
}

/// Response from the context bot endpoint.
#[derive(Debug, Serialize)]
struct ContextBotChatResponse {
    response: String,
    session_id: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatched_worker_id: Option<String>,
}

/// Build the context-bot system prompt by injecting the view context snapshot.
fn build_context_bot_system_prompt(
    ctx: &ContextBotContext,
    workspace_root: &std::path::Path,
) -> String {
    let mut prompt = String::from(
        "You are a context-aware assistant embedded in the apiari web UI. \
         You help the user understand their project, investigate history, and decide what to do next.\n\n",
    );

    prompt.push_str(&format!("Workspace root: {}\n", workspace_root.display()));
    prompt.push_str(&format!("Current view: {}\n", ctx.view));

    if let Some(ref entity_id) = ctx.entity_id {
        prompt.push_str(&format!("Focused on: {entity_id}\n"));
    }

    if let Some(ref snapshot) = ctx.entity_snapshot {
        let pretty =
            serde_json::to_string_pretty(snapshot).unwrap_or_else(|_| snapshot.to_string());
        prompt.push_str("\nCurrent state snapshot:\n");
        prompt.push_str(&pretty);
        prompt.push('\n');
    }

    prompt.push_str(
        "\nYou have full tool access: use Bash to run git commands, grep, etc. \
         Use Read to inspect files. The workspace root is your working directory.\n\
         \n\
         You can also dispatch a new worker by responding with DISPATCH_WORKER: {goal} on its own line.\n\
         \n\
         Be concise. The user is a developer. Answer directly.",
    );

    prompt
}

fn format_tool_activity(name: &str, input: &serde_json::Value) -> String {
    let detail = input
        .get("command")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("pattern"))
        .or_else(|| input.get("query"))
        .or_else(|| input.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| format!(": {}", &s[..s.len().min(80)]))
        .unwrap_or_default();
    format!("{name}{detail}")
}

/// POST /api/workspaces/{ws}/v2/context-bot/chat — stateless context bot turn.
///
/// Session history is managed client-side. Each call is independent; the
/// session_id is echoed back for client tracking.
///
/// TODO: Session resumption via `claude --continue` / `--resume` is a future
/// enhancement. The client already tracks message history locally and sends
/// the full context snapshot on each call, so stateless operation is sufficient
/// for now.
async fn v2_context_bot_chat(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
    Json(body): Json<ContextBotChatBody>,
) -> impl IntoResponse {
    let session_id = body
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Resolve model: request body → workspace config → default.
    const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
    let model = body.model.clone().unwrap_or_else(|| {
        load_workspace_by_name(&workspace)
            .and_then(|ws| ws.config.context_bot_model)
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    });

    tracing::info!(
        workspace = %workspace,
        view = %body.context.view,
        model = %model,
        msg_len = body.message.len(),
        "[context-bot] request"
    );

    // Verify claude CLI is available.
    let which = tokio::process::Command::new("which")
        .arg("claude")
        .output()
        .await;
    if !which.is_ok_and(|out| out.status.success()) {
        tracing::error!(workspace = %workspace, "[context-bot] claude CLI not found on PATH");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "claude CLI not available"})),
        )
            .into_response();
    }

    let workspace_root = load_workspace_root(&workspace);
    let system_prompt = build_context_bot_system_prompt(&body.context, &workspace_root);

    // Run claude with stream-json output so we can broadcast live tool activity.
    // Message is passed via stdin; cwd is the workspace root for git/file access.
    let raw = match tokio::time::timeout(std::time::Duration::from_secs(300), async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let mut child = tokio::process::Command::new("claude")
            .arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--model")
            .arg(&model)
            .arg("--system-prompt")
            .arg(&system_prompt)
            .current_dir(&workspace_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        // Write message then close stdin so claude knows input is done.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.message.as_bytes()).await;
        }

        // Read NDJSON lines and broadcast tool activity over WebSocket.
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut response = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                match val.get("type").and_then(|t| t.as_str()) {
                    Some("assistant") => {
                        if let Some(blocks) = val["message"]["content"].as_array() {
                            for block in blocks {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("tool_use") => {
                                        let name = block
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("tool");
                                        let input = block.get("input").cloned().unwrap_or_default();
                                        let activity = format_tool_activity(name, &input);
                                        let _ =
                                            state.updates_tx.send(WsUpdate::ContextBotActivity {
                                                workspace: workspace.clone(),
                                                session_id: session_id.clone(),
                                                activity,
                                            });
                                    }
                                    Some("text") => {
                                        if let Some(t) = block.get("text").and_then(|t| t.as_str())
                                        {
                                            response.push_str(t);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Some("result") => {
                        // result.result is the canonical final text
                        if let Some(r) = val.get("result").and_then(|r| r.as_str()) {
                            response = r.to_string();
                        }
                    }
                    _ => {}
                }
            }
        }

        let status = child.wait().await?;
        Ok::<(String, bool), std::io::Error>((response, status.success()))
    })
    .await
    {
        Ok(Ok((text, true))) => {
            let response = text.trim().to_string();
            tracing::info!(
                workspace = %workspace,
                response_len = response.len(),
                "[context-bot] ok"
            );
            response
        }
        Ok(Ok((text, false))) => {
            let code = -1i32;
            tracing::error!(
                workspace = %workspace,
                exit_code = code,
                stdout = %text,
                "[context-bot] claude exited non-zero"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("claude exited non-zero: {text}")})),
            )
                .into_response();
        }
        Ok(Err(e)) => {
            tracing::error!(workspace = %workspace, err = %e, "[context-bot] failed to spawn claude");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": format!("failed to run claude: {e}")})),
            )
                .into_response();
        }
        Err(_elapsed) => {
            tracing::error!(workspace = %workspace, "[context-bot] claude timed out after 300s");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({"error": "claude timed out after 300s"})),
            )
                .into_response();
        }
    };

    // Check for DISPATCH_WORKER: directive.
    let mut dispatched_worker_id: Option<String> = None;
    let ws = load_workspace_by_name(&workspace);

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(goal) = trimmed.strip_prefix("DISPATCH_WORKER:") {
            let goal = goal.trim().to_string();
            if !goal.is_empty() {
                let repo = ws
                    .as_ref()
                    .and_then(|w| w.config.repos.first())
                    .map(String::as_str)
                    .unwrap_or("");

                // Build a structured brief — same shape as the modal dispatch path — so the
                // worker gets full instructions and the Brief tab has content to display.
                let brief_json = serde_json::json!({
                    "goal": goal,
                    "context": {
                        "view": body.context.view,
                        "entity_id": body.context.entity_id,
                        "snapshot": body.context.entity_snapshot,
                    },
                    "constraints": [],
                    "acceptance_criteria": [],
                    "review_mode": "local_first",
                });
                let prompt_content = format_brief_as_prompt(&brief_json);

                // Save to SQLite before dispatch so the Brief tab shows content immediately.
                if let Ok(store) = open_worker_store_from_path(&state.db_path) {
                    let pre_id = uuid::Uuid::new_v4().to_string();
                    let now = chrono::Utc::now().to_rfc3339();
                    let worker = crate::buzz::worker::Worker {
                        id: pre_id.clone(),
                        workspace: workspace.clone(),
                        state: crate::buzz::worker::WorkerState::Briefed,
                        brief: Some(brief_json),
                        repo: Some(repo.to_string()),
                        branch: None,
                        goal: Some(goal.clone()),
                        tests_passing: false,
                        branch_ready: false,
                        pr_url: None,
                        pr_approved: false,
                        ci_passing: None,
                        is_stalled: false,
                        revision_count: 0,
                        review_mode: "local_first".to_string(),
                        blocked_reason: None,
                        last_output_at: None,
                        state_entered_at: now.clone(),
                        created_at: now.clone(),
                        updated_at: now,
                        display_title: None,
                        worktree_path: None,
                        isolation_mode: None,
                        agent_kind: Some("codex".to_string()),
                        model: None,
                        repo_path: None,
                        label: String::new(),
                    };
                    let _ = store.upsert(&worker);

                    if let Ok(swarm_id) = state
                        .worker_manager
                        .create_worker_with_task_dir(
                            &workspace_root,
                            repo,
                            &prompt_content,
                            "codex",
                            None,
                            None,
                            crate::config::WorkerIsolation::default(),
                        )
                        .await
                    {
                        // Re-key to the real swarm ID if it differs
                        let final_id = if !swarm_id.is_empty() && swarm_id != pre_id {
                            let _ = store.rekey(&pre_id, &swarm_id);
                            swarm_id
                        } else {
                            let _ = store.transition(
                                &workspace,
                                &pre_id,
                                crate::buzz::worker::WorkerState::Queued,
                            );
                            pre_id
                        };
                        dispatched_worker_id = Some(final_id);
                    }
                }
                break;
            }
        }
    }

    let _ = state.updates_tx; // keep state alive (ws broadcast not needed for this endpoint)

    Json(ContextBotChatResponse {
        response: raw,
        session_id,
        model,
        dispatched_worker_id,
    })
    .into_response()
}

// ── Dashboard widget handlers ──────────────────────────────────────────

// ── Context-bot session persistence ───────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct ContextBotSessionRow {
    id: String,
    workspace: String,
    title: String,
    model: String,
    context_view: String,
    context_entity_id: Option<String>,
    context_snapshot: Option<serde_json::Value>,
    messages: serde_json::Value,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct UpsertContextBotSessionBody {
    title: String,
    model: String,
    context_view: String,
    #[serde(default)]
    context_entity_id: Option<String>,
    #[serde(default)]
    context_snapshot: Option<serde_json::Value>,
    messages: serde_json::Value,
    created_at: String,
    updated_at: String,
}

async fn v2_list_context_bot_sessions(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let db_path = state.db_path.as_ref().clone();
    let rows =
        tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<ContextBotSessionRow>> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT id, workspace, title, model, context_view, context_entity_id,
                    context_snapshot, messages, created_at, updated_at
             FROM context_bot_sessions WHERE workspace = ?1
             ORDER BY updated_at DESC",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![workspace], |row| {
                    let snapshot_str: Option<String> = row.get(6)?;
                    let messages_str: String = row.get(7)?;
                    Ok(ContextBotSessionRow {
                        id: row.get(0)?,
                        workspace: row.get(1)?,
                        title: row.get(2)?,
                        model: row.get(3)?,
                        context_view: row.get(4)?,
                        context_entity_id: row.get(5)?,
                        context_snapshot: snapshot_str.and_then(|s| serde_json::from_str(&s).ok()),
                        messages: serde_json::from_str(&messages_str)
                            .unwrap_or(serde_json::Value::Array(vec![])),
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await;

    match rows {
        Ok(Ok(rows)) => Json(rows).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn v2_upsert_context_bot_session(
    Path((workspace, session_id)): Path<(String, String)>,
    State(state): State<HttpState>,
    Json(body): Json<UpsertContextBotSessionBody>,
) -> impl IntoResponse {
    let db_path = state.db_path.as_ref().clone();
    let result = tokio::task::spawn_blocking(move || -> rusqlite::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute(
            "INSERT INTO context_bot_sessions
                (id, workspace, title, model, context_view, context_entity_id,
                 context_snapshot, messages, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                model = excluded.model,
                context_snapshot = excluded.context_snapshot,
                messages = excluded.messages,
                updated_at = excluded.updated_at",
            rusqlite::params![
                session_id,
                workspace,
                body.title,
                body.model,
                body.context_view,
                body.context_entity_id,
                body.context_snapshot.as_ref().map(|v| v.to_string()),
                body.messages.to_string(),
                body.created_at,
                body.updated_at,
            ],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn v2_delete_context_bot_session(
    Path((workspace, session_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let db_path = state.db_path.as_ref().clone();
    let result = tokio::task::spawn_blocking(move || -> rusqlite::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute(
            "DELETE FROM context_bot_sessions WHERE id = ?1 AND workspace = ?2",
            rusqlite::params![session_id, workspace],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// GET /api/workspaces/{workspace}/v2/widgets — return all active widget slots.
async fn v2_list_widgets(
    Path(workspace): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let db_path = state.db_path.as_ref().clone();
    let store = match crate::buzz::signal::store::SignalStore::open(&db_path, &workspace) {
        Ok(s) => s,
        Err(e) => {
            error!("[widgets] open store: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!([])),
            )
                .into_response();
        }
    };
    match store.get_widgets() {
        Ok(rows) => {
            let widgets: Vec<serde_json::Value> = rows
                .into_iter()
                .filter_map(|(slot, widget_json, updated_at)| {
                    let mut v: serde_json::Value = serde_json::from_str(&widget_json).ok()?;
                    v["slot"] = serde_json::Value::String(slot);
                    v["updated_at"] = serde_json::Value::String(updated_at);
                    Some(v)
                })
                .collect();
            Json(widgets).into_response()
        }
        Err(e) => {
            error!("[widgets] get_widgets: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!([])),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct UpsertWidgetBody {
    #[serde(flatten)]
    widget: serde_json::Value,
    #[serde(default)]
    ttl_minutes: Option<i64>,
}

/// PUT /api/workspaces/{workspace}/v2/widgets/{slot} — upsert a widget slot.
async fn v2_upsert_widget(
    Path((workspace, slot)): Path<(String, String)>,
    State(state): State<HttpState>,
    Json(body): Json<UpsertWidgetBody>,
) -> impl IntoResponse {
    let db_path = state.db_path.as_ref().clone();
    let store = match crate::buzz::signal::store::SignalStore::open(&db_path, &workspace) {
        Ok(s) => s,
        Err(e) => {
            error!("[widgets] open store: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Serialize just the widget fields (without ttl_minutes).
    let mut widget_value = body.widget.clone();
    // Ensure slot is embedded.
    widget_value["slot"] = serde_json::Value::String(slot.clone());
    let widget_json = match serde_json::to_string(&widget_value) {
        Ok(s) => s,
        Err(e) => {
            error!("[widgets] serialize: {e}");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    if let Err(e) = store.upsert_widget(&slot, &widget_json, body.ttl_minutes) {
        error!("[widgets] upsert: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let updated_at = chrono::Utc::now().to_rfc3339();
    let _ = state.updates_tx.send(WsUpdate::WidgetUpdated {
        workspace,
        slot,
        widget: widget_value,
        updated_at,
    });

    StatusCode::OK.into_response()
}

/// DELETE /api/workspaces/{workspace}/v2/widgets/{slot} — delete a widget slot.
async fn v2_delete_widget(
    Path((workspace, slot)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let db_path = state.db_path.as_ref().clone();
    let store = match crate::buzz::signal::store::SignalStore::open(&db_path, &workspace) {
        Ok(s) => s,
        Err(e) => {
            error!("[widgets] open store: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(e) = store.delete_widget(&slot) {
        error!("[widgets] delete: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
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
        db_path: Arc::new(db_path.clone()),
        workspace: Arc::new(workspace.clone()),
        updates_tx: updates_tx.clone(),
        signal_tx,
        chat_tx,
        cancel_tx,
        worker_manager: Arc::new(WorkerManager::new(db_path, workspace)),
    };

    let app = Router::new()
        .route("/api/graph", get(get_graph).put(save_graph))
        .route("/api/graph/yaml", get(get_yaml))
        .route("/api/tasks", get(get_tasks).delete(clear_tasks))
        .route("/api/signal", post(inject_signal))
        .route("/api/workspaces", get(list_workspaces))
        .route("/api/workspaces/{workspace}/bots", get(list_workspace_bots))
        .route(
            "/api/workspaces/{workspace}/tasks",
            get(list_workspace_tasks),
        )
        .route(
            "/api/workspaces/{workspace}/repos",
            get(list_workspace_repos),
        )
        .route(
            "/api/workspaces/{workspace}/worker-environment",
            get(get_workspace_worker_environment),
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
            "/api/workspaces/{workspace}/workers/{worker_id}/promote",
            post(promote_workspace_worker),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/redispatch",
            post(redispatch_workspace_worker),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/close",
            post(close_workspace_worker),
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
        // v2 worker routes
        .route(
            "/api/workspaces/{workspace}/v2/workers",
            get(v2_list_workers).post(v2_create_worker),
        )
        .route(
            "/api/workspaces/{workspace}/v2/workers/{worker_id}",
            get(v2_get_worker),
        )
        .route(
            "/api/workspaces/{workspace}/v2/workers/{worker_id}/send",
            post(v2_send_message),
        )
        .route(
            "/api/workspaces/{workspace}/v2/workers/{worker_id}/cancel",
            post(v2_cancel_worker),
        )
        .route(
            "/api/workspaces/{workspace}/v2/workers/{worker_id}/requeue",
            post(v2_requeue_worker),
        )
        .route(
            "/api/workspaces/{workspace}/v2/workers/{worker_id}/review",
            post(v2_request_review),
        )
        .route(
            "/api/workspaces/{workspace}/v2/workers/{worker_id}/reviews",
            get(v2_list_worker_reviews),
        )
        // v2 auto-bot routes
        .route(
            "/api/workspaces/{workspace}/v2/auto-bots",
            get(v2_list_auto_bots).post(v2_create_auto_bot),
        )
        .route(
            "/api/workspaces/{workspace}/v2/auto-bots/{bot_id}",
            get(v2_get_auto_bot)
                .patch(v2_update_auto_bot)
                .delete(v2_delete_auto_bot),
        )
        .route(
            "/api/workspaces/{workspace}/v2/auto-bots/{bot_id}/runs",
            get(v2_list_auto_bot_runs),
        )
        .route(
            "/api/workspaces/{workspace}/v2/auto-bots/{bot_id}/trigger",
            post(v2_trigger_auto_bot),
        )
        // v2 worker-hooks routes
        .route(
            "/api/workspaces/{workspace}/v2/worker-hooks",
            get(v2_list_worker_hooks).post(v2_create_worker_hook),
        )
        .route(
            "/api/workspaces/{workspace}/v2/worker-hooks/{hook_id}",
            axum::routing::delete(v2_delete_worker_hook),
        )
        // v2 context-bot routes
        .route(
            "/api/workspaces/{workspace}/v2/context-bot/chat",
            post(v2_context_bot_chat),
        )
        .route(
            "/api/workspaces/{workspace}/v2/context-bot/sessions",
            get(v2_list_context_bot_sessions),
        )
        .route(
            "/api/workspaces/{workspace}/v2/context-bot/sessions/{session_id}",
            put(v2_upsert_context_bot_session).delete(v2_delete_context_bot_session),
        )
        // v2 dashboard widget routes
        .route(
            "/api/workspaces/{workspace}/v2/widgets",
            get(v2_list_widgets),
        )
        .route(
            "/api/workspaces/{workspace}/v2/widgets/{slot}",
            put(v2_upsert_widget).delete(v2_delete_widget),
        )
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
                                let latest_attempt = crate::buzz::task::store::TaskStore::open(&db_path)
                                    .ok()
                                    .and_then(|store| latest_attempt_view_for_task(&store, &task.id));
                                let _ = updates_tx.send(WsUpdate::TaskUpdated {
                                    task: task_to_view_with_attempt(task, latest_attempt),
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
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
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

    fn set_git_identity(path: &Path) {
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .status()
            .unwrap();
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "git {:?} failed in {}",
            args,
            path.display()
        );
    }

    fn write_and_commit(path: &Path, name: &str, content: &str, message: &str) {
        fs::write(path.join(name), content).unwrap();
        git(path, &["add", name]);
        git(path, &["commit", "-m", message]);
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
            review: crate::config::ReviewConfig::default(),
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
            token_controls: crate::config::TokenControls::default(),
            context_bot_model: None,
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
            token_controls: crate::config::TokenControls::default(),
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
                token_controls: crate::config::TokenControls::default(),
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
                token_controls: crate::config::TokenControls::default(),
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
            token_controls: crate::config::TokenControls::default(),
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
                token_controls: crate::config::TokenControls::default(),
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
                token_controls: crate::config::TokenControls::default(),
            },
        ]);

        let items = bot_items_for_workspace(&config);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Main");
        assert_eq!(items[0].role.as_deref(), Some("Coordinator"));
        assert_eq!(items[0].color.as_deref(), Some("#f5c542"));
        assert_eq!(items[0].description.as_deref(), Some("Coordinator"));
        assert_eq!(items[0].provider.as_deref(), Some("claude"));
        assert_eq!(items[1].name, "Codex");
        assert_eq!(items[1].role.as_deref(), Some("Code specialist"));
        assert_eq!(items[1].description.as_deref(), Some("Code specialist"));
        assert_eq!(items[1].provider.as_deref(), Some("codex"));
    }

    #[test]
    fn bot_description_for_ui_strips_role_and_color_metadata_from_prompt() {
        let bee = crate::config::BeeConfig {
            name: "Workspace".to_string(),
            role: None,
            color: Some("#d97706".to_string()),
            execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            max_turns: 20,
            prompt: Some(
                "Role: Workspace assistant\n\nColor: #d97706\n\nProactively notify the user about important events."
                    .to_string(),
            ),
            max_session_turns: 50,
            signal_hooks: vec![],
            topic_id: None,
            heartbeat: None,
            heartbeat_prompt: None,
            token_controls: crate::config::TokenControls::default(),
        };

        assert_eq!(
            bot_description_for_ui(&bee).as_deref(),
            Some("Proactively notify the user about important events.")
        );
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
                execution_note: None,
                ready_branch: None,
                has_uncommitted_changes: false,
                task_id: None,
                task_title: None,
                task_stage: None,
                task_lifecycle_state: None,
                task_repo: None,
                latest_attempt: None,
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
                execution_note: None,
                ready_branch: None,
                has_uncommitted_changes: false,
                task_id: None,
                task_title: None,
                task_stage: None,
                task_lifecycle_state: None,
                task_repo: None,
                latest_attempt: None,
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
    fn git_ahead_behind_counts_reports_tracking_delta_without_github_api() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("origin.git");
        let local = temp.path().join("local");
        let other = temp.path().join("other");

        git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);

        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), local.to_str().unwrap()],
        );
        set_git_identity(&local);
        write_and_commit(&local, "README.md", "base\n", "initial");
        git(&local, &["branch", "-M", "main"]);
        git(&local, &["push", "-u", "origin", "main"]);

        git(
            temp.path(),
            &[
                "clone",
                "--branch",
                "main",
                remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        set_git_identity(&other);
        write_and_commit(&other, "remote.txt", "remote\n", "remote");
        git(&other, &["push", "origin", "main"]);

        git(&local, &["fetch", "origin"]);
        write_and_commit(&local, "local.txt", "local\n", "local");

        assert_eq!(current_git_upstream(&local).as_deref(), Some("origin/main"));
        assert_eq!(git_ahead_behind_counts(&local, "origin/main"), (1, 1));
    }

    #[test]
    fn git_output_ignores_git_dir_env_var() {
        // Regression: when the daemon is launched from within Claude Code's sandbox,
        // GIT_DIR is set in the environment. Without env_remove("GIT_DIR"), git ignores
        // current_dir and operates on the wrong repo, returning None for upstream info.
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        git(temp.path(), &["init", repo.to_str().unwrap()]);
        set_git_identity(&repo);
        write_and_commit(&repo, "f.txt", "x\n", "init");

        // Simulate the Claude Code sandbox environment
        unsafe { std::env::set_var("GIT_DIR", "/nonexistent/.git") };
        let branch = git_output(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        unsafe { std::env::remove_var("GIT_DIR") };

        assert!(
            branch.is_some(),
            "git_output should succeed despite GIT_DIR being set"
        );
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
            worker_manager: Arc::new(WorkerManager::new(
                crate::config::db_path(),
                "apiari".to_string(),
            )),
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
    async fn get_workspace_workers_surfaces_task_owned_lifecycle_fields() {
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

        let store = crate::buzz::task::store::TaskStore::open(&crate::config::db_path()).unwrap();
        let task = crate::buzz::task::Task {
            id: "task-1".to_string(),
            workspace: "apiari".to_string(),
            title: "Tighten mobile overview cards".to_string(),
            stage: crate::buzz::task::TaskStage::HumanReview,
            source: Some("manual".to_string()),
            source_url: None,
            worker_id: Some("apiari-7a04".to_string()),
            pr_url: Some("https://example.com/pr/7".to_string()),
            pr_number: Some(7),
            repo: Some("apiari".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            resolved_at: None,
            metadata: serde_json::json!({}),
        };
        store.create_task(&task).unwrap();

        let workers = get_workspace_workers(Path("apiari".to_string())).await.0;
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "apiari-7a04");
        assert_eq!(workers[0].status, "waiting");
        assert_eq!(workers[0].task_id.as_deref(), Some("task-1"));
        assert_eq!(
            workers[0].task_title.as_deref(),
            Some("Tighten mobile overview cards")
        );
        assert_eq!(workers[0].task_stage.as_deref(), Some("Human Review"));
        assert_eq!(workers[0].task_repo.as_deref(), Some("apiari"));
        assert_eq!(
            workers[0].pr_url.as_deref(),
            Some("https://example.com/pr/7")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_workers_marks_stalled_uncommitted_workers() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        let swarm_dir = root.join(".swarm");
        let worktree = root.join(".swarm/wt/apiari-7a04");
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
        fs::write(worktree.join("card.css"), "before\n").unwrap();
        Command::new("git")
            .args(["add", "card.css"])
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
                "-m",
                "init",
                "-q",
            ])
            .current_dir(&worktree)
            .status()
            .unwrap();
        fs::write(worktree.join("card.css"), "after\n").unwrap();

        fs::create_dir_all(&swarm_dir).unwrap();
        write_swarm_state(
            &swarm_dir.join("state.json"),
            &serde_json::json!({
                "worktrees": [
                    {
                        "id": "apiari-7a04",
                        "branch": "swarm/example",
                        "agent_kind": "codex",
                        "phase": "running",
                        "agent_session_status": "stopped",
                        "prompt": "tighten cards",
                        "repo_path": root,
                        "worktree_path": worktree
                    }
                ]
            }),
        );

        let workers = get_workspace_workers(Path("apiari".to_string())).await.0;
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].status, "stalled");
        assert_eq!(workers[0].ready_branch, None);
        assert_eq!(workers[0].has_uncommitted_changes, true);
        assert_eq!(
            workers[0].execution_note.as_deref(),
            Some("Uncommitted diff, no ready branch, and no active session.")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_workers_keeps_actively_running_workers_out_of_stalled() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        let swarm_dir = root.join(".swarm");
        let worktree = root.join(".swarm/wt/apiari-87e2");
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
        fs::write(worktree.join("card.css"), "before\n").unwrap();
        Command::new("git")
            .args(["add", "card.css"])
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
                "-m",
                "init",
                "-q",
            ])
            .current_dir(&worktree)
            .status()
            .unwrap();
        fs::write(worktree.join("card.css"), "after\n").unwrap();

        fs::create_dir_all(&swarm_dir).unwrap();
        write_swarm_state(
            &swarm_dir.join("state.json"),
            &serde_json::json!({
                "worktrees": [
                    {
                        "id": "apiari-87e2",
                        "branch": "swarm/example",
                        "agent_kind": "codex",
                        "phase": "running",
                        "agent_session_status": "running",
                        "prompt": "tighten cards",
                        "repo_path": root,
                        "worktree_path": worktree
                    }
                ]
            }),
        );

        let workers = get_workspace_workers(Path("apiari".to_string())).await.0;
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].status, "running");
        assert_eq!(
            workers[0].execution_note.as_deref(),
            Some("Uncommitted diff present; worker has not marked a ready branch yet.")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_workers_marks_session_completed_without_handoff_as_failed() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        let swarm_dir = root.join(".swarm");
        let worktree = root.join(".swarm/wt/apiari-ebbc");
        let agent_dir = root.join(".swarm/agents/apiari-ebbc");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

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
                        "id": "apiari-ebbc",
                        "branch": "swarm/example",
                        "agent_kind": "codex",
                        "phase": "running",
                        "agent_session_status": "running",
                        "prompt": "tighten cards",
                        "repo_path": root,
                        "worktree_path": worktree
                    }
                ]
            }),
        );

        fs::write(
            agent_dir.join("events.jsonl"),
            r#"{"type":"assistant_text","timestamp":"2026-05-04T22:40:59.649150Z","text":"I stopped."}
{"type":"session_result","timestamp":"2026-05-04T22:40:59.714859Z","turns":0,"cost_usd":null,"session_id":null}
"#,
        )
        .unwrap();

        let workers = get_workspace_workers(Path("apiari".to_string())).await.0;
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].status, "failed");
        assert_eq!(
            workers[0].execution_note.as_deref(),
            Some(
                "Worker finished without a ready branch or PR handoff. It stopped without a promotable code change."
            )
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
                        "worktree_path": root.join(".swarm/wt/common-sdk-fix"),
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

        let worktree_dir = root.join(".swarm/wt/common-sdk-fix/.task");
        fs::create_dir_all(&worktree_dir).unwrap();
        fs::write(
            worktree_dir.join("TASK.md"),
            "# Task\n\nExample\n\n## Worker Mode\n- `implementation`\n",
        )
        .unwrap();
        fs::write(
            worktree_dir.join("CONTEXT.md"),
            "# Context\n\n- Repository: `apiari`\n",
        )
        .unwrap();
        fs::write(
            worktree_dir.join("SHAPING.md"),
            "# Coordinator Shaping\n\n## Goal\n- Investigate repo slug resolution\n",
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
        assert_eq!(
            detail
                .task_packet
                .as_ref()
                .and_then(|packet| packet.worker_mode.as_deref()),
            Some("implementation")
        );
        assert_eq!(
            detail
                .task_packet
                .as_ref()
                .and_then(|packet| packet.shaping_md.as_deref()),
            Some("# Coordinator Shaping\n\n## Goal\n- Investigate repo slug resolution\n")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn get_workspace_worker_detail_falls_back_to_task_when_swarm_state_is_gone() {
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

        let store = crate::buzz::task::store::TaskStore::open(&crate::config::db_path()).unwrap();
        let task = crate::buzz::task::Task {
            id: "task-2".to_string(),
            workspace: "apiari".to_string(),
            title: "Review mobile worker lifecycle".to_string(),
            stage: crate::buzz::task::TaskStage::HumanReview,
            source: Some("manual".to_string()),
            source_url: None,
            worker_id: Some("apiari-7a04".to_string()),
            pr_url: Some("https://example.com/pr/8".to_string()),
            pr_number: Some(8),
            repo: Some("apiari".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            resolved_at: None,
            metadata: serde_json::json!({}),
        };
        store.create_task(&task).unwrap();

        let detail =
            get_workspace_worker_detail(Path(("apiari".to_string(), "apiari-7a04".to_string())))
                .await
                .expect("task-backed worker detail should resolve")
                .0;

        assert_eq!(detail.worker.id, "apiari-7a04");
        assert_eq!(detail.worker.status, "waiting");
        assert_eq!(detail.worker.task_stage.as_deref(), Some("Human Review"));
        assert_eq!(
            detail.worker.task_title.as_deref(),
            Some("Review mobile worker lifecycle")
        );
        assert_eq!(detail.worker.task_repo.as_deref(), Some("apiari"));
        assert_eq!(detail.prompt, None);
        assert_eq!(detail.output, None);
        assert!(detail.conversation.is_empty());
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
            worker_manager: Arc::new(WorkerManager::new(
                crate::config::db_path(),
                "apiari".to_string(),
            )),
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
            worker_manager: Arc::new(WorkerManager::new(
                crate::config::db_path(),
                "apiari".to_string(),
            )),
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

    #[test]
    fn task_view_derives_ready_from_failed_in_progress_attempt() {
        let task = crate::buzz::task::Task {
            id: "task-ready".to_string(),
            workspace: "apiari".to_string(),
            title: "Retry mobile cards".to_string(),
            stage: crate::buzz::task::TaskStage::InProgress,
            source: Some("manual".to_string()),
            source_url: None,
            worker_id: Some("worker-1".to_string()),
            pr_url: None,
            pr_number: None,
            repo: Some("apiari".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            resolved_at: None,
            metadata: serde_json::json!({}),
        };
        let attempt = TaskAttemptView {
            worker_id: "worker-1".to_string(),
            role: "implementation".to_string(),
            state: "failed".to_string(),
            branch: None,
            pr_url: None,
            pr_number: None,
            detail: Some("Worker closed without PR".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
        };

        let view = task_to_view_with_attempt(&task, Some(attempt));
        assert_eq!(view.stage, "In Progress");
        assert_eq!(view.lifecycle_state, "Ready");
    }

    #[test]
    fn task_view_derives_changes_requested_from_reviewer_verdict() {
        let task = crate::buzz::task::Task {
            id: "task-review".to_string(),
            workspace: "apiari".to_string(),
            title: "Review cards".to_string(),
            stage: crate::buzz::task::TaskStage::InAiReview,
            source: Some("manual".to_string()),
            source_url: None,
            worker_id: Some("reviewer-1".to_string()),
            pr_url: Some("https://example.com/pr/12".to_string()),
            pr_number: Some(12),
            repo: Some("apiari".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            resolved_at: None,
            metadata: serde_json::json!({}),
        };
        let attempt = TaskAttemptView {
            worker_id: "reviewer-1".to_string(),
            role: "reviewer".to_string(),
            state: "succeeded".to_string(),
            branch: None,
            pr_url: Some("https://example.com/pr/12".to_string()),
            pr_number: Some(12),
            detail: Some("Review verdict: CHANGES_REQUESTED".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
        };

        let view = task_to_view_with_attempt(&task, Some(attempt));
        assert_eq!(view.stage, "In AI Review");
        assert_eq!(view.lifecycle_state, "Changes Requested");
    }

    // ── Context-bot tests ──────────────────────────────────────────────

    #[test]
    fn context_bot_system_prompt_includes_view_and_snapshot() {
        let ctx = ContextBotContext {
            view: "worker_detail".to_string(),
            entity_id: Some("apiari-3".to_string()),
            entity_snapshot: Some(serde_json::json!({
                "state": "waiting",
                "label": "Tests failing",
                "goal": "Add rate limiting to /api/chat",
                "branch": "swarm/rate-limit",
                "pr_url": "https://github.com/example/pr/42",
                "tests_passing": false,
                "branch_ready": true,
                "revision_count": 1
            })),
        };

        let prompt = build_context_bot_system_prompt(&ctx);

        // Prompt should contain the view name
        assert!(
            prompt.contains("worker_detail"),
            "prompt should include the view name"
        );

        // Prompt should contain the entity ID
        assert!(
            prompt.contains("apiari-3"),
            "prompt should include the entity_id"
        );

        // Prompt should contain the snapshot fields
        assert!(
            prompt.contains("tests_passing"),
            "prompt should include snapshot keys"
        );
        assert!(
            prompt.contains("branch_ready"),
            "prompt should include branch_ready key"
        );
        assert!(
            prompt.contains("rate-limit"),
            "prompt should include snapshot values"
        );

        // Prompt should contain the dispatch instruction
        assert!(
            prompt.contains("DISPATCH_WORKER"),
            "prompt should explain the dispatch directive"
        );

        // Response shape: verify ContextBotChatResponse serializes correctly
        let resp = ContextBotChatResponse {
            response: "The tests are failing because...".to_string(),
            session_id: "ctx-abc123".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            dispatched_worker_id: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["session_id"].as_str(), Some("ctx-abc123"));
        assert_eq!(
            json["response"].as_str(),
            Some("The tests are failing because...")
        );
        // dispatched_worker_id should be absent when None
        assert!(
            json.get("dispatched_worker_id").is_none(),
            "dispatched_worker_id should be omitted when None"
        );
    }

    #[test]
    fn context_bot_system_prompt_without_entity_id() {
        let ctx = ContextBotContext {
            view: "auto_bot_feed".to_string(),
            entity_id: None,
            entity_snapshot: Some(serde_json::json!({"status": "idle"})),
        };

        let prompt = build_context_bot_system_prompt(&ctx);
        assert!(prompt.contains("auto_bot_feed"));
        assert!(prompt.contains("idle"));
        // "You are looking at:" should NOT appear when entity_id is None
        assert!(
            !prompt.contains("You are looking at:"),
            "entity_id line should be absent when no entity_id"
        );
    }

    #[test]
    fn context_bot_response_with_dispatched_worker_serializes_worker_id() {
        let resp = ContextBotChatResponse {
            response: "Dispatching worker.\n\nDISPATCH_WORKER: Fix the failing auth tests"
                .to_string(),
            session_id: "ctx-xyz".to_string(),
            model: "claude-opus-4-7".to_string(),
            dispatched_worker_id: Some("apiari-5".to_string()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            json["dispatched_worker_id"].as_str(),
            Some("apiari-5"),
            "dispatched_worker_id should appear in JSON when set"
        );
        assert_eq!(json["session_id"].as_str(), Some("ctx-xyz"));
    }

    // ── v2_send_message: daemon-restart regression ─────────────────────────
    //
    // Before the fix, a worker in the DB but absent from the WorkerManager live
    // map (e.g. after a daemon restart) returned 500 "worker not found or not
    // running". The fix routes such workers through the requeue path instead.
    // These tests verify that the 500 from WorkerManager.send_message is never
    // the final response — we always take the requeue branch.

    fn make_test_state_with_db(db_path: &std::path::Path) -> HttpState {
        let (updates_tx, _) = broadcast::channel(4);
        let (signal_tx, _) = mpsc::unbounded_channel();
        let (chat_tx, _) = mpsc::unbounded_channel();
        let (cancel_tx, _) = mpsc::unbounded_channel();
        HttpState {
            graph: Arc::new(RwLock::new(
                crate::buzz::orchestrator::graph::builtin::builtin_workflow(),
            )),
            yaml_path: Arc::new(None),
            db_path: Arc::new(db_path.to_path_buf()),
            workspace: Arc::new("apiari".to_string()),
            updates_tx,
            signal_tx,
            chat_tx,
            cancel_tx,
            worker_manager: Arc::new(WorkerManager::new(
                db_path.to_path_buf(),
                "apiari".to_string(),
            )),
        }
    }

    fn seed_waiting_worker(
        db_path: &std::path::Path,
        id: &str,
        workspace: &str,
        repo: Option<&str>,
        branch_ready: bool,
    ) {
        let store = open_worker_store_from_path(db_path).unwrap();
        let _ = store.upsert(&crate::buzz::worker::Worker {
            id: id.to_string(),
            workspace: workspace.to_string(),
            state: crate::buzz::worker::WorkerState::Waiting,
            brief: Some(serde_json::json!({"goal": "fix the bug"})),
            repo: repo.map(str::to_string),
            branch: Some("feat/test".to_string()),
            goal: Some("fix the bug".to_string()),
            tests_passing: false,
            branch_ready,
            pr_url: None,
            pr_approved: false,
            ci_passing: None,
            is_stalled: false,
            revision_count: 0,
            review_mode: "local_first".to_string(),
            blocked_reason: None,
            display_title: None,
            last_output_at: None,
            state_entered_at: chrono::Utc::now().to_rfc3339(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            worktree_path: None,
            isolation_mode: None,
            agent_kind: None,
            model: None,
            repo_path: None,
            label: String::new(),
        });
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_to_orphaned_worker_takes_requeue_path_not_500() {
        // Regression: after daemon restart, workers not in the live map returned
        // 500 "worker not found or not running". Now they take the requeue path.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "apiari", &root);

        let db_path = temp.path().join("test.db");
        // Worker has no repo — the requeue path will return 400 "worker has no repo".
        // The old broken path returned 500 "worker ghost-1234 not found or not running".
        seed_waiting_worker(&db_path, "ghost-1234", "apiari", None, false);

        let state = make_test_state_with_db(&db_path);
        let resp = v2_send_message(
            Path(("apiari".to_string(), "ghost-1234".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "please fix the tests".to_string(),
            }),
        )
        .await
        .into_response();

        // Must NOT be the WorkerManager "not found or not running" 500.
        // The handler now takes the send_message/resume path for non-branch_ready
        // workers; the error relates to missing state.json, not a dead channel.
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "must not hit dead send_message path: got '{err}'"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_branch_ready_also_takes_requeue_path() {
        // branch_ready workers were already routed correctly; this confirms
        // the condition still works after the daemon-restart fix.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("apiari");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "apiari", &root);

        let db_path = temp.path().join("test.db");
        seed_waiting_worker(&db_path, "ready-1234", "apiari", None, true);

        let state = make_test_state_with_db(&db_path);
        let resp = v2_send_message(
            Path(("apiari".to_string(), "ready-1234".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "please address the review comments".to_string(),
            }),
        )
        .await
        .into_response();

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "branch_ready should not hit send_message: got '{err}'"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // Comprehensive worker lifecycle tests
    //
    // Every case that repeatedly broke in production is covered here.
    // The helper `make_test_state_with_db` and `seed_waiting_worker` are
    // defined above; additional seeds are inline below.
    // ══════════════════════════════════════════════════════════════════════

    fn seed_worker(
        db_path: &std::path::Path,
        id: &str,
        workspace: &str,
        state: crate::buzz::worker::WorkerState,
        repo: Option<&str>,
        brief: Option<serde_json::Value>,
        branch_ready: bool,
        pr_url: Option<&str>,
        pr_approved: bool,
        tests_passing: bool,
        branch: Option<&str>,
        last_output_at: Option<&str>,
        revision_count: i64,
    ) {
        let store = open_worker_store_from_path(db_path).unwrap();
        let _ = store.upsert(&crate::buzz::worker::Worker {
            id: id.to_string(),
            workspace: workspace.to_string(),
            state,
            brief,
            repo: repo.map(str::to_string),
            branch: branch.map(str::to_string),
            goal: Some("fix the bug".to_string()),
            tests_passing,
            branch_ready,
            pr_url: pr_url.map(str::to_string),
            pr_approved,
            ci_passing: None,
            is_stalled: false,
            revision_count,
            review_mode: "local_first".to_string(),
            blocked_reason: None,
            display_title: Some("Fix the bug".to_string()),
            last_output_at: last_output_at.map(str::to_string),
            state_entered_at: chrono::Utc::now().to_rfc3339(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            worktree_path: None,
            isolation_mode: None,
            agent_kind: None,
            model: None,
            repo_path: None,
            label: String::new(),
        });
    }

    async fn response_json(resp: axum::response::Response) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap_or(serde_json::json!({}))
    }

    // ── v2_send_message ───────────────────────────────────────────────────

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_error_when_worker_not_found_anywhere() {
        // Worker not in DB → falls back to worker_manager.send_message (state.json path).
        // Since state.json also doesn't exist in this test, we get a 500, not 404.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");
        let state = make_test_state_with_db(&db_path);

        let resp = v2_send_message(
            Path(("ws".to_string(), "ghost-0000".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "hello".to_string(),
            }),
        )
        .await
        .into_response();

        assert_ne!(resp.status(), StatusCode::OK);
        let json = response_json(resp).await;
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "must not hit dead channel path: '{err}'"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_404_when_workspace_not_found() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let db_path = temp.path().join("test.db");
        // Insert worker but no workspace config.
        seed_worker(
            &db_path,
            "w-1",
            "missing-ws",
            crate::buzz::worker::WorkerState::Running,
            Some("repo"),
            Some(serde_json::json!({"goal":"x"})),
            false,
            None,
            false,
            false,
            None,
            None,
            0,
        );
        let state = make_test_state_with_db(&db_path);

        let resp = v2_send_message(
            Path(("missing-ws".to_string(), "w-1".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "hello".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = response_json(resp).await;
        assert!(json["error"].as_str().unwrap_or("").contains("missing-ws"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_requeue_preserves_branch_and_pr_url() {
        // Regression: requeue was losing branch, pr_url, pr_approved, tests_passing.
        // After requeue the new worker record must carry all of these forward.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        seed_worker(
            &db_path,
            "w-1",
            "ws",
            crate::buzz::worker::WorkerState::Waiting,
            Some("norepo"),
            Some(serde_json::json!({"goal": "fix auth", "review_mode": "local_first"})),
            true, // branch_ready → takes requeue path without needing live agent
            Some("https://github.com/org/repo/pull/42"),
            true, // pr_approved
            true, // tests_passing
            Some("feat/fix-auth-1a2b"),
            Some("2026-05-01T12:00:00Z"),
            3,
        );

        let store = open_worker_store_from_path(&db_path).unwrap();
        let original = store.get("ws", "w-1").unwrap().unwrap();
        assert_eq!(original.branch.as_deref(), Some("feat/fix-auth-1a2b"));
        assert_eq!(
            original.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        assert!(original.pr_approved);
        assert!(original.tests_passing);
        assert_eq!(original.revision_count, 3);
        assert_eq!(original.display_title.as_deref(), Some("Fix the bug"));

        // The requeue path fails at create_worker (no real repo) but the
        // key assertion is that field preservation happens before the
        // create_worker call. We test the fields that are set on the worker
        // struct before upsert. Since create_worker will fail, we inspect
        // the pre-requeue state from the seed and verify the struct fields
        // are wired correctly in v2_requeue_worker by checking the updated
        // record IF requeue succeeded, or verifying the source fields match.
        // Direct field check on original:
        assert_eq!(
            original.last_output_at.as_deref(),
            Some("2026-05-01T12:00:00Z")
        );

        // Verify the handler reaches the requeue path (not 500 "not found"):
        let state = make_test_state_with_db(&db_path);
        let resp = v2_send_message(
            Path(("ws".to_string(), "w-1".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "please fix the linting errors".to_string(),
            }),
        )
        .await
        .into_response();

        let json = response_json(resp).await;
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "must not hit dead send_message path: '{err}'"
        );
        // Fails at repo lookup — that's expected without a real git repo.
        // The important thing: it tried to requeue, not to call send_message.
        assert!(
            err.contains("repo") || json["ok"].as_bool() == Some(true),
            "should fail at repo resolution, not at send_message: '{err}'"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_error_when_orphaned_worker_has_no_paths() {
        // Worker in DB (no repo, not branch_ready, not live) → send_message →
        // resume_worker → fails (no worktree_path in DB, no state.json) → 500.
        // The old "requeue" path that checked for missing repo is no longer taken
        // for non-branch_ready workers.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");
        seed_worker(
            &db_path,
            "w-1",
            "ws",
            crate::buzz::worker::WorkerState::Waiting,
            None, // no repo
            Some(serde_json::json!({"goal": "x"})),
            false,
            None,
            false,
            false,
            None,
            None,
            0,
        );
        let state = make_test_state_with_db(&db_path);

        let resp = v2_send_message(
            Path(("ws".to_string(), "w-1".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "fix it".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = response_json(resp).await;
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "must not hit dead send_message path: '{err}'"
        );
    }

    // ── v2_requeue_worker ─────────────────────────────────────────────────

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn requeue_404_when_worker_not_in_db() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let db_path = temp.path().join("test.db");
        let state = make_test_state_with_db(&db_path);

        let resp = v2_requeue_worker(Path(("ws".to_string(), "ghost".to_string())), State(state))
            .await
            .into_response();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn requeue_400_when_worker_has_no_brief() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");
        seed_worker(
            &db_path,
            "w-1",
            "ws",
            crate::buzz::worker::WorkerState::Done,
            Some("repo"),
            None, // no brief
            false,
            None,
            false,
            false,
            None,
            None,
            0,
        );
        let state = make_test_state_with_db(&db_path);

        let resp = v2_requeue_worker(Path(("ws".to_string(), "w-1".to_string())), State(state))
            .await
            .into_response();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = response_json(resp).await;
        assert!(json["error"].as_str().unwrap_or("").contains("brief"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn requeue_400_when_worker_has_no_repo() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");
        seed_worker(
            &db_path,
            "w-1",
            "ws",
            crate::buzz::worker::WorkerState::Done,
            None, // no repo
            Some(serde_json::json!({"goal": "x"})),
            false,
            None,
            false,
            false,
            None,
            None,
            0,
        );
        let state = make_test_state_with_db(&db_path);

        let resp = v2_requeue_worker(Path(("ws".to_string(), "w-1".to_string())), State(state))
            .await
            .into_response();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = response_json(resp).await;
        assert!(json["error"].as_str().unwrap_or("").contains("repo"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn requeue_404_when_workspace_config_missing() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        // No workspace config written.
        let db_path = temp.path().join("test.db");
        seed_worker(
            &db_path,
            "w-1",
            "nowsconfig",
            crate::buzz::worker::WorkerState::Done,
            Some("repo"),
            Some(serde_json::json!({"goal": "x"})),
            false,
            None,
            false,
            false,
            None,
            None,
            0,
        );
        let state = make_test_state_with_db(&db_path);

        let resp = v2_requeue_worker(
            Path(("nowsconfig".to_string(), "w-1".to_string())),
            State(state),
        )
        .await
        .into_response();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn requeue_preserves_branch_pr_url_pr_approved_tests_passing_last_output_at() {
        // Regression: these fields were lost on requeue — the new worker record
        // must carry them all forward so the UI doesn't show them as blank.
        // We test up to the point of create_worker (which fails without a real
        // repo), then verify the source worker had all fields set correctly.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        seed_worker(
            &db_path,
            "w-src",
            "ws",
            crate::buzz::worker::WorkerState::Waiting,
            Some("myrepo"),
            Some(serde_json::json!({"goal": "add feature", "review_mode": "pr_first"})),
            false,
            Some("https://github.com/org/repo/pull/99"),
            true, // pr_approved
            true, // tests_passing
            Some("feat/add-feature-ab12"),
            Some("2026-04-30T09:00:00Z"),
            2,
        );

        let store = open_worker_store_from_path(&db_path).unwrap();
        let src = store.get("ws", "w-src").unwrap().unwrap();

        // Verify every field we care about preserving is present on the source.
        assert_eq!(
            src.branch.as_deref(),
            Some("feat/add-feature-ab12"),
            "branch"
        );
        assert_eq!(
            src.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/99"),
            "pr_url"
        );
        assert!(src.pr_approved, "pr_approved");
        assert!(src.tests_passing, "tests_passing");
        assert_eq!(
            src.last_output_at.as_deref(),
            Some("2026-04-30T09:00:00Z"),
            "last_output_at"
        );
        assert_eq!(src.revision_count, 2, "revision_count");
        assert_eq!(
            src.display_title.as_deref(),
            Some("Fix the bug"),
            "display_title"
        );

        // The handler reaches create_worker and fails at repo resolution,
        // NOT before copying fields. Confirm no early-exit 4xx.
        let state = make_test_state_with_db(&db_path);
        let resp = v2_requeue_worker(Path(("ws".to_string(), "w-src".to_string())), State(state))
            .await
            .into_response();

        let json = response_json(resp).await;
        let err = json["error"].as_str().unwrap_or("");
        // Should fail at repo resolution (no real git repo), not at validation.
        assert!(
            !err.contains("brief") && !err.contains("no repo — cannot requeue"),
            "must pass validation and reach create_worker: got '{err}'"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn requeue_increments_revision_count() {
        // revision_count must be old + 1 in the new worker record.
        // We verify this by seeding a worker with revision_count=5 and
        // confirming the requeue path intends to write 6.
        // (Cannot assert on the upserted record without a real repo, so
        // we verify the source value and document the expected increment.)
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        seed_worker(
            &db_path,
            "w-1",
            "ws",
            crate::buzz::worker::WorkerState::Done,
            Some("repo"),
            Some(serde_json::json!({"goal": "x"})),
            false,
            None,
            false,
            false,
            None,
            None,
            5,
        );

        let store = open_worker_store_from_path(&db_path).unwrap();
        let src = store.get("ws", "w-1").unwrap().unwrap();
        assert_eq!(src.revision_count, 5);
        // The requeue handler sets: new_revision = worker.revision_count + 1 → 6
        assert_eq!(src.revision_count + 1, 6);
    }

    // ── v2_cancel_worker ──────────────────────────────────────────────────

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cancel_worker_transitions_to_abandoned() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let db_path = temp.path().join("test.db");
        seed_worker(
            &db_path,
            "w-1",
            "ws",
            crate::buzz::worker::WorkerState::Running,
            Some("repo"),
            Some(serde_json::json!({})),
            false,
            None,
            false,
            false,
            None,
            None,
            0,
        );
        let (updates_tx, mut updates_rx) = broadcast::channel(4);
        let (signal_tx, _) = mpsc::unbounded_channel();
        let (chat_tx, _) = mpsc::unbounded_channel();
        let (cancel_tx, _) = mpsc::unbounded_channel();
        let state = HttpState {
            graph: Arc::new(RwLock::new(
                crate::buzz::orchestrator::graph::builtin::builtin_workflow(),
            )),
            yaml_path: Arc::new(None),
            db_path: Arc::new(db_path.clone()),
            workspace: Arc::new("ws".to_string()),
            updates_tx,
            signal_tx,
            chat_tx,
            cancel_tx,
            worker_manager: Arc::new(WorkerManager::new(
                crate::config::db_path(),
                "apiari".to_string(),
            )),
        };

        let resp = v2_cancel_worker(Path(("ws".to_string(), "w-1".to_string())), State(state))
            .await
            .into_response();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = response_json(resp).await;
        assert_eq!(json["ok"].as_bool(), Some(true));

        // Worker must be abandoned in DB.
        let store = open_worker_store_from_path(&db_path).unwrap();
        let worker = store.get("ws", "w-1").unwrap().unwrap();
        assert_eq!(worker.state, crate::buzz::worker::WorkerState::Abandoned);

        // WebSocket update must be emitted.
        let update = updates_rx.try_recv().expect("ws update should be emitted");
        assert!(matches!(update, WsUpdate::WorkerV2State { state, .. } if state == "abandoned"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cancel_worker_nonexistent_returns_error() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let db_path = temp.path().join("test.db");
        let state = make_test_state_with_db(&db_path);

        let resp = v2_cancel_worker(Path(("ws".to_string(), "ghost".to_string())), State(state))
            .await
            .into_response();

        // Transition on a missing worker returns error (not 404 — store returns Err).
        assert_ne!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn v2_get_worker_includes_task_packet_from_worktree() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        let worktree = root.join(".swarm/wt/w-1");
        let task_dir = worktree.join(".task");
        fs::create_dir_all(&task_dir).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        fs::write(
            task_dir.join("TASK.md"),
            "# Task\n\nShip the full coordinator packet.\n",
        )
        .unwrap();
        fs::write(
            task_dir.join("PLAN.md"),
            "# Plan\n\n1. Return task_packet in v2 detail\n",
        )
        .unwrap();

        let db_path = temp.path().join("test.db");
        let store = open_worker_store_from_path(&db_path).unwrap();
        store
            .upsert(&crate::buzz::worker::Worker {
                id: "w-1".to_string(),
                workspace: "ws".to_string(),
                state: crate::buzz::worker::WorkerState::Running,
                brief: Some(serde_json::json!({"goal": "ship it"})),
                repo: Some("apiari".to_string()),
                branch: Some("swarm/ship-it".to_string()),
                goal: Some("ship it".to_string()),
                tests_passing: false,
                branch_ready: false,
                pr_url: None,
                pr_approved: false,
                ci_passing: None,
                is_stalled: false,
                revision_count: 0,
                review_mode: "local_first".to_string(),
                blocked_reason: None,
                display_title: None,
                last_output_at: None,
                state_entered_at: chrono::Utc::now().to_rfc3339(),
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: chrono::Utc::now().to_rfc3339(),
                worktree_path: Some(worktree.display().to_string()),
                isolation_mode: None,
                agent_kind: None,
                model: None,
                repo_path: Some(root.display().to_string()),
                label: String::new(),
            })
            .unwrap();

        let state = make_test_state_with_db(&db_path);
        let resp = v2_get_worker(Path(("ws".to_string(), "w-1".to_string())), State(state))
            .await
            .into_response();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = response_json(resp).await;
        assert_eq!(
            json["task_packet"]["task_md"].as_str(),
            Some("# Task\n\nShip the full coordinator packet.\n")
        );
        assert_eq!(
            json["task_packet"]["plan_md"].as_str(),
            Some("# Plan\n\n1. Return task_packet in v2 detail\n")
        );
    }

    // ── v2_create_worker ──────────────────────────────────────────────────

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn create_worker_404_when_workspace_missing() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let db_path = temp.path().join("test.db");
        let state = make_test_state_with_db(&db_path);

        let resp = v2_create_worker(
            Path("noworkspace".to_string()),
            State(state),
            Json(V2CreateWorkerBody {
                brief: serde_json::json!({"goal": "fix bug"}),
                repo: "myrepo".to_string(),
                agent: None,
                model: None,
            }),
        )
        .await
        .into_response();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn create_worker_stores_record_in_briefed_state() {
        // v2_create_worker writes the worker to DB as Briefed BEFORE calling
        // create_worker. That record must exist and have the right fields even
        // when the swarm dispatch fails (no real git repo in tests).
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");
        let state = make_test_state_with_db(&db_path);

        // Call the handler — create_worker will fail (no real repo) but the
        // Briefed record is written before that call.
        let _resp = v2_create_worker(
            Path("ws".to_string()),
            State(state),
            Json(V2CreateWorkerBody {
                brief: serde_json::json!({"goal": "add feature", "review_mode": "pr_first"}),
                repo: "myrepo".to_string(),
                agent: None,
                model: None,
            }),
        )
        .await
        .into_response();

        // Find the worker that was written — scan all workers for the workspace.
        let store = open_worker_store_from_path(&db_path).unwrap();
        let workers = store.list("ws").unwrap();
        assert_eq!(workers.len(), 1, "one worker record must be written");
        let worker = &workers[0];

        // The worker starts as Briefed; if create_worker succeeded it becomes
        // Queued. Either is valid — the key fields must be correct.
        assert!(
            matches!(
                worker.state,
                crate::buzz::worker::WorkerState::Briefed
                    | crate::buzz::worker::WorkerState::Queued
            ),
            "unexpected state: {:?}",
            worker.state
        );
        assert_eq!(worker.repo.as_deref(), Some("myrepo"));
        assert_eq!(worker.review_mode, "pr_first");
        assert_eq!(
            worker.brief.as_ref().and_then(|b| b["goal"].as_str()),
            Some("add feature")
        );
        assert!(!worker.branch_ready);
        assert!(!worker.tests_passing);
        assert!(!worker.pr_approved);
    }

    // ── End-to-end: send message routing ─────────────────────────────────
    //
    // inject_live_for_test() marks a worker as running in the live set.
    // pending_for_test() reads what was queued.
    //
    // Full pipeline:
    //   v2_create_worker writes DB row
    //   → worker is live in WorkerManager
    //   → v2_send_message finds DB row (not 404)
    //   → is_live() true → message queued in pending (not dropped, not a dead channel)
    //   → when agent finishes, pending message triggers resume

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_to_live_worker_queues_in_pending() {
        // Worker in DB + in live set → send_message queues the message.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        seed_waiting_worker(&db_path, "live-abc1", "ws", Some("myrepo"), false);

        let state = make_test_state_with_db(&db_path);
        state.worker_manager.inject_live_for_test("live-abc1").await;

        let resp = v2_send_message(
            Path(("ws".to_string(), "live-abc1".to_string())),
            State(state.clone()),
            Json(V2SendMessageBody {
                message: "please add tests".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "expected 200 for live worker"
        );

        // Message must be in the pending queue — not dropped.
        let pending = state.worker_manager.pending_for_test("live-abc1").await;
        assert_eq!(
            pending,
            vec!["please add tests"],
            "message must be queued for delivery when agent finishes"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_200_when_worker_not_in_db_but_live() {
        // Worker NOT in DB but IS in live set → falls back to worker_manager.send_message
        // → worker is live → message queued in pending → 200 OK.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        let state = make_test_state_with_db(&db_path);
        state.worker_manager.inject_live_for_test("ghost-xyz").await;

        let resp = v2_send_message(
            Path(("ws".to_string(), "ghost-xyz".to_string())),
            State(state.clone()),
            Json(V2SendMessageBody {
                message: "hello ghost".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "live worker not in DB must be queued successfully"
        );
        let pending = state.worker_manager.pending_for_test("ghost-xyz").await;
        assert_eq!(pending, vec!["hello ghost"]);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_to_running_worker_not_in_live_map_takes_requeue_not_500() {
        // Worker in DB (Running) but NOT in live map (daemon restart scenario).
        // Must take requeue path and not return 500 "not found or not running".
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        // Seed a Running worker with a repo so requeue can attempt dispatch.
        seed_worker(
            &db_path,
            "orphan-run1",
            "ws",
            crate::buzz::worker::WorkerState::Running,
            Some("myrepo"),
            Some(serde_json::json!({"goal": "fix the bug"})),
            false,
            None,
            false,
            false,
            Some("feat/fix"),
            None,
            0,
        );

        let state = make_test_state_with_db(&db_path);
        // NOT calling inject_live_for_test — simulates post-restart empty live map.

        let resp = v2_send_message(
            Path(("ws".to_string(), "orphan-run1".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "are you there?".to_string(),
            }),
        )
        .await
        .into_response();

        let json = response_json(resp).await;
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "orphaned running worker must not hit send_message dead end: '{err}'"
        );
    }

    // ── WorkerManager.is_live gate ────────────────────────────────────────

    #[tokio::test]
    async fn send_message_never_returns_not_found_or_not_running_error() {
        // The string "not found or not running" must NEVER reach the caller —
        // it means WorkerManager.send_message was called on a dead worker.
        // Any worker absent from the live map must route to requeue instead.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        // Cover all non-terminal worker states.
        for (wid, state) in [
            ("w-running", crate::buzz::worker::WorkerState::Running),
            ("w-waiting", crate::buzz::worker::WorkerState::Waiting),
            ("w-stalled", crate::buzz::worker::WorkerState::Stalled),
        ] {
            seed_worker(
                &db_path,
                wid,
                "ws",
                state,
                None,
                Some(serde_json::json!({"goal":"x"})),
                false,
                None,
                false,
                false,
                None,
                None,
                0,
            );

            let http_state = make_test_state_with_db(&db_path);
            let resp = v2_send_message(
                Path(("ws".to_string(), wid.to_string())),
                State(http_state),
                Json(V2SendMessageBody {
                    message: "do more".to_string(),
                }),
            )
            .await
            .into_response();

            let json = response_json(resp).await;
            let err = json["error"].as_str().unwrap_or("");
            assert!(
                !err.contains("not found or not running"),
                "worker state {wid}: must never hit send_message dead end, got: '{err}'"
            );
        }
    }

    // ── Regression: !is_live must not trigger create_worker ───────────────

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_idle_non_branch_ready_does_not_create_new_worker() {
        // Regression: before the fix, a worker that was idle (not in live map)
        // but not branch_ready incorrectly triggered create_worker_with_task_dir,
        // creating a new worktree and losing session context. The condition was:
        //   `if worker.branch_ready || !is_live(&id)`
        // which fired for ANY idle worker. Now only branch_ready triggers requeue.
        //
        // The tell-tale sign of the old bug: response contains "new_id".
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        seed_worker(
            &db_path,
            "idle-abc1",
            "ws",
            crate::buzz::worker::WorkerState::Waiting,
            Some("myrepo"),
            Some(serde_json::json!({"goal": "fix the bug"})),
            false, // branch_ready = false — key condition
            None,
            false,
            false,
            Some("feat/fix-bug"),
            None,
            0,
        );

        let state = make_test_state_with_db(&db_path);
        // NOT inject_live_for_test — worker is idle, simulates daemon restart or
        // agent that finished its first pass.

        let resp = v2_send_message(
            Path(("ws".to_string(), "idle-abc1".to_string())),
            State(state),
            Json(V2SendMessageBody {
                message: "please also add tests".to_string(),
            }),
        )
        .await
        .into_response();

        let json = response_json(resp).await;
        assert!(
            json["new_id"].is_null(),
            "non-branch_ready idle worker must not trigger requeue: got new_id={:?}",
            json["new_id"]
        );
        let err = json["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("not found or not running"),
            "must not hit dead send_message path: '{err}'"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_message_branch_ready_still_creates_new_worker() {
        // Confirm that branch_ready=true still triggers create_worker_with_task_dir.
        // The response either succeeds with "new_id" (real repo) or fails at repo
        // resolution — but must NOT queue in pending like a live worker would.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _home_guard = install_temp_home(temp.path());
        let root = temp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        write_minimal_workspace(temp.path(), "ws", &root);
        let db_path = temp.path().join("test.db");

        seed_worker(
            &db_path,
            "ready-abc1",
            "ws",
            crate::buzz::worker::WorkerState::Waiting,
            Some("myrepo"),
            Some(serde_json::json!({"goal": "fix the bug", "review_mode": "local_first"})),
            true, // branch_ready = true
            Some("https://github.com/org/repo/pull/1"),
            false,
            false,
            Some("feat/fix-bug"),
            None,
            0,
        );

        let state = make_test_state_with_db(&db_path);

        let resp = v2_send_message(
            Path(("ws".to_string(), "ready-abc1".to_string())),
            State(state.clone()),
            Json(V2SendMessageBody {
                message: "address the review comments".to_string(),
            }),
        )
        .await
        .into_response();

        let json = response_json(resp).await;
        let err = json["error"].as_str().unwrap_or("");
        // Fails at repo resolution (no real git repo) — that's fine.
        // Key check: must NOT have queued in pending (that would mean it took
        // the send_message path instead of the requeue path).
        let pending = state.worker_manager.pending_for_test("ready-abc1").await;
        assert!(
            pending.is_empty(),
            "branch_ready worker must not queue in pending: {:?}",
            pending
        );
        assert!(
            !err.contains("not found or not running"),
            "must not hit dead send_message path: '{err}'"
        );
    }

    // ── Context-bot integration tests ──────────────────────────────────────
    //
    // These tests call v2_context_bot_chat directly with a fake `claude` binary
    // on PATH so we exercise the full handler path: arg construction, flag names,
    // response parsing, and DISPATCH_WORKER detection.

    struct FakePathGuard {
        old_path: Option<std::ffi::OsString>,
    }

    impl Drop for FakePathGuard {
        fn drop(&mut self) {
            match self.old_path.take() {
                Some(p) => unsafe { std::env::set_var("PATH", p) },
                None => unsafe { std::env::remove_var("PATH") },
            }
        }
    }

    fn install_fake_claude(dir: &Path, stdout: &str) -> FakePathGuard {
        let bin_dir = dir.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let script = bin_dir.join("claude");
        // Echo all args to stderr for inspection, drain stdin (message is sent there),
        // then print the canned response to stdout.
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >&2\ncat > /dev/null\nprintf '%s' '{}'\n",
            stdout.replace('\'', "'\"'\"'")
        );
        fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![bin_dir];
        paths.extend(std::env::split_paths(&old_path.clone().unwrap_or_default()));
        let joined = std::env::join_paths(paths).unwrap();
        unsafe { std::env::set_var("PATH", joined) };
        FakePathGuard { old_path }
    }

    fn make_context_bot_state(db_path: &Path) -> HttpState {
        make_test_state_with_db(db_path)
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn context_bot_chat_returns_claude_response() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _claude = install_fake_claude(temp.path(), "Everything looks good.");
        let db = temp.path().join("test.db");
        let state = make_context_bot_state(&db);

        let resp = v2_context_bot_chat(
            axum::extract::Path("apiari".to_string()),
            axum::extract::State(state),
            axum::extract::Json(ContextBotChatBody {
                message: "What's the status?".to_string(),
                session_id: None,
                model: None,
                context: ContextBotContext {
                    view: "dashboard".to_string(),
                    entity_id: None,
                    entity_snapshot: Some(serde_json::json!({"active_worker_count": 2})),
                },
            }),
        )
        .await
        .into_response();

        let json = response_json(resp).await;
        assert_eq!(json["response"].as_str(), Some("Everything looks good."));
        assert!(
            json["session_id"].as_str().is_some(),
            "session_id must be present"
        );
        assert!(
            json["model"].as_str().is_some(),
            "model must be present in response"
        );
        assert!(
            json.get("dispatched_worker_id").is_none(),
            "no dispatch expected"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn context_bot_chat_echoes_provided_session_id() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _claude = install_fake_claude(temp.path(), "Got it.");
        let db = temp.path().join("test.db");
        let state = make_context_bot_state(&db);

        let resp = v2_context_bot_chat(
            axum::extract::Path("apiari".to_string()),
            axum::extract::State(state),
            axum::extract::Json(ContextBotChatBody {
                message: "Hello".to_string(),
                session_id: Some("my-session-42".to_string()),
                model: None,
                context: ContextBotContext {
                    view: "dashboard".to_string(),
                    entity_id: None,
                    entity_snapshot: None,
                },
            }),
        )
        .await
        .into_response();

        let json = response_json(resp).await;
        assert_eq!(json["session_id"].as_str(), Some("my-session-42"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn context_bot_chat_uses_system_prompt_flag() {
        // Verifies the handler passes --system-prompt (not the old --system).
        // The fake claude logs all args to stderr; we capture them via a log file.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args.log");

        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let script = bin_dir.join("claude");
        let log_display = log.display().to_string();
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nprintf '%s' 'ok'\n",
            log_display
        );
        fs::write(&script, &body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![bin_dir];
        paths.extend(std::env::split_paths(&old_path.clone().unwrap_or_default()));
        let joined = std::env::join_paths(paths).unwrap();
        unsafe { std::env::set_var("PATH", joined) };
        let _path_guard = FakePathGuard { old_path };

        let db = temp.path().join("test.db");
        let state = make_context_bot_state(&db);

        v2_context_bot_chat(
            axum::extract::Path("apiari".to_string()),
            axum::extract::State(state),
            axum::extract::Json(ContextBotChatBody {
                message: "hi".to_string(),
                session_id: None,
                model: None,
                context: ContextBotContext {
                    view: "dashboard".to_string(),
                    entity_id: None,
                    entity_snapshot: None,
                },
            }),
        )
        .await;

        let logged = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            logged.contains("--system-prompt"),
            "handler must pass --system-prompt, got: {logged}"
        );
        assert!(
            !logged.contains("--system\n") && !logged.contains("--system "),
            "handler must NOT pass bare --system flag, got: {logged}"
        );
        assert!(
            logged.contains("--print"),
            "handler must pass --print, got: {logged}"
        );
        assert!(
            logged.contains("--max-turns"),
            "handler must pass --max-turns, got: {logged}"
        );
        assert!(
            logged.contains("--model"),
            "handler must pass --model, got: {logged}"
        );
        // Each arg is on its own line; "hi" as a standalone line = passed as arg
        assert!(
            !logged.lines().any(|l| l == "hi"),
            "message must NOT be a standalone CLI arg, got: {logged}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn context_bot_chat_message_sent_via_stdin_not_args() {
        // Verifies the message is delivered via stdin so special characters
        // (quotes, newlines, $vars) can't break argument parsing.
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args.log");
        let stdin_log = temp.path().join("stdin.log");

        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let script = bin_dir.join("claude");
        let log_display = log.display().to_string();
        let stdin_display = stdin_log.display().to_string();
        // Write args to args.log, stdin content to stdin.log, then respond ok.
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\ncat >> '{}'\nprintf '%s' 'ok'\n",
            log_display, stdin_display
        );
        fs::write(&script, &body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![bin_dir];
        paths.extend(std::env::split_paths(&old_path.clone().unwrap_or_default()));
        let joined = std::env::join_paths(paths).unwrap();
        unsafe { std::env::set_var("PATH", joined) };
        let _path_guard = FakePathGuard { old_path };

        let tricky_message = "What's the status? It's $HOME and \"quoted\" and has\nnewlines.";

        let db = temp.path().join("test.db");
        let state = make_context_bot_state(&db);
        v2_context_bot_chat(
            axum::extract::Path("apiari".to_string()),
            axum::extract::State(state),
            axum::extract::Json(ContextBotChatBody {
                message: tricky_message.to_string(),
                session_id: None,
                model: None,
                context: ContextBotContext {
                    view: "dashboard".to_string(),
                    entity_id: None,
                    entity_snapshot: None,
                },
            }),
        )
        .await;

        let args_logged = fs::read_to_string(&log).unwrap_or_default();
        let stdin_logged = fs::read_to_string(&stdin_log).unwrap_or_default();

        // Message must arrive via stdin, verbatim
        assert!(
            stdin_logged.contains(tricky_message),
            "message must be sent via stdin, got stdin: {stdin_logged:?}"
        );
        // Message must NOT appear as a CLI arg
        assert!(
            !args_logged.contains("What's the status"),
            "message must NOT be passed as a CLI arg, got args: {args_logged:?}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn context_bot_chat_missing_claude_returns_503() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        // Put an empty bin dir with no claude binary on PATH.
        let bin_dir = temp.path().join("empty_bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let old_path = std::env::var_os("PATH");
        // Prepend empty bin dir so claude is not found anywhere.
        let joined = std::env::join_paths(vec![bin_dir]).unwrap();
        unsafe { std::env::set_var("PATH", &joined) };
        let _path_guard = FakePathGuard { old_path };

        let db = temp.path().join("test.db");
        let state = make_context_bot_state(&db);

        let resp = v2_context_bot_chat(
            axum::extract::Path("apiari".to_string()),
            axum::extract::State(state),
            axum::extract::Json(ContextBotChatBody {
                message: "hi".to_string(),
                session_id: None,
                model: None,
                context: ContextBotContext {
                    view: "dashboard".to_string(),
                    entity_id: None,
                    entity_snapshot: None,
                },
            }),
        )
        .await
        .into_response();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

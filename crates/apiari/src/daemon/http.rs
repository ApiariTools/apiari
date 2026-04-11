//! HTTP + WebSocket server for the web UI.
//!
//! Serves the workflow graph definition, task state, and live updates over WebSocket.
//! In production, static assets are embedded via rust-embed; in dev mode, Vite proxies here.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
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
    pub response_tx: mpsc::UnboundedSender<WebChatEvent>,
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
            let description = match &node.action {
                Some(a) => {
                    let kind = format!("{:?}", a.kind).to_lowercase();
                    match &a.prompt {
                        Some(p) => Some(format!("{kind}: {}", truncate(p, 60))),
                        None => Some(kind),
                    }
                }
                None => node.wait_for.as_ref().map(|w| {
                    w.description
                        .clone()
                        .unwrap_or_else(|| format!("waits for {}", w.source))
                }),
            };

            NodeView {
                id: id.clone(),
                label: node.label.clone(),
                node_type: format!("{:?}", node.node_type).to_lowercase(),
                stage: node.stage.clone(),
                action: node
                    .action
                    .as_ref()
                    .and_then(|a| serde_json::to_value(a).ok()),
                wait_for: node
                    .wait_for
                    .as_ref()
                    .and_then(|w| serde_json::to_value(w).ok()),
                notify: node
                    .notify
                    .as_ref()
                    .map(|n| format!("{:?}", n).to_lowercase()),
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
                priority: edge.priority,
            }
        })
        .collect();

    GraphView {
        name: graph.name.clone(),
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
    use crate::buzz::orchestrator::graph::{Condition, Edge, Guard, Node, NodeType, NotifyConfig};

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
                wait_for: nv
                    .wait_for
                    .as_ref()
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| color_eyre::eyre::eyre!("invalid wait_for: {e}"))?,
                notify: nv.notify.as_ref().and_then(|n| match n.as_str() {
                    "silent" => Some(NotifyConfig::Silent),
                    "badge" => Some(NotifyConfig::Badge),
                    "chat" => Some(NotifyConfig::Chat),
                    _ => None,
                }),
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
            priority: ev.priority,
        });
    }

    let graph = WorkflowGraph {
        name: view.name.clone(),
        description: String::new(),
        nodes,
        edges,
    };

    graph.validate()?;
    Ok(graph)
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
}

// ── Bee config handlers ────────────────────────────────────────────────

/// GET /api/workspaces — list all configured workspaces.
async fn list_workspaces() -> Json<Vec<String>> {
    let names = crate::config::discover_workspaces()
        .map(|ws| ws.into_iter().map(|w| w.name).collect())
        .unwrap_or_default();
    Json(names)
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

        // Workers waiting → action (with detail from metadata)
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

        // PRs with CI pass → check if bot reviewed too → action
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

#[derive(Debug, Serialize)]
struct WorkerView {
    id: String,
    workspace: String,
    branch: String,
    agent: String,
    status: String,
    pr_url: Option<String>,
}

/// GET /api/workers — all swarm workers across all workspaces.
async fn get_workers() -> Json<Vec<WorkerView>> {
    let mut workers = Vec::new();
    let workspaces = crate::config::discover_workspaces().unwrap_or_default();

    for ws in &workspaces {
        let state_path = ws
            .config
            .watchers
            .swarm
            .as_ref()
            .map(|s| s.state_path.clone());
        let Some(path) = state_path else {
            continue;
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let state: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(wts) = state.get("worktrees").and_then(|v| v.as_array()) {
            for wt in wts {
                let id = wt
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let branch = wt
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent = wt
                    .get("agent_kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("claude")
                    .to_string();
                let status = wt
                    .get("agent_session_status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let pr_url = wt
                    .get("pr")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                workers.push(WorkerView {
                    id,
                    workspace: ws.name.clone(),
                    branch,
                    agent,
                    status,
                    pr_url,
                });
            }
        }
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
}

/// GET /api/signals?workspace=mgm&limit=50 — recent signals for the Briefing feed.
async fn get_signals(
    State(state): State<HttpState>,
    axum::extract::Query(q): axum::extract::Query<WorkspaceQuery>,
) -> Json<Vec<SignalView>> {
    let workspace = q.workspace.as_deref().unwrap_or(state.workspace.as_str());

    let store = match crate::buzz::signal::store::SignalStore::open(&state.db_path, workspace) {
        Ok(s) => s,
        Err(_) => return Json(vec![]),
    };

    let signals = store.get_open_signals().unwrap_or_default();
    let views: Vec<SignalView> = signals
        .iter()
        .take(50)
        .map(|s| SignalView {
            id: s.id,
            workspace: workspace.to_string(),
            source: s.source.clone(),
            title: s.title.clone(),
            severity: format!("{:?}", s.severity),
            status: format!("{:?}", s.status),
            url: s.url.clone(),
            created_at: s.created_at.to_rfc3339(),
        })
        .collect();

    Json(views)
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

            if let Some(ref action) = node.action
                && let Some(ref prompt) = action.prompt
            {
                let in_lane = body.lane.as_ref().is_none_or(|lane| {
                    action.role.as_deref() == Some(lane.as_str())
                        || node.label.to_lowercase().contains(&lane.to_lowercase())
                });
                if in_lane {
                    steps.push((current.clone(), node.label.clone(), prompt.clone()));
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
)> {
    let (updates_tx, _) = broadcast::channel(256);
    let (signal_tx, signal_rx) = mpsc::unbounded_channel();
    let (chat_tx, chat_rx) = mpsc::unbounded_channel();

    let state = HttpState {
        graph: Arc::new(RwLock::new(graph)),
        yaml_path: Arc::new(yaml_path),
        db_path: Arc::new(db_path),
        workspace: Arc::new(workspace),
        updates_tx: updates_tx.clone(),
        signal_tx,
        chat_tx,
    };

    let app = Router::new()
        .route("/api/graph", get(get_graph).put(save_graph))
        .route("/api/graph/yaml", get(get_yaml))
        .route("/api/tasks", get(get_tasks).delete(clear_tasks))
        .route("/api/signal", post(inject_signal))
        .route("/api/workspaces", get(list_workspaces))
        .route("/api/chat", post(chat_handler))
        .route("/api/workflow/run", post(workflow_run_handler))
        .route("/api/briefing", get(get_briefing))
        .route("/api/bee-activity", get(get_bee_activity))
        .route("/api/canvas", get(get_canvas))
        .route("/api/workers", get(get_workers))
        .route("/api/worker/send", post(send_worker_message))
        .route("/api/briefing/dismiss", post(dismiss_signal))
        .route("/api/briefing/snooze", post(snooze_signal))
        .route("/api/signals", get(get_signals))
        .route("/api/conversations", get(get_conversations))
        .route("/api/bees", get(get_bees).put(save_bees))
        .route("/api/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("[http] listening on http://0.0.0.0:{port}");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("[http] server error: {e}");
        }
    });

    Ok((updates_tx, signal_rx, chat_rx))
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

    eprintln!("apiari web dev server");
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

    let (updates_tx, mut signal_rx, _chat_rx) = start_http_server(
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

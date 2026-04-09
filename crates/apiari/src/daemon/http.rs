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
    response::IntoResponse,
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
    Token { text: String },
    Done,
    Error { text: String },
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
async fn get_tasks(State(state): State<HttpState>) -> Json<Vec<TaskView>> {
    let store = match TaskStore::open(&state.db_path) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to open task store: {e}");
            return Json(vec![]);
        }
    };

    let tasks = store
        .get_all_tasks(&state.workspace)
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

        let mut by_source: std::collections::HashMap<
            &str,
            Vec<&crate::buzz::signal::SignalRecord>,
        > = std::collections::HashMap::new();
        for sig in &signals {
            by_source.entry(sig.source.as_str()).or_default().push(sig);
        }

        // Workers waiting → action
        for sig in by_source.get("swarm_worker_waiting").unwrap_or(&vec![]) {
            items.push(BriefingItem {
                id: format!("wait-{}", sig.id),
                priority: "action".into(),
                icon: "⏸".into(),
                title: sig.title.clone(),
                body: Some("Worker waiting for input or review".into()),
                workspace: ws.name.clone(),
                source: "swarm".into(),
                url: sig.url.clone(),
                actions: vec![
                    BriefingAction {
                        label: "View".into(),
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
) -> impl IntoResponse {
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<WebChatEvent>();

    let req = WebChatRequest {
        workspace: body.workspace,
        bee: body.bee,
        text: body.text,
        response_tx,
    };

    if state.chat_tx.send(req).is_err() {
        return axum::response::Json(serde_json::json!({
            "type": "error",
            "text": "daemon chat channel closed"
        }))
        .into_response();
    }

    // Collect the full response (for simplicity — SSE streaming comes later)
    let mut full_response = String::new();
    let mut had_error = false;
    while let Some(event) = response_rx.recv().await {
        match event {
            WebChatEvent::Token { text } => full_response.push_str(&text),
            WebChatEvent::Done => break,
            WebChatEvent::Error { text } => {
                full_response = text;
                had_error = true;
                break;
            }
        }
    }

    axum::response::Json(serde_json::json!({
        "type": if had_error { "error" } else { "response" },
        "text": full_response,
    }))
    .into_response()
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
        .route("/api/briefing", get(get_briefing))
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

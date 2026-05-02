//! Unified orchestrator — single entry point for all signal processing.
//!
//! Replaces the three overlapping systems:
//! - `buzz/pipeline/` (notification pipeline)
//! - `coordinator.signal_hooks` (coordinator follow-throughs)
//! - `buzz/task/engine` + `buzz/task/rules` (task state transitions)
//!
//! Every signal flows through `Orchestrator::process_signal()`, which:
//! 1. Updates task state directly (no separate engine/rules)
//! 2. Routes notifications to the correct tier (Silent/Badge/Chat)
//! 3. Fires any matching orchestrator actions (coordinator follow-throughs)

pub mod graph;
pub mod notify;
pub mod reconcile;
pub mod workflow;

use std::collections::HashMap;

use chrono::Utc;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use self::{
    graph::{
        WorkflowGraph,
        walker::{GraphCursor, GraphWalker},
    },
    notify::{NotificationRouter, NotificationTier},
    workflow::{WorkflowAction, WorkflowConfig, WorkflowEngine},
};
use crate::buzz::{
    signal::SignalRecord,
    task::{Task, TaskStage, store::TaskStore},
};

/// Configuration for an orchestrator action — triggers coordinator follow-through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorAction {
    /// Signal source to match (exact or prefix with `_`).
    pub trigger: String,
    /// Natural-language instruction for the coordinator.
    pub action: String,
    /// Playbook skills to load for this action's coordinator session.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Max seconds to wait in queue before dropping. Default: 120.
    #[serde(default = "default_action_ttl")]
    pub ttl_secs: u64,
}

fn default_action_ttl() -> u64 {
    120
}

/// Full orchestrator configuration from workspace TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    /// How often to run reconciliation (seconds).
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval_secs: u64,
    /// Per-source notification tier overrides.
    #[serde(default)]
    pub notification_tiers: HashMap<String, NotificationTier>,
    /// Workflow configuration.
    #[serde(default)]
    pub workflow: WorkflowConfig,
    /// Coordinator follow-through actions.
    #[serde(default = "default_actions")]
    pub actions: Vec<OrchestratorAction>,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            reconcile_interval_secs: default_reconcile_interval(),
            notification_tiers: HashMap::new(),
            workflow: WorkflowConfig::default(),
            actions: default_actions(),
        }
    }
}

fn default_reconcile_interval() -> u64 {
    60
}

fn default_actions() -> Vec<OrchestratorAction> {
    vec![
        OrchestratorAction {
            trigger: "swarm".into(),
            action: "Assess the situation. If a worker opened a PR, check if Copilot has reviewed it and if so forward any comments to the worker. If a worker is stuck or failed, investigate and either send a fix or dispatch a new worker.".into(),
            skills: vec![],
            ttl_secs: 300,
        },
        OrchestratorAction {
            trigger: "github_bot_review".into(),
            action: "Find the swarm worker whose branch matches this PR and forward the review comments directly to it so it can address them.".into(),
            skills: vec![],
            ttl_secs: 300,
        },
        OrchestratorAction {
            trigger: "github".into(),
            action: "Find the relevant swarm worker for this PR. If a worker exists, send it the CI error details so it can fix them. If no worker exists, dispatch a new one to fix the failure.".into(),
            skills: vec![],
            ttl_secs: 300,
        },
    ]
}

/// Result of processing a signal through the task engine portion of the orchestrator.
#[derive(Debug)]
pub struct EngineResult {
    /// The task that was affected (if any).
    pub task: Option<Task>,
    /// Messages to forward to the task's worker (if any).
    pub worker_messages: Vec<(String, String)>, // (worker_id, message)
    /// Notification messages for the user.
    pub notifications: Vec<String>,
    /// Whether a stage transition occurred.
    pub transitioned: bool,
    /// Stage the task was in BEFORE the transition (None if no stage change, or no task matched).
    pub from_stage: Option<TaskStage>,
}

/// Result of processing a signal through the orchestrator.
#[derive(Debug)]
pub struct OrchestratorResult {
    /// Task engine result (task transitions, worker messages, etc.).
    pub engine_result: EngineResult,
    /// Notification tier this signal was routed to.
    pub notification_tier: NotificationTier,
    /// Formatted notification message (None for Silent tier).
    pub notification_message: Option<String>,
    /// Matched orchestrator actions to fire as coordinator follow-throughs.
    pub matched_actions: Vec<MatchedAction>,
    /// Workflow actions to execute (dispatch reviewer, create PR, etc.).
    pub workflow_actions: Vec<WorkflowAction>,
}

/// An orchestrator action that matched this signal.
#[derive(Debug, Clone)]
pub struct MatchedAction {
    pub trigger: String,
    pub action: String,
    pub skills: Vec<String>,
    pub ttl_secs: u64,
    /// Signal description for the action prompt.
    pub signal_description: String,
}

/// The unified orchestrator — single entry point for signal processing.
pub struct Orchestrator {
    router: NotificationRouter,
    workflow: WorkflowEngine,
    actions: Vec<OrchestratorAction>,
    /// The workflow graph (loaded from YAML or built-in default).
    workflow_graph: WorkflowGraph,
}

impl Orchestrator {
    /// Create a new orchestrator from config.
    pub fn new(config: &OrchestratorConfig) -> Self {
        Self {
            router: NotificationRouter::new(config.notification_tiers.clone()),
            workflow: WorkflowEngine::new(config.workflow.clone()),
            actions: config.actions.clone(),
            workflow_graph: graph::builtin::builtin_workflow(),
        }
    }

    /// Create an orchestrator with a custom workflow graph.
    pub fn with_graph(config: &OrchestratorConfig, graph: WorkflowGraph) -> Self {
        Self {
            router: NotificationRouter::new(config.notification_tiers.clone()),
            workflow: WorkflowEngine::new(config.workflow.clone()),
            actions: config.actions.clone(),
            workflow_graph: graph,
        }
    }

    /// Get a reference to the workflow graph.
    pub fn workflow_graph(&self) -> &WorkflowGraph {
        &self.workflow_graph
    }

    /// Process a signal through the entire orchestration pipeline.
    ///
    /// This is the single entry point. It:
    /// 1. Updates task state directly (matching, transitions, creation)
    /// 2. Routes the signal to the correct notification tier
    /// 3. Matches orchestrator actions for coordinator follow-throughs
    /// 4. Evaluates workflow rules (branch_ready → review/PR)
    pub fn process_signal(
        &self,
        store: &TaskStore,
        workspace: &str,
        signal: &SignalRecord,
    ) -> Result<OrchestratorResult> {
        // 1. Task transitions — direct logic, no separate engine/rules
        let engine_result = self.process_task_signal(store, workspace, signal)?;

        // 2. Notification routing
        let routing = self
            .router
            .route(&signal.source, &signal.title, signal.url.as_deref());

        // 3. Match orchestrator actions (coordinator follow-throughs)
        let matched_actions = self.match_actions(signal);

        // 4. Workflow evaluation (legacy hardcoded path)
        let workflow_actions = self.evaluate_workflow(signal, &engine_result);

        // 5. Graph evaluation — walk the workflow graph and advance the cursor.
        // Graph-produced actions are collected separately for now; they will
        // replace the legacy workflow_actions once the graph is fully proven.
        let graph_actions = self.evaluate_graph(store, signal, &engine_result);

        info!(
            "[orchestrator] signal '{}' (source={}) → tier={:?}, actions={}, workflow_actions={}, graph_actions={}",
            signal.title,
            signal.source,
            routing.tier,
            matched_actions.len(),
            workflow_actions.len(),
            graph_actions.len(),
        );

        Ok(OrchestratorResult {
            engine_result,
            notification_tier: routing.tier,
            notification_message: routing.message,
            matched_actions,
            workflow_actions,
        })
    }

    /// Process task transitions directly — replaces the old engine + rules modules.
    ///
    /// Handles task matching, creation, and monotonic stage transitions.
    fn process_task_signal(
        &self,
        store: &TaskStore,
        workspace: &str,
        signal: &SignalRecord,
    ) -> Result<EngineResult> {
        let mut result = EngineResult {
            task: None,
            worker_messages: Vec::new(),
            notifications: Vec::new(),
            transitioned: false,
            from_stage: None,
        };

        // Determine the effective signal kind from source + external_id
        let kind = signal_kind(signal);

        // For worker_spawned with no existing task, create one
        if kind == SignalKind::WorkerSpawned {
            let worker_id = extract_worker_id_from_external_id(&signal.external_id)
                .or_else(|| extract_metadata_str(signal, "worker_id"));
            if let Some(ref wid) = worker_id
                && store.find_task_by_worker(workspace, wid)?.is_none()
            {
                let is_reviewer = signal
                    .body
                    .as_ref()
                    .and_then(|b| b.lines().nth(1))
                    .map(|l| l.trim_start().starts_with("Review PR"))
                    .unwrap_or(false);
                if !is_reviewer {
                    let title = signal
                        .body
                        .as_ref()
                        .and_then(|b| b.lines().nth(1))
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("Worker {wid}"));
                    let title = if title.len() > 80 {
                        format!("{}…", &title[..79])
                    } else {
                        title
                    };
                    let now = Utc::now();
                    let task = Task {
                        id: uuid::Uuid::new_v4().to_string(),
                        workspace: workspace.to_string(),
                        title,
                        stage: TaskStage::InProgress,
                        source: Some("swarm".to_string()),
                        source_url: None,
                        worker_id: Some(wid.clone()),
                        pr_url: None,
                        pr_number: None,
                        repo: None,
                        created_at: now,
                        updated_at: now,
                        resolved_at: None,
                        metadata: serde_json::json!({}),
                    };
                    store.create_task(&task)?;
                    info!(
                        "[orchestrator] created task '{}' for worker {wid}",
                        task.title
                    );
                    result.transitioned = true;
                    result.from_stage = None; // new task, no previous stage
                    result.task = Some(task);
                    return Ok(result);
                }
            }
        }

        // Find matching task
        let task = find_task_for_signal(store, workspace, signal)?;
        let task = match task {
            Some(t) => t,
            None => return Ok(result),
        };

        info!(
            "[orchestrator] matched signal '{}' (source={}) to task '{}' (stage={})",
            signal.title,
            signal.source,
            task.title,
            task.stage.as_str()
        );

        // Determine target stage and any side-effect messages based on signal kind
        let transition = match kind {
            SignalKind::WorkerSpawned | SignalKind::WorkerRunning => {
                // Only advance to InProgress if task hasn't already moved past it
                if task.stage.stage_order() < TaskStage::InProgress.stage_order() {
                    Some(TransitionIntent {
                        to: TaskStage::InProgress,
                        reason: "Worker running".to_string(),
                        notification: Some(format!(
                            "Worker is running — task moved to InProgress (was {})",
                            task.stage.as_str()
                        )),
                        forward_to_worker: None,
                    })
                } else {
                    None // no-op
                }
            }
            SignalKind::BranchReady => {
                // Transition to InAiReview; workflow engine handles dispatch
                if task.stage.stage_order() < TaskStage::InAiReview.stage_order() {
                    Some(TransitionIntent {
                        to: TaskStage::InAiReview,
                        reason: "Branch ready for review".to_string(),
                        notification: Some("Branch ready — dispatching review/PR".to_string()),
                        forward_to_worker: None,
                    })
                } else {
                    None
                }
            }
            SignalKind::PrOpened => {
                // Update PR info on the task
                if let Some(ref meta_str) = signal.metadata
                    && let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str)
                {
                    let pr_url = meta
                        .get("pr_url")
                        .and_then(|v| v.as_str())
                        .or(signal.url.as_deref());
                    let pr_number = meta.get("pr_number").and_then(|v| v.as_i64());
                    let repo = meta.get("repo").and_then(|v| v.as_str());

                    if let Some(url) = pr_url
                        && let Some(num) = pr_number
                    {
                        let _ = store.update_task_pr(&task.id, url, num);
                    }
                    if let Some(r) = repo {
                        let _ = store.update_task_repo(&task.id, r);
                    }
                }
                // Transition to HumanReview
                Some(TransitionIntent {
                    to: TaskStage::HumanReview,
                    reason: "PR opened".to_string(),
                    notification: Some("PR opened — ready for human review".to_string()),
                    forward_to_worker: None,
                })
            }
            SignalKind::MergedPr => {
                if !task.stage.is_terminal() {
                    Some(TransitionIntent {
                        to: TaskStage::Merged,
                        reason: "PR merged".to_string(),
                        notification: Some("PR merged — task complete".to_string()),
                        forward_to_worker: None,
                    })
                } else {
                    None
                }
            }
            SignalKind::PrClosed => {
                if !task.stage.is_terminal() {
                    Some(TransitionIntent {
                        to: TaskStage::Dismissed,
                        reason: "PR closed without merge".to_string(),
                        notification: Some(
                            "PR closed without merging — task dismissed".to_string(),
                        ),
                        forward_to_worker: None,
                    })
                } else {
                    None
                }
            }
            // Informational signals — no stage change
            SignalKind::CiPass
            | SignalKind::CiFailure
            | SignalKind::BotReview
            | SignalKind::WorkerWaiting
            | SignalKind::ReviewVerdict
            | SignalKind::WorkerClosed
            | SignalKind::Other => None,
        };

        // Apply transition with monotonic guard
        if let Some(intent) = transition {
            let from = task.stage.clone();
            result.from_stage = Some(from.clone());

            // Terminal transitions (Merged/Dismissed) always apply.
            // Non-terminal transitions must be forward-only (monotonic).
            let should_apply =
                intent.to.is_terminal() || intent.to.stage_order() > from.stage_order();

            if should_apply && from != intent.to {
                store.transition_task(&task.id, &from, &intent.to, Some(intent.reason.clone()))?;
                result.transitioned = true;
                info!(
                    "[orchestrator] transitioned task '{}': {} → {} (reason: {})",
                    task.title,
                    from.as_str(),
                    intent.to.as_str(),
                    intent.reason,
                );
            } else if !should_apply {
                debug!(
                    "[orchestrator] skipping backward transition {}→{} for task '{}' (monotonic guard)",
                    from.as_str(),
                    intent.to.as_str(),
                    task.title,
                );
            }

            // Collect side effects
            if let Some(msg) = intent.notification {
                result.notifications.push(msg);
            }
            if let Some((wid, msg)) = intent.forward_to_worker {
                result.worker_messages.push((wid, msg));
            }
        }

        // Reload task after any changes
        result.task = store.get_task(&task.id)?;
        Ok(result)
    }

    /// Find all orchestrator actions matching this signal.
    fn match_actions(&self, signal: &SignalRecord) -> Vec<MatchedAction> {
        let description = if let Some(ref url) = signal.url {
            format!("{} — {}", signal.title, url)
        } else if let Some(ref body) = signal.body {
            format!("{} — {}", signal.title, body.lines().next().unwrap_or(""))
        } else {
            signal.title.clone()
        };

        // Find the first matching action (first match wins, same as old signal_hooks)
        if let Some(action) = self.actions.iter().find(|a| {
            signal.source == a.trigger || signal.source.starts_with(&format!("{}_", a.trigger))
        }) {
            vec![MatchedAction {
                trigger: action.trigger.clone(),
                action: action.action.clone(),
                skills: action.skills.clone(),
                ttl_secs: action.ttl_secs,
                signal_description: description,
            }]
        } else {
            vec![]
        }
    }

    /// Evaluate workflow rules for branch_ready and review_verdict signals.
    fn evaluate_workflow(
        &self,
        signal: &SignalRecord,
        engine_result: &EngineResult,
    ) -> Vec<WorkflowAction> {
        let mut actions = Vec::new();
        let meta = signal
            .metadata
            .as_ref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok());

        match signal.source.as_str() {
            "swarm_branch_ready" => {
                let branch_name = meta
                    .as_ref()
                    .and_then(|m| m.get("branch_name").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                let worker_id = meta
                    .as_ref()
                    .and_then(|m| m.get("worker_id").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");

                // Only trigger workflow if we matched a task
                if engine_result.task.is_some() {
                    let task_id = engine_result
                        .task
                        .as_ref()
                        .map(|t| t.id.as_str())
                        .unwrap_or("unknown");
                    actions.push(
                        self.workflow
                            .on_branch_ready(task_id, branch_name, worker_id),
                    );
                }
            }
            "swarm_review_verdict" => {
                let verdict = meta
                    .as_ref()
                    .and_then(|m| m.get("verdict").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let feedback = meta
                    .as_ref()
                    .and_then(|m| m.get("comments").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let branch_name = meta
                    .as_ref()
                    .and_then(|m| m.get("branch_name").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                let review_cycle = meta
                    .as_ref()
                    .and_then(|m| m.get("review_cycle").and_then(|v| v.as_u64()))
                    .unwrap_or(1) as u32;

                if engine_result.task.is_some() {
                    let task_id = engine_result
                        .task
                        .as_ref()
                        .map(|t| t.id.as_str())
                        .unwrap_or("unknown");
                    if let Some(action) = self.workflow.on_review_verdict(
                        task_id,
                        branch_name,
                        verdict,
                        feedback,
                        review_cycle,
                    ) {
                        actions.push(action);
                    }
                }
            }
            _ => {}
        }

        actions
    }

    /// Walk the workflow graph for a task, advancing the cursor and returning actions.
    ///
    /// This is the graph-based replacement for `evaluate_workflow()`. It loads the
    /// cursor from task metadata, steps through the graph, saves the cursor back,
    /// and returns any workflow actions the graph produced.
    fn evaluate_graph(
        &self,
        store: &TaskStore,
        signal: &SignalRecord,
        engine_result: &EngineResult,
    ) -> Vec<WorkflowAction> {
        let task = match engine_result.task.as_ref() {
            Some(t) => t,
            None => return vec![],
        };

        let mut cursor = GraphCursor::from_task(task, &self.workflow_graph);
        let walker = GraphWalker::new(&self.workflow_graph);
        let outputs = walker.step(&mut cursor, signal, task);

        if outputs.is_empty() {
            return vec![];
        }

        // Save cursor back to task metadata
        let cursor_json = cursor.to_json();
        let mut metadata = task.metadata.clone();
        metadata["graph_cursor"] = cursor_json;
        let _ = store.update_task_metadata(&task.id, &metadata);

        // Collect workflow actions from graph outputs
        outputs.into_iter().filter_map(|o| o.action).collect()
    }

    /// Get a reference to the workflow engine.
    pub fn workflow(&self) -> &WorkflowEngine {
        &self.workflow
    }
}

// ── Signal classification ──────────────────────────────────────────────

/// Classified signal kind for transition dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SignalKind {
    WorkerSpawned,
    WorkerRunning,
    WorkerWaiting,
    WorkerClosed,
    BranchReady,
    PrOpened,
    ReviewVerdict,
    CiPass,
    CiFailure,
    BotReview,
    MergedPr,
    PrClosed,
    Other,
}

/// Classify a signal into a `SignalKind` based on source and external_id.
fn signal_kind(signal: &SignalRecord) -> SignalKind {
    match signal.source.as_str() {
        // New proper signal sources (Phase 3)
        "swarm_worker_spawned" => SignalKind::WorkerSpawned,
        "swarm_worker_running" => SignalKind::WorkerRunning,
        "swarm_worker_waiting" => SignalKind::WorkerWaiting,
        "swarm_worker_closed" => SignalKind::WorkerClosed,
        "swarm_pr_opened" => SignalKind::PrOpened,
        "swarm_branch_ready" => SignalKind::BranchReady,
        "swarm_review_verdict" => SignalKind::ReviewVerdict,
        // GitHub signals
        "github_ci_pass" => SignalKind::CiPass,
        "github_ci_failure" => SignalKind::CiFailure,
        "github_bot_review" => SignalKind::BotReview,
        "github_merged_pr" => SignalKind::MergedPr,
        "github_pr_closed" => SignalKind::PrClosed,
        // Backward compat: old "swarm" source with external_id disambiguation
        "swarm" => {
            if signal.external_id.starts_with("swarm-spawned-") {
                SignalKind::WorkerSpawned
            } else if signal.external_id.starts_with("swarm-pr-") {
                SignalKind::PrOpened
            } else if signal.external_id.starts_with("swarm-waiting-") {
                SignalKind::WorkerWaiting
            } else if signal.external_id.starts_with("swarm-completed-") {
                SignalKind::WorkerClosed
            } else {
                SignalKind::Other
            }
        }
        _ => SignalKind::Other,
    }
}

/// A desired stage transition with associated side effects.
struct TransitionIntent {
    to: TaskStage,
    reason: String,
    notification: Option<String>,
    forward_to_worker: Option<(String, String)>,
}

// ── Task matching helpers ──────────────────────────────────────────────

/// Find a task that matches this signal.
/// Tries PR matching first, then worker_id matching for swarm lifecycle signals.
fn find_task_for_signal(
    store: &TaskStore,
    workspace: &str,
    signal: &SignalRecord,
) -> Result<Option<Task>> {
    // Try matching by PR
    if let Some((repo, pr_number)) = match_signal_to_task_pr(signal)
        && let Some(task) = store.find_task_by_pr(workspace, &repo, pr_number)?
    {
        return Ok(Some(task));
    }

    // For swarm_branch_ready signals, match by worker_id in metadata
    if signal.source == "swarm_branch_ready"
        && let Some(ref meta) = signal.metadata
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(meta)
        && let Some(worker_id) = v.get("worker_id").and_then(|w| w.as_str())
        && let Some(task) = store.find_task_by_worker(workspace, worker_id)?
    {
        return Ok(Some(task));
    }

    // For swarm_review_verdict signals in the branch-first flow (no PR), match by reviewer_worker_id
    if signal.source == "swarm_review_verdict"
        && let Some(ref meta) = signal.metadata
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(meta)
        && let Some(reviewer_id) = v.get("reviewer_worker_id").and_then(|w| w.as_str())
        && let Some(task) = store.find_task_by_reviewer_worker(workspace, reviewer_id)?
    {
        return Ok(Some(task));
    }

    // For swarm worker lifecycle signals, match by worker_id from metadata
    if matches!(
        signal.source.as_str(),
        "swarm_worker_running"
            | "swarm_worker_waiting"
            | "swarm_worker_closed"
            | "swarm"
            | "swarm_pr_opened"
    ) && let Some(worker_id) = extract_metadata_str(signal, "worker_id")
        .or_else(|| extract_worker_id_from_external_id(&signal.external_id))
    {
        let role = extract_metadata_str(signal, "role");

        if role.as_deref() == Some("reviewer") {
            if let Some(task) = store.find_task_by_reviewer_worker(workspace, &worker_id)? {
                return Ok(Some(task));
            }
        } else if let Some(task) = store.find_task_by_worker(workspace, &worker_id)? {
            return Ok(Some(task));
        }
    }

    Ok(None)
}

/// Extract a string field from signal metadata JSON.
fn extract_metadata_str(signal: &SignalRecord, key: &str) -> Option<String> {
    signal
        .metadata
        .as_ref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| v.get(key).and_then(|s| s.as_str()).map(String::from))
}

/// Extract worker_id from external_id patterns like "swarm-spawned-{id}" or "swarm-pr-{id}".
fn extract_worker_id_from_external_id(external_id: &str) -> Option<String> {
    for prefix in &[
        "swarm-spawned-",
        "swarm-pr-",
        "swarm-waiting-",
        "swarm-completed-",
    ] {
        if let Some(id) = external_id.strip_prefix(prefix) {
            return Some(id.to_string());
        }
    }
    None
}

/// Try to match a signal to an existing task by PR number + repo.
/// Extracts PR info from signal metadata, title, or URL.
pub fn match_signal_to_task_pr(signal: &SignalRecord) -> Option<(String, i64)> {
    // Try metadata first (structured)
    if let Some(ref meta) = signal.metadata
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(meta)
    {
        let repo = v.get("repo").and_then(|r| r.as_str()).map(String::from);
        let pr_num = v
            .get("pr_number")
            .and_then(|n| n.as_i64())
            .or_else(|| v.get("number").and_then(|n| n.as_i64()));
        if let (Some(repo), Some(num)) = (repo, pr_num) {
            return Some((repo, num));
        }
    }

    // Try URL pattern: https://github.com/{owner}/{repo}/pull/{number}
    if let Some(ref url) = signal.url
        && let Some(caps) = extract_github_pr_from_url(url)
    {
        return Some(caps);
    }

    None
}

/// Extract (repo, pr_number) from a GitHub PR URL.
pub fn extract_github_pr_from_url(url: &str) -> Option<(String, i64)> {
    // Pattern: https://github.com/{owner}/{repo}/pull/{number}
    let parts: Vec<&str> = url.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "pull" || *part == "pulls") && i >= 2 && i + 1 < parts.len() {
            let owner = parts[i - 2];
            let repo_name = parts[i - 1];
            let num_str = parts[i + 1].split('#').next()?.split('?').next()?;
            if let Ok(num) = num_str.parse::<i64>() {
                return Some((format!("{owner}/{repo_name}"), num));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::buzz::{
        signal::{Severity, SignalStatus},
        task::{Task, TaskStage, store::TaskStore},
    };

    fn make_signal(source: &str, title: &str) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.to_string(),
            external_id: "ext-1".to_string(),
            title: title.to_string(),
            body: None,
            severity: Severity::Info,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
            snoozed_until: None,
        }
    }

    fn make_task(workspace: &str, stage: TaskStage) -> Task {
        let now = Utc::now();
        Task {
            id: uuid::Uuid::new_v4().to_string(),
            workspace: workspace.to_string(),
            title: "Test task".to_string(),
            stage,
            source: None,
            source_url: None,
            worker_id: Some("worker-1".to_string()),
            pr_url: Some("https://github.com/org/repo/pull/42".to_string()),
            pr_number: Some(42),
            repo: Some("org/repo".to_string()),
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    #[test]
    fn test_process_signal_routes_to_correct_tier() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // Silent signal
        let signal = make_signal("github_ci_pass", "CI passed");
        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert_eq!(result.notification_tier, NotificationTier::Silent);
        assert!(result.notification_message.is_none());

        // Chat signal
        let signal = make_signal("github_ci_failure", "CI failed");
        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert_eq!(result.notification_tier, NotificationTier::Chat);
        assert!(result.notification_message.is_some());
    }

    #[test]
    fn test_process_signal_matches_actions() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // "swarm_worker_running" should match the "swarm" action (prefix match)
        let signal = make_signal("swarm_worker_running", "Worker running");
        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert_eq!(result.matched_actions.len(), 1);
        assert_eq!(result.matched_actions[0].trigger, "swarm");

        // "github_bot_review" should match exactly
        let signal = make_signal("github_bot_review", "Bot review");
        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert_eq!(result.matched_actions.len(), 1);
        assert_eq!(result.matched_actions[0].trigger, "github_bot_review");

        // "sentry" should not match any default action
        let signal = make_signal("sentry", "Error alert");
        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert!(result.matched_actions.is_empty());
    }

    #[test]
    fn test_merged_pr_transitions_to_merged() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let task = make_task("test", TaskStage::HumanReview);
        store.create_task(&task).unwrap();

        let mut signal = make_signal("github_merged_pr", "PR merged");
        signal.url = Some("https://github.com/org/repo/pull/42".to_string());
        signal.metadata =
            Some(serde_json::json!({"repo": "org/repo", "pr_number": 42}).to_string());

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();

        assert!(result.engine_result.transitioned);
        let updated = result.engine_result.task.unwrap();
        assert_eq!(updated.stage, TaskStage::Merged);
    }

    #[test]
    fn test_monotonic_no_backward_transition() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // Task is already in HumanReview
        let task = make_task("test", TaskStage::HumanReview);
        store.create_task(&task).unwrap();

        // A late swarm_worker_running signal arrives — should NOT regress to InProgress
        let mut signal = make_signal("swarm_worker_running", "Worker running");
        signal.metadata = Some(serde_json::json!({"worker_id": "worker-1"}).to_string());

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();

        // Task should still be HumanReview
        let updated = result.engine_result.task.unwrap();
        assert_eq!(updated.stage, TaskStage::HumanReview);
        assert!(!result.engine_result.transitioned);
    }

    #[test]
    fn test_task_creation_from_worker_spawned() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let mut signal = make_signal("swarm", "Worker spawned: abc-123");
        signal.external_id = "swarm-spawned-abc-123".to_string();
        signal.body = Some("agent: claude\nFix the login bug".to_string());

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();

        assert!(result.engine_result.transitioned);
        let task = result.engine_result.task.unwrap();
        assert_eq!(task.stage, TaskStage::InProgress);
        assert_eq!(task.worker_id.as_deref(), Some("abc-123"));
        assert_eq!(task.title, "Fix the login bug");
    }

    #[test]
    fn test_stale_task_detection() {
        use chrono::Duration;

        let store = TaskStore::open_memory().unwrap();
        let mut task = make_task("test", TaskStage::InProgress);
        task.pr_url = None;
        task.pr_number = None;
        task.repo = None;
        task.updated_at = Utc::now() - Duration::hours(25);
        store.create_task(&task).unwrap();

        let reconciler = reconcile::Reconciler::new(60);
        let checks = reconciler.check_tasks(&store, "test").unwrap();

        // Should flag as needing attention (worker check + stale)
        let has_stale = checks
            .iter()
            .any(|c| matches!(c, reconcile::ReconcileCheck::Stale { .. }));
        assert!(has_stale, "task stuck >24h should be flagged as stale");
    }

    #[test]
    fn test_workflow_branch_ready_direct_pr() {
        let config = OrchestratorConfig {
            workflow: WorkflowConfig {
                branch_ready_action: workflow::BranchReadyAction::DirectPr,
                ..Default::default()
            },
            ..Default::default()
        };
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let mut task = make_task("test", TaskStage::InProgress);
        task.pr_url = None;
        task.pr_number = None;
        task.repo = None;
        store.create_task(&task).unwrap();

        let mut signal = make_signal("swarm_branch_ready", "Branch ready");
        signal.metadata = Some(
            serde_json::json!({
                "branch_name": "feat/foo",
                "worker_id": task.worker_id.as_ref().unwrap(),
            })
            .to_string(),
        );

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert_eq!(result.workflow_actions.len(), 1);
        assert!(matches!(
            &result.workflow_actions[0],
            WorkflowAction::CreatePr { .. }
        ));
    }

    #[test]
    fn test_workflow_branch_ready_ai_review() {
        let config = OrchestratorConfig {
            workflow: WorkflowConfig {
                branch_ready_action: workflow::BranchReadyAction::AiReview,
                ..Default::default()
            },
            ..Default::default()
        };
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let mut task = make_task("test", TaskStage::InProgress);
        task.pr_url = None;
        task.pr_number = None;
        task.repo = None;
        store.create_task(&task).unwrap();

        let mut signal = make_signal("swarm_branch_ready", "Branch ready");
        signal.metadata = Some(
            serde_json::json!({
                "branch_name": "feat/foo",
                "worker_id": task.worker_id.as_ref().unwrap(),
            })
            .to_string(),
        );

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();
        assert_eq!(result.workflow_actions.len(), 1);
        assert!(matches!(
            &result.workflow_actions[0],
            WorkflowAction::DispatchReviewer { .. }
        ));
    }

    #[test]
    fn test_config_deserialization() {
        let toml_str = r#"
reconcile_interval_secs = 120

[workflow]
branch_ready_action = "ai_review"
max_review_cycles = 5

[notification_tiers]
github_ci_failure = "chat"
github_ci_pass = "silent"
swarm_worker_spawned = "badge"

[[actions]]
trigger = "github_ci_failure"
action = "Forward CI error to worker"
skills = ["ci-triage"]
ttl_secs = 300

[[actions]]
trigger = "swarm_pr_opened"
action = "Report the PR"
"#;
        let config: OrchestratorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.reconcile_interval_secs, 120);
        assert_eq!(
            config.workflow.branch_ready_action,
            workflow::BranchReadyAction::AiReview
        );
        assert_eq!(config.workflow.max_review_cycles, 5);
        assert_eq!(config.notification_tiers.len(), 3);
        assert_eq!(
            config.notification_tiers.get("github_ci_failure"),
            Some(&NotificationTier::Chat)
        );
        assert_eq!(config.actions.len(), 2);
        assert_eq!(config.actions[0].trigger, "github_ci_failure");
        assert_eq!(config.actions[0].skills, vec!["ci-triage"]);
        assert_eq!(config.actions[1].ttl_secs, 120); // default
    }

    #[test]
    fn test_empty_config_uses_defaults() {
        let toml_str = "";
        let config: OrchestratorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.reconcile_interval_secs, 60);
        assert_eq!(
            config.workflow.branch_ready_action,
            workflow::BranchReadyAction::DirectPr
        );
        assert_eq!(config.workflow.max_review_cycles, 3);
        assert!(config.notification_tiers.is_empty());
        assert_eq!(config.actions.len(), 3); // default actions
    }

    #[test]
    fn test_pr_closed_transitions_to_dismissed() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let task = make_task("test", TaskStage::HumanReview);
        store.create_task(&task).unwrap();

        let mut signal = make_signal("github_pr_closed", "PR closed");
        signal.metadata =
            Some(serde_json::json!({"repo": "org/repo", "pr_number": 42}).to_string());

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();

        assert!(result.engine_result.transitioned);
        let updated = result.engine_result.task.unwrap();
        assert_eq!(updated.stage, TaskStage::Dismissed);
    }

    // ── Graph integration tests ─────────────────────────────────────────

    fn make_signal_with_meta(source: &str, title: &str, meta: serde_json::Value) -> SignalRecord {
        let mut sig = make_signal(source, title);
        sig.metadata = Some(meta.to_string());
        sig
    }

    fn make_signal_with_ext(source: &str, title: &str, ext_id: &str) -> SignalRecord {
        let mut sig = make_signal(source, title);
        sig.external_id = ext_id.to_string();
        sig
    }

    /// Helper: get the graph cursor from a task's metadata.
    fn get_cursor(task: &Task) -> Option<graph::walker::GraphCursor> {
        task.metadata
            .get("graph_cursor")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    #[test]
    fn test_graph_full_workflow_spawned_to_merged() {
        // Tests the full signal chain: spawned → waiting → verdict(approved) → pr_opened → merged
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // 1. Worker spawned → creates task, cursor at "coding"
        let sig = make_signal_with_ext(
            "swarm_worker_spawned",
            "Worker spawned: w1",
            "swarm-spawned-w1",
        );
        let result = orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task_id = result.engine_result.task.unwrap().id;
        let task = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(task.stage, TaskStage::InProgress);
        let cursor = get_cursor(&task).expect("cursor should exist after spawn");
        assert_eq!(cursor.current_node, "coding");
        assert_eq!(cursor.history.len(), 2); // triage→write_code→coding

        // 2. Worker waiting → cursor should advance to "await_verdict" (coding→ai_review→await_verdict)
        let sig = make_signal_with_meta(
            "swarm_worker_waiting",
            "Worker waiting: w1",
            serde_json::json!({"worker_id": "w1"}),
        );
        orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task = store.get_task(&task_id).unwrap().unwrap();
        let cursor = get_cursor(&task).expect("cursor should exist after waiting");
        assert_eq!(
            cursor.current_node, "await_verdict",
            "cursor should advance from coding through ai_review to await_verdict"
        );

        // 3. Review verdict: APPROVED → cursor should advance to "human_review"
        //    Need reviewer_worker_id stored on task for signal matching
        let mut meta = task.metadata.clone();
        meta["reviewer_worker_id"] = serde_json::json!("reviewer-1");
        store.update_task_metadata(&task_id, &meta).unwrap();

        let sig = make_signal_with_meta(
            "swarm_review_verdict",
            "Review: APPROVED",
            serde_json::json!({
                "verdict": "APPROVED",
                "branch_name": "feat/foo",
                "reviewer_worker_id": "reviewer-1",
            }),
        );
        orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task = store.get_task(&task_id).unwrap().unwrap();
        let cursor = get_cursor(&task).expect("cursor should exist after verdict");
        assert_eq!(
            cursor.current_node, "human_review",
            "cursor should advance through create_pr to human_review"
        );

        // 4. PR merged → cursor at "merged"
        let _ = store.update_task_pr(&task_id, "https://github.com/org/repo/pull/42", 42);
        let _ = store.update_task_repo(&task_id, "org/repo");
        let sig = make_signal_with_meta(
            "github_merged_pr",
            "PR merged",
            serde_json::json!({"repo": "org/repo", "pr_number": 42}),
        );
        orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(task.stage, TaskStage::Merged);
        let cursor = get_cursor(&task).expect("cursor should exist after merge");
        assert_eq!(cursor.current_node, "merged");
    }

    #[test]
    fn test_graph_worker_waiting_advances_from_coding() {
        // The key signal routing test: swarm_worker_waiting must find the task and advance the graph
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // Create task via worker_spawned
        let sig = make_signal_with_ext(
            "swarm_worker_spawned",
            "Worker spawned: w1",
            "swarm-spawned-w1",
        );
        let result = orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task_id = result.engine_result.task.unwrap().id;
        // Re-fetch from DB to get cursor saved by evaluate_graph
        let task = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(get_cursor(&task).unwrap().current_node, "coding");

        // swarm_worker_waiting with metadata worker_id
        let sig = make_signal_with_meta(
            "swarm_worker_waiting",
            "Worker waiting: w1",
            serde_json::json!({"worker_id": "w1"}),
        );
        let result = orchestrator.process_signal(&store, "test", &sig).unwrap();
        assert!(
            result.engine_result.task.is_some(),
            "task should be found for swarm_worker_waiting"
        );
        // Re-fetch from DB to get updated cursor
        let task = store.get_task(&task_id).unwrap().unwrap();
        let cursor = get_cursor(&task).expect("cursor should exist");
        assert_eq!(
            cursor.current_node, "await_verdict",
            "worker_waiting should advance coding→ai_review→await_verdict"
        );
    }

    #[test]
    fn test_graph_worker_waiting_via_external_id() {
        // Same test but using external_id pattern instead of metadata
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let sig = make_signal_with_ext(
            "swarm_worker_spawned",
            "Worker spawned: w1",
            "swarm-spawned-w1",
        );
        let task_id = orchestrator
            .process_signal(&store, "test", &sig)
            .unwrap()
            .engine_result
            .task
            .unwrap()
            .id;

        // swarm_worker_waiting WITHOUT metadata, but with external_id pattern
        let sig = make_signal_with_ext(
            "swarm_worker_waiting",
            "Worker waiting: w1",
            "swarm-waiting-w1",
        );
        let result = orchestrator.process_signal(&store, "test", &sig).unwrap();
        assert!(
            result.engine_result.task.is_some(),
            "task should be found via external_id"
        );
        let task = store.get_task(&task_id).unwrap().unwrap();
        let cursor = get_cursor(&task).expect("cursor should exist");
        assert_eq!(cursor.current_node, "await_verdict");
    }

    #[test]
    fn test_graph_rework_loop_increments_counter() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // Create task and advance to await_verdict
        let sig = make_signal_with_ext(
            "swarm_worker_spawned",
            "Worker spawned: w1",
            "swarm-spawned-w1",
        );
        let task_id = orchestrator
            .process_signal(&store, "test", &sig)
            .unwrap()
            .engine_result
            .task
            .unwrap()
            .id;

        let sig = make_signal_with_meta(
            "swarm_worker_waiting",
            "Worker waiting: w1",
            serde_json::json!({"worker_id": "w1"}),
        );
        orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(get_cursor(&task).unwrap().current_node, "await_verdict");

        // Store reviewer_worker_id on task for matching (preserve cursor)
        let mut meta = task.metadata.clone();
        meta["reviewer_worker_id"] = serde_json::json!("reviewer-1");
        store.update_task_metadata(&task_id, &meta).unwrap();

        // Changes requested → should loop back to write_code→coding
        let sig = make_signal_with_meta(
            "swarm_review_verdict",
            "CHANGES_REQUESTED",
            serde_json::json!({
                "verdict": "CHANGES_REQUESTED",
                "comments": "Fix the tests",
                "reviewer_worker_id": "reviewer-1",
            }),
        );
        orchestrator.process_signal(&store, "test", &sig).unwrap();
        let task = store.get_task(&task_id).unwrap().unwrap();
        let cursor = get_cursor(&task).unwrap();
        assert_eq!(
            cursor.current_node, "coding",
            "CHANGES_REQUESTED should loop back to coding"
        );
        assert_eq!(
            cursor.counters.get("review_cycles").copied(),
            Some(1),
            "review_cycles counter should be 1"
        );
    }

    #[test]
    fn test_graph_unmatched_signal_doesnt_advance() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // Create task at "coding"
        let sig = make_signal_with_ext(
            "swarm_worker_spawned",
            "Worker spawned: w1",
            "swarm-spawned-w1",
        );
        orchestrator.process_signal(&store, "test", &sig).unwrap();

        // Random signal that doesn't match any edge from "coding"
        let sig = make_signal_with_meta(
            "github_ci_pass",
            "CI passed",
            serde_json::json!({"worker_id": "w1"}),
        );
        let _result = orchestrator.process_signal(&store, "test", &sig).unwrap();
        // github_ci_pass won't match the task (not in worker lifecycle matching)
        // so graph won't run — task stays at coding
        let tasks = store.get_all_tasks("test").unwrap();
        assert_eq!(tasks.len(), 1);
        let cursor = get_cursor(&tasks[0]).unwrap();
        assert_eq!(cursor.current_node, "coding");
    }

    #[test]
    fn test_graph_pr_closed_reaches_dismissed() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        // Create a task already at human_review with PR info
        let mut task = make_task("test", TaskStage::HumanReview);
        // Set graph cursor to human_review
        let cursor = graph::walker::GraphCursor {
            current_node: "human_review".to_string(),
            counters: Default::default(),
            artifacts: Default::default(),
            history: vec![],
        };
        task.metadata["graph_cursor"] = serde_json::to_value(&cursor).unwrap();
        store.create_task(&task).unwrap();
        let task_id = task.id.clone();

        let sig = make_signal_with_meta(
            "github_pr_closed",
            "PR closed",
            serde_json::json!({"repo": "org/repo", "pr_number": 42}),
        );
        let result = orchestrator.process_signal(&store, "test", &sig).unwrap();
        assert!(result.engine_result.transitioned);
        // Re-fetch from DB
        let task = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(task.stage, TaskStage::Dismissed);
        let cursor = get_cursor(&task).unwrap();
        assert_eq!(cursor.current_node, "dismissed");
    }

    #[test]
    fn test_signal_kind_classification() {
        // New proper signal sources (Phase 3)
        let s = make_signal("swarm_worker_spawned", "Worker spawned");
        assert_eq!(signal_kind(&s), SignalKind::WorkerSpawned);

        let s = make_signal("swarm_pr_opened", "PR opened");
        assert_eq!(signal_kind(&s), SignalKind::PrOpened);

        let s = make_signal("swarm_worker_running", "Worker running");
        assert_eq!(signal_kind(&s), SignalKind::WorkerRunning);

        let s = make_signal("swarm_worker_waiting", "Worker waiting");
        assert_eq!(signal_kind(&s), SignalKind::WorkerWaiting);

        let s = make_signal("swarm_worker_closed", "Worker closed");
        assert_eq!(signal_kind(&s), SignalKind::WorkerClosed);

        let s = make_signal("swarm_branch_ready", "Branch ready");
        assert_eq!(signal_kind(&s), SignalKind::BranchReady);

        let s = make_signal("swarm_review_verdict", "Review verdict");
        assert_eq!(signal_kind(&s), SignalKind::ReviewVerdict);

        // Backward compat: old "swarm" source with external_id disambiguation
        let mut s = make_signal("swarm", "Worker spawned");
        s.external_id = "swarm-spawned-abc".to_string();
        assert_eq!(signal_kind(&s), SignalKind::WorkerSpawned);

        let mut s = make_signal("swarm", "PR opened");
        s.external_id = "swarm-pr-abc".to_string();
        assert_eq!(signal_kind(&s), SignalKind::PrOpened);

        // GitHub signals
        let s = make_signal("github_merged_pr", "Merged");
        assert_eq!(signal_kind(&s), SignalKind::MergedPr);

        let s = make_signal("github_ci_failure", "CI failed");
        assert_eq!(signal_kind(&s), SignalKind::CiFailure);
    }
}

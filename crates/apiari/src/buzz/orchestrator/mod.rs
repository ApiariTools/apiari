//! Unified orchestrator — single entry point for all signal processing.
//!
//! Replaces the three overlapping systems:
//! - `buzz/pipeline/` (notification pipeline)
//! - `coordinator.signal_hooks` (coordinator follow-throughs)
//! - `buzz/task/engine` (task state transitions)
//!
//! Every signal flows through `Orchestrator::process_signal()`, which:
//! 1. Updates task state (delegating to the existing task engine)
//! 2. Routes notifications to the correct tier (Silent/Badge/Chat)
//! 3. Fires any matching orchestrator actions (coordinator follow-throughs)

pub mod notify;
pub mod reconcile;
pub mod workflow;

use std::collections::HashMap;

use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use self::{
    notify::{NotificationRouter, NotificationTier},
    workflow::{WorkflowAction, WorkflowConfig, WorkflowEngine},
};
use crate::buzz::{
    signal::SignalRecord,
    task::{self, engine::EngineResult, store::TaskStore},
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
}

impl Orchestrator {
    /// Create a new orchestrator from config.
    pub fn new(config: &OrchestratorConfig) -> Self {
        Self {
            router: NotificationRouter::new(config.notification_tiers.clone()),
            workflow: WorkflowEngine::new(config.workflow.clone()),
            actions: config.actions.clone(),
        }
    }

    /// Process a signal through the entire orchestration pipeline.
    ///
    /// This is the single entry point. It:
    /// 1. Runs the signal through the task engine (state transitions)
    /// 2. Routes the signal to the correct notification tier
    /// 3. Matches orchestrator actions for coordinator follow-throughs
    /// 4. Evaluates workflow rules (branch_ready → review/PR)
    pub fn process_signal(
        &self,
        store: &TaskStore,
        workspace: &str,
        signal: &SignalRecord,
    ) -> Result<OrchestratorResult> {
        // 1. Task engine: match signal to task, evaluate rules, apply transitions
        let engine_result = task::engine::process_signal(store, workspace, signal)?;

        // 2. Notification routing
        let routing = self
            .router
            .route(&signal.source, &signal.title, signal.url.as_deref());

        // 3. Match orchestrator actions (coordinator follow-throughs)
        let matched_actions = self.match_actions(signal);

        // 4. Workflow evaluation
        let workflow_actions = self.evaluate_workflow(signal, &engine_result);

        info!(
            "[orchestrator] signal '{}' (source={}) → tier={:?}, actions={}, workflow_actions={}",
            signal.title,
            signal.source,
            routing.tier,
            matched_actions.len(),
            workflow_actions.len(),
        );

        Ok(OrchestratorResult {
            engine_result,
            notification_tier: routing.tier,
            notification_message: routing.message,
            matched_actions,
            workflow_actions,
        })
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

    /// Get a reference to the workflow engine.
    pub fn workflow(&self) -> &WorkflowEngine {
        &self.workflow
    }
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

        // "swarm_worker_spawned" should match the "swarm" action (prefix match)
        let signal = make_signal("swarm_worker_spawned", "Worker spawned");
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
    fn test_process_signal_fires_task_engine() {
        let config = OrchestratorConfig::default();
        let orchestrator = Orchestrator::new(&config);
        let store = TaskStore::open_memory().unwrap();

        let task = make_task("test", TaskStage::InAiReview);
        store.create_task(&task).unwrap();

        let mut signal = make_signal("github_ci_pass", "CI passed on PR #42");
        signal.url = Some("https://github.com/org/repo/pull/42".to_string());

        let result = orchestrator
            .process_signal(&store, "test", &signal)
            .unwrap();

        // Task engine should have transitioned the task
        assert!(result.engine_result.transitioned);
        let updated_task = result.engine_result.task.unwrap();
        assert_eq!(updated_task.stage, TaskStage::HumanReview);
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
}

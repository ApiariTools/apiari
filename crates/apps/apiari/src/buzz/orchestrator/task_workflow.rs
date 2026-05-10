//! Task lifecycle workflow built on apiari-workflow.
//!
//! Models the branch-ready → (optional AI review) → PR creation → merge
//! lifecycle as a durable, SQLite-backed workflow. Replaces the stateless
//! WorkflowEngine and the prototype graph/walker.
//!
//! Signal patterns are task-scoped ("branch_ready:{task_id}") so multiple
//! concurrent tasks don't cross-contaminate each other.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use apiari_workflow::{StepContext, StepResult, WorkflowDef, WorkflowEngine, WorkflowRun};

use super::workflow::{BranchReadyAction, WorkflowAction, WorkflowConfig};

// ── Step definitions ─────────────────────────────────────────────────────────

/// Parks on "branch_ready:{task_id}" — waits for a worker to push a branch.
struct WaitBranchReadyStep;

#[async_trait]
impl apiari_workflow::Step for WaitBranchReadyStep {
    fn name(&self) -> &'static str {
        "wait_branch_ready"
    }
    async fn run(&self, ctx: &StepContext) -> StepResult {
        let task_id = ctx.get::<String>("task_id").unwrap_or_default();
        StepResult::WaitForSignal {
            pattern: format!("branch_ready:{task_id}"),
            timeout: None,
        }
    }
}

/// Decides based on config: DirectPr → GoTo("queue_pr"); AiReview → dispatch reviewer.
struct RouteAfterBranchStep {
    action: BranchReadyAction,
}

#[async_trait]
impl apiari_workflow::Step for RouteAfterBranchStep {
    fn name(&self) -> &'static str {
        "route_after_branch"
    }
    async fn run(&self, ctx: &StepContext) -> StepResult {
        let task_id = ctx.get::<String>("task_id").unwrap_or_default();
        let branch_name = ctx.get::<String>("branch_name").unwrap_or_default();
        let worker_id = ctx.get::<String>("worker_id").unwrap_or_default();

        match self.action {
            BranchReadyAction::DirectPr => StepResult::GoTo {
                step: "queue_pr",
                output: json!({
                    "pending_action": "create_pr",
                    "action_task_id": task_id,
                    "action_branch_name": branch_name,
                }),
            },
            BranchReadyAction::AiReview => StepResult::Complete(json!({
                "pending_action": "dispatch_reviewer",
                "action_task_id": task_id,
                "action_branch_name": branch_name,
                "action_worker_id": worker_id,
            })),
        }
    }
}

/// Parks on "verdict:{task_id}" — waits for AI review result.
struct WaitVerdictStep;

#[async_trait]
impl apiari_workflow::Step for WaitVerdictStep {
    fn name(&self) -> &'static str {
        "wait_verdict"
    }
    async fn run(&self, ctx: &StepContext) -> StepResult {
        let task_id = ctx.get::<String>("task_id").unwrap_or_default();
        StepResult::WaitForSignal {
            pattern: format!("verdict:{task_id}"),
            timeout: None,
        }
    }
}

/// Decides based on verdict: APPROVED → proceed to queue_pr;
/// CHANGES_REQUESTED → dispatch rework (GoTo wait_branch_ready for re-review);
/// unknown verdict → re-park at wait_verdict.
struct HandleVerdictStep {
    max_review_cycles: u32,
}

#[async_trait]
impl apiari_workflow::Step for HandleVerdictStep {
    fn name(&self) -> &'static str {
        "handle_verdict"
    }
    async fn run(&self, ctx: &StepContext) -> StepResult {
        let task_id = ctx.get::<String>("task_id").unwrap_or_default();
        let branch_name = ctx.get::<String>("branch_name").unwrap_or_default();
        let verdict = ctx.get::<String>("verdict").unwrap_or_default();
        let feedback = ctx.get::<String>("feedback").unwrap_or_default();
        let cycles = ctx.get::<u32>("review_cycles").unwrap_or(0);

        match verdict.as_str() {
            "APPROVED" => StepResult::Complete(json!({
                "pending_action": "create_pr",
                "action_task_id": task_id,
                "action_branch_name": branch_name,
            })),
            "CHANGES_REQUESTED" if cycles >= self.max_review_cycles => StepResult::GoTo {
                step: "queue_pr",
                output: json!({
                    "pending_action": "force_create_pr",
                    "action_task_id": task_id,
                    "action_branch_name": branch_name,
                    "cycle_count": cycles,
                }),
            },
            "CHANGES_REQUESTED" => StepResult::GoTo {
                step: "wait_branch_ready",
                output: json!({
                    "pending_action": "dispatch_rework",
                    "action_task_id": task_id,
                    "feedback": feedback,
                    "review_cycles": cycles + 1,
                }),
            },
            _ => {
                // Unknown verdict — re-park and wait for another signal.
                StepResult::GoTo {
                    step: "wait_verdict",
                    output: json!({}),
                }
            }
        }
    }
}

/// Pass-through step: pending_action is already set by the step that jumped here.
/// Advances to wait_merge.
struct QueuePrStep;

#[async_trait]
impl apiari_workflow::Step for QueuePrStep {
    fn name(&self) -> &'static str {
        "queue_pr"
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        StepResult::Complete(json!({}))
    }
}

/// Parks on "merge:{task_id}" — waits for GitHub merged/closed event.
struct WaitMergeStep;

#[async_trait]
impl apiari_workflow::Step for WaitMergeStep {
    fn name(&self) -> &'static str {
        "wait_merge"
    }
    async fn run(&self, ctx: &StepContext) -> StepResult {
        let task_id = ctx.get::<String>("task_id").unwrap_or_default();
        StepResult::WaitForSignal {
            pattern: format!("merge:{task_id}"),
            timeout: None,
        }
    }
}

// ── Workflow builder ──────────────────────────────────────────────────────────

/// Build the task lifecycle WorkflowDef for the given config.
pub fn build_task_lifecycle(config: &WorkflowConfig) -> WorkflowDef {
    let max_revisions = config.max_review_cycles * 4 + 20;

    let mut def = WorkflowDef::new("task_lifecycle")
        .max_revisions(max_revisions)
        .step(WaitBranchReadyStep)
        .step(RouteAfterBranchStep {
            action: config.branch_ready_action.clone(),
        });

    if config.branch_ready_action == BranchReadyAction::AiReview {
        def = def.step(WaitVerdictStep).step(HandleVerdictStep {
            max_review_cycles: config.max_review_cycles,
        });
    }

    def.step(QueuePrStep).step(WaitMergeStep)
}

// ── Manager ───────────────────────────────────────────────────────────────────

/// Wraps `apiari_workflow::WorkflowEngine` and translates apiari task signals
/// into task-scoped workflow signals, returning `WorkflowAction`s for the daemon.
pub struct TaskWorkflowManager {
    engine: Arc<WorkflowEngine>,
}

impl TaskWorkflowManager {
    /// Open (or create) the workflow DB at `db_path` and register the task lifecycle.
    pub fn open(db_path: &str, config: &WorkflowConfig) -> apiari_workflow::Result<Self> {
        let mut engine = WorkflowEngine::new(db_path)?;
        engine.register(build_task_lifecycle(config));
        Ok(Self {
            engine: Arc::new(engine),
        })
    }

    /// Start a new workflow run for a task. Returns the run ID to store in task metadata.
    pub async fn start_task(
        &self,
        task_id: &str,
        workspace: &str,
    ) -> apiari_workflow::Result<String> {
        self.engine
            .start(
                "task_lifecycle",
                json!({ "task_id": task_id, "workspace": workspace }),
            )
            .await
    }

    /// Signal that a branch is ready for task `task_id`.
    pub async fn on_branch_ready(
        &self,
        run_id: &str,
        task_id: &str,
        branch_name: &str,
        worker_id: &str,
    ) -> Vec<WorkflowAction> {
        let payload = json!({
            "task_id": task_id,
            "branch_name": branch_name,
            "worker_id": worker_id,
        });
        if let Err(e) = self.engine.signal_run(run_id, payload).await {
            tracing::warn!(run_id, "workflow signal_run(branch_ready) error: {e}");
            return vec![];
        }
        self.extract_actions(run_id).await
    }

    /// Signal a review verdict for task `task_id`.
    pub async fn on_review_verdict(
        &self,
        run_id: &str,
        task_id: &str,
        branch_name: &str,
        verdict: &str,
        feedback: &str,
    ) -> Vec<WorkflowAction> {
        let payload = json!({
            "task_id": task_id,
            "branch_name": branch_name,
            "verdict": verdict,
            "feedback": feedback,
        });
        if let Err(e) = self.engine.signal_run(run_id, payload).await {
            tracing::warn!(run_id, "workflow signal_run(verdict) error: {e}");
            return vec![];
        }
        self.extract_actions(run_id).await
    }

    /// Signal that the PR was merged or closed for task `task_id`.
    pub async fn on_merge(&self, run_id: &str) -> Vec<WorkflowAction> {
        if let Err(e) = self.engine.signal_run(run_id, json!({})).await {
            tracing::warn!(run_id, "workflow signal_run(merge) error: {e}");
        }
        vec![]
    }

    async fn extract_actions(&self, run_id: &str) -> Vec<WorkflowAction> {
        match self.engine.get_run(run_id).await {
            Ok(run) => actions_from_context(&run),
            Err(e) => {
                tracing::warn!(run_id, "failed to read workflow run after signal: {e}");
                vec![]
            }
        }
    }
}

/// Convert a workflow run's context into daemon-executable WorkflowActions.
fn actions_from_context(run: &WorkflowRun) -> Vec<WorkflowAction> {
    let ctx = &run.context;
    let pending = ctx.get("pending_action").and_then(|v| v.as_str());
    let task_id = || {
        ctx.get("action_task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let branch_name = || {
        ctx.get("action_branch_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    match pending {
        Some("create_pr") => vec![WorkflowAction::CreatePr {
            task_id: task_id(),
            branch_name: branch_name(),
        }],
        Some("force_create_pr") => {
            let cycle_count = ctx.get("cycle_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            vec![WorkflowAction::ForceCreatePr {
                task_id: task_id(),
                branch_name: branch_name(),
                cycle_count,
            }]
        }
        Some("dispatch_reviewer") => {
            let worker_id = ctx
                .get("action_worker_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            vec![WorkflowAction::DispatchReviewer {
                task_id: task_id(),
                branch_name: branch_name(),
                worker_id,
            }]
        }
        Some("dispatch_rework") => {
            let feedback = ctx
                .get("feedback")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            vec![WorkflowAction::DispatchRework {
                task_id: task_id(),
                feedback,
            }]
        }
        _ => vec![],
    }
}

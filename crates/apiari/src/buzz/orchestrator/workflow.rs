//! Configurable workflow engine for signal-driven task transitions.
//!
//! Handles the core workflow: branch ready → (optional AI review) → PR creation.
//! The `branch_ready_action` config controls whether AI review is injected
//! before PR creation or skipped entirely.

use std::path::Path;

use color_eyre::eyre::bail;
use serde::{Deserialize, Serialize};
use tracing::info;

/// What happens when a worker pushes a branch (BRANCH_READY signal).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchReadyAction {
    /// Create PR immediately via `gh pr create` (no AI review).
    #[default]
    DirectPr,
    /// Dispatch a review worker first; PR on pass.
    AiReview,
}

/// Workflow configuration from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// What happens when a worker pushes a branch.
    #[serde(default)]
    pub branch_ready_action: BranchReadyAction,
    /// Max review→rework cycles before forcing PR creation.
    #[serde(default = "default_max_review_cycles")]
    pub max_review_cycles: u32,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            branch_ready_action: BranchReadyAction::default(),
            max_review_cycles: default_max_review_cycles(),
        }
    }
}

fn default_max_review_cycles() -> u32 {
    3
}

/// Actions the workflow engine can request the orchestrator to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowAction {
    /// Dispatch an AI review worker for the given branch.
    DispatchReviewer {
        task_id: String,
        branch_name: String,
        worker_id: String,
    },
    /// Create a PR via `gh pr create` for the given branch.
    CreatePr {
        task_id: String,
        branch_name: String,
    },
    /// Dispatch a new code worker with review feedback.
    DispatchRework { task_id: String, feedback: String },
    /// Max review cycles exceeded — force PR creation.
    ForceCreatePr {
        task_id: String,
        branch_name: String,
        cycle_count: u32,
    },
}

/// The workflow engine evaluates signals and proposes workflow actions.
pub struct WorkflowEngine {
    config: WorkflowConfig,
}

impl WorkflowEngine {
    pub fn new(config: WorkflowConfig) -> Self {
        Self { config }
    }

    /// When a branch is ready, decide what to do based on config.
    pub fn on_branch_ready(
        &self,
        task_id: &str,
        branch_name: &str,
        worker_id: &str,
    ) -> WorkflowAction {
        match self.config.branch_ready_action {
            BranchReadyAction::DirectPr => WorkflowAction::CreatePr {
                task_id: task_id.to_string(),
                branch_name: branch_name.to_string(),
            },
            BranchReadyAction::AiReview => WorkflowAction::DispatchReviewer {
                task_id: task_id.to_string(),
                branch_name: branch_name.to_string(),
                worker_id: worker_id.to_string(),
            },
        }
    }

    /// When a review verdict arrives, decide what to do.
    pub fn on_review_verdict(
        &self,
        task_id: &str,
        branch_name: &str,
        verdict: &str,
        feedback: &str,
        review_cycle: u32,
    ) -> Option<WorkflowAction> {
        match verdict {
            "APPROVED" => Some(WorkflowAction::CreatePr {
                task_id: task_id.to_string(),
                branch_name: branch_name.to_string(),
            }),
            "CHANGES_REQUESTED" => {
                if review_cycle >= self.config.max_review_cycles {
                    Some(WorkflowAction::ForceCreatePr {
                        task_id: task_id.to_string(),
                        branch_name: branch_name.to_string(),
                        cycle_count: review_cycle,
                    })
                } else {
                    Some(WorkflowAction::DispatchRework {
                        task_id: task_id.to_string(),
                        feedback: feedback.to_string(),
                    })
                }
            }
            _ => None,
        }
    }

    pub fn config(&self) -> &WorkflowConfig {
        &self.config
    }
}

/// Result of a system PR creation.
#[derive(Debug, Clone)]
pub struct PrCreationResult {
    /// The URL of the created PR.
    pub pr_url: String,
    /// The PR number extracted from the URL.
    pub pr_number: Option<i64>,
}

/// Create a PR via `gh pr create` as a system action.
///
/// The system (not the agent) creates PRs — this makes the workflow
/// agent-agnostic (works with Claude, Codex, or any sandboxed agent).
pub async fn create_system_pr(
    work_dir: &Path,
    branch_name: &str,
    title: &str,
    body: &str,
) -> color_eyre::Result<PrCreationResult> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "create",
            "--head",
            branch_name,
            "--base",
            "main",
            "--title",
            title,
            "--body",
            body,
        ])
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to run gh pr create: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr create failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    info!("[workflow] system PR created: {stdout}");

    // Extract PR number from URL (e.g. https://github.com/owner/repo/pull/123)
    let pr_number = stdout
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<i64>().ok());

    Ok(PrCreationResult {
        pr_url: stdout,
        pr_number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_direct_pr_skips_review() {
        let engine = WorkflowEngine::new(WorkflowConfig {
            branch_ready_action: BranchReadyAction::DirectPr,
            max_review_cycles: 3,
        });

        let action = engine.on_branch_ready("task-1", "feat/foo", "worker-1");
        assert_eq!(
            action,
            WorkflowAction::CreatePr {
                task_id: "task-1".to_string(),
                branch_name: "feat/foo".to_string(),
            }
        );
    }

    #[test]
    fn test_ai_review_dispatches_reviewer() {
        let engine = WorkflowEngine::new(WorkflowConfig {
            branch_ready_action: BranchReadyAction::AiReview,
            max_review_cycles: 3,
        });

        let action = engine.on_branch_ready("task-1", "feat/foo", "worker-1");
        assert_eq!(
            action,
            WorkflowAction::DispatchReviewer {
                task_id: "task-1".to_string(),
                branch_name: "feat/foo".to_string(),
                worker_id: "worker-1".to_string(),
            }
        );
    }

    #[test]
    fn test_review_approved_creates_pr() {
        let engine = WorkflowEngine::new(WorkflowConfig::default());

        let action = engine.on_review_verdict("task-1", "feat/foo", "APPROVED", "", 1);
        assert_eq!(
            action,
            Some(WorkflowAction::CreatePr {
                task_id: "task-1".to_string(),
                branch_name: "feat/foo".to_string(),
            })
        );
    }

    #[test]
    fn test_review_changes_requested_dispatches_rework() {
        let engine = WorkflowEngine::new(WorkflowConfig {
            branch_ready_action: BranchReadyAction::AiReview,
            max_review_cycles: 3,
        });

        let action =
            engine.on_review_verdict("task-1", "feat/foo", "CHANGES_REQUESTED", "Fix tests", 1);
        assert_eq!(
            action,
            Some(WorkflowAction::DispatchRework {
                task_id: "task-1".to_string(),
                feedback: "Fix tests".to_string(),
            })
        );
    }

    #[test]
    fn test_ai_review_happy_path_progresses_from_branch_ready_to_pr() {
        let engine = WorkflowEngine::new(WorkflowConfig {
            branch_ready_action: BranchReadyAction::AiReview,
            max_review_cycles: 3,
        });

        assert_eq!(
            engine.on_branch_ready("task-1", "feat/foo", "worker-1"),
            WorkflowAction::DispatchReviewer {
                task_id: "task-1".to_string(),
                branch_name: "feat/foo".to_string(),
                worker_id: "worker-1".to_string(),
            }
        );

        assert_eq!(
            engine.on_review_verdict("task-1", "feat/foo", "CHANGES_REQUESTED", "Fix tests", 1),
            Some(WorkflowAction::DispatchRework {
                task_id: "task-1".to_string(),
                feedback: "Fix tests".to_string(),
            })
        );

        assert_eq!(
            engine.on_review_verdict("task-1", "feat/foo", "APPROVED", "", 2),
            Some(WorkflowAction::CreatePr {
                task_id: "task-1".to_string(),
                branch_name: "feat/foo".to_string(),
            })
        );
    }

    #[test]
    fn test_max_review_cycles_forces_pr() {
        let engine = WorkflowEngine::new(WorkflowConfig {
            branch_ready_action: BranchReadyAction::AiReview,
            max_review_cycles: 2,
        });

        // At max cycles, should force PR
        let action =
            engine.on_review_verdict("task-1", "feat/foo", "CHANGES_REQUESTED", "Fix tests", 2);
        assert_eq!(
            action,
            Some(WorkflowAction::ForceCreatePr {
                task_id: "task-1".to_string(),
                branch_name: "feat/foo".to_string(),
                cycle_count: 2,
            })
        );
    }

    #[test]
    fn test_unknown_verdict_returns_none() {
        let engine = WorkflowEngine::new(WorkflowConfig::default());
        let action = engine.on_review_verdict("task-1", "feat/foo", "UNKNOWN", "", 1);
        assert!(action.is_none());
    }

    #[test]
    fn test_default_config() {
        let config = WorkflowConfig::default();
        assert_eq!(config.branch_ready_action, BranchReadyAction::DirectPr);
        assert_eq!(config.max_review_cycles, 3);
    }
}

//! Periodic reconciliation — checks active tasks against reality.
//!
//! Prevents tasks from getting permanently stuck by verifying actual
//! GitHub PR state and worker state at regular intervals.

use std::time::{Duration, Instant};

use color_eyre::Result;
use tracing::{debug, info};

use crate::buzz::task::{TaskStage, store::TaskStore};

/// Reconciliation engine that periodically checks task state against reality.
pub struct Reconciler {
    interval: Duration,
    last_run: Option<Instant>,
}

/// A single reconciliation action to take.
#[derive(Debug, Clone)]
pub struct ReconcileAction {
    pub task_id: String,
    pub task_title: String,
    pub action: ReconcileActionKind,
}

#[derive(Debug, Clone)]
pub enum ReconcileActionKind {
    /// PR was merged — transition task to Merged.
    PrMerged { pr_url: String },
    /// PR was closed without merge — transition task to Dismissed.
    PrClosed { pr_url: String },
    /// Worker closed without PR — task needs attention.
    WorkerGone { worker_id: String },
}

impl Reconciler {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            interval: Duration::from_secs(interval_secs),
            last_run: None,
        }
    }

    /// Returns true if enough time has elapsed since the last reconciliation.
    pub fn is_due(&self) -> bool {
        match self.last_run {
            None => true,
            Some(last) => last.elapsed() >= self.interval,
        }
    }

    /// Mark reconciliation as having just run.
    pub fn mark_run(&mut self) {
        self.last_run = Some(Instant::now());
    }

    /// Check active tasks and produce reconciliation actions.
    ///
    /// The caller is responsible for executing the actions (checking PR state
    /// via `gh pr view`, etc.) since that requires async I/O.
    pub fn check_tasks(&self, store: &TaskStore, workspace: &str) -> Result<Vec<ReconcileCheck>> {
        let tasks = store.get_active_tasks(workspace)?;
        let mut checks = Vec::new();

        for task in tasks {
            // Tasks with a PR URL: need to verify PR state
            if let Some(ref pr_url) = task.pr_url {
                if !task.stage.is_terminal() {
                    debug!(
                        "[reconcile] task '{}' has PR {} in stage {} — needs PR state check",
                        task.title,
                        pr_url,
                        task.stage.as_str()
                    );
                    checks.push(ReconcileCheck::VerifyPr {
                        task_id: task.id.clone(),
                        task_title: task.title.clone(),
                        pr_url: pr_url.clone(),
                        current_stage: task.stage.clone(),
                    });
                }
            }
            // Tasks with a worker but no PR: check if worker is still alive
            else if let Some(ref worker_id) = task.worker_id
                && matches!(task.stage, TaskStage::InProgress | TaskStage::InAiReview)
            {
                checks.push(ReconcileCheck::VerifyWorker {
                    task_id: task.id.clone(),
                    task_title: task.title.clone(),
                    worker_id: worker_id.clone(),
                    current_stage: task.stage.clone(),
                });
            }
        }

        Ok(checks)
    }

    /// Apply a reconciliation result to the task store.
    pub fn apply_action(store: &TaskStore, action: &ReconcileAction) -> Result<()> {
        match &action.action {
            ReconcileActionKind::PrMerged { pr_url } => {
                info!(
                    "[reconcile] PR {} merged — transitioning task '{}' to Merged",
                    pr_url, action.task_title
                );
                store.update_task_stage(&action.task_id, &TaskStage::Merged)?;
            }
            ReconcileActionKind::PrClosed { pr_url } => {
                info!(
                    "[reconcile] PR {} closed — transitioning task '{}' to Dismissed",
                    pr_url, action.task_title
                );
                store.update_task_stage(&action.task_id, &TaskStage::Dismissed)?;
            }
            ReconcileActionKind::WorkerGone { worker_id } => {
                info!(
                    "[reconcile] worker {} gone — transitioning task '{}' to Triage",
                    worker_id, action.task_title
                );
                store.update_task_stage(&action.task_id, &TaskStage::Triage)?;
            }
        }
        Ok(())
    }
}

/// A check that the reconciler wants the caller to perform.
#[derive(Debug, Clone)]
pub enum ReconcileCheck {
    /// Verify the state of a PR via `gh pr view`.
    VerifyPr {
        task_id: String,
        task_title: String,
        pr_url: String,
        current_stage: TaskStage,
    },
    /// Verify whether a worker is still alive.
    VerifyWorker {
        task_id: String,
        task_title: String,
        worker_id: String,
        current_stage: TaskStage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reconciler_is_due_initially() {
        let reconciler = Reconciler::new(60);
        assert!(reconciler.is_due());
    }

    #[test]
    fn test_reconciler_not_due_after_run() {
        let mut reconciler = Reconciler::new(60);
        reconciler.mark_run();
        assert!(!reconciler.is_due());
    }

    #[test]
    fn test_reconciler_due_after_interval() {
        let mut reconciler = Reconciler::new(0); // 0s interval
        reconciler.mark_run();
        // With 0s interval, it should be immediately due
        assert!(reconciler.is_due());
    }

    #[test]
    fn test_check_tasks_with_pr() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();
        let task = crate::buzz::task::Task {
            id: "task-1".to_string(),
            workspace: "test".to_string(),
            title: "Fix bug".to_string(),
            stage: TaskStage::HumanReview,
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
        };
        store.create_task(&task).unwrap();

        let reconciler = Reconciler::new(60);
        let checks = reconciler.check_tasks(&store, "test").unwrap();
        assert_eq!(checks.len(), 1);
        assert!(
            matches!(&checks[0], ReconcileCheck::VerifyPr { task_id, .. } if task_id == "task-1")
        );
    }

    #[test]
    fn test_check_tasks_with_worker_no_pr() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();
        let task = crate::buzz::task::Task {
            id: "task-2".to_string(),
            workspace: "test".to_string(),
            title: "Add feature".to_string(),
            stage: TaskStage::InProgress,
            source: None,
            source_url: None,
            worker_id: Some("worker-2".to_string()),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        store.create_task(&task).unwrap();

        let reconciler = Reconciler::new(60);
        let checks = reconciler.check_tasks(&store, "test").unwrap();
        assert_eq!(checks.len(), 1);
        assert!(
            matches!(&checks[0], ReconcileCheck::VerifyWorker { task_id, .. } if task_id == "task-2")
        );
    }

    #[test]
    fn test_apply_pr_merged() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();
        let task = crate::buzz::task::Task {
            id: "task-3".to_string(),
            workspace: "test".to_string(),
            title: "Merged task".to_string(),
            stage: TaskStage::HumanReview,
            source: None,
            source_url: None,
            worker_id: None,
            pr_url: Some("https://github.com/org/repo/pull/99".to_string()),
            pr_number: Some(99),
            repo: Some("org/repo".to_string()),
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        store.create_task(&task).unwrap();

        let action = ReconcileAction {
            task_id: "task-3".to_string(),
            task_title: "Merged task".to_string(),
            action: ReconcileActionKind::PrMerged {
                pr_url: "https://github.com/org/repo/pull/99".to_string(),
            },
        };
        Reconciler::apply_action(&store, &action).unwrap();

        let updated = store.get_task("task-3").unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::Merged);
    }

    #[test]
    fn test_terminal_tasks_not_checked() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();

        // Merged task with PR — should NOT produce a check
        let task = crate::buzz::task::Task {
            id: "task-done".to_string(),
            workspace: "test".to_string(),
            title: "Done".to_string(),
            stage: TaskStage::Merged,
            source: None,
            source_url: None,
            worker_id: None,
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            pr_number: Some(1),
            repo: Some("org/repo".to_string()),
            created_at: now,
            updated_at: now,
            resolved_at: Some(now),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        store.create_task(&task).unwrap();

        let reconciler = Reconciler::new(60);
        let checks = reconciler.check_tasks(&store, "test").unwrap();
        assert!(checks.is_empty());
    }
}

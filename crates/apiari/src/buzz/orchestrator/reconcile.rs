//! Periodic reconciliation — checks active tasks against reality.
//!
//! Prevents tasks from getting permanently stuck by verifying actual
//! GitHub PR state and worker state at regular intervals.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use chrono::Utc;
use color_eyre::eyre::{Result, WrapErr};
use tracing::{debug, info, warn};

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
    /// Task has been in the same stage for >24h — needs attention.
    Stale { stage: TaskStage, hours_stuck: i64 },
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

    /// Check active tasks and produce reconciliation checks.
    ///
    /// The caller is responsible for executing async checks (PR state via `gh pr view`).
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

            // Stale detection: task stuck in same stage for >24h
            let age = Utc::now() - task.updated_at;
            if age > chrono::Duration::hours(24) && !task.stage.is_terminal() {
                let hours = age.num_hours();
                checks.push(ReconcileCheck::Stale {
                    task_id: task.id.clone(),
                    task_title: task.title.clone(),
                    stage: task.stage.clone(),
                    hours_stuck: hours,
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
            ReconcileActionKind::Stale { stage, hours_stuck } => {
                info!(
                    "[reconcile] task '{}' stuck in {} for {}h — flagging for attention",
                    action.task_title,
                    stage.as_str(),
                    hours_stuck,
                );
                // Stale tasks get a Badge notification but no stage change
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
    /// Task has been stuck in the same stage for >24h.
    Stale {
        task_id: String,
        task_title: String,
        stage: TaskStage,
        hours_stuck: i64,
    },
}

/// PR state as reported by `gh pr view`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
    Unknown,
}

/// Check the actual state of a PR via `gh pr view`.
///
/// Returns the PR state. This is async because it shells out to `gh`.
pub async fn check_pr_state(pr_url: &str) -> Result<PrState> {
    let output = tokio::process::Command::new("gh")
        .args(["pr", "view", pr_url, "--json", "state,mergedAt"])
        .output()
        .await
        .wrap_err("failed to run gh pr view")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("[reconcile] gh pr view failed for {}: {}", pr_url, stderr);
        return Ok(PrState::Unknown);
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).wrap_err("failed to parse gh pr view output")?;

    let state = json.get("state").and_then(|s| s.as_str()).unwrap_or("");
    let merged_at = json.get("mergedAt").and_then(|s| s.as_str()).unwrap_or("");

    if !merged_at.is_empty() {
        Ok(PrState::Merged)
    } else if state == "CLOSED" {
        Ok(PrState::Closed)
    } else if state == "OPEN" {
        Ok(PrState::Open)
    } else {
        Ok(PrState::Unknown)
    }
}

/// Run a full reconciliation pass for a workspace.
///
/// This is the async entry point called by the daemon's background task.
/// It checks active tasks, verifies PR state, and applies transitions.
pub async fn run_reconciliation(db_path: &Path, workspace: &str) -> Result<Vec<ReconcileAction>> {
    let store = TaskStore::open(db_path)?;
    let reconciler = Reconciler::new(0); // interval not relevant here
    let checks = reconciler.check_tasks(&store, workspace)?;

    if checks.is_empty() {
        debug!("[reconcile] no active tasks to reconcile for workspace {workspace}");
        return Ok(Vec::new());
    }

    info!(
        "[reconcile] running {} checks for workspace {workspace}",
        checks.len()
    );

    let mut actions = Vec::new();

    for check in checks {
        match check {
            ReconcileCheck::VerifyPr {
                task_id,
                task_title,
                pr_url,
                ..
            } => match check_pr_state(&pr_url).await? {
                PrState::Merged => {
                    let action = ReconcileAction {
                        task_id,
                        task_title,
                        action: ReconcileActionKind::PrMerged {
                            pr_url: pr_url.clone(),
                        },
                    };
                    Reconciler::apply_action(&store, &action)?;
                    actions.push(action);
                }
                PrState::Closed => {
                    let action = ReconcileAction {
                        task_id,
                        task_title,
                        action: ReconcileActionKind::PrClosed {
                            pr_url: pr_url.clone(),
                        },
                    };
                    Reconciler::apply_action(&store, &action)?;
                    actions.push(action);
                }
                PrState::Open | PrState::Unknown => {
                    // No change needed
                }
            },
            ReconcileCheck::VerifyWorker {
                task_id,
                task_title,
                worker_id,
                ..
            } => {
                // Check if worker still exists by looking at swarm state file
                // For now, just log — full worker liveness check requires swarm state access
                debug!(
                    "[reconcile] worker {} for task '{}' — would check liveness",
                    worker_id, task_title
                );
                // Future: read .swarm/state.json and check if worker is still listed
                let _ = (task_id, worker_id); // suppress unused warnings
            }
            ReconcileCheck::Stale {
                task_id,
                task_title,
                stage,
                hours_stuck,
            } => {
                let action = ReconcileAction {
                    task_id,
                    task_title,
                    action: ReconcileActionKind::Stale { stage, hours_stuck },
                };
                // Don't apply stage change for stale — just report
                info!(
                    "[reconcile] {}",
                    format!("task '{}' stuck for {}h", action.task_title, hours_stuck)
                );
                actions.push(action);
            }
        }
    }

    Ok(actions)
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
        assert!(
            checks.iter().any(
                |c| matches!(c, ReconcileCheck::VerifyPr { task_id, .. } if task_id == "task-1")
            )
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
        assert!(checks.iter().any(
            |c| matches!(c, ReconcileCheck::VerifyWorker { task_id, .. } if task_id == "task-2")
        ));
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
    fn test_apply_pr_closed() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();
        let task = crate::buzz::task::Task {
            id: "task-4".to_string(),
            workspace: "test".to_string(),
            title: "Closed task".to_string(),
            stage: TaskStage::HumanReview,
            source: None,
            source_url: None,
            worker_id: None,
            pr_url: Some("https://github.com/org/repo/pull/50".to_string()),
            pr_number: Some(50),
            repo: Some("org/repo".to_string()),
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        store.create_task(&task).unwrap();

        let action = ReconcileAction {
            task_id: "task-4".to_string(),
            task_title: "Closed task".to_string(),
            action: ReconcileActionKind::PrClosed {
                pr_url: "https://github.com/org/repo/pull/50".to_string(),
            },
        };
        Reconciler::apply_action(&store, &action).unwrap();

        let updated = store.get_task("task-4").unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::Dismissed);
    }

    #[test]
    fn test_terminal_tasks_not_checked() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();

        // Merged task with PR — should NOT produce a VerifyPr check
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

    #[test]
    fn test_stale_task_detection() {
        let store = TaskStore::open_memory().unwrap();
        let now = chrono::Utc::now();

        let task = crate::buzz::task::Task {
            id: "task-stale".to_string(),
            workspace: "test".to_string(),
            title: "Stuck task".to_string(),
            stage: TaskStage::InProgress,
            source: None,
            source_url: None,
            worker_id: Some("worker-1".to_string()),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: now - chrono::Duration::hours(30),
            updated_at: now - chrono::Duration::hours(25),
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        store.create_task(&task).unwrap();

        let reconciler = Reconciler::new(60);
        let checks = reconciler.check_tasks(&store, "test").unwrap();
        let stale = checks.iter().any(|c| {
            matches!(c, ReconcileCheck::Stale { task_id, hours_stuck, .. } if task_id == "task-stale" && *hours_stuck >= 24)
        });
        assert!(stale, "should detect stale task stuck >24h");
    }
}

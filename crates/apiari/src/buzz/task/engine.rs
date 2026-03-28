//! Task engine — processes signals through the transition rules and applies changes.
//!
//! This is what the daemon calls after upserting a new signal. It matches the
//! signal to a task (by PR number/repo), evaluates the rules, and applies any
//! `Auto` transitions. Non-auto transitions are skipped until future phases.

use color_eyre::Result;
use tracing::info;

use super::rules::{self, Approval, TransitionAction};
use super::{Task, store::TaskStore};
use crate::buzz::signal::SignalRecord;

/// Result of processing a signal through the task engine.
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
}

/// Process a signal through the task engine.
///
/// 1. Try to match the signal to an existing task (by PR number/repo).
/// 2. If matched, evaluate transition rules.
/// 3. Apply any Auto transitions.
/// 4. Return the result with side effects for the daemon to execute.
pub fn process_signal(
    store: &TaskStore,
    workspace: &str,
    signal: &SignalRecord,
) -> Result<EngineResult> {
    let mut result = EngineResult {
        task: None,
        worker_messages: Vec::new(),
        notifications: Vec::new(),
        transitioned: false,
    };

    // Step 1: Match signal to task
    let task = find_task_for_signal(store, workspace, signal)?;
    let task = match task {
        Some(t) => t,
        None => return Ok(result), // No matching task — signal is standalone
    };

    info!(
        "[task-engine] matched signal '{}' (source={}) to task '{}' (stage={})",
        signal.title,
        signal.source,
        task.title,
        task.stage.as_str()
    );

    // Step 2: Evaluate rules
    let proposed = rules::evaluate_signal(&task, signal);
    let proposed = match proposed {
        Some(p) => p,
        None => {
            result.task = Some(task);
            return Ok(result); // No rule matched
        }
    };

    // Step 3: Only apply Auto transitions for now
    if proposed.approval != Approval::Auto {
        info!(
            "[task-engine] transition {}→{} requires {:?} approval, skipping",
            proposed.from.as_str(),
            proposed.to.as_str(),
            proposed.approval,
        );
        result.task = Some(task);
        return Ok(result);
    }

    // Step 4: Apply transition (if stage actually changes)
    if proposed.from != proposed.to {
        store.transition_task(
            &task.id,
            &proposed.from,
            &proposed.to,
            Some(proposed.reason.clone()),
        )?;
        info!(
            "[task-engine] transitioned task '{}': {} → {}  (reason: {})",
            task.title,
            proposed.from.as_str(),
            proposed.to.as_str(),
            proposed.reason,
        );
        result.transitioned = true;
    }

    // Step 5: Collect side effects
    match proposed.action {
        TransitionAction::ForwardToWorker { message } => {
            if let Some(ref worker_id) = task.worker_id {
                result.worker_messages.push((worker_id.clone(), message));
            }
        }
        TransitionAction::Notify { message } => {
            result.notifications.push(message);
        }
        TransitionAction::None => {}
    }

    // Reload task after transition
    result.task = store.get_task(&task.id)?;

    Ok(result)
}

/// Find a task that matches this signal.
/// Tries PR matching first.
fn find_task_for_signal(
    store: &TaskStore,
    workspace: &str,
    signal: &SignalRecord,
) -> Result<Option<Task>> {
    // Try matching by PR
    if let Some((repo, pr_number)) = rules::match_signal_to_task_pr(signal)
        && let Some(task) = store.find_task_by_pr(workspace, &repo, pr_number)?
    {
        return Ok(Some(task));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::{Severity, SignalRecord, SignalStatus};
    use crate::buzz::task::{Task, TaskStage, store::TaskStore};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_task(workspace: &str, stage: TaskStage, repo: &str, pr_number: i64) -> Task {
        let now = Utc::now();
        Task {
            id: Uuid::new_v4().to_string(),
            workspace: workspace.to_string(),
            title: "Test task".to_string(),
            stage,
            source: None,
            source_url: None,
            worker_id: Some("worker-1".to_string()),
            pr_url: Some(format!("https://github.com/{repo}/pull/{pr_number}")),
            pr_number: Some(pr_number),
            repo: Some(repo.to_string()),
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    fn make_signal(source: &str, url: Option<&str>) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.to_string(),
            external_id: "ext-1".to_string(),
            title: format!("Signal from {source}"),
            body: None,
            severity: Severity::Info,
            status: SignalStatus::Open,
            url: url.map(String::from),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
            snoozed_until: None,
        }
    }

    #[test]
    fn test_signal_with_no_matching_task_returns_empty_result() {
        let store = TaskStore::open_memory().unwrap();
        let signal = make_signal(
            "github_ci_failure",
            Some("https://github.com/org/repo/pull/42"),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();
        assert!(result.task.is_none());
        assert!(!result.transitioned);
        assert!(result.worker_messages.is_empty());
        assert!(result.notifications.is_empty());
    }

    #[test]
    fn test_signal_matching_task_but_no_rule_returns_task_unchanged() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", TaskStage::Triage, "org/repo", 42);
        store.create_task(&task).unwrap();

        // sentry signal doesn't match any rule
        let mut signal = make_signal("sentry", None);
        signal.metadata = Some(r#"{"repo": "org/repo", "pr_number": 42}"#.to_string());
        let result = process_signal(&store, "acme", &signal).unwrap();
        assert!(result.task.is_some()); // task was found
        assert!(!result.transitioned);
    }

    #[test]
    fn test_ci_pass_transitions_in_ai_review_to_merge_ready() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", TaskStage::InAiReview, "org/repo", 42);
        store.create_task(&task).unwrap();

        let signal = make_signal(
            "github_ci_pass",
            Some("https://github.com/org/repo/pull/42"),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();

        assert!(result.transitioned);
        let updated = result.task.unwrap();
        assert_eq!(updated.stage, TaskStage::MergeReady);
        assert!(!result.notifications.is_empty());
        assert!(result.worker_messages.is_empty());
    }

    #[test]
    fn test_ci_failure_forwards_to_worker() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", TaskStage::InAiReview, "org/repo", 55);
        store.create_task(&task).unwrap();

        let signal = make_signal(
            "github_ci_failure",
            Some("https://github.com/org/repo/pull/55"),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();

        assert!(result.transitioned);
        assert_eq!(result.worker_messages.len(), 1);
        assert_eq!(result.worker_messages[0].0, "worker-1");
        assert!(result.notifications.is_empty());
    }

    #[test]
    fn test_merged_pr_transitions_any_active_task_to_merged() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("acme", TaskStage::MergeReady, "org/repo", 10);
        store.create_task(&task).unwrap();

        let signal = make_signal(
            "github_merged_pr",
            Some("https://github.com/org/repo/pull/10"),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();

        assert!(result.transitioned);
        let updated = result.task.unwrap();
        assert_eq!(updated.stage, TaskStage::Merged);
        assert!(updated.resolved_at.is_some());
    }

    #[test]
    fn test_workspace_isolation() {
        let store = TaskStore::open_memory().unwrap();
        let task = make_task("workspace-a", TaskStage::InAiReview, "org/repo", 42);
        store.create_task(&task).unwrap();

        let signal = make_signal(
            "github_ci_pass",
            Some("https://github.com/org/repo/pull/42"),
        );
        // Process for wrong workspace — should find no task
        let result = process_signal(&store, "workspace-b", &signal).unwrap();
        assert!(!result.transitioned);
        assert!(result.task.is_none());
    }

    #[test]
    fn test_no_worker_id_skips_forward_to_worker() {
        let store = TaskStore::open_memory().unwrap();
        let mut task = make_task("acme", TaskStage::InAiReview, "org/repo", 7);
        task.worker_id = None; // no worker assigned
        store.create_task(&task).unwrap();

        let signal = make_signal(
            "github_ci_failure",
            Some("https://github.com/org/repo/pull/7"),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();
        assert!(result.transitioned);
        assert!(result.worker_messages.is_empty()); // no worker to forward to
    }
}

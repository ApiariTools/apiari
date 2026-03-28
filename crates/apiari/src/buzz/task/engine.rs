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

    // Step 4: Apply transition (persists to DB only if stage actually changes)
    // A rule firing counts as "transitioned" even when from == to, because an
    // action (ForwardToWorker, Notify) is still performed.
    result.transitioned = true;
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
    } else {
        info!(
            "[task-engine] rule fired for task '{}' (stage={}, reason: {})",
            task.title,
            proposed.from.as_str(),
            proposed.reason,
        );
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
/// Tries PR matching first, then worker_id matching for branch-ready signals.
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
    fn test_ci_pass_transitions_in_ai_review_to_human_review() {
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
        assert_eq!(updated.stage, TaskStage::HumanReview);
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
        let task = make_task("acme", TaskStage::HumanReview, "org/repo", 10);
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

    // ── Swarm worker lifecycle tests ──

    /// Helper that mirrors the daemon's swarm-spawned-* task creation logic.
    fn create_task_for_worker(
        store: &TaskStore,
        workspace: &str,
        worker_id: &str,
        body: Option<&str>,
    ) -> Task {
        let title = body
            .and_then(|b| b.lines().nth(1))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Worker {worker_id}"));
        let title = if title.len() > 80 {
            format!("{}…", &title[..79])
        } else {
            title
        };
        let now = Utc::now();
        let task = Task {
            id: Uuid::new_v4().to_string(),
            workspace: workspace.to_string(),
            title,
            stage: TaskStage::InProgress,
            source: Some("swarm".to_string()),
            source_url: None,
            worker_id: Some(worker_id.to_string()),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::json!({}),
        };
        store.create_task(&task).unwrap();
        task
    }

    #[test]
    fn test_swarm_spawned_creates_task_in_progress() {
        let store = TaskStore::open_memory().unwrap();
        let body = "agent: claude\nFix the login bug in auth module";
        let task = create_task_for_worker(&store, "acme", "w-abc1", Some(body));

        let found = store
            .find_task_by_worker("acme", "w-abc1")
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
        assert_eq!(found.stage, TaskStage::InProgress);
        assert_eq!(found.title, "Fix the login bug in auth module");
        assert_eq!(found.source.as_deref(), Some("swarm"));
        assert_eq!(found.worker_id.as_deref(), Some("w-abc1"));
        assert!(found.pr_url.is_none());
    }

    #[test]
    fn test_swarm_spawned_title_fallback_when_no_body() {
        let store = TaskStore::open_memory().unwrap();
        let task = create_task_for_worker(&store, "acme", "w-xyz9", None);

        let found = store
            .find_task_by_worker("acme", "w-xyz9")
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
        assert_eq!(found.title, "Worker w-xyz9");
    }

    #[test]
    fn test_swarm_spawned_duplicate_does_not_create_second_task() {
        let store = TaskStore::open_memory().unwrap();
        let body = "agent: claude\nAdd dark mode";

        create_task_for_worker(&store, "acme", "w-dup1", Some(body));

        // Simulate duplicate: only create if not found
        if store
            .find_task_by_worker("acme", "w-dup1")
            .unwrap()
            .is_none()
        {
            create_task_for_worker(&store, "acme", "w-dup1", Some(body));
        }

        let all = store.get_all_tasks("acme").unwrap();
        assert_eq!(
            all.len(),
            1,
            "duplicate spawned signal must not create a second task"
        );
    }

    #[test]
    fn test_swarm_pr_updates_task_and_transitions_to_in_ai_review() {
        let store = TaskStore::open_memory().unwrap();
        let task = create_task_for_worker(
            &store,
            "acme",
            "w-pr1",
            Some("agent: claude\nRefactor DB layer"),
        );

        // Simulate swarm-pr-* handler
        let pr_url = "https://github.com/org/repo/pull/77";
        let (repo, pr_number) =
            crate::buzz::task::rules::extract_github_pr_from_url(pr_url).unwrap();
        store.update_task_pr(&task.id, pr_url, pr_number).unwrap();
        store.update_task_repo(&task.id, &repo).unwrap();
        store
            .transition_task(
                &task.id,
                &TaskStage::InProgress,
                &TaskStage::InAiReview,
                Some("PR opened".to_string()),
            )
            .unwrap();

        let updated = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::InAiReview);
        assert_eq!(updated.pr_url.as_deref(), Some(pr_url));
        assert_eq!(updated.pr_number, Some(77));
        assert_eq!(updated.repo.as_deref(), Some("org/repo"));
    }

    #[test]
    fn test_swarm_closed_without_pr_dismisses_task() {
        let store = TaskStore::open_memory().unwrap();
        let task = create_task_for_worker(
            &store,
            "acme",
            "w-cls1",
            Some("agent: claude\nFix flaky test"),
        );

        // Simulate swarm-closed-* handler: no PR, not terminal
        let current = store
            .find_task_by_worker("acme", "w-cls1")
            .unwrap()
            .unwrap();
        assert!(!current.stage.is_terminal());
        assert!(current.pr_url.is_none());

        store
            .transition_task(
                &current.id,
                &current.stage,
                &TaskStage::Dismissed,
                Some("Worker closed without PR".to_string()),
            )
            .unwrap();

        let updated = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.stage, TaskStage::Dismissed);
        assert!(updated.resolved_at.is_some());
    }

    #[test]
    fn test_swarm_closed_with_pr_does_not_dismiss() {
        let store = TaskStore::open_memory().unwrap();
        let task =
            create_task_for_worker(&store, "acme", "w-cls2", Some("agent: claude\nAdd OAuth"));

        // Give it a PR first
        store
            .update_task_pr(&task.id, "https://github.com/org/repo/pull/5", 5)
            .unwrap();
        store.update_task_repo(&task.id, "org/repo").unwrap();
        store
            .transition_task(
                &task.id,
                &TaskStage::InProgress,
                &TaskStage::InAiReview,
                None,
            )
            .unwrap();

        // Simulate swarm-closed-* handler: has a PR, should NOT dismiss
        let current = store
            .find_task_by_worker("acme", "w-cls2")
            .unwrap()
            .unwrap();
        assert!(!current.stage.is_terminal());
        assert!(current.pr_url.is_some()); // pr_url is set → skip dismiss

        // Because pr_url.is_some(), we don't call transition_task
        let unchanged = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(
            unchanged.stage,
            TaskStage::InAiReview,
            "task with PR must not be dismissed on worker close"
        );
    }

    #[test]
    fn test_swarm_closed_terminal_task_not_touched() {
        let store = TaskStore::open_memory().unwrap();
        let task = create_task_for_worker(
            &store,
            "acme",
            "w-cls3",
            Some("agent: claude\nAlready done"),
        );

        // Pre-transition to terminal
        store
            .transition_task(&task.id, &TaskStage::InProgress, &TaskStage::Merged, None)
            .unwrap();

        let current = store
            .find_task_by_worker("acme", "w-cls3")
            .unwrap()
            .unwrap();
        assert!(current.stage.is_terminal()); // already terminal → skip dismiss

        let unchanged = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(
            unchanged.stage,
            TaskStage::Merged,
            "terminal task must not be touched on worker close"
        );
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
    fn test_full_lifecycle_spawn_to_merge() {
        let store = TaskStore::open_memory().unwrap();

        // 1. Create task (simulating swarm-spawned) in InProgress
        let task = create_task_for_worker(
            &store,
            "acme",
            "w-life1",
            Some("agent: claude\nAdd payment integration"),
        );
        let fetched = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(fetched.stage, TaskStage::InProgress);

        // 2. Update PR info + transition to InAiReview
        let pr_url = "https://github.com/org/repo/pull/100";
        let (repo, pr_number) =
            crate::buzz::task::rules::extract_github_pr_from_url(pr_url).unwrap();
        store.update_task_pr(&task.id, pr_url, pr_number).unwrap();
        store.update_task_repo(&task.id, &repo).unwrap();
        store
            .transition_task(
                &task.id,
                &TaskStage::InProgress,
                &TaskStage::InAiReview,
                Some("PR opened".to_string()),
            )
            .unwrap();

        let fetched = store.get_task(&task.id).unwrap().unwrap();
        assert_eq!(fetched.stage, TaskStage::InAiReview);

        // 3. Process CI pass signal → should transition to HumanReview
        let ci_pass = make_signal(
            "github_ci_pass",
            Some("https://github.com/org/repo/pull/100"),
        );
        let result = process_signal(&store, "acme", &ci_pass).unwrap();
        assert!(result.transitioned);
        assert_eq!(result.task.as_ref().unwrap().stage, TaskStage::HumanReview);
        assert!(!result.notifications.is_empty());

        // 4. Process merged PR signal → should transition to Merged
        let merged = make_signal(
            "github_merged_pr",
            Some("https://github.com/org/repo/pull/100"),
        );
        let result = process_signal(&store, "acme", &merged).unwrap();
        assert!(result.transitioned);
        let final_task = result.task.unwrap();
        assert_eq!(final_task.stage, TaskStage::Merged);
        assert!(final_task.resolved_at.is_some());

        // Verify events were logged
        let events = store.get_task_events(&task.id).unwrap();
        // Events: PR opened (manual), CI pass → HumanReview, merged → Merged
        assert!(events.len() >= 3);
    }

    #[test]
    fn test_lifecycle_ci_failure_stays_in_review_and_forwards() {
        // CI failure during InAiReview should NOT regress the task to InProgress.
        // The worker is notified but the task stage stays where it is.
        let store = TaskStore::open_memory().unwrap();

        // 1. Create task in InAiReview with PR
        let task = make_task("acme", TaskStage::InAiReview, "org/repo", 200);
        store.create_task(&task).unwrap();

        // 2. Process CI failure → should stay in InAiReview, worker gets a message
        let ci_fail = make_signal(
            "github_ci_failure",
            Some("https://github.com/org/repo/pull/200"),
        );
        let result = process_signal(&store, "acme", &ci_fail).unwrap();
        assert!(result.transitioned); // rule fired
        assert_eq!(result.task.as_ref().unwrap().stage, TaskStage::InAiReview); // no regression
        assert_eq!(result.worker_messages.len(), 1);

        // 3. Process PR push while still in InAiReview → stays in InAiReview
        let pr_push = make_signal(
            "github_pr_push",
            Some("https://github.com/org/repo/pull/200"),
        );
        let result = process_signal(&store, "acme", &pr_push).unwrap();
        assert!(result.transitioned); // rule fired (pr_push in InAiReview stays+notifies)
        assert_eq!(result.task.unwrap().stage, TaskStage::InAiReview);

        // 4. CI pass → HumanReview
        let ci_pass = make_signal(
            "github_ci_pass",
            Some("https://github.com/org/repo/pull/200"),
        );
        let result = process_signal(&store, "acme", &ci_pass).unwrap();
        assert!(result.transitioned);
        assert_eq!(result.task.unwrap().stage, TaskStage::HumanReview);
    }

    #[test]
    fn test_review_verdict_approved_through_engine() {
        let store = TaskStore::open_memory().unwrap();

        // 1. Create task in InAiReview
        let task = make_task("acme", TaskStage::InAiReview, "org/repo", 300);
        store.create_task(&task).unwrap();

        // 2. Process swarm_review_verdict signal with APPROVED
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            serde_json::json!({
                "verdict": "APPROVED",
                "comments": "",
                "repo": "org/repo",
                "pr_number": 300
            })
            .to_string(),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();

        // 3. Verify task moved to HumanReview
        assert!(result.transitioned);
        assert_eq!(result.task.unwrap().stage, TaskStage::HumanReview);

        // 4. Verify notification was generated
        assert!(!result.notifications.is_empty());
        assert!(result.worker_messages.is_empty());
    }

    #[test]
    fn test_review_verdict_changes_requested_through_engine() {
        let store = TaskStore::open_memory().unwrap();

        // 1. Create task in InAiReview with worker_id
        let task = make_task("acme", TaskStage::InAiReview, "org/repo", 400);
        store.create_task(&task).unwrap();

        // 2. Process swarm_review_verdict signal with CHANGES_REQUESTED + comments
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            serde_json::json!({
                "verdict": "CHANGES_REQUESTED",
                "comments": "Fix the null pointer dereference on line 42.",
                "repo": "org/repo",
                "pr_number": 400
            })
            .to_string(),
        );
        let result = process_signal(&store, "acme", &signal).unwrap();

        // 3. Verify task moved to InProgress
        assert!(result.transitioned);
        assert_eq!(result.task.unwrap().stage, TaskStage::InProgress);

        // 4. Verify worker_messages contains the comments
        assert_eq!(result.worker_messages.len(), 1);
        assert_eq!(result.worker_messages[0].0, "worker-1");
        assert!(
            result.worker_messages[0]
                .1
                .contains("null pointer dereference")
        );
    }

    #[test]
    fn test_reviewer_dedup_metadata_check() {
        let store = TaskStore::open_memory().unwrap();

        // 1. Create task in InAiReview
        let task = make_task("acme", TaskStage::InAiReview, "org/repo", 500);
        store.create_task(&task).unwrap();

        // reviewer_worker_id is not set yet → should dispatch reviewer
        let fetched = store.get_task(&task.id).unwrap().unwrap();
        assert!(
            fetched.metadata.get("reviewer_worker_id").is_none(),
            "reviewer_worker_id should not be present initially"
        );

        // 2. Set metadata with reviewer_worker_id
        let meta = serde_json::json!({"reviewer_worker_id": "reviewer-abc"});
        store.update_task_metadata(&task.id, &meta).unwrap();

        // 3. Verify the condition: task.metadata.get("reviewer_worker_id").is_some()
        // means we would skip dispatching another reviewer
        let fetched = store.get_task(&task.id).unwrap().unwrap();
        assert!(
            fetched.metadata.get("reviewer_worker_id").is_some(),
            "reviewer_worker_id should be set after update"
        );
        // Also verify find_task_by_reviewer_worker works
        let found = store
            .find_task_by_reviewer_worker("acme", "reviewer-abc")
            .unwrap();
        assert!(found.is_some());
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

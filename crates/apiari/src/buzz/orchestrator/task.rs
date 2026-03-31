use std::path::{Path, PathBuf};

use chrono::Utc;
use color_eyre::eyre::Result;

use crate::buzz::{
    signal::{Severity, SignalRecord, SignalUpdate, store::SignalStore},
    task::{ActivityEventStore, Task, TaskStage, engine, store::TaskStore},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEffect {
    SendWorkerMessage {
        worker_id: String,
        message: String,
    },
    CloseWorker {
        worker_id: String,
    },
    SpawnReviewerForPr {
        task_id: String,
        repo: String,
        pr_number: i64,
        task_title: String,
    },
    SpawnReviewerForBranch {
        task_id: String,
        repo: String,
        branch_name: String,
        task_title: String,
    },
    Notify(String),
}

#[derive(Debug, Default)]
pub struct TaskOutcome {
    pub effects: Vec<TaskEffect>,
}

pub fn process_signal(
    db_path: &Path,
    workspace: &str,
    root: &Path,
    signal: &SignalRecord,
) -> Result<TaskOutcome> {
    let task_store = TaskStore::open(db_path)?;
    let signal_store = SignalStore::open(db_path, workspace)?;
    let activity = ActivityEventStore::open(db_path)?;
    let mut outcome = TaskOutcome::default();

    apply_engine_signal(&task_store, &activity, workspace, signal, &mut outcome)?;
    handle_swarm_spawned(&task_store, &activity, workspace, signal)?;
    handle_swarm_pr_opened(&task_store, &activity, workspace, signal, &mut outcome)?;
    handle_swarm_branch_ready(&task_store, &activity, workspace, signal, &mut outcome)?;
    handle_swarm_completed(
        &task_store,
        &signal_store,
        &activity,
        workspace,
        root,
        signal,
        &mut outcome,
    )?;
    handle_swarm_closed(&task_store, &activity, workspace, signal)?;

    Ok(outcome)
}

pub fn record_reviewer_assignment(
    db_path: &Path,
    workspace: &str,
    task_id: &str,
    reviewer_worker_id: &str,
    summary: &str,
    metadata: serde_json::Value,
) -> Result<()> {
    let task_store = TaskStore::open(db_path)?;
    let activity = ActivityEventStore::open(db_path)?;
    if let Some(mut task) = task_store.get_task(task_id)? {
        task.metadata["reviewer_worker_id"] =
            serde_json::Value::String(reviewer_worker_id.to_string());
        task_store.update_task_metadata(task_id, &task.metadata)?;
        activity.log_event(
            workspace,
            Some(task_id),
            "worker",
            summary,
            None,
            Some("swarm"),
            None,
            Some(&metadata.to_string()),
        )?;
    }
    Ok(())
}

fn apply_engine_signal(
    task_store: &TaskStore,
    activity: &ActivityEventStore,
    workspace: &str,
    signal: &SignalRecord,
    outcome: &mut TaskOutcome,
) -> Result<()> {
    let engine_result = engine::process_signal(task_store, workspace, signal)?;
    for (worker_id, message) in engine_result.worker_messages {
        outcome
            .effects
            .push(TaskEffect::SendWorkerMessage { worker_id, message });
    }
    for notification in engine_result.notifications {
        outcome.effects.push(TaskEffect::Notify(notification));
    }
    if let Some(ref task) = engine_result.task {
        activity.log_event(
            workspace,
            Some(&task.id),
            "signal",
            &format!("Signal: {}", signal.title),
            signal.body.as_deref(),
            Some(&signal.source),
            Some(signal.id),
            None,
        )?;
        if engine_result.transitioned
            && let Some(ref from) = engine_result.from_stage
        {
            let to = &task.stage;
            if from != to {
                let meta = serde_json::json!({
                    "from": from.as_str(),
                    "to": to.as_str(),
                    "reason": signal.source,
                });
                activity.log_event(
                    workspace,
                    Some(&task.id),
                    "stage_change",
                    &format!("{} → {}", from.as_str(), to.as_str()),
                    None,
                    Some(&signal.source),
                    Some(signal.id),
                    Some(&meta.to_string()),
                )?;
            }
        }
    }
    Ok(())
}

fn handle_swarm_spawned(
    task_store: &TaskStore,
    activity: &ActivityEventStore,
    workspace: &str,
    signal: &SignalRecord,
) -> Result<()> {
    if signal.source != "swarm_worker_spawned" || !signal.external_id.starts_with("swarm-spawned-")
    {
        return Ok(());
    }

    let worker_id = signal
        .external_id
        .strip_prefix("swarm-spawned-")
        .unwrap_or_default()
        .to_string();
    let is_reviewer = signal
        .body
        .as_ref()
        .and_then(|b| b.lines().nth(1))
        .map(|l| l.trim_start().starts_with("Review PR"))
        .unwrap_or(false);
    if worker_id.is_empty()
        || is_reviewer
        || task_store
            .find_task_by_worker(workspace, &worker_id)?
            .is_some()
    {
        return Ok(());
    }

    let title = signal
        .body
        .as_ref()
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
        id: uuid::Uuid::new_v4().to_string(),
        workspace: workspace.to_string(),
        title,
        stage: TaskStage::InProgress,
        source: Some("swarm_worker_spawned".to_string()),
        source_url: None,
        worker_id: Some(worker_id.clone()),
        pr_url: None,
        pr_number: None,
        repo: None,
        created_at: now,
        updated_at: now,
        resolved_at: None,
        metadata: serde_json::json!({}),
    };
    task_store.create_task(&task)?;
    let meta = serde_json::json!({
        "from": serde_json::Value::Null,
        "to": "In Progress",
        "worker_id": worker_id,
    });
    activity.log_event(
        workspace,
        Some(&task.id),
        "stage_change",
        &format!("Task created: {}", task.title),
        None,
        Some("swarm_worker_spawned"),
        None,
        Some(&meta.to_string()),
    )?;
    activity.log_event(
        workspace,
        Some(&task.id),
        "worker",
        &format!("Worker {} spawned", worker_id),
        None,
        Some("swarm_worker_spawned"),
        None,
        Some(&serde_json::json!({"worker_id": worker_id}).to_string()),
    )?;
    Ok(())
}

fn handle_swarm_pr_opened(
    task_store: &TaskStore,
    activity: &ActivityEventStore,
    workspace: &str,
    signal: &SignalRecord,
    outcome: &mut TaskOutcome,
) -> Result<()> {
    if signal.source != "swarm_pr_opened" || !signal.external_id.starts_with("swarm-pr-") {
        return Ok(());
    }
    let worker_id = signal
        .external_id
        .strip_prefix("swarm-pr-")
        .unwrap_or_default();
    if worker_id.is_empty() {
        return Ok(());
    }
    let Some(task) = task_store.find_task_by_worker(workspace, worker_id)? else {
        return Ok(());
    };

    if let Some(ref url) = signal.url
        && let Some((repo, pr_number)) = crate::buzz::task::rules::extract_github_pr_from_url(url)
    {
        task_store.update_task_pr(&task.id, url, pr_number)?;
        task_store.update_task_repo(&task.id, &repo)?;
        if task.stage == TaskStage::InProgress {
            task_store.transition_task(
                &task.id,
                &TaskStage::InProgress,
                &TaskStage::InAiReview,
                Some("PR opened".to_string()),
            )?;
            let meta = serde_json::json!({
                "from": "In Progress",
                "to": "In AI Review",
                "reason": "PR opened",
                "pr_url": url,
            });
            activity.log_event(
                workspace,
                Some(&task.id),
                "pr",
                &format!("PR opened: {url}"),
                None,
                Some("swarm_pr_opened"),
                None,
                Some(&serde_json::json!({"pr_url": url}).to_string()),
            )?;
            activity.log_event(
                workspace,
                Some(&task.id),
                "stage_change",
                "In Progress → In AI Review",
                None,
                Some("swarm_pr_opened"),
                None,
                Some(&meta.to_string()),
            )?;
        }
        let refreshed = task_store.get_task(&task.id)?.unwrap_or(task.clone());
        if refreshed.stage == TaskStage::InAiReview
            && refreshed.metadata.get("reviewer_worker_id").is_none()
        {
            let short_repo = repo
                .split('/')
                .next_back()
                .unwrap_or(repo.as_str())
                .to_string();
            outcome.effects.push(TaskEffect::SpawnReviewerForPr {
                task_id: refreshed.id.clone(),
                repo: short_repo,
                pr_number,
                task_title: refreshed.title.clone(),
            });
        }
    }

    outcome.effects.push(TaskEffect::CloseWorker {
        worker_id: worker_id.to_string(),
    });
    Ok(())
}

fn handle_swarm_branch_ready(
    task_store: &TaskStore,
    _activity: &ActivityEventStore,
    workspace: &str,
    signal: &SignalRecord,
    outcome: &mut TaskOutcome,
) -> Result<()> {
    if signal.source != "swarm_branch_ready"
        || !signal.external_id.starts_with("swarm-branch-ready-")
    {
        return Ok(());
    }
    let worker_id = signal
        .external_id
        .strip_prefix("swarm-branch-ready-")
        .unwrap_or_default();
    if worker_id.is_empty() {
        return Ok(());
    }
    let Some(task) = task_store.find_task_by_worker(workspace, worker_id)? else {
        return Ok(());
    };
    if task.stage != TaskStage::InAiReview || task.metadata.get("reviewer_worker_id").is_some() {
        return Ok(());
    }
    let meta = signal
        .metadata
        .as_ref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let branch_name = meta
        .get("branch_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if branch_name.is_empty() {
        return Ok(());
    }
    let repo = meta
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let mut task_meta = task.metadata.clone();
    task_meta["ready_branch"] = serde_json::Value::String(branch_name.clone());
    task_store.update_task_metadata(&task.id, &task_meta)?;
    outcome.effects.push(TaskEffect::SpawnReviewerForBranch {
        task_id: task.id.clone(),
        repo,
        branch_name,
        task_title: task.title.clone(),
    });
    Ok(())
}

fn handle_swarm_completed(
    task_store: &TaskStore,
    signal_store: &SignalStore,
    activity: &ActivityEventStore,
    workspace: &str,
    root: &Path,
    signal: &SignalRecord,
    outcome: &mut TaskOutcome,
) -> Result<()> {
    if signal.source != "swarm_worker_completed"
        || !signal.external_id.starts_with("swarm-completed-")
    {
        return Ok(());
    }
    let worker_id = signal
        .external_id
        .strip_prefix("swarm-completed-")
        .unwrap_or_default()
        .to_string();
    if worker_id.is_empty() {
        return Ok(());
    }
    let Some(task) = task_store.find_task_by_reviewer_worker(workspace, &worker_id)? else {
        return Ok(());
    };
    let Some((verdict, comments)) = read_review_verdict(root, &worker_id)? else {
        return Ok(());
    };

    let is_branch_flow = task.pr_number.is_none();
    let (metadata, signal_title) =
        if let (Some(pr_number), Some(repo)) = (task.pr_number, task.repo.as_deref()) {
            (
                serde_json::json!({
                    "verdict": verdict,
                    "comments": comments,
                    "repo": repo,
                    "pr_number": pr_number,
                    "reviewer_worker_id": worker_id,
                }),
                format!("Review verdict for PR #{pr_number}: {verdict}"),
            )
        } else {
            let ready_branch = task
                .metadata
                .get("ready_branch")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            (
                serde_json::json!({
                    "verdict": verdict,
                    "comments": comments,
                    "reviewer_worker_id": worker_id,
                    "ready_branch": ready_branch,
                }),
                format!("Review verdict for branch {ready_branch}: {verdict}"),
            )
        };
    let verdict_signal = SignalUpdate::new(
        "swarm_review_verdict",
        format!("swarm-review-verdict-{worker_id}"),
        signal_title,
        Severity::Info,
    )
    .with_metadata(metadata.to_string());

    let (signal_id, is_new) = signal_store.upsert_signal(&verdict_signal)?;
    if is_new && let Some(record) = signal_store.get_signal(signal_id)? {
        activity.log_event(
            workspace,
            Some(&task.id),
            "review",
            &format!("Review verdict: {verdict}"),
            if comments.is_empty() {
                None
            } else {
                Some(comments.as_str())
            },
            Some("swarm_worker_completed"),
            Some(signal_id),
            Some(
                &serde_json::json!({
                    "verdict": verdict,
                    "reviewer_worker_id": worker_id,
                })
                .to_string(),
            ),
        )?;
        apply_engine_signal(task_store, activity, workspace, &record, outcome)?;
        if verdict == "APPROVED"
            && is_branch_flow
            && let Some(ref original_worker_id) = task.worker_id
        {
            outcome.effects.push(TaskEffect::SendWorkerMessage {
                worker_id: original_worker_id.clone(),
                message: format!(
                    "Your code was approved by the reviewer. Please run: `gh pr create --title '{}' --body 'Approved by AI reviewer'`",
                    task.title
                ),
            });
        }
        outcome.effects.push(TaskEffect::CloseWorker { worker_id });
    }
    Ok(())
}

fn handle_swarm_closed(
    task_store: &TaskStore,
    activity: &ActivityEventStore,
    workspace: &str,
    signal: &SignalRecord,
) -> Result<()> {
    if signal.source != "swarm_worker_closed" || !signal.external_id.starts_with("swarm-closed-") {
        return Ok(());
    }
    let worker_id = signal
        .external_id
        .strip_prefix("swarm-closed-")
        .unwrap_or_default();
    if worker_id.is_empty() {
        return Ok(());
    }
    let Some(task) = task_store.find_task_by_worker(workspace, worker_id)? else {
        return Ok(());
    };
    if task.stage.is_terminal() || task.pr_url.is_some() {
        return Ok(());
    }
    let from_stage = task.stage.clone();
    task_store.transition_task(
        &task.id,
        &task.stage,
        &TaskStage::Dismissed,
        Some("Worker closed without PR".to_string()),
    )?;
    let meta = serde_json::json!({
        "from": from_stage.as_str(),
        "to": "Dismissed",
        "reason": "Worker closed without PR",
    });
    activity.log_event(
        workspace,
        Some(&task.id),
        "stage_change",
        &format!("{} → Dismissed", from_stage.as_str()),
        None,
        Some("swarm_worker_closed"),
        None,
        Some(&meta.to_string()),
    )?;
    Ok(())
}

fn read_review_verdict(root: &Path, worker_id: &str) -> Result<Option<(String, String)>> {
    let state_path: PathBuf = root.join(".swarm").join("state.json");
    let Ok(raw) = std::fs::read_to_string(&state_path) else {
        return Ok(None);
    };
    let Ok(state_json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(None);
    };
    let verdict = state_json
        .get("worktrees")
        .and_then(|wts| wts.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|wt| wt.get("id").and_then(|id| id.as_str()) == Some(worker_id))
        })
        .and_then(|wt| wt.get("review_verdict"))
        .and_then(|v| {
            let approved = v.get("approved").and_then(|a| a.as_bool())?;
            Some(if approved {
                "APPROVED".to_string()
            } else {
                "CHANGES_REQUESTED".to_string()
            })
        });
    let comments = state_json
        .get("worktrees")
        .and_then(|wts| wts.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|wt| wt.get("id").and_then(|id| id.as_str()) == Some(worker_id))
        })
        .and_then(|wt| wt.get("review_verdict"))
        .and_then(|v| v.get("comments"))
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    Ok(verdict.map(|verdict| (verdict, comments)))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn reviewer_assignment_updates_task_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let store = TaskStore::open(&db_path).unwrap();
        let now = Utc::now();
        let task = Task {
            id: "task-1".into(),
            workspace: "acme".into(),
            title: "Test".into(),
            stage: TaskStage::InAiReview,
            source: None,
            source_url: None,
            worker_id: Some("w1".into()),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::json!({}),
        };
        store.create_task(&task).unwrap();

        record_reviewer_assignment(
            &db_path,
            "acme",
            "task-1",
            "reviewer-1",
            "Reviewer dispatched",
            serde_json::json!({"reviewer_worker_id": "reviewer-1"}),
        )
        .unwrap();

        let updated = store.get_task("task-1").unwrap().unwrap();
        assert_eq!(
            updated
                .metadata
                .get("reviewer_worker_id")
                .and_then(|v| v.as_str()),
            Some("reviewer-1")
        );
    }
}

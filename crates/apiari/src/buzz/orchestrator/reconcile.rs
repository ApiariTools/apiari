use std::path::Path;

use color_eyre::eyre::Result;
use serde::Deserialize;
use tokio::process::Command;

use crate::buzz::task::{ActivityEventStore, TaskStage, store::TaskStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Deserialize)]
struct GhPullRequestState {
    #[serde(rename = "state")]
    state: String,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
}

pub async fn reconcile_workspace(db_path: &Path, workspace: &str) -> Result<Vec<String>> {
    let task_store = TaskStore::open(db_path)?;
    let activity = ActivityEventStore::open(db_path)?;
    let mut notifications = Vec::new();

    for task in task_store.get_active_tasks(workspace)? {
        let (Some(repo), Some(pr_number)) = (task.repo.clone(), task.pr_number) else {
            continue;
        };
        let Some(pr_state) = fetch_pr_state(&repo, pr_number).await? else {
            continue;
        };
        if let Some(note) = apply_pr_state(&task_store, &activity, workspace, &task, pr_state)? {
            notifications.push(note);
        }
    }

    Ok(notifications)
}

async fn fetch_pr_state(repo: &str, pr_number: i64) -> Result<Option<PullRequestState>> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--repo",
            repo,
            "--json",
            "state,mergedAt",
        ])
        .output()
        .await;
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let parsed: GhPullRequestState = serde_json::from_slice(&output.stdout)?;
    if parsed.merged_at.is_some() {
        Ok(Some(PullRequestState::Merged))
    } else if parsed.state.eq_ignore_ascii_case("closed") {
        Ok(Some(PullRequestState::Closed))
    } else {
        Ok(Some(PullRequestState::Open))
    }
}

fn apply_pr_state(
    task_store: &TaskStore,
    activity: &ActivityEventStore,
    workspace: &str,
    task: &crate::buzz::task::Task,
    pr_state: PullRequestState,
) -> Result<Option<String>> {
    match pr_state {
        PullRequestState::Open => Ok(None),
        PullRequestState::Merged if task.stage != TaskStage::Merged => {
            let from = task.stage.clone();
            task_store.transition_task(
                &task.id,
                &task.stage,
                &TaskStage::Merged,
                Some("PR merged via reconciliation".to_string()),
            )?;
            let summary = format!("{} → Merged", from.as_str());
            let meta = serde_json::json!({
                "from": from.as_str(),
                "to": "Merged",
                "reason": "reconciliation",
            });
            activity.log_event(
                workspace,
                Some(&task.id),
                "stage_change",
                &summary,
                None,
                Some("github"),
                None,
                Some(&meta.to_string()),
            )?;
            Ok(Some(format!(
                "Reconciled merged PR for task '{}'",
                task.title
            )))
        }
        PullRequestState::Closed if !task.stage.is_terminal() => {
            let from = task.stage.clone();
            task_store.transition_task(
                &task.id,
                &task.stage,
                &TaskStage::Dismissed,
                Some("PR closed via reconciliation".to_string()),
            )?;
            let summary = format!("{} → Dismissed", from.as_str());
            let meta = serde_json::json!({
                "from": from.as_str(),
                "to": "Dismissed",
                "reason": "reconciliation",
            });
            activity.log_event(
                workspace,
                Some(&task.id),
                "stage_change",
                &summary,
                None,
                Some("github"),
                None,
                Some(&meta.to_string()),
            )?;
            Ok(Some(format!(
                "Reconciled closed PR for task '{}'",
                task.title
            )))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::buzz::task::{Task, TaskStage};

    fn make_task(stage: TaskStage) -> Task {
        let now = Utc::now();
        Task {
            id: "task-1".into(),
            workspace: "acme".into(),
            title: "Task".into(),
            stage,
            source: None,
            source_url: None,
            worker_id: None,
            pr_url: Some("https://github.com/org/repo/pull/42".into()),
            pr_number: Some(42),
            repo: Some("org/repo".into()),
            created_at: now,
            updated_at: now,
            resolved_at: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn reconcile_marks_merged_task() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let store = TaskStore::open(&db_path).unwrap();
        let activity = ActivityEventStore::open(&db_path).unwrap();
        let task = make_task(TaskStage::HumanReview);
        store.create_task(&task).unwrap();

        let note =
            apply_pr_state(&store, &activity, "acme", &task, PullRequestState::Merged).unwrap();

        assert!(note.is_some());
        assert_eq!(
            store.get_task("task-1").unwrap().unwrap().stage,
            TaskStage::Merged
        );
    }

    #[test]
    fn reconcile_marks_closed_task_dismissed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let store = TaskStore::open(&db_path).unwrap();
        let activity = ActivityEventStore::open(&db_path).unwrap();
        let task = make_task(TaskStage::InAiReview);
        store.create_task(&task).unwrap();

        let note =
            apply_pr_state(&store, &activity, "acme", &task, PullRequestState::Closed).unwrap();

        assert!(note.is_some());
        assert_eq!(
            store.get_task("task-1").unwrap().unwrap().stage,
            TaskStage::Dismissed
        );
    }
}

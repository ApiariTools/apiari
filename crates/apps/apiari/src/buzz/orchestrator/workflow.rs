//! Workflow configuration, action types, and system PR creation utility.
//!
//! The actual workflow state machine is in `task_workflow.rs`, backed by
//! `apiari-workflow`. This module retains the public types consumed by the
//! daemon's `execute_workflow_action` function.

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

/// PR title + body written by the worker to `.swarm/agents/{id}/pr.json`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PrDescriptionFile {
    pub title: String,
    pub body: String,
}

/// Append a `system` event to a worker's `events.jsonl`.
///
/// Silently does nothing if the file or directory can't be written — timeline
/// events are best-effort and must never block the orchestrator.
pub fn append_worker_system_event(workspace_root: &Path, worker_id: &str, text: &str) {
    use std::io::Write;
    let path = workspace_root
        .join(".swarm")
        .join("agents")
        .join(worker_id)
        .join("events.jsonl");
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let line = serde_json::json!({
        "type": "system",
        "text": text,
        "timestamp": timestamp,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// Read a worker-authored PR description.
///
/// Checks the worktree root first (workers write `pr.json` to their working
/// directory), then falls back to `.swarm/agents/{id}/pr.json` for compat.
pub fn read_pr_description(
    workspace_root: &Path,
    worker_id: &str,
    worktree_path: Option<&Path>,
) -> Option<PrDescriptionFile> {
    if let Some(wt) = worktree_path {
        let path = wt.join("pr.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(desc) = serde_json::from_str(&content) {
                return Some(desc);
            }
        }
    }
    let path = workspace_root
        .join(".swarm")
        .join("agents")
        .join(worker_id)
        .join("pr.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
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
        // If the worker already opened the PR, gh exits non-zero with "already exists: <url>".
        // Treat this as success — extract the URL from the error message.
        if let Some(url) = stderr
            .lines()
            .find(|l| l.contains("already exists"))
            .and_then(|l| l.split_whitespace().find(|w| w.starts_with("https://")))
            .map(|s| s.trim_end_matches('.').to_string())
        {
            info!("[workflow] PR already exists for {branch_name}: {url}");
            let pr_number = url.rsplit('/').next().and_then(|s| s.parse::<i64>().ok());
            return Ok(PrCreationResult {
                pr_url: url,
                pr_number,
            });
        }
        bail!("gh pr create failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    info!("[workflow] system PR created: {stdout}");

    let pr_number = stdout
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<i64>().ok());

    Ok(PrCreationResult {
        pr_url: stdout,
        pr_number,
    })
}

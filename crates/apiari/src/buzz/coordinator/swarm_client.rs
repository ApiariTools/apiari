//! Thin async wrapper around `apiari_swarm::daemon::ipc_client::send_daemon_request`.
//!
//! All daemon calls are synchronous (Unix socket I/O), so we use
//! `tokio::task::spawn_blocking` to avoid blocking the async runtime.

use std::path::{Path, PathBuf};

use apiari_swarm::daemon::{
    ipc_client::send_daemon_request,
    protocol::{DaemonRequest, DaemonResponse, TaskDirPayload, WorkerInfo},
};
use color_eyre::eyre::{Result, bail};

/// Async client for the swarm daemon.
pub struct SwarmClient {
    work_dir: PathBuf,
}

impl SwarmClient {
    pub fn new(work_dir: PathBuf) -> Self {
        Self { work_dir }
    }

    /// Send a request to the daemon, offloading blocking I/O to a thread.
    async fn request(&self, req: DaemonRequest) -> Result<DaemonResponse> {
        let dir = self.work_dir.clone();
        tokio::task::spawn_blocking(move || send_daemon_request(&dir, &req)).await?
    }

    /// Create a new worker. Returns the worktree ID.
    pub async fn create_worker(&self, repo: &str, prompt: &str, agent: &str) -> Result<String> {
        self.create_worker_with_task_dir(repo, prompt, agent, None).await
    }

    /// Create a new worker with seeded `.task/` artifacts.
    pub async fn create_worker_with_task_dir(
        &self,
        repo: &str,
        prompt: &str,
        agent: &str,
        task_dir: Option<TaskDirPayload>,
    ) -> Result<String> {
        let resp = self
            .request(DaemonRequest::CreateWorker {
                prompt: prompt.to_string(),
                agent: agent.to_string(),
                repo: Some(repo.to_string()),
                start_point: None,
                workspace: Some(self.work_dir.clone()),
                profile: None,
                task_dir,
                role: None,
                review_pr: None,
                base_branch: None,
            })
            .await?;

        match resp {
            DaemonResponse::Ok { data } => {
                // The daemon returns the worktree ID in the data field.
                let id = data
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                Ok(id)
            }
            DaemonResponse::Error { message } => bail!("create_worker failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Load the code-review skill from `.apiari/skills/code-review.md` if it exists.
    fn load_review_skill(&self) -> String {
        let path = self.work_dir.join(".apiari/skills/code-review.md");
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    /// Build the reviewer prompt, prepending the skill file if present.
    fn reviewer_prompt(&self, base: &str) -> String {
        let skill = self.load_review_skill();
        if skill.is_empty() {
            base.to_string()
        } else {
            format!("{skill}\n\n---\n\n{base}")
        }
    }

    /// Create a reviewer worker for a PR. Returns the worktree ID.
    ///
    /// Uses the `reviewer` profile, which instructs the agent to review the diff
    /// and emit a structured verdict (`REVIEW_VERDICT: APPROVED` or
    /// `REVIEW_VERDICT: CHANGES_REQUESTED`).
    pub async fn create_reviewer_worker(&self, repo: &str, pr_number: i64) -> Result<String> {
        let prompt = self.reviewer_prompt(&format!("Review PR #{pr_number}"));
        let resp = self
            .request(DaemonRequest::CreateWorker {
                prompt,
                agent: "claude".to_string(),
                repo: Some(repo.to_string()),
                start_point: None,
                workspace: Some(self.work_dir.clone()),
                profile: None,
                task_dir: None,
                role: Some("reviewer".to_string()),
                review_pr: Some(pr_number as u64),
                base_branch: Some("main".to_string()),
            })
            .await?;

        match resp {
            DaemonResponse::Ok { data } => {
                let id = data
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                Ok(id)
            }
            DaemonResponse::Error { message } => bail!("create_reviewer_worker failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Create a reviewer worker for a branch. Returns the worktree ID.
    pub async fn create_reviewer_worker_for_branch(
        &self,
        repo: &str,
        branch_name: &str,
    ) -> Result<String> {
        let prompt = self.reviewer_prompt(&format!("Review branch {branch_name}"));
        let resp = self
            .request(DaemonRequest::CreateWorker {
                prompt,
                agent: "claude".to_string(),
                repo: Some(repo.to_string()),
                start_point: None,
                workspace: Some(self.work_dir.clone()),
                profile: None,
                task_dir: None,
                role: Some("reviewer".to_string()),
                review_pr: None,
                base_branch: Some("main".to_string()),
            })
            .await?;

        match resp {
            DaemonResponse::Ok { data } => {
                let id = data
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                Ok(id)
            }
            DaemonResponse::Error { message } => {
                bail!("create_reviewer_worker_for_branch failed: {message}")
            }
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Send a follow-up message to a running worker.
    pub async fn send_message(&self, worktree_id: &str, message: &str) -> Result<()> {
        let resp = self
            .request(DaemonRequest::SendMessage {
                worktree_id: worktree_id.to_string(),
                message: message.to_string(),
            })
            .await?;

        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => bail!("send_message failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Close (tear down) a worker.
    pub async fn close_worker(&self, worktree_id: &str) -> Result<()> {
        let resp = self
            .request(DaemonRequest::CloseWorker {
                worktree_id: worktree_id.to_string(),
            })
            .await?;

        match resp {
            DaemonResponse::Ok { .. } => Ok(()),
            DaemonResponse::Error { message } => bail!("close_worker failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// List all workers in this workspace.
    pub async fn list_workers(&self) -> Result<Vec<WorkerInfo>> {
        let resp = self
            .request(DaemonRequest::ListWorkers {
                workspace: Some(self.work_dir.clone()),
            })
            .await?;

        match resp {
            DaemonResponse::Workers { workers } => Ok(workers),
            DaemonResponse::Error { message } => bail!("list_workers failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    /// Health check — returns true if the daemon responds to ping.
    pub async fn ping(&self) -> bool {
        self.request(DaemonRequest::Ping).await.is_ok()
    }

    /// Ping the swarm daemon synchronously (for use in non-async contexts).
    pub fn ping_sync(work_dir: &Path) -> bool {
        send_daemon_request(work_dir, &DaemonRequest::Ping).is_ok()
    }
}

fn worker_title(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or(prompt).trim();
    let trimmed = line.trim_end_matches(['.', '!', '?']).trim();
    if trimmed.is_empty() {
        "Worker task".to_string()
    } else {
        trimmed.to_string()
    }
}

fn anti_goals_from_prompt(prompt: &str) -> Vec<String> {
    let mut anti_goals = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("do not ")
            || lower.starts_with("don't ")
            || lower.starts_with("only ")
        {
            anti_goals.push(trimmed.to_string());
        }
    }
    anti_goals
}

pub fn build_worker_task_dir(repo: &str, prompt: &str) -> TaskDirPayload {
    let anti_goals = anti_goals_from_prompt(prompt);
    let anti_goals_md = if anti_goals.is_empty() {
        "- Do not modify unrelated files or adjacent panels just because they look similar.\n- If you cannot identify the exact target confidently, stop and say so in `.swarm/output.md` instead of guessing.\n".to_string()
    } else {
        anti_goals
            .into_iter()
            .map(|goal| format!("- {goal}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    TaskDirPayload {
        task_md: Some(format!(
            "# Task\n\n{}\n\n## Scope\n- Work only in repo `{repo}`.\n- Implement exactly the request below.\n\n## Request\n{}\n\n## Acceptance Criteria\n- The requested change is implemented and stays narrowly scoped.\n- The worker does not edit adjacent panels/components unless the target clearly requires it.\n- If the target is ambiguous, the worker stops and records that instead of guessing.\n\n## Anti-Goals\n{}",
            worker_title(prompt),
            prompt.trim(),
            anti_goals_md
        )),
        context_md: Some(format!(
            "# Context\n\n- Repository: `{repo}`\n- Source of truth is the current repo contents, not guessed product language.\n- Prefer identifying the exact target component or stylesheet before editing.\n- If multiple similarly named panels exist, verify the right one before changing code.\n"
        )),
        plan_md: Some(
            "# Plan\n\n1. Identify the exact component/style files that correspond to the requested target.\n2. Make the smallest scoped change that satisfies the request.\n3. Verify the relevant checks if local toolchain is available.\n4. Commit the change on the swarm branch and output `BRANCH_READY: <branch-name>` when done.\n".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_worker_task_dir_includes_repo_and_request() {
        let task_dir = build_worker_task_dir(
            "apiari",
            "Make the mobile overview cards more compact on phones.",
        );

        assert!(task_dir.task_md.as_ref().unwrap().contains("repo `apiari`"));
        assert!(task_dir.task_md.as_ref().unwrap().contains("mobile overview cards"));
        assert!(task_dir.context_md.as_ref().unwrap().contains("Repository: `apiari`"));
        assert!(task_dir.plan_md.as_ref().unwrap().contains("Identify the exact component"));
    }

    #[test]
    fn build_worker_task_dir_preserves_anti_goals_from_prompt() {
        let task_dir = build_worker_task_dir(
            "apiari",
            "Make the mobile overview cards more compact on phones.\nDo not change BotNav, ReposPanel, WorkersPanel, chat, docs, or routing.",
        );

        let task_md = task_dir.task_md.unwrap();
        assert!(task_md.contains("Do not change BotNav, ReposPanel, WorkersPanel, chat, docs, or routing."));
    }
}

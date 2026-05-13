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
        self.create_worker_with_task_dir(repo, prompt, agent, None)
            .await
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
        if lower.starts_with("do not ") || lower.starts_with("don't ") || lower.starts_with("only ")
        {
            anti_goals.push(trimmed.to_string());
        }
    }
    anti_goals
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    Implementation,
    Investigator,
}

impl WorkerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Investigator => "investigator",
        }
    }
}

pub fn infer_worker_mode(prompt: &str) -> WorkerMode {
    let lower = prompt.to_ascii_lowercase();
    let investigator_terms = [
        "why ",
        "why is",
        "investigate",
        "diagnose",
        "debug",
        "trace",
        "root cause",
        "figure out",
        "what is happening",
        "analyze",
        "review",
        "look into",
        "explain",
    ];
    if investigator_terms
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return WorkerMode::Investigator;
    }

    WorkerMode::Implementation
}

pub fn build_worker_task_dir(repo: &str, prompt: &str) -> TaskDirPayload {
    build_worker_task_dir_with_mode(repo, prompt, infer_worker_mode(prompt))
}

pub fn build_worker_task_dir_with_mode(
    repo: &str,
    prompt: &str,
    mode: WorkerMode,
) -> TaskDirPayload {
    build_worker_task_dir_with_mode_and_shaping(repo, prompt, mode, None)
}

pub fn build_worker_task_dir_with_mode_and_shaping(
    repo: &str,
    prompt: &str,
    mode: WorkerMode,
    shaping_md: Option<String>,
) -> TaskDirPayload {
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

    let (task_scope, acceptance_criteria, context_extra, plan_text) = match mode {
        WorkerMode::Implementation => (
            format!(
                "- Work only in repo `{repo}`.\n- Treat this as an implementation subagent task.\n- Implement the request below using reasonable inference within the requested product area.\n"
            ),
            "- The requested change is implemented in the best-matching target surface.\n- The worker may inspect nearby candidate files before editing.\n- The worker records reasoning in `.task/PROGRESS.md` if it had to choose among multiple likely targets.\n- The worker avoids unrelated panels and broad refactors.\n- When done: commit changes and push the branch. Do NOT run `gh pr create` or open a pull request (PR) — the system creates it automatically.\n".to_string(),
            "- This is a normal implementation worker: act like a subagent with bounded autonomy.\n- Use the parent request, anti-goals, and repo contents to choose the best-supported target surface.\n- If there are multiple likely targets, inspect candidates and choose the best-supported one.\n- If the ambiguity is fundamental, stop and explain exactly what additional pointer is needed.\n".to_string(),
            "# Plan\n\n1. Identify the most likely code surface for the request by examining candidate components/styles in the relevant product area.\n2. Choose the best-supported target, noting the reasoning in `.task/PROGRESS.md` if multiple candidates were considered.\n3. Implement the smallest cohesive change that satisfies the request.\n4. Verify the relevant checks if local toolchain is available.\n5. Commit the change and push the swarm branch. Do NOT run `gh pr create` or open a pull request (PR) — the system creates it automatically.\n".to_string(),
        ),
        WorkerMode::Investigator => (
            format!(
                "- Work only in repo `{repo}`.\n- Treat this as an investigator subagent task.\n- Explore the codebase and runtime clues needed to answer the request before changing code.\n"
            ),
            "- The worker explains findings clearly.\n- The worker only makes code changes if the evidence strongly supports a specific fix.\n- If no safe change is clear, it returns findings and next steps instead of guessing.\n".to_string(),
            "- This is an investigator worker: broad inference is allowed, but unsupported code edits are not.\n- Prefer diagnosis, candidate causes, and exact file pointers before patching.\n".to_string(),
            "# Plan\n\n1. Explore the relevant code paths, logs, and nearby files to understand the problem.\n2. Write findings and candidate causes in `.task/PROGRESS.md`.\n3. If a concrete safe fix is clear, implement it narrowly and verify it.\n4. If no safe fix is clear, stop with a precise summary and recommended next step.\n5. If you do implement a fix, commit the change and push the swarm branch. Do NOT run `gh pr create` or open a pull request (PR) — the system creates it automatically.\n".to_string(),
        ),
    };

    TaskDirPayload {
        task_md: Some(format!(
            "# Task\n\n{}\n\n## Worker Mode\n- `{}`\n\n## Scope\n{}\n## Request\n{}\n\n## Acceptance Criteria\n{}\n## Anti-Goals\n{}",
            worker_title(prompt),
            mode.as_str(),
            task_scope,
            prompt.trim(),
            acceptance_criteria,
            anti_goals_md,
        )),
        context_md: Some(format!(
            "# Context\n\n- Repository: `{repo}`\n- Worker mode: `{}`\n- Source of truth is the current repo contents, not guessed product language.\n{}\n",
            mode.as_str(),
            context_extra,
        )),
        plan_md: Some(plan_text),
        shaping_md,
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

        assert!(
            task_dir
                .task_md
                .as_ref()
                .unwrap()
                .contains("Work only in repo `apiari`")
        );
        assert!(
            task_dir
                .task_md
                .as_ref()
                .unwrap()
                .contains("mobile overview cards")
        );
        assert!(
            task_dir
                .context_md
                .as_ref()
                .unwrap()
                .contains("Repository: `apiari`")
        );
        assert!(
            task_dir
                .task_md
                .as_ref()
                .unwrap()
                .contains("`implementation`")
        );
        assert!(
            task_dir
                .plan_md
                .as_ref()
                .unwrap()
                .contains("most likely code surface")
        );
        assert!(task_dir.shaping_md.is_none());
    }

    #[test]
    fn build_worker_task_dir_preserves_anti_goals_from_prompt() {
        let task_dir = build_worker_task_dir(
            "apiari",
            "Make the mobile overview cards more compact on phones.\nDo not change BotNav, ReposPanel, WorkersPanel, chat, docs, or routing.",
        );

        let task_md = task_dir.task_md.unwrap();
        assert!(
            task_md.contains(
                "Do not change BotNav, ReposPanel, WorkersPanel, chat, docs, or routing."
            )
        );
    }

    #[test]
    fn build_worker_task_dir_preserves_optional_shaping_packet() {
        let task_dir = build_worker_task_dir_with_mode_and_shaping(
            "apiari",
            "Tighten worker cards on mobile.",
            WorkerMode::Implementation,
            Some("# Coordinator Shaping\n\n## Goal\n- Tighten worker cards.".to_string()),
        );

        assert!(
            task_dir
                .shaping_md
                .as_ref()
                .unwrap()
                .contains("Coordinator Shaping")
        );
    }

    #[test]
    fn infer_worker_mode_distinguishes_patch_feature_and_investigate() {
        assert_eq!(
            infer_worker_mode("Make the mobile overview cards more compact on phones."),
            WorkerMode::Implementation
        );
        assert_eq!(
            infer_worker_mode(
                "Make the mobile overview cards more compact on phones. Only edit the actual overview panel component/styles."
            ),
            WorkerMode::Implementation
        );
        assert_eq!(
            infer_worker_mode("Investigate why repeated PRs are being created for the same issue."),
            WorkerMode::Investigator
        );
    }
}

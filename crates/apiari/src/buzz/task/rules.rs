//! Deterministic transition rules engine for the task lifecycle.
//!
//! Evaluates incoming signals against the current task state and proposes
//! stage transitions. Only `Auto` transitions are applied immediately;
//! `CoordinatorConfirm` and `UserConfirm` are defined but not yet executed.

use super::{Task, TaskStage};
use crate::buzz::signal::SignalRecord;

/// What approval is needed for a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// Transition happens automatically, no confirmation needed.
    Auto,
    /// Coordinator LLM decides (future — not implemented in this PR).
    CoordinatorConfirm,
    /// User must confirm (future — not implemented in this PR).
    UserConfirm,
}

/// Side effect to perform when a transition fires.
#[derive(Debug, Clone)]
pub enum TransitionAction {
    /// No side effect beyond the stage change.
    None,
    /// Forward signal details to the task's worker.
    ForwardToWorker { message: String },
    /// Notify the user (via TUI/Telegram).
    Notify { message: String },
}

/// A proposed stage transition.
#[derive(Debug)]
pub struct ProposedTransition {
    pub task_id: String,
    pub from: TaskStage,
    pub to: TaskStage,
    pub reason: String,
    pub approval: Approval,
    pub action: TransitionAction,
}

/// Evaluate what transitions should happen for a signal on a given task.
/// Returns None if no rule matches. Returns the proposed transition if one does.
///
/// Only Auto transitions are returned for now — CoordinatorConfirm and UserConfirm
/// will be implemented in a future phase.
pub fn evaluate_signal(task: &Task, signal: &SignalRecord) -> Option<ProposedTransition> {
    let source = signal.source.as_str();

    match (&task.stage, source) {
        // ── In Progress → In AI Review ──
        // When a swarm worker opens a PR (pr_url transitions from None to Some),
        // the task moves to AI Review. This is detected via the "swarm" signal
        // source when a PrOpened event fires.
        // NOTE: This transition is handled separately via update_task_pr() +
        // explicit stage change, not through this rules engine, because the
        // PrOpened signal carries structured data (pr_url, pr_number) that needs
        // to be written to the task before transitioning.

        // ── In AI Review: CI failed → stay in AI Review, notify worker ──
        // InProgress is reserved for when a worker is actively coding.
        // CI failures during review are notifications — not state regressions.
        (TaskStage::InAiReview, "github_ci_failure") => {
            let pr_ref = extract_pr_ref(signal);
            Some(ProposedTransition {
                task_id: task.id.clone(),
                from: TaskStage::InAiReview,
                to: TaskStage::InAiReview,
                reason: format!("CI failed on {}", pr_ref.as_deref().unwrap_or("PR")),
                approval: Approval::Auto,
                action: TransitionAction::ForwardToWorker {
                    message: format!(
                        "CI failed on your PR. Check the failure and push a fix.\n\nSignal: {}",
                        signal.title
                    ),
                },
            })
        }

        // ── In AI Review: bot review → stay in AI Review, forward to worker if comments ──
        // InProgress is reserved for when a worker is actively coding.
        // Bot review comments are forwarded as notifications, not state regressions.
        (TaskStage::InAiReview, "github_bot_review") => {
            let has_comments = bot_review_has_comments(signal);

            if has_comments {
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InAiReview,
                    to: TaskStage::InAiReview,
                    reason: "Bot review has comments".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::ForwardToWorker {
                        message: format!(
                            "Copilot reviewed your PR and left comments. Please address them.\n\nReview: {}",
                            signal.url.as_deref().unwrap_or("")
                        ),
                    },
                })
            } else {
                // Clean review — propose move to HumanReview (but only if we could
                // also verify CI is green, which we can't from a single signal).
                // For now, just notify. The github_ci_pass rule handles the actual
                // HumanReview transition.
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InAiReview,
                    to: TaskStage::InAiReview, // no stage change
                    reason: "Bot review is clean".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::Notify {
                        message: format!(
                            "Copilot review looks clean on {}",
                            extract_pr_ref(signal).as_deref().unwrap_or("PR")
                        ),
                    },
                })
            }
        }

        // ── In AI Review: CI passed + reviews clean → Human Review ──
        (TaskStage::InAiReview, "github_ci_pass") => {
            let pr_ref = extract_pr_ref(signal);
            Some(ProposedTransition {
                task_id: task.id.clone(),
                from: TaskStage::InAiReview,
                to: TaskStage::HumanReview,
                reason: format!("CI passed on {}", pr_ref.as_deref().unwrap_or("PR")),
                approval: Approval::Auto,
                action: TransitionAction::Notify {
                    message: format!(
                        "CI passed on {} — ready for human review",
                        pr_ref.as_deref().unwrap_or("PR")
                    ),
                },
            })
        }

        // ── In Progress: branch ready → In AI Review ──
        // A worker pushed a branch and output BRANCH_READY, triggering this
        // signal. The task moves to InAiReview so a reviewer worker can be
        // dispatched. This is the branch-first alternative to PrOpened.
        (TaskStage::InProgress, "swarm_branch_ready") => {
            let meta = signal
                .metadata
                .as_ref()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok());
            let branch_name = meta
                .as_ref()
                .and_then(|m| m.get("branch_name").and_then(|v| v.as_str()))
                .unwrap_or("unknown branch")
                .to_string();
            Some(ProposedTransition {
                task_id: task.id.clone(),
                from: TaskStage::InProgress,
                to: TaskStage::InAiReview,
                reason: format!("Branch ready for review: {branch_name}"),
                approval: Approval::Auto,
                action: TransitionAction::Notify {
                    message: format!("Branch {branch_name} is ready — dispatching AI reviewer"),
                },
            })
        }

        // ── In Progress: CI failure (worker is still coding, CI ran on push) ──
        (TaskStage::InProgress, "github_ci_failure") => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: TaskStage::InProgress,
            to: TaskStage::InProgress, // stay in progress
            reason: "CI failed while in progress".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::ForwardToWorker {
                message: format!(
                    "CI is failing on your PR. Fix the issue and push.\n\nSignal: {}",
                    signal.title
                ),
            },
        }),

        // ── Human Review: bot review → stay in Human Review, forward to worker if comments ──
        // InProgress is reserved for when a worker is actively coding.
        // Bot review comments during human review are forwarded as notifications.
        (TaskStage::HumanReview, "github_bot_review") => {
            let has_comments = bot_review_has_comments(signal);

            if has_comments {
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::HumanReview,
                    to: TaskStage::HumanReview,
                    reason: "Bot review has comments after reaching human review".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::ForwardToWorker {
                        message: format!(
                            "Copilot reviewed your PR and left comments. Please address them.\n\nReview: {}",
                            signal.url.as_deref().unwrap_or("")
                        ),
                    },
                })
            } else {
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::HumanReview,
                    to: TaskStage::HumanReview, // no stage change
                    reason: "Bot review is clean".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::Notify {
                        message: format!(
                            "Copilot review looks clean on {}",
                            extract_pr_ref(signal).as_deref().unwrap_or("PR")
                        ),
                    },
                })
            }
        }

        // ── Human Review: CI failure → stay in Human Review, notify worker ──
        // InProgress is reserved for when a worker is actively coding.
        // CI failures during review are forwarded as notifications, not state regressions.
        (TaskStage::HumanReview, "github_ci_failure") => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: TaskStage::HumanReview,
            to: TaskStage::HumanReview,
            reason: "CI failed after reaching human review".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::ForwardToWorker {
                message: format!(
                    "CI failed after PR was in human review. Please fix.\n\nSignal: {}",
                    signal.title
                ),
            },
        }),

        // ── Human Review: new commits pushed → back to In AI Review ──
        (TaskStage::HumanReview, "github_pr_push") => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: TaskStage::HumanReview,
            to: TaskStage::InAiReview,
            reason: "New commits pushed after reaching human review".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::Notify {
                message: "New commits pushed — moved back to AI Review".to_string(),
            },
        }),

        // ── In AI Review: review verdict from AI reviewer worker ──
        (TaskStage::InAiReview, "swarm_review_verdict") => {
            let meta = signal
                .metadata
                .as_ref()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok());
            let verdict = meta
                .as_ref()
                .and_then(|m| m.get("verdict").and_then(|v| v.as_str()))
                .unwrap_or("");
            let comments = meta
                .as_ref()
                .and_then(|m| m.get("comments").and_then(|c| c.as_str()))
                .unwrap_or("")
                .to_string();

            match verdict {
                "APPROVED" => Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InAiReview,
                    to: TaskStage::HumanReview,
                    reason: "AI reviewer approved PR".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::Notify {
                        message: "AI reviewer approved — PR is ready for human review".to_string(),
                    },
                }),
                "CHANGES_REQUESTED" => Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InAiReview,
                    to: TaskStage::InProgress,
                    reason: "AI reviewer requested changes".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::ForwardToWorker {
                        message: if comments.is_empty() {
                            "AI reviewer requested changes. Please address the review feedback."
                                .to_string()
                        } else {
                            format!(
                                "AI reviewer requested changes. Please address the following:\n\n{comments}"
                            )
                        },
                    },
                }),
                _ => None,
            }
        }

        // ── In AI Review: new commits pushed → stay (re-review needed) ──
        (TaskStage::InAiReview, "github_pr_push") => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: TaskStage::InAiReview,
            to: TaskStage::InAiReview, // stay
            reason: "New commits pushed, awaiting re-review".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::Notify {
                message: "New commits pushed — awaiting CI and re-review".to_string(),
            },
        }),

        // ── Worker running → InProgress ──
        // A non-reviewer worker transitioned to Running phase. Only move the task to
        // InProgress if it hasn't already advanced past that stage. If the task is in
        // InAiReview, HumanReview, or Merged, the worker_running signal must NOT regress
        // it backwards — that is a bug, not an intentional transition.
        (stage, "swarm_worker_running")
            if stage.stage_order() < TaskStage::InProgress.stage_order() =>
        {
            Some(ProposedTransition {
                task_id: task.id.clone(),
                from: task.stage.clone(),
                to: TaskStage::InProgress,
                reason: "Worker resumed running".to_string(),
                approval: Approval::Auto,
                action: TransitionAction::Notify {
                    message: format!(
                        "Worker is running — task moved to InProgress (was {})",
                        task.stage.as_str()
                    ),
                },
            })
        }

        // ── Worker closed without completing → Triage ──
        // Non-reviewer worker disappeared while task was InProgress — needs attention.
        (TaskStage::InProgress, "swarm_worker_closed") => {
            let role = extract_metadata_str(signal, "role");
            if role.as_deref() != Some("reviewer") {
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InProgress,
                    to: TaskStage::Triage,
                    reason: "Worker closed without completing".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::Notify {
                        message: "Worker closed unexpectedly — task needs attention".to_string(),
                    },
                })
            } else {
                None
            }
        }

        // ── Reviewer closed without verdict → Triage ──
        // Reviewer worker disappeared while task was InAiReview — needs attention.
        (TaskStage::InAiReview, "swarm_worker_closed") => {
            let role = extract_metadata_str(signal, "role");
            if role.as_deref() == Some("reviewer") {
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InAiReview,
                    to: TaskStage::Triage,
                    reason: "Reviewer closed without verdict".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::Notify {
                        message: "AI reviewer closed without a verdict — task needs attention"
                            .to_string(),
                    },
                })
            } else {
                None
            }
        }

        // ── Merged PR signal on any active task ──
        (stage, "github_merged_pr") if !stage.is_terminal() => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: task.stage.clone(),
            to: TaskStage::Merged,
            reason: "PR merged".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::Notify {
                message: "PR merged — task complete".to_string(),
            },
        }),

        // ── Closed PR (without merge) signal on any active task ──
        (stage, "github_pr_closed") if !stage.is_terminal() => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: task.stage.clone(),
            to: TaskStage::Dismissed,
            reason: "PR closed without merge".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::Notify {
                message: "PR closed without merging — task dismissed".to_string(),
            },
        }),

        _ => None,
    }
}

/// Extract a string field from signal metadata JSON.
fn extract_metadata_str(signal: &SignalRecord, key: &str) -> Option<String> {
    signal
        .metadata
        .as_ref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| v.get(key).and_then(|s| s.as_str()).map(String::from))
}

/// Returns true if a bot review signal has actionable comments that require
/// the worker to go back to InProgress.
///
/// Prefers structured `review_state` from signal metadata (CHANGES_REQUESTED or
/// COMMENTED). Falls back to body text heuristic for signals that lack metadata.
fn bot_review_has_comments(signal: &SignalRecord) -> bool {
    // Prefer structured review_state from metadata.
    if let Some(ref meta) = signal.metadata
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(meta)
        && let Some(state) = v.get("review_state").and_then(|s| s.as_str())
    {
        return matches!(state, "CHANGES_REQUESTED" | "COMMENTED");
    }
    // Fall back to body text heuristic (Copilot inline comment marker).
    signal
        .body
        .as_ref()
        .map(|b| b.contains("generated") && b.contains("comment"))
        .unwrap_or(false)
}

/// Try to match a signal to an existing task by PR number + repo.
/// Extracts PR info from signal metadata, title, or URL.
pub fn match_signal_to_task_pr(signal: &SignalRecord) -> Option<(String, i64)> {
    // Try metadata first (structured)
    if let Some(ref meta) = signal.metadata
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(meta)
    {
        let repo = v.get("repo").and_then(|r| r.as_str()).map(String::from);
        let pr_num = v
            .get("pr_number")
            .and_then(|n| n.as_i64())
            .or_else(|| v.get("number").and_then(|n| n.as_i64()));
        if let (Some(repo), Some(num)) = (repo, pr_num) {
            return Some((repo, num));
        }
    }

    // Try URL pattern: https://github.com/{owner}/{repo}/pull/{number}
    if let Some(ref url) = signal.url
        && let Some(caps) = extract_github_pr_from_url(url)
    {
        return Some(caps);
    }

    None
}

/// Extract (repo, pr_number) from a GitHub PR URL.
pub fn extract_github_pr_from_url(url: &str) -> Option<(String, i64)> {
    // Pattern: https://github.com/{owner}/{repo}/pull/{number}
    let parts: Vec<&str> = url.split('/').collect();
    // Find "pull" and get the number after it, and owner/repo before it
    for (i, part) in parts.iter().enumerate() {
        if (*part == "pull" || *part == "pulls") && i >= 2 && i + 1 < parts.len() {
            let owner = parts[i - 2];
            let repo_name = parts[i - 1];
            // Strip any fragment/query from the number
            let num_str = parts[i + 1].split('#').next()?.split('?').next()?;
            if let Ok(num) = num_str.parse::<i64>() {
                return Some((format!("{owner}/{repo_name}"), num));
            }
        }
    }
    None
}

/// Extract a PR reference string from a signal for display purposes.
fn extract_pr_ref(signal: &SignalRecord) -> Option<String> {
    match_signal_to_task_pr(signal).map(|(repo, num)| format!("PR #{num} ({repo})"))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::buzz::signal::{Severity, SignalRecord, SignalStatus};

    fn make_task(stage: TaskStage) -> Task {
        use crate::buzz::task::Task;
        let now = Utc::now();
        Task {
            id: Uuid::new_v4().to_string(),
            workspace: "test".to_string(),
            title: "Test task".to_string(),
            stage,
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
    fn test_ci_failure_in_ai_review_stays_in_ai_review() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal(
            "github_ci_failure",
            Some("https://github.com/org/repo/pull/42"),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::InAiReview);
        assert_eq!(result.approval, Approval::Auto);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
    }

    #[test]
    fn test_ci_pass_in_ai_review_transitions_to_human_review() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal(
            "github_ci_pass",
            Some("https://github.com/org/repo/pull/42"),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::HumanReview);
        assert_eq!(result.approval, Approval::Auto);
    }

    #[test]
    fn test_ci_failure_in_progress_stays_in_progress() {
        let task = make_task(TaskStage::InProgress);
        let signal = make_signal("github_ci_failure", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InProgress);
        assert_eq!(result.to, TaskStage::InProgress);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
    }

    #[test]
    fn test_ci_failure_in_human_review_stays_in_human_review() {
        let task = make_task(TaskStage::HumanReview);
        let signal = make_signal("github_ci_failure", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::HumanReview);
        assert_eq!(result.to, TaskStage::HumanReview);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
    }

    #[test]
    fn test_pr_push_in_human_review_transitions_to_ai_review() {
        let task = make_task(TaskStage::HumanReview);
        let signal = make_signal("github_pr_push", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::HumanReview);
        assert_eq!(result.to, TaskStage::InAiReview);
    }

    #[test]
    fn test_pr_push_in_ai_review_stays() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal("github_pr_push", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::InAiReview);
    }

    #[test]
    fn test_merged_pr_on_active_task_transitions_to_merged() {
        for stage in [
            TaskStage::Triage,
            TaskStage::InProgress,
            TaskStage::InAiReview,
            TaskStage::HumanReview,
        ] {
            let task = make_task(stage.clone());
            let signal = make_signal("github_merged_pr", None);
            let result = evaluate_signal(&task, &signal).unwrap();
            assert_eq!(result.to, TaskStage::Merged, "stage={}", stage.as_str());
        }
    }

    #[test]
    fn test_merged_pr_on_terminal_task_no_match() {
        for stage in [TaskStage::Merged, TaskStage::Dismissed] {
            let task = make_task(stage);
            let signal = make_signal("github_merged_pr", None);
            assert!(evaluate_signal(&task, &signal).is_none());
        }
    }

    #[test]
    fn test_closed_pr_on_active_task_transitions_to_dismissed() {
        for stage in [
            TaskStage::Triage,
            TaskStage::InProgress,
            TaskStage::InAiReview,
            TaskStage::HumanReview,
        ] {
            let task = make_task(stage.clone());
            let signal = make_signal("github_pr_closed", None);
            let result = evaluate_signal(&task, &signal).unwrap();
            assert_eq!(result.to, TaskStage::Dismissed, "stage={}", stage.as_str());
        }
    }

    #[test]
    fn test_closed_pr_on_terminal_task_no_match() {
        for stage in [TaskStage::Merged, TaskStage::Dismissed] {
            let task = make_task(stage);
            let signal = make_signal("github_pr_closed", None);
            assert!(evaluate_signal(&task, &signal).is_none());
        }
    }

    #[test]
    fn test_unmatched_source_returns_none() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal("sentry", None);
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_match_signal_to_task_pr_from_url() {
        let mut signal = make_signal("github_ci_pass", None);
        signal.url = Some("https://github.com/ApiariTools/apiari/pull/170".to_string());
        let result = match_signal_to_task_pr(&signal).unwrap();
        assert_eq!(result.0, "ApiariTools/apiari");
        assert_eq!(result.1, 170);
    }

    #[test]
    fn test_match_signal_to_task_pr_from_url_with_fragment() {
        let mut signal = make_signal("github_bot_review", None);
        signal.url = Some(
            "https://github.com/ApiariTools/apiari/pull/170#pullrequestreview-123".to_string(),
        );
        let result = match_signal_to_task_pr(&signal).unwrap();
        assert_eq!(result.0, "ApiariTools/apiari");
        assert_eq!(result.1, 170);
    }

    #[test]
    fn test_match_signal_to_task_pr_from_url_with_query() {
        let mut signal = make_signal("github_ci_pass", None);
        signal.url = Some(
            "https://github.com/ApiariTools/apiari/pull/170/checks?check_suite_id=123".to_string(),
        );
        // The /checks segment shifts the index, so "pull" is at i, "170" is i+1 but actually
        // it's "170" then "checks". Let me verify the URL parsing handles this.
        // URL: github.com / ApiariTools / apiari / pull / 170 / checks?...
        // parts[i-2]=ApiariTools, parts[i-1]=apiari, parts[i]=pull, parts[i+1]=170
        let result = match_signal_to_task_pr(&signal).unwrap();
        assert_eq!(result.0, "ApiariTools/apiari");
        assert_eq!(result.1, 170);
    }

    #[test]
    fn test_match_signal_to_task_pr_from_metadata() {
        let mut signal = make_signal("github_ci_pass", None);
        signal.metadata = Some(r#"{"repo": "org/myrepo", "pr_number": 99}"#.to_string());
        let result = match_signal_to_task_pr(&signal).unwrap();
        assert_eq!(result.0, "org/myrepo");
        assert_eq!(result.1, 99);
    }

    #[test]
    fn test_match_signal_to_task_pr_no_match() {
        let signal = make_signal("sentry", None);
        assert!(match_signal_to_task_pr(&signal).is_none());
    }

    #[test]
    fn test_extract_github_pr_from_url_basic() {
        let result = extract_github_pr_from_url("https://github.com/owner/repo/pull/42");
        assert_eq!(result, Some(("owner/repo".to_string(), 42)));
    }

    #[test]
    fn test_extract_github_pr_from_url_with_fragment() {
        let result = extract_github_pr_from_url(
            "https://github.com/owner/repo/pull/42#pullrequestreview-123",
        );
        assert_eq!(result, Some(("owner/repo".to_string(), 42)));
    }

    #[test]
    fn test_extract_github_pr_from_url_invalid() {
        assert!(extract_github_pr_from_url("https://github.com/owner/repo/issues/42").is_none());
        assert!(extract_github_pr_from_url("not-a-url").is_none());
    }

    #[test]
    fn test_bot_review_with_comments_stays_in_ai_review() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("github_bot_review", None);
        signal.body = Some("Copilot generated comment on line 5".to_string());
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::InAiReview);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
    }

    #[test]
    fn test_bot_review_clean_stays_in_ai_review() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("github_bot_review", None);
        signal.body = Some("Looks good!".to_string());
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::InAiReview);
        assert!(matches!(result.action, TransitionAction::Notify { .. }));
    }

    #[test]
    fn test_bot_review_changes_requested_via_metadata_stays_in_stage() {
        // Verify structured review_state takes precedence over body text matching,
        // and that the task stays in its current stage (no regression to InProgress).
        for stage in [TaskStage::InAiReview, TaskStage::HumanReview] {
            let task = make_task(stage.clone());
            let mut signal = make_signal("github_bot_review", None);
            // No body text matching keywords — relies entirely on metadata.
            signal.body = Some("Please see my inline suggestions.".to_string());
            signal.metadata = Some(
                r#"{"review_state": "CHANGES_REQUESTED", "repo": "org/repo", "pr_number": 42}"#
                    .to_string(),
            );
            let result = evaluate_signal(&task, &signal).unwrap();
            assert_eq!(result.to, stage, "stage={}", stage.as_str());
            assert!(
                matches!(result.action, TransitionAction::ForwardToWorker { .. }),
                "stage={}",
                stage.as_str()
            );
        }
    }

    #[test]
    fn test_bot_review_approved_via_metadata_stays() {
        // APPROVED review_state → clean, no transition back to InProgress.
        for (stage, expected_to) in [
            (TaskStage::InAiReview, TaskStage::InAiReview),
            (TaskStage::HumanReview, TaskStage::HumanReview),
        ] {
            let task = make_task(stage);
            let mut signal = make_signal("github_bot_review", None);
            signal.metadata = Some(
                r#"{"review_state": "APPROVED", "repo": "org/repo", "pr_number": 42}"#.to_string(),
            );
            let result = evaluate_signal(&task, &signal).unwrap();
            assert_eq!(result.to, expected_to);
            assert!(matches!(result.action, TransitionAction::Notify { .. }));
        }
    }

    #[test]
    fn test_bot_review_with_comments_in_human_review_stays_in_human_review() {
        let task = make_task(TaskStage::HumanReview);
        let mut signal = make_signal("github_bot_review", None);
        signal.body = Some("Copilot generated comment on line 5".to_string());
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::HumanReview);
        assert_eq!(result.to, TaskStage::HumanReview);
        assert_eq!(result.approval, Approval::Auto);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
    }

    #[test]
    fn test_bot_review_clean_in_human_review_stays() {
        let task = make_task(TaskStage::HumanReview);
        let mut signal = make_signal("github_bot_review", None);
        signal.body = Some("Looks good!".to_string());
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::HumanReview);
        assert_eq!(result.to, TaskStage::HumanReview);
        assert!(matches!(result.action, TransitionAction::Notify { .. }));
    }

    #[test]
    fn test_review_verdict_approved_transitions_to_human_review() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            r#"{"verdict": "APPROVED", "comments": "", "repo": "org/repo", "pr_number": 42}"#
                .to_string(),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::HumanReview);
        assert_eq!(result.approval, Approval::Auto);
        assert!(matches!(result.action, TransitionAction::Notify { .. }));
    }

    #[test]
    fn test_review_verdict_changes_requested_transitions_to_in_progress() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            r#"{"verdict": "CHANGES_REQUESTED", "comments": "Fix the null check on line 42.", "repo": "org/repo", "pr_number": 42}"#
                .to_string(),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::InProgress);
        assert_eq!(result.approval, Approval::Auto);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
        if let TransitionAction::ForwardToWorker { message } = result.action {
            assert!(message.contains("Fix the null check on line 42."));
        }
    }

    #[test]
    fn test_review_verdict_changes_requested_no_comments_uses_fallback_message() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            r#"{"verdict": "CHANGES_REQUESTED", "comments": "", "repo": "org/repo", "pr_number": 42}"#
                .to_string(),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        if let TransitionAction::ForwardToWorker { message } = result.action {
            assert!(message.contains("review feedback"));
        } else {
            panic!("expected ForwardToWorker");
        }
    }

    #[test]
    fn test_review_verdict_unknown_returns_none() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            r#"{"verdict": "UNKNOWN_VERDICT", "repo": "org/repo", "pr_number": 42}"#.to_string(),
        );
        // Unknown verdict — no rule matches
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_review_verdict_no_metadata_returns_none() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal("swarm_review_verdict", None);
        // No metadata → verdict is "" → no rule matches
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_branch_ready_in_progress_transitions_to_in_ai_review() {
        let task = make_task(TaskStage::InProgress);
        let mut signal = make_signal("swarm_branch_ready", None);
        signal.metadata = Some(
            r#"{"worker_id": "w-abc1", "branch_name": "swarm/my-feature", "repo": "/tmp/repo"}"#
                .to_string(),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InProgress);
        assert_eq!(result.to, TaskStage::InAiReview);
        assert_eq!(result.approval, Approval::Auto);
        assert!(matches!(result.action, TransitionAction::Notify { .. }));
        if let TransitionAction::Notify { message } = result.action {
            assert!(message.contains("swarm/my-feature"));
        }
    }

    #[test]
    fn test_branch_ready_in_progress_no_metadata_still_transitions() {
        // Signal without metadata should still transition (branch_name defaults to "unknown branch")
        let task = make_task(TaskStage::InProgress);
        let signal = make_signal("swarm_branch_ready", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InProgress);
        assert_eq!(result.to, TaskStage::InAiReview);
    }

    #[test]
    fn test_branch_ready_in_ai_review_no_rule() {
        // swarm_branch_ready while already InAiReview should not fire a rule
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("swarm_branch_ready", None);
        signal.metadata = Some(
            r#"{"worker_id": "w-abc1", "branch_name": "swarm/my-feature", "repo": ""}"#.to_string(),
        );
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_ci_pass_in_progress_no_rule() {
        // CI pass while InProgress should return None (no rule matches)
        // The task stays InProgress until PR push moves it back to review
        let task = make_task(TaskStage::InProgress);
        let signal = make_signal("github_ci_pass", None);
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_ci_failure_in_triage_no_rule() {
        // CI failure in Triage stage should return None
        let task = make_task(TaskStage::Triage);
        let signal = make_signal("github_ci_failure", None);
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_bot_review_outside_ai_review_no_rule() {
        // Bot review while InProgress should return None
        let task = make_task(TaskStage::InProgress);
        let mut signal = make_signal("github_bot_review", None);
        signal.body = Some("Copilot generated comment on line 5".to_string());
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    // ── swarm_worker_running rules ──

    fn make_worker_running_signal(worker_id: &str) -> SignalRecord {
        let mut s = make_signal("swarm_worker_running", None);
        s.metadata =
            Some(serde_json::json!({"worker_id": worker_id, "role": "worker"}).to_string());
        s
    }

    fn make_worker_closed_signal(worker_id: &str, role: &str) -> SignalRecord {
        let mut s = make_signal("swarm_worker_closed", None);
        s.metadata = Some(serde_json::json!({"worker_id": worker_id, "role": role}).to_string());
        s
    }

    #[test]
    fn test_worker_running_triage_to_in_progress() {
        let task = make_task(TaskStage::Triage);
        let signal = make_worker_running_signal("w1");
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::Triage);
        assert_eq!(result.to, TaskStage::InProgress);
        assert_eq!(result.approval, Approval::Auto);
    }

    #[test]
    fn test_worker_running_human_review_no_regression() {
        // Task already in HumanReview — worker_running must NOT regress it to InProgress.
        let task = make_task(TaskStage::HumanReview);
        let signal = make_worker_running_signal("w1");
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_worker_running_in_ai_review_no_regression() {
        // Task already in InAiReview — worker_running must NOT regress it to InProgress.
        let task = make_task(TaskStage::InAiReview);
        let signal = make_worker_running_signal("w1");
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_ci_pass_in_ai_review_to_human_review_forward_allowed() {
        // Forward transition: InAiReview + ci_pass → HumanReview is unaffected by the fix.
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal("github_ci_pass", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::HumanReview);
    }

    #[test]
    fn test_worker_running_already_in_progress_no_rule() {
        // Already InProgress — no rule fires
        let task = make_task(TaskStage::InProgress);
        let signal = make_worker_running_signal("w1");
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_worker_closed_in_progress_non_reviewer_to_triage() {
        let task = make_task(TaskStage::InProgress);
        let signal = make_worker_closed_signal("w1", "worker");
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InProgress);
        assert_eq!(result.to, TaskStage::Triage);
        assert_eq!(result.approval, Approval::Auto);
    }

    #[test]
    fn test_worker_closed_in_progress_reviewer_no_match() {
        // Reviewer closing while InProgress should not match
        let task = make_task(TaskStage::InProgress);
        let signal = make_worker_closed_signal("rev1", "reviewer");
        assert!(evaluate_signal(&task, &signal).is_none());
    }

    #[test]
    fn test_worker_closed_in_ai_review_reviewer_to_triage() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_worker_closed_signal("rev1", "reviewer");
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::Triage);
        assert_eq!(result.approval, Approval::Auto);
    }

    #[test]
    fn test_worker_closed_in_ai_review_non_reviewer_no_match() {
        // Non-reviewer closing while InAiReview — no rule fires (worker isn't the reviewer)
        let task = make_task(TaskStage::InAiReview);
        let signal = make_worker_closed_signal("w1", "worker");
        assert!(evaluate_signal(&task, &signal).is_none());
    }
}

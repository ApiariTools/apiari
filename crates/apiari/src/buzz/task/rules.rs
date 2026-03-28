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

        // ── In AI Review: CI failed → back to In Progress ──
        (TaskStage::InAiReview, "github_ci_failure") => {
            let pr_ref = extract_pr_ref(signal);
            Some(ProposedTransition {
                task_id: task.id.clone(),
                from: TaskStage::InAiReview,
                to: TaskStage::InProgress,
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

        // ── In AI Review: bot review with comments → back to In Progress ──
        // Note: We can't easily determine if comments are actionable here without
        // LLM judgment. For now, any bot review with body content triggers a
        // forward to the worker. The worker/coordinator can decide if it's noise.
        (TaskStage::InAiReview, "github_bot_review") => {
            // Check if the review body suggests there are inline comments
            let has_comments = signal
                .body
                .as_ref()
                .map(|b| b.contains("generated") && b.contains("comment"))
                .unwrap_or(false);

            if has_comments {
                Some(ProposedTransition {
                    task_id: task.id.clone(),
                    from: TaskStage::InAiReview,
                    to: TaskStage::InProgress,
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
                // Clean review — propose move to MergeReady (but only if we could
                // also verify CI is green, which we can't from a single signal).
                // For now, just notify. The github_ci_pass rule handles the actual
                // MergeReady transition.
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

        // ── In AI Review: CI passed + reviews clean → Merge Ready ──
        (TaskStage::InAiReview, "github_ci_pass") => {
            let pr_ref = extract_pr_ref(signal);
            Some(ProposedTransition {
                task_id: task.id.clone(),
                from: TaskStage::InAiReview,
                to: TaskStage::MergeReady,
                reason: format!("CI passed on {}", pr_ref.as_deref().unwrap_or("PR")),
                approval: Approval::Auto,
                action: TransitionAction::Notify {
                    message: format!(
                        "CI passed on {} — ready for merge review",
                        pr_ref.as_deref().unwrap_or("PR")
                    ),
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

        // ── Merge Ready: CI failure → back to In Progress ──
        (TaskStage::MergeReady, "github_ci_failure") => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: TaskStage::MergeReady,
            to: TaskStage::InProgress,
            reason: "CI failed after reaching Merge Ready".to_string(),
            approval: Approval::Auto,
            action: TransitionAction::ForwardToWorker {
                message: format!(
                    "CI failed after PR was merge-ready. Please fix.\n\nSignal: {}",
                    signal.title
                ),
            },
        }),

        // ── Merge Ready: new commits pushed → back to In AI Review ──
        (TaskStage::MergeReady, "github_pr_push") => Some(ProposedTransition {
            task_id: task.id.clone(),
            from: TaskStage::MergeReady,
            to: TaskStage::InAiReview,
            reason: "New commits pushed after reaching Merge Ready".to_string(),
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
                    to: TaskStage::MergeReady,
                    reason: "AI reviewer approved PR".to_string(),
                    approval: Approval::Auto,
                    action: TransitionAction::Notify {
                        message: "AI reviewer approved — PR is ready for merge".to_string(),
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

        _ => None,
    }
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
    use super::*;
    use crate::buzz::signal::{Severity, SignalRecord, SignalStatus};
    use chrono::Utc;
    use uuid::Uuid;

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
    fn test_ci_failure_in_ai_review_transitions_to_in_progress() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal(
            "github_ci_failure",
            Some("https://github.com/org/repo/pull/42"),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::InProgress);
        assert_eq!(result.approval, Approval::Auto);
        assert!(matches!(
            result.action,
            TransitionAction::ForwardToWorker { .. }
        ));
    }

    #[test]
    fn test_ci_pass_in_ai_review_transitions_to_merge_ready() {
        let task = make_task(TaskStage::InAiReview);
        let signal = make_signal(
            "github_ci_pass",
            Some("https://github.com/org/repo/pull/42"),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::MergeReady);
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
    fn test_ci_failure_in_merge_ready_transitions_to_in_progress() {
        let task = make_task(TaskStage::MergeReady);
        let signal = make_signal("github_ci_failure", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::MergeReady);
        assert_eq!(result.to, TaskStage::InProgress);
    }

    #[test]
    fn test_pr_push_in_merge_ready_transitions_to_ai_review() {
        let task = make_task(TaskStage::MergeReady);
        let signal = make_signal("github_pr_push", None);
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::MergeReady);
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
            TaskStage::MergeReady,
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
    fn test_bot_review_with_comments_transitions_to_in_progress() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("github_bot_review", None);
        signal.body = Some("Copilot generated comment on line 5".to_string());
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.to, TaskStage::InProgress);
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
    fn test_review_verdict_approved_transitions_to_merge_ready() {
        let task = make_task(TaskStage::InAiReview);
        let mut signal = make_signal("swarm_review_verdict", None);
        signal.metadata = Some(
            r#"{"verdict": "APPROVED", "comments": "", "repo": "org/repo", "pr_number": 42}"#
                .to_string(),
        );
        let result = evaluate_signal(&task, &signal).unwrap();
        assert_eq!(result.from, TaskStage::InAiReview);
        assert_eq!(result.to, TaskStage::MergeReady);
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
}

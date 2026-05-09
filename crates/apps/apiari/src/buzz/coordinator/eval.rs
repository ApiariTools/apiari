//! Coordinator eval framework — golden-test suite for prompt properties and
//! action parsing correctness.
//!
//! These tests catch regressions without running a live LLM. They serve as the
//! specification for what the coordinator system should produce given known
//! inputs, giving us fast confidence that parsing and prompt construction stay
//! correct as code evolves.

/// Check that a prompt satisfies a list of property assertions.
///
/// Each property is a `(label, bool)` — the label describes what should be
/// true, and the bool is whether it actually is. Returns a list of failure
/// descriptions, empty on success.
pub fn eval_prompt_properties(_prompt: &str, properties: &[(&str, bool)]) -> Vec<String> {
    properties
        .iter()
        .filter_map(|(label, ok)| {
            if *ok {
                None
            } else {
                Some(format!("FAIL: {label}"))
            }
        })
        .collect()
}

/// A single action-parsing golden test case.
pub struct ActionEvalCase {
    /// Human-readable label for test output.
    pub label: &'static str,
    /// Raw LLM response text.
    pub input: &'static str,
    /// Expected parsed actions (in order).
    pub expected_actions: Vec<super::actions::BeeAction>,
    /// Expected malformed marker warnings (partial string matches are fine).
    pub expected_malformed_contains: Vec<&'static str>,
}

impl ActionEvalCase {
    /// Run the eval case, panicking with a descriptive message on failure.
    pub fn run(&self) {
        use super::actions::{find_malformed_markers, parse_actions};

        let actual_actions = parse_actions(self.input);
        assert_eq!(
            actual_actions, self.expected_actions,
            "[{}] action mismatch\ninput: {:?}",
            self.label, self.input
        );

        let warnings = find_malformed_markers(self.input);
        for expected_substr in &self.expected_malformed_contains {
            assert!(
                warnings.iter().any(|w| w.contains(expected_substr)),
                "[{}] expected malformed warning containing {:?} but got: {:?}",
                self.label,
                expected_substr,
                warnings
            );
        }
        if self.expected_malformed_contains.is_empty() {
            assert!(
                warnings.is_empty(),
                "[{}] expected no malformed warnings but got: {:?}",
                self.label,
                warnings
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::actions::BeeAction;
    use super::*;

    // ── Prompt property checks ──────────────────────────────────────────

    #[test]
    fn default_preamble_contains_identity_and_role_boundaries() {
        let prompt = super::super::prompt::default_preamble("Main");
        let failures = eval_prompt_properties(
            &prompt,
            &[
                (
                    "contains coordinator identity",
                    prompt.contains("coordinator"),
                ),
                (
                    "contains role boundaries section",
                    prompt.contains("Role Boundaries"),
                ),
                (
                    "prohibits direct coding",
                    prompt.contains("NOT a coding assistant"),
                ),
                (
                    "allows Bash for research",
                    prompt.contains("use Bash freely"),
                ),
                (
                    "prohibits modifying workspace directly",
                    prompt.contains("NEVER use Bash to modify"),
                ),
                ("mentions devmode", prompt.contains("devmode")),
                ("mentions swarm dispatch", prompt.contains("swarm create")),
            ],
        );
        assert!(
            failures.is_empty(),
            "prompt property failures:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn default_preamble_names_coordinator_correctly() {
        for name in ["Main", "Ops", "Alice"] {
            let prompt = super::super::prompt::default_preamble(name);
            assert!(
                prompt.contains(name),
                "preamble for {:?} doesn't contain the name",
                name
            );
        }
    }

    #[test]
    fn context_window_limits_are_sane() {
        use super::super::super::super::buzz::coordinator::max_context_tokens;
        // All models should have a non-zero context window.
        for model in [
            "claude-opus-4-7",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "gpt-5.3-codex",
            "o4-mini",
            "o3",
            "gemini-2.5-pro",
            "gemini-2.0-flash",
            "gemini-2.5-flash",
        ] {
            let tokens = max_context_tokens(model);
            assert!(
                tokens >= 128_000,
                "model {:?} context window {:?} is suspiciously small",
                model,
                tokens
            );
        }
    }

    #[test]
    fn cost_estimation_produces_positive_values_for_known_models() {
        use super::super::estimate_cost_usd;
        let cases = [
            ("claude-opus-4-7", 1000u64, 200u64),
            ("claude-sonnet-4-6", 1000, 200),
            ("claude-haiku-4-5", 1000, 200),
            ("gpt-5.3-codex", 1000, 200),
            ("o4-mini", 1000, 200),
            ("o3", 1000, 200),
            ("gemini-2.5-pro", 1000, 200),
            ("gemini-2.0-flash", 1000, 200),
            ("gemini-2.5-flash", 1000, 200),
        ];
        for (model, input, output) in cases {
            let cost = estimate_cost_usd(model, input, output);
            assert!(
                cost.is_some() && cost.unwrap() > 0.0,
                "model {:?} should produce a positive cost but got {:?}",
                model,
                cost
            );
        }
        // Unknown model → None
        assert_eq!(estimate_cost_usd("unknown-model-xyz", 100, 100), None);
    }

    // ── Action parsing golden tests ─────────────────────────────────────

    #[test]
    fn golden_dismiss_simple() {
        ActionEvalCase {
            label: "DISMISS: simple integer",
            input: "Looks resolved. [DISMISS: 42]",
            expected_actions: vec![BeeAction::Dismiss { signal_id: 42 }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_dismiss_negative_is_rejected() {
        // Negative IDs are technically valid i64; ensure they parse.
        ActionEvalCase {
            label: "DISMISS: negative id",
            input: "[DISMISS: -1]",
            expected_actions: vec![BeeAction::Dismiss { signal_id: -1 }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_escalate() {
        ActionEvalCase {
            label: "ESCALATE: message",
            input: "This needs human attention. [ESCALATE: prod is on fire]",
            expected_actions: vec![BeeAction::Escalate {
                message: "prod is on fire".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_fix() {
        ActionEvalCase {
            label: "FIX: description",
            input: "[FIX: retry logic missing in payment service]",
            expected_actions: vec![BeeAction::Fix {
                description: "retry logic missing in payment service".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_snooze() {
        ActionEvalCase {
            label: "SNOOZE: id and hours",
            input: "Will check later. [SNOOZE: 99, 24]",
            expected_actions: vec![BeeAction::Snooze {
                signal_id: 99,
                hours: 24,
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_task() {
        ActionEvalCase {
            label: "TASK: title",
            input: "[TASK: investigate memory leak in worker pool]",
            expected_actions: vec![BeeAction::Task {
                title: "investigate memory leak in worker pool".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_research() {
        ActionEvalCase {
            label: "RESEARCH: topic",
            input: "[RESEARCH: best practices for Rust async cancellation]",
            expected_actions: vec![BeeAction::Research {
                topic: "best practices for Rust async cancellation".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_followup_duration() {
        ActionEvalCase {
            label: "FOLLOWUP: duration form",
            input: "[FOLLOWUP: 2h | ping me if CI still failing]",
            expected_actions: vec![BeeAction::Followup {
                when: "2h".to_string(),
                action: "ping me if CI still failing".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_followup_rfc3339() {
        ActionEvalCase {
            label: "FOLLOWUP: RFC3339 timestamp form",
            input: "[FOLLOWUP: 2026-05-06T15:00:00Z | check deploy status]",
            expected_actions: vec![BeeAction::Followup {
                when: "2026-05-06T15:00:00Z".to_string(),
                action: "check deploy status".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_canvas_block() {
        ActionEvalCase {
            label: "CANVAS: freeform block",
            input: "Here is your summary:\n[CANVAS]\n# Status\nAll systems go.\n[/CANVAS]",
            expected_actions: vec![BeeAction::Canvas {
                content: "# Status\nAll systems go.".to_string(),
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_multiple_actions_in_one_response() {
        ActionEvalCase {
            label: "multiple actions: dismiss + task",
            input: "Signal is stale. [DISMISS: 7] Creating a follow-up task. [TASK: audit signal sources]",
            expected_actions: vec![
                BeeAction::Dismiss { signal_id: 7 },
                BeeAction::Task {
                    title: "audit signal sources".to_string(),
                },
            ],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_no_actions_in_plain_response() {
        ActionEvalCase {
            label: "no actions — plain text",
            input: "Everything looks fine. No action needed.",
            expected_actions: vec![],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_malformed_lowercase_dismiss() {
        ActionEvalCase {
            label: "malformed: lowercase [dismiss:]",
            input: "Closing this. [dismiss: 10]",
            expected_actions: vec![],
            expected_malformed_contains: vec!["wrong case"],
        }
        .run();
    }

    #[test]
    fn golden_malformed_missing_colon() {
        ActionEvalCase {
            label: "malformed: missing colon [DISMISS 10]",
            input: "[DISMISS 10]",
            expected_actions: vec![],
            expected_malformed_contains: vec!["missing `:`"],
        }
        .run();
    }

    #[test]
    fn golden_malformed_mixed_case_escalate() {
        ActionEvalCase {
            label: "malformed: mixed case [Escalate:]",
            input: "[Escalate: danger]",
            expected_actions: vec![],
            expected_malformed_contains: vec!["wrong case"],
        }
        .run();
    }

    #[test]
    fn golden_dismiss_with_surrounding_prose() {
        ActionEvalCase {
            label: "DISMISS: embedded in verbose LLM output",
            input: "After reviewing the signal history, I believe this alert is no longer \
                    relevant — the deploy rolled back cleanly. [DISMISS: 123] Please let me \
                    know if you'd like me to create a post-mortem task.",
            expected_actions: vec![BeeAction::Dismiss { signal_id: 123 }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_snooze_with_whitespace_variations() {
        // Parser should trim whitespace around values.
        ActionEvalCase {
            label: "SNOOZE: whitespace around values",
            input: "[SNOOZE:  55 ,  6 ]",
            expected_actions: vec![BeeAction::Snooze {
                signal_id: 55,
                hours: 6,
            }],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_canvas_empty_is_skipped() {
        ActionEvalCase {
            label: "CANVAS: empty block is skipped",
            input: "[CANVAS]\n   \n[/CANVAS]",
            expected_actions: vec![],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    #[test]
    fn golden_non_marker_brackets_ignored() {
        ActionEvalCase {
            label: "non-marker brackets don't cause false positives",
            input: "Use [this] library. [Some other thing] is fine. No [UNKNOWN: stuff] either.",
            expected_actions: vec![],
            expected_malformed_contains: vec![],
        }
        .run();
    }

    // ── Scenario-level behavioral tests ────────────────────────────────
    //
    // These test that realistic coordinator response patterns produce the
    // correct action mix — catching regressions in the complete parsing
    // pipeline without needing a live LLM.

    #[test]
    fn scenario_ci_failure_response_dispatches_fix() {
        // A coordinator that sees CI failing should emit [FIX:] so CodeBee picks it up.
        let response = "\
            I've checked the CI logs and the failure is in the payment service — \
            the retry logic is missing after the gateway timeout. \
            [FIX: add retry with exponential backoff to payment gateway client]";
        let actions = super::super::actions::parse_actions(response);
        assert!(
            actions.iter().any(|a| matches!(a, BeeAction::Fix { .. })),
            "CI failure scenario should produce a FIX action, got: {actions:?}"
        );
        // Should NOT produce noise actions
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, BeeAction::Escalate { .. })),
            "CI failure with known root cause should not escalate"
        );
    }

    #[test]
    fn scenario_unknown_prod_incident_escalates() {
        // When the root cause is unknown and prod is affected, coordinator should escalate.
        let response = "\
            The error rate is spiking across all services simultaneously — \
            this doesn't match any known pattern. Paging on-call. \
            [ESCALATE: unknown prod incident — 500 error rate 40% across all services since 14:22 UTC]";
        let actions = super::super::actions::parse_actions(response);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, BeeAction::Escalate { .. })),
            "unknown prod incident should escalate, got: {actions:?}"
        );
    }

    #[test]
    fn scenario_resolved_signal_gets_dismissed() {
        let response = "The deploy completed successfully and the error rate is back to baseline. \
                         Closing signal 88. [DISMISS: 88]";
        let actions = super::super::actions::parse_actions(response);
        assert_eq!(
            actions,
            vec![BeeAction::Dismiss { signal_id: 88 }],
            "resolved signal should be dismissed"
        );
    }

    #[test]
    fn scenario_coordinator_creates_task_for_work() {
        let response = "The memory leak in the worker pool is real — I can reproduce it. \
                         Creating a task so we can track this properly. \
                         [TASK: fix memory leak in worker pool — workers never release connection handles]";
        let actions = super::super::actions::parse_actions(response);
        assert!(
            actions.iter().any(|a| matches!(a, BeeAction::Task { .. })),
            "coordinator should create a task for tracked work, got: {actions:?}"
        );
    }

    #[test]
    fn scenario_multi_action_triage_response() {
        // Realistic triage: acknowledge a stale signal, create a task for real work,
        // and schedule a follow-up.
        let response = "\
            Signal 12 is a known flap from the canary — safe to close. [DISMISS: 12]\n\
            The authentication timeout on signal 13 is worth investigating. \
            [TASK: investigate auth timeout — signal 13]\n\
            I'll check back after the deploy goes out. [FOLLOWUP: 4h | check if auth timeout cleared after deploy]";
        let actions = super::super::actions::parse_actions(response);

        assert_eq!(actions.len(), 3, "expected 3 actions, got: {actions:?}");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, BeeAction::Dismiss { signal_id: 12 }))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, BeeAction::Task { title } if title.contains("auth timeout")))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, BeeAction::Followup { when, .. } if when == "4h"))
        );
    }

    #[test]
    fn scenario_canvas_used_for_status_board() {
        // Coordinator updates the canvas with a formatted status board.
        let response = "Here's the current workspace summary:\n\
                         [CANVAS]\n\
                         ## Workers\n\
                         - cli-1 — running (auth rewrite)\n\
                         - cli-2 — waiting (PR open)\n\n\
                         ## Open Signals\n\
                         - CI failing on main (P1)\n\
                         [/CANVAS]\n\
                         Let me know if you need anything.";
        let actions = super::super::actions::parse_actions(response);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], BeeAction::Canvas { content } if content.contains("Workers")),
            "expected canvas with workspace summary"
        );
    }

    #[test]
    fn scenario_snoozed_signal_not_dismissed() {
        // Coordinator that snoozes (not dismisses) a signal for later review.
        let response = "CI failure on PR #42 might recover after the infra change lands. \
                         Snoozing for 2 hours rather than closing. [SNOOZE: 42, 2]";
        let actions = super::super::actions::parse_actions(response);
        assert_eq!(
            actions,
            vec![BeeAction::Snooze {
                signal_id: 42,
                hours: 2
            }]
        );
        // Must not also dismiss
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, BeeAction::Dismiss { .. }))
        );
    }

    #[test]
    fn scenario_prompt_properties_include_signal_count() {
        use super::super::prompt::build_system_prompt;
        use crate::buzz::signal::SignalRecord;

        let signals: Vec<SignalRecord> = vec![]; // no signals
        let prompt = build_system_prompt(&signals, &[], None, Some("Main"), None, None);
        let failures = eval_prompt_properties(
            &prompt,
            &[
                ("prompt is non-empty", !prompt.is_empty()),
                (
                    "prompt mentions the coordinator name",
                    prompt.contains("Main"),
                ),
            ],
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

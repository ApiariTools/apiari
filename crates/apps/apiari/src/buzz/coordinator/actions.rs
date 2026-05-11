//! Bee action marker parsing.
//!
//! When a Bee processes a signal (via hook or heartbeat), its response can
//! include action markers that the system executes. This module parses those
//! markers from the response text.

/// Known action marker names — used for both parsing and malformation detection.
const KNOWN_MARKERS: &[&str] = &[
    "DISMISS", "ESCALATE", "FIX", "SNOOZE", "TASK", "RESEARCH", "FOLLOWUP", "CANVAS", "WIDGET",
];

/// Scan a coordinator response for likely-intended-but-malformed action markers.
///
/// Returns a list of human-readable descriptions of what looks wrong. Use these
/// for diagnostics/logging — the parser itself silently skips malformed markers,
/// so this function surfaces what would otherwise be invisible data loss.
///
/// Detects:
/// - Wrong case: `[dismiss: 42]` instead of `[DISMISS: 42]`
/// - Missing colon: `[DISMISS 42]` instead of `[DISMISS: 42]`
pub fn find_malformed_markers(response: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    let bytes = response.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        // Collect alphabetic characters after `[`
        let tag_start = i + 1;
        let mut j = tag_start;
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        if j == tag_start {
            i += 1;
            continue;
        }
        let candidate = std::str::from_utf8(&bytes[tag_start..j]).unwrap_or("");
        let candidate_upper = candidate.to_uppercase();

        if !KNOWN_MARKERS.contains(&candidate_upper.as_str()) {
            i = j;
            continue;
        }

        let is_exact_case = candidate == candidate_upper;
        let has_colon = j < bytes.len() && bytes[j] == b':';

        if !is_exact_case {
            warnings.push(format!(
                "marker `[{candidate}` near offset {i} looks like `[{candidate_upper}: ...]` but uses wrong case — markers must be UPPERCASE"
            ));
        } else if !has_colon && candidate_upper != "CANVAS" {
            warnings.push(format!(
                "marker `[{candidate_upper}` near offset {i} is missing `:` separator — expected `[{candidate_upper}: ...]`"
            ));
        }

        i = j;
    }

    warnings
}

/// An action extracted from a Bee's response text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeeAction {
    /// Resolve a signal by ID.
    Dismiss { signal_id: i64 },
    /// Create a high-priority briefing-visible signal.
    Escalate { message: String },
    /// Emit a signal for CodeBee to pick up (source = `bee/<bee_name>`).
    Fix { description: String },
    /// Snooze a signal for N hours.
    Snooze { signal_id: i64, hours: u64 },
    /// Create a task in the task store.
    Task { title: String },
    /// Trigger a research workflow (already handled elsewhere).
    Research { topic: String },
    /// Schedule a follow-up action for later.
    Followup { when: String, action: String },
    /// Update the Bee's canvas — a freeform markdown display.
    Canvas { content: String },
    /// Write a widget to the dashboard for the given slot.
    Widget {
        slot: String,
        widget_json: String,
        ttl_minutes: Option<i64>,
    },
}

/// Parse all action markers from a Bee's response text.
///
/// Recognised markers (case-sensitive):
/// - `[DISMISS: <signal_id>]`
/// - `[ESCALATE: <message>]`
/// - `[FIX: <description>]`
/// - `[SNOOZE: <signal_id>, <hours>]`
/// - `[TASK: <title>]`
/// - `[RESEARCH: <topic>]`
/// - `[FOLLOWUP: <delay-or-rfc3339> | <action>]`
pub fn parse_actions(response: &str) -> Vec<BeeAction> {
    let mut actions = Vec::new();
    let markers = [
        ("DISMISS", parse_dismiss as fn(&str) -> Option<BeeAction>),
        ("ESCALATE", parse_escalate),
        ("FIX", parse_fix),
        ("SNOOZE", parse_snooze),
        ("TASK", parse_task),
        ("RESEARCH", parse_research),
        ("FOLLOWUP", parse_followup),
    ];

    // Scan for each `[MARKER: ...]` occurrence.
    for (tag, parser) in &markers {
        let prefix = format!("[{tag}:");
        let mut search_from = 0;
        while let Some(start) = response[search_from..].find(&prefix) {
            let abs_start = search_from + start;
            // Find the closing `]` after the prefix.
            let content_start = abs_start + prefix.len();
            if let Some(rel_end) = response[content_start..].find(']') {
                let content = response[content_start..content_start + rel_end].trim();
                if let Some(action) = parser(content) {
                    actions.push(action);
                }
                search_from = content_start + rel_end + 1;
            } else {
                // No closing bracket — stop searching for this marker.
                break;
            }
        }
    }

    // Parse [CANVAS]...[/CANVAS] blocks (multi-line).
    let canvas_open = "[CANVAS]";
    let canvas_close = "[/CANVAS]";
    let mut search_from = 0;
    while let Some(start) = response[search_from..].find(canvas_open) {
        let abs_start = search_from + start;
        let content_start = abs_start + canvas_open.len();
        if let Some(rel_end) = response[content_start..].find(canvas_close) {
            let content = response[content_start..content_start + rel_end].trim();
            if !content.is_empty() {
                actions.push(BeeAction::Canvas {
                    content: content.to_string(),
                });
            }
            search_from = content_start + rel_end + canvas_close.len();
        } else {
            break;
        }
    }

    // Parse [WIDGET: slot | ttl?]...json...[/WIDGET] blocks (multi-line).
    // Opening tag format: [WIDGET: slot-name] or [WIDGET: slot-name | 60]
    let widget_close = "[/WIDGET]";
    let mut search_from = 0;
    while let Some(start) = response[search_from..].find("[WIDGET:") {
        let abs_start = search_from + start;
        // Find the closing `]` of the opening tag.
        let header_content_start = abs_start + "[WIDGET:".len();
        if let Some(header_end) = response[header_content_start..].find(']') {
            let header = response[header_content_start..header_content_start + header_end].trim();
            let (slot, ttl_minutes) = if let Some((s, t)) = header.split_once('|') {
                (s.trim().to_string(), t.trim().parse::<i64>().ok())
            } else {
                (header.to_string(), None)
            };
            let content_start = header_content_start + header_end + 1;
            if !slot.is_empty() {
                if let Some(rel_end) = response[content_start..].find(widget_close) {
                    let widget_json = response[content_start..content_start + rel_end].trim();
                    if !widget_json.is_empty() {
                        actions.push(BeeAction::Widget {
                            slot,
                            widget_json: widget_json.to_string(),
                            ttl_minutes,
                        });
                    }
                    search_from = content_start + rel_end + widget_close.len();
                } else {
                    break;
                }
            } else {
                search_from = content_start;
            }
        } else {
            break;
        }
    }

    actions
}

fn parse_dismiss(content: &str) -> Option<BeeAction> {
    content
        .trim()
        .parse::<i64>()
        .ok()
        .map(|signal_id| BeeAction::Dismiss { signal_id })
}

fn parse_escalate(content: &str) -> Option<BeeAction> {
    let message = content.trim().to_string();
    if message.is_empty() {
        None
    } else {
        Some(BeeAction::Escalate { message })
    }
}

fn parse_fix(content: &str) -> Option<BeeAction> {
    let description = content.trim().to_string();
    if description.is_empty() {
        None
    } else {
        Some(BeeAction::Fix { description })
    }
}

fn parse_snooze(content: &str) -> Option<BeeAction> {
    // Expected format: "<signal_id>, <hours>"
    let parts: Vec<&str> = content.splitn(2, ',').collect();
    if parts.len() != 2 {
        return None;
    }
    let signal_id = parts[0].trim().parse::<i64>().ok()?;
    let hours = parts[1].trim().parse::<u64>().ok()?;
    Some(BeeAction::Snooze { signal_id, hours })
}

fn parse_task(content: &str) -> Option<BeeAction> {
    let title = content.trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(BeeAction::Task { title })
    }
}

fn parse_research(content: &str) -> Option<BeeAction> {
    let topic = content.trim().to_string();
    if topic.is_empty() {
        None
    } else {
        Some(BeeAction::Research { topic })
    }
}

fn parse_followup(content: &str) -> Option<BeeAction> {
    let (when, action) = content.split_once('|')?;
    let when = when.trim().to_string();
    let action = action.trim().to_string();
    if when.is_empty() || action.is_empty() {
        None
    } else {
        Some(BeeAction::Followup { when, action })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dismiss() {
        let actions = parse_actions("Known issue. [DISMISS: 123]");
        assert_eq!(actions, vec![BeeAction::Dismiss { signal_id: 123 }]);
    }

    #[test]
    fn test_parse_escalate() {
        let actions = parse_actions("[ESCALATE: New error pattern in /api/events]");
        assert_eq!(
            actions,
            vec![BeeAction::Escalate {
                message: "New error pattern in /api/events".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_fix() {
        let actions = parse_actions("[FIX: Fix the MatchError in events endpoint]");
        assert_eq!(
            actions,
            vec![BeeAction::Fix {
                description: "Fix the MatchError in events endpoint".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_snooze() {
        let actions = parse_actions("[SNOOZE: 456, 2]");
        assert_eq!(
            actions,
            vec![BeeAction::Snooze {
                signal_id: 456,
                hours: 2,
            }]
        );
    }

    #[test]
    fn test_parse_task() {
        let actions = parse_actions("[TASK: Investigate DB timeout pattern]");
        assert_eq!(
            actions,
            vec![BeeAction::Task {
                title: "Investigate DB timeout pattern".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_research() {
        let actions = parse_actions("[RESEARCH: Neon cold start mitigation strategies]");
        assert_eq!(
            actions,
            vec![BeeAction::Research {
                topic: "Neon cold start mitigation strategies".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_followup() {
        let actions = parse_actions("[FOLLOWUP: 2h | Check PR status again]");
        assert_eq!(
            actions,
            vec![BeeAction::Followup {
                when: "2h".to_string(),
                action: "Check PR status again".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_multiple_actions() {
        let text = "New error. [ESCALATE: MatchError in /api/events] [FIX: Fix the pattern match failure in events controller]";
        let actions = parse_actions(text);
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[0],
            BeeAction::Escalate {
                message: "MatchError in /api/events".to_string(),
            }
        );
        assert_eq!(
            actions[1],
            BeeAction::Fix {
                description: "Fix the pattern match failure in events controller".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_no_actions() {
        let actions = parse_actions("Just a regular response with no markers.");
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_mixed_with_text() {
        let text =
            "This error is transient. [DISMISS: 123] No action needed for the other signals.";
        let actions = parse_actions(text);
        assert_eq!(actions, vec![BeeAction::Dismiss { signal_id: 123 }]);
    }

    #[test]
    fn test_parse_dismiss_invalid_id() {
        let actions = parse_actions("[DISMISS: not_a_number]");
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_snooze_missing_hours() {
        let actions = parse_actions("[SNOOZE: 123]");
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_empty_content() {
        let actions = parse_actions("[ESCALATE: ]");
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_multiple_same_marker() {
        let text = "[DISMISS: 1] [DISMISS: 2] [DISMISS: 3]";
        let actions = parse_actions(text);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], BeeAction::Dismiss { signal_id: 1 });
        assert_eq!(actions[1], BeeAction::Dismiss { signal_id: 2 });
        assert_eq!(actions[2], BeeAction::Dismiss { signal_id: 3 });
    }

    // ── Case sensitivity ───────────────────────────────────────────────

    #[test]
    fn test_parse_lowercase_marker_ignored() {
        // Markers are case-sensitive — lowercase must not parse.
        assert!(parse_actions("[dismiss: 42]").is_empty());
        assert!(parse_actions("[escalate: oh no]").is_empty());
        assert!(parse_actions("[fix: something]").is_empty());
        assert!(parse_actions("[task: do it]").is_empty());
        assert!(parse_actions("[snooze: 1, 2]").is_empty());
    }

    #[test]
    fn test_parse_mixed_case_marker_ignored() {
        assert!(parse_actions("[Dismiss: 42]").is_empty());
        assert_eq!(parse_actions("[ESCALATE: msg]").len(), 1); // uppercase works
        assert!(parse_actions("[Escalate: msg]").is_empty()); // mixed case does not
    }

    // ── Whitespace handling ────────────────────────────────────────────

    #[test]
    fn test_parse_dismiss_leading_whitespace_in_content() {
        let actions = parse_actions("[DISMISS:   42  ]");
        assert_eq!(actions, vec![BeeAction::Dismiss { signal_id: 42 }]);
    }

    #[test]
    fn test_parse_task_with_inner_whitespace() {
        let actions = parse_actions("[TASK:  Investigate   the   thing  ]");
        assert_eq!(
            actions,
            vec![BeeAction::Task {
                title: "Investigate   the   thing".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_followup_extra_spaces_around_pipe() {
        let actions = parse_actions("[FOLLOWUP: 4h  |  Re-check CI ]");
        assert_eq!(
            actions,
            vec![BeeAction::Followup {
                when: "4h".to_string(),
                action: "Re-check CI".to_string(),
            }]
        );
    }

    // ── Canvas blocks ──────────────────────────────────────────────────

    #[test]
    fn test_parse_canvas_block() {
        let text = "[CANVAS]\n## Status\nAll green\n[/CANVAS]";
        let actions = parse_actions(text);
        assert_eq!(
            actions,
            vec![BeeAction::Canvas {
                content: "## Status\nAll green".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_canvas_empty_block_ignored() {
        let text = "[CANVAS]\n  \n[/CANVAS]";
        let actions = parse_actions(text);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_canvas_unclosed_block_ignored() {
        let text = "[CANVAS]\n## Status\nNo close tag";
        let actions = parse_actions(text);
        assert!(actions.is_empty());
    }

    // ── Snooze edge cases ──────────────────────────────────────────────

    #[test]
    fn test_parse_snooze_zero_hours() {
        let actions = parse_actions("[SNOOZE: 5, 0]");
        assert_eq!(
            actions,
            vec![BeeAction::Snooze {
                signal_id: 5,
                hours: 0,
            }]
        );
    }

    #[test]
    fn test_parse_snooze_negative_id_rejected() {
        // Negative signal_id should still parse (i64 allows it)
        let actions = parse_actions("[SNOOZE: -1, 2]");
        assert_eq!(
            actions,
            vec![BeeAction::Snooze {
                signal_id: -1,
                hours: 2,
            }]
        );
    }

    #[test]
    fn test_parse_snooze_non_numeric_hours_rejected() {
        assert!(parse_actions("[SNOOZE: 5, tomorrow]").is_empty());
    }

    // ── Unclosed bracket handling ──────────────────────────────────────

    #[test]
    fn test_parse_unclosed_bracket_ignored() {
        // No closing bracket — should produce no actions and not panic.
        assert!(parse_actions("[DISMISS: 42").is_empty());
        assert!(parse_actions("[ESCALATE: alert with no end").is_empty());
    }

    // ── Malformed marker detection ─────────────────────────────────────

    #[test]
    fn test_find_malformed_wrong_case() {
        let warnings = find_malformed_markers("[dismiss: 42]");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("wrong case"));
    }

    #[test]
    fn test_find_malformed_missing_colon() {
        let warnings = find_malformed_markers("[DISMISS 42]");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing `:`"));
    }

    #[test]
    fn test_find_malformed_multiple() {
        let text = "Try [dismiss: 1] and [TASK hello]";
        let warnings = find_malformed_markers(text);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn test_find_malformed_correct_markers_not_flagged() {
        let text = "[DISMISS: 1] [TASK: do it] [ESCALATE: problem]";
        let warnings = find_malformed_markers(text);
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn test_find_malformed_canvas_no_colon_ok() {
        // [CANVAS] is special — no colon expected.
        let warnings = find_malformed_markers("[CANVAS]\ncontent\n[/CANVAS]");
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn test_find_malformed_unknown_tag_ignored() {
        // Unknown tags like [FOO] should not produce warnings.
        let warnings = find_malformed_markers("[FOO: bar] [UNKNOWN: baz]");
        assert!(warnings.is_empty());
    }

    // ── Widget blocks ──────────────────────────────────────────────────

    #[test]
    fn test_parse_widget_basic() {
        let text = "[WIDGET: ci-status]\n{\"type\":\"stat\",\"value\":\"3\"}\n[/WIDGET]";
        let actions = parse_actions(text);
        assert_eq!(
            actions,
            vec![BeeAction::Widget {
                slot: "ci-status".to_string(),
                widget_json: "{\"type\":\"stat\",\"value\":\"3\"}".to_string(),
                ttl_minutes: None,
            }]
        );
    }

    #[test]
    fn test_parse_widget_with_ttl() {
        let text = "[WIDGET: deploy-health | 60]\n{\"type\":\"stat\",\"value\":\"ok\"}\n[/WIDGET]";
        let actions = parse_actions(text);
        assert_eq!(
            actions,
            vec![BeeAction::Widget {
                slot: "deploy-health".to_string(),
                widget_json: "{\"type\":\"stat\",\"value\":\"ok\"}".to_string(),
                ttl_minutes: Some(60),
            }]
        );
    }

    #[test]
    fn test_parse_widget_multiline_json() {
        let text = "[WIDGET: pr-queue]\n{\n  \"type\": \"list\",\n  \"items\": []\n}\n[/WIDGET]";
        let actions = parse_actions(text);
        assert_eq!(actions.len(), 1);
        if let BeeAction::Widget { slot, ttl_minutes, .. } = &actions[0] {
            assert_eq!(slot, "pr-queue");
            assert!(ttl_minutes.is_none());
        } else {
            panic!("expected Widget action");
        }
    }

    #[test]
    fn test_parse_widget_empty_body_ignored() {
        let text = "[WIDGET: ci-status]\n   \n[/WIDGET]";
        let actions = parse_actions(text);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_widget_unclosed_ignored() {
        let text = "[WIDGET: ci-status]\n{\"type\":\"stat\"}";
        let actions = parse_actions(text);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_widget_mixed_with_other_actions() {
        let text = "[DISMISS: 5]\n[WIDGET: ci-status]\n{\"type\":\"stat\"}\n[/WIDGET]\n[TASK: Fix CI]";
        let actions = parse_actions(text);
        assert_eq!(actions.len(), 3);
        // Regular markers (DISMISS, TASK) are collected before block markers (WIDGET).
        assert_eq!(actions[0], BeeAction::Dismiss { signal_id: 5 });
        assert_eq!(actions[1], BeeAction::Task { title: "Fix CI".to_string() });
        assert!(matches!(actions[2], BeeAction::Widget { .. }));
    }
}

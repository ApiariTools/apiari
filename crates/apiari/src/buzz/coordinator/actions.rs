//! Bee action marker parsing.
//!
//! When a Bee processes a signal (via hook or heartbeat), its response can
//! include action markers that the system executes. This module parses those
//! markers from the response text.

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
}

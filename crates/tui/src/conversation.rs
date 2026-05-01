//! Shared conversation types and rendering for agent TUI views.
//!
//! Used by both `swarm` (live agent TUI) and `apiari` (worker detail chat view).

use crate::markdown;
use crate::theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// A rendered conversation entry in the TUI.
#[derive(Debug, Clone)]
pub enum ConversationEntry {
    /// User message.
    User { text: String, timestamp: String },
    /// Assistant text block (may be streamed incrementally).
    AssistantText { text: String, timestamp: String },
    /// A tool call with its result.
    ToolCall {
        tool: String,
        input: String,
        output: Option<String>,
        is_error: bool,
        collapsed: bool,
    },
    /// Assistant message that requires user response.
    Question { text: String, timestamp: String },
    /// Status message (e.g. "Session started", "Rate limited").
    Status { text: String },
}

/// Render conversation entries into ratatui lines.
///
/// This is the shared "turn entries -> Lines" logic used by both swarm's agent TUI
/// and apiari's worker detail view. Each caller handles scroll + frame rendering.
///
/// `focused_tool` is the index of the currently focused ToolCall entry (for
/// keyboard navigation in swarm's TUI). Pass `None` when focus isn't relevant.
///
/// Returns an entry-line map: `Vec<(start_line, line_count)>` per entry,
/// useful for scroll-to-focus calculations.
pub fn render_conversation<'a>(
    lines: &mut Vec<Line<'a>>,
    entries: &'a [ConversationEntry],
    focused_tool: Option<usize>,
    assistant_label: Option<&str>,
) -> Vec<(u32, u32)> {
    let label = assistant_label.unwrap_or("Claude");
    let mut last_shown_ts = String::new();
    let mut entry_line_map: Vec<(u32, u32)> = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter().enumerate() {
        let start = lines.len() as u32;
        let is_focused = focused_tool == Some(i);
        match entry {
            ConversationEntry::User { text, timestamp } => {
                // Divider before user messages (visual turn boundary)
                if i > 0 {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        format!("  {}", "\u{2500}".repeat(40)),
                        Style::default().fg(theme::STEEL),
                    )));
                }
                lines.push(Line::from(""));
                let ts_span = dedup_timestamp(timestamp, &mut last_shown_ts);
                lines.push(Line::from(vec![
                    Span::styled(
                        "  You:",
                        Style::default()
                            .fg(theme::HONEY)
                            .add_modifier(Modifier::BOLD),
                    ),
                    ts_span,
                ]));
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", line),
                        theme::text(),
                    )));
                }
            }
            ConversationEntry::AssistantText { text, timestamp } => {
                let in_same_turn = is_continuation_of_assistant_turn(entries, i);
                if !in_same_turn {
                    lines.push(Line::from(""));
                    let ts_span = dedup_timestamp(timestamp, &mut last_shown_ts);
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {label}:"),
                            Style::default()
                                .fg(theme::MINT)
                                .add_modifier(Modifier::BOLD),
                        ),
                        ts_span,
                    ]));
                }
                lines.extend(markdown::render_markdown(text));
            }
            ConversationEntry::ToolCall {
                tool,
                input,
                output,
                is_error,
                collapsed,
            } => {
                let focus_prefix = if is_focused { "\u{25b6} " } else { "  " };
                let tool_style_expanded = if is_focused {
                    Style::default()
                        .fg(theme::HONEY)
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme::tool_name()
                };
                if *collapsed {
                    let (icon, icon_style) = if output.is_none() {
                        ("\u{22ef}", theme::muted())
                    } else if *is_error {
                        ("\u{2716}", theme::error())
                    } else {
                        ("\u{2714}", Style::default().fg(theme::STEEL))
                    };
                    let collapsed_tool_style = if is_focused {
                        Style::default()
                            .fg(theme::HONEY)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme::STEEL)
                    };
                    let preview = input
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(50)
                        .collect::<String>();
                    let ellipsis = if input.lines().next().is_some_and(|l| l.len() > 50) {
                        "..."
                    } else {
                        ""
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{}{} ", focus_prefix, icon), icon_style),
                        Span::styled(tool.as_str(), collapsed_tool_style),
                        Span::styled(
                            format!("  {}{}", preview, ellipsis),
                            Style::default().fg(theme::STEEL),
                        ),
                    ]));
                } else {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::styled(focus_prefix, theme::muted()),
                        Span::styled(format!(" {} ", tool), tool_style_expanded),
                        Span::styled(
                            " \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                            Style::default().fg(theme::STEEL),
                        ),
                    ]));
                    for line in input.lines().take(5) {
                        lines.push(Line::from(Span::styled(
                            format!("  \u{2502} {}", line),
                            Style::default().fg(theme::SLATE),
                        )));
                    }
                    if input.lines().count() > 5 {
                        lines.push(Line::from(Span::styled(
                            format!("  \u{2502} ... ({} more lines)", input.lines().count() - 5),
                            theme::muted(),
                        )));
                    }
                    if let Some(out) = output {
                        let out_style = if *is_error {
                            theme::error()
                        } else {
                            theme::muted()
                        };
                        lines.push(Line::from(Span::styled(
                            "  \u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                            Style::default().fg(theme::STEEL),
                        )));
                        for line in out.lines().take(10) {
                            lines.push(Line::from(Span::styled(
                                format!("  \u{2502} {}", line),
                                out_style,
                            )));
                        }
                        if out.lines().count() > 10 {
                            lines.push(Line::from(Span::styled(
                                format!("  \u{2502} ... ({} more lines)", out.lines().count() - 10),
                                theme::muted(),
                            )));
                        }
                    }
                    lines.push(Line::from(Span::styled(
                        "  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                        Style::default().fg(theme::STEEL),
                    )));
                }
            }
            ConversationEntry::Question { text, timestamp } => {
                let in_same_turn = is_continuation_of_assistant_turn(entries, i);
                if !in_same_turn {
                    lines.push(Line::from(""));
                    let ts_span = dedup_timestamp(timestamp, &mut last_shown_ts);
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  \u{2753} {label}:"),
                            Style::default()
                                .fg(theme::HONEY)
                                .add_modifier(Modifier::BOLD),
                        ),
                        ts_span,
                    ]));
                } else {
                    // Even in continuation mode, show a visual marker so
                    // action-needed messages are always distinguishable.
                    lines.push(Line::from(Span::styled(
                        "  \u{2753}",
                        Style::default()
                            .fg(theme::HONEY)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                lines.extend(markdown::render_markdown(text));
            }
            ConversationEntry::Status { text } => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  {}", text),
                    theme::muted(),
                )));
            }
        }
        let count = lines.len() as u32 - start;
        entry_line_map.push((start, count));
    }

    entry_line_map
}

/// Check if entry at `idx` is a continuation of an assistant turn (looking past tool calls).
fn is_continuation_of_assistant_turn(entries: &[ConversationEntry], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    for j in (0..idx).rev() {
        match &entries[j] {
            ConversationEntry::AssistantText { .. } | ConversationEntry::Question { .. } => {
                return true;
            }
            ConversationEntry::ToolCall { .. } => continue,
            _ => return false,
        }
    }
    false
}

/// Show timestamp only if it differs from the last shown one.
fn dedup_timestamp<'a>(timestamp: &'a str, last_shown: &mut String) -> Span<'a> {
    if timestamp == last_shown.as_str() || timestamp.is_empty() {
        Span::raw("")
    } else {
        *last_shown = timestamp.to_string();
        Span::styled(
            format!("  {}", timestamp),
            Style::default().fg(theme::SMOKE),
        )
    }
}

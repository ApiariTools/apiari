//! Rendering logic for the keeper dashboard.

use super::app::{App, Overlay, Panel};
use super::theme;
use crate::keeper::discovery::{AgentEventEntry, Severity};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

/// Main draw entry point.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Background fill
    frame.render_widget(Block::default().style(theme::overlay_bg()), area);

    // Overall layout: header (3 rows) + body + buzz bar (if signals) + footer (1 row).
    let has_buzz = app.buzz.as_ref().is_some_and(|b| !b.signals.is_empty());
    let buzz_height = if has_buzz { 3 } else { 0 };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),           // header
            Constraint::Min(0),              // body
            Constraint::Length(buzz_height), // buzz signals bar
            Constraint::Length(1),           // footer
        ])
        .split(area);

    draw_header(frame, outer[0], app);
    draw_body(frame, outer[1], app);

    if has_buzz {
        draw_buzz_bar(frame, outer[2], app);
    }

    draw_footer(frame, outer[3], app);

    // Overlays
    if app.overlay == Overlay::Help {
        draw_help_overlay(frame, area);
    } else if app.overlay == Overlay::EventDetail {
        draw_event_detail_overlay(frame, area, app);
    }
}

// ── Header ────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let (alive, total) = app.total_agents();

    let mut title_spans = vec![
        Span::styled(" keeper ", theme::logo()),
        Span::styled("-- swarm dashboard", theme::subtitle()),
    ];

    if total > 0 {
        let status_style = if alive > 0 {
            theme::status_running()
        } else {
            theme::status_idle()
        };
        title_spans.push(Span::styled(
            format!("  [{}/{}]", alive, total),
            status_style,
        ));
    }

    let title_line = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(theme::border());

    let header = Paragraph::new(title_line).block(block);
    frame.render_widget(header, area);
}

// ── Footer ────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, area: Rect, _app: &App) {
    let hints = Line::from(vec![
        Span::styled(" q", theme::key_hint()),
        Span::styled(" quit  ", theme::key_desc()),
        Span::styled("j/k", theme::key_hint()),
        Span::styled(" navigate  ", theme::key_desc()),
        Span::styled("Tab", theme::key_hint()),
        Span::styled(" panel  ", theme::key_desc()),
        Span::styled("\u{21b5}", theme::key_hint()),
        Span::styled(" jump  ", theme::key_desc()),
        Span::styled("e", theme::key_hint()),
        Span::styled(" events  ", theme::key_desc()),
        Span::styled("r", theme::key_hint()),
        Span::styled(" refresh  ", theme::key_desc()),
        Span::styled("?", theme::key_hint()),
        Span::styled(" help", theme::key_desc()),
    ]);

    let footer = Paragraph::new(hints);
    frame.render_widget(footer, area);
}

// ── Body ──────────────────────────────────────────────────

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    if app.sessions.is_empty() {
        draw_empty(frame, area);
        return;
    }

    // Responsive: if terminal is very narrow, give more space to detail panel.
    let (left_pct, right_pct) = if area.width < 80 {
        (40, 60)
    } else if area.width < 120 {
        (35, 65)
    } else {
        (30, 70)
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Percentage(right_pct),
        ])
        .split(area);

    draw_session_list(frame, chunks[0], app);
    draw_detail_panel(frame, chunks[1], app);
}

// ── Empty State ───────────────────────────────────────────

fn draw_empty(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "  No active swarm sessions found.",
            theme::muted(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Start a swarm to see it here:",
            theme::subtitle(),
        )),
        Line::from(Span::styled("  $ swarm", theme::accent())),
        Line::from(""),
        Line::from(Span::styled(
            "  Sessions refresh automatically every 5s.",
            theme::subtitle(),
        )),
        Line::from(Span::styled(
            "  Press r to force refresh, q to quit.",
            theme::subtitle(),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(" sessions ", theme::title()));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

// ── Session List ──────────────────────────────────────────

fn draw_session_list(frame: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.panel == Panel::Sessions;

    let border_style = if is_focused {
        theme::border_active()
    } else {
        theme::border()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" sessions ({}) ", app.sessions.len()),
            theme::title(),
        ));

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, session)| {
            let wt_count = session.worktrees.len();
            let alive_count = session.worktrees.iter().filter(|w| w.agent_alive).count();
            let dead_count = wt_count - alive_count;

            // Extract project name from session_name (strip "swarm-" prefix).
            let project = session
                .session_name
                .strip_prefix("swarm-")
                .unwrap_or(&session.session_name);

            let is_selected = i == app.selected_session;

            // Selection marker
            let marker = if is_selected {
                Span::styled("\u{25b8} ", theme::accent())
            } else {
                Span::raw("  ")
            };

            // Agent count with color coding
            let count_style = if alive_count > 0 && dead_count == 0 {
                theme::status_running() // all alive = green
            } else if alive_count > 0 {
                theme::status_pending() // mixed = yellow
            } else if wt_count > 0 {
                theme::status_dead() // all dead = red
            } else {
                theme::status_idle() // no worktrees = gray
            };

            // Line 1: project name + agent count
            let line1 = Line::from(vec![
                marker,
                Span::styled(
                    project.to_string(),
                    if is_selected {
                        theme::selected()
                    } else {
                        theme::text()
                    },
                ),
                Span::styled(format!(" {}/{}", alive_count, wt_count), count_style),
            ]);

            // Line 2: project directory (muted, truncated)
            let dir_display = truncate(
                &session.project_dir.display().to_string(),
                area.width.saturating_sub(6) as usize,
            );
            let line2 = Line::from(vec![
                Span::raw("  "),
                Span::styled(dir_display, theme::muted()),
            ]);

            ListItem::new(vec![line1, line2])
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

// ── Detail Panel ──────────────────────────────────────────

fn draw_detail_panel(frame: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.panel == Panel::Worktrees;

    let border_style = if is_focused {
        theme::border_active()
    } else {
        theme::border()
    };

    let Some(session) = app.current_session() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(" worktrees ", theme::title()));
        frame.render_widget(block, area);
        return;
    };

    let project = session
        .session_name
        .strip_prefix("swarm-")
        .unwrap_or(&session.session_name);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {} worktrees ({}) ", project, session.worktrees.len()),
            theme::title(),
        ));

    if session.worktrees.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No worktrees in this session.",
                theme::muted(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Agents will appear here when started in swarm.",
                theme::subtitle(),
            )),
        ])
        .block(block);
        frame.render_widget(msg, area);
        return;
    }

    // Calculate available width inside the block borders
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = session
        .worktrees
        .iter()
        .enumerate()
        .map(|(i, wt)| {
            let is_selected = is_focused && i == app.selected_worktree;

            // Status indicator
            let (status_icon, status_label, status_style) = if wt.agent_alive {
                ("\u{25cf}", "running", theme::status_running())
            } else if wt.agent_pane_id.is_some() {
                ("\u{25cb}", "done", theme::status_dead())
            } else {
                ("\u{25cb}", "no agent", theme::status_idle())
            };

            // Selection marker
            let marker = if is_selected {
                Span::styled("\u{25b8} ", theme::accent())
            } else {
                Span::raw("  ")
            };

            let branch_style = if is_selected {
                theme::selected()
            } else {
                theme::text()
            };

            // Line 1: marker + status icon + branch + [agent kind] + status label
            let mut line1_spans = vec![
                marker,
                Span::styled(format!("{} ", status_icon), status_style),
                Span::styled(
                    truncate(&wt.branch, inner_width.saturating_sub(25)),
                    branch_style,
                ),
                Span::styled(format!(" [{}]", wt.agent_kind), theme::agent_color()),
                Span::styled(format!(" {}", status_label), status_style),
            ];

            // Terminal count if any
            if wt.terminal_count > 0 {
                line1_spans.push(Span::styled(
                    format!(" +{}t", wt.terminal_count),
                    theme::muted(),
                ));
            }

            let line1 = Line::from(line1_spans);

            // Line 2: prompt (truncated)
            let prompt_display = truncate(&wt.prompt, inner_width.saturating_sub(6));
            let line2 = Line::from(vec![
                Span::raw("      "),
                Span::styled(prompt_display, theme::muted()),
            ]);

            let mut lines = vec![line1, line2];

            // Line 3: summary if present
            if let Some(ref summary) = wt.summary {
                let summary_display = truncate(summary, inner_width.saturating_sub(9));
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("\u{2192} {}", summary_display), theme::subtitle()),
                ]));
            }

            // Line 4: PR info if present
            if let Some(ref pr) = wt.pr {
                let pr_style = match pr.state.as_str() {
                    "MERGED" => theme::pr_merged(),
                    "OPEN" => theme::pr_open(),
                    "CLOSED" => theme::pr_closed(),
                    _ => theme::muted(),
                };
                let pr_title = truncate(&pr.title, inner_width.saturating_sub(20));
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("PR #{}", pr.number), pr_style),
                    Span::styled(format!(" {}", pr.state.to_lowercase()), pr_style),
                    Span::styled(format!(" - {}", pr_title), theme::muted()),
                ]));
                let url_display = truncate(&pr.url, inner_width.saturating_sub(6));
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(url_display, theme::accent()),
                ]));
            }

            // Line 5: created_at if available
            if let Some(created) = wt.created_at {
                let elapsed = chrono::Local::now().signed_duration_since(created);
                let ago = if elapsed.num_minutes() < 1 {
                    "just now".to_string()
                } else if elapsed.num_minutes() < 60 {
                    format!("{}m ago", elapsed.num_minutes())
                } else if elapsed.num_hours() < 24 {
                    format!("{}h ago", elapsed.num_hours())
                } else {
                    format!("{}d ago", elapsed.num_days())
                };
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("started {}", ago), theme::muted()),
                ]));
            }

            // Blank line for spacing between worktrees
            lines.push(Line::from(""));

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

// ── Buzz Signal Bar ───────────────────────────────────────

fn draw_buzz_bar(frame: &mut Frame, area: Rect, app: &App) {
    let Some(ref buzz) = app.buzz else {
        return;
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(" buzz signals ", theme::title()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Summary line: counts by severity
    let mut summary_spans = vec![Span::styled(" ", theme::text())];

    if buzz.critical_count > 0 {
        summary_spans.push(Span::styled(
            format!("{} critical", buzz.critical_count),
            theme::severity_critical(),
        ));
        summary_spans.push(Span::styled("  ", theme::text()));
    }
    if buzz.warning_count > 0 {
        summary_spans.push(Span::styled(
            format!("{} warning", buzz.warning_count),
            theme::severity_warning(),
        ));
        summary_spans.push(Span::styled("  ", theme::text()));
    }
    if buzz.info_count > 0 {
        summary_spans.push(Span::styled(
            format!("{} info", buzz.info_count),
            theme::severity_info(),
        ));
    }

    // Show the most recent signal title on the right
    if let Some(latest) = buzz.signals.first() {
        let sev_style = match latest.severity {
            Severity::Critical => theme::severity_critical(),
            Severity::Warning => theme::severity_warning(),
            Severity::Info => theme::severity_info(),
        };
        let max_title = inner.width.saturating_sub(40) as usize;
        summary_spans.push(Span::styled(" | ", theme::muted()));
        summary_spans.push(Span::styled(
            format!("[{}] ", latest.source),
            theme::muted(),
        ));
        summary_spans.push(Span::styled(truncate(&latest.title, max_title), sev_style));
    }

    let line = Line::from(summary_spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, inner);
}

// ── Help Overlay ──────────────────────────────────────────

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let popup_width = 50u16.min(area.width.saturating_sub(4));
    let popup_height = 18u16.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(" help ", theme::title()))
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .style(theme::overlay_bg());

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let keys = vec![
        ("j/k", "navigate up/down"),
        ("\u{2191}/\u{2193}", "navigate up/down"),
        ("Tab", "switch panels"),
        ("\u{21b5}", "jump to agent"),
        ("e", "agent event detail"),
        ("r", "force refresh"),
        ("?", "toggle this help"),
        ("Esc", "back / dismiss"),
        ("q", "quit keeper"),
        ("Ctrl+C", "force quit"),
    ];

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled("  keeper -- swarm dashboard", theme::accent())),
        Line::from(""),
        Line::from(Span::styled("  Keybindings:", theme::text())),
        Line::from(""),
    ];

    for (key, desc) in &keys {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:>8} ", key), theme::key_hint()),
            Span::styled(*desc, theme::key_desc()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  v{}", env!("CARGO_PKG_VERSION")),
        theme::muted(),
    )));

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ── Event Detail Overlay ──────────────────────────────────

fn draw_event_detail_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let Some(ref detail) = app.event_detail else {
        return;
    };

    // Use most of the screen.
    let popup_width = (area.width - 4).min(100);
    let popup_height = (area.height - 4).min(40);
    let popup = centered_rect(popup_width, popup_height, area);
    frame.render_widget(Clear, popup);

    let short_branch = detail
        .branch
        .strip_prefix("swarm/")
        .unwrap_or(&detail.branch);

    let block = Block::default()
        .title(Span::styled(
            format!(" events — {} ", short_branch),
            theme::title(),
        ))
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .style(theme::overlay_bg());

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if detail.events.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No events recorded for this agent.",
                theme::muted(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Events appear when the agent uses the ClaudeTui protocol.",
                theme::subtitle(),
            )),
        ]);
        frame.render_widget(msg, inner);
        return;
    }

    let inner_width = inner.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for event in &detail.events {
        let line = format_event_line(event, inner_width);
        lines.push(line);
    }

    // Show most recent events at the bottom (scroll to end).
    let visible_height = inner.height as usize;
    if lines.len() > visible_height {
        lines.drain(..lines.len() - visible_height);
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

/// Format a single agent event as a colored Line.
fn format_event_line(event: &AgentEventEntry, max_width: usize) -> Line<'static> {
    let ts = event
        .timestamp
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "        ".into());

    let (icon, detail) = match event.r#type.as_str() {
        "assistant_text" => {
            let text = event
                .text
                .as_deref()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            let max = max_width.saturating_sub(14);
            let display = truncate(text, max);
            ("\u{270e}", display) // ✎
        }
        "tool_use" => {
            let tool = event.tool.as_deref().unwrap_or("?");
            ("\u{2699}", tool.to_string()) // ⚙
        }
        "tool_result" => {
            let tool = event.tool.as_deref().unwrap_or("?");
            let suffix = if event.is_error { " error" } else { " done" };
            ("\u{2714}", format!("{tool}{suffix}")) // ✔
        }
        "error" => {
            let msg = event
                .message
                .as_deref()
                .or(event.text.as_deref())
                .unwrap_or("unknown error");
            let max = max_width.saturating_sub(14);
            let display = truncate(msg, max);
            ("\u{2718}", display) // ✘
        }
        "complete" => {
            let mut parts = Vec::new();
            if let Some(turns) = event.turns {
                parts.push(format!("{turns} turns"));
            }
            if let Some(cost) = event.cost_usd {
                parts.push(format!("${cost:.4}"));
            }
            let detail = if parts.is_empty() {
                "done".to_string()
            } else {
                parts.join(", ")
            };
            ("\u{2713}", detail) // ✓
        }
        "start" => ("\u{25b6}", "session started".to_string()), // ▶
        other => ("\u{00b7}", other.to_string()),               // ·
    };

    let type_style = match event.r#type.as_str() {
        "error" => theme::severity_critical(),
        "tool_use" => theme::accent(),
        "tool_result" => theme::status_running(),
        "complete" => theme::severity_warning(),
        "start" => theme::accent(),
        _ => theme::text(),
    };

    Line::from(vec![
        Span::styled(format!(" {ts} "), theme::muted()),
        Span::styled(format!("{icon} "), type_style),
        Span::styled(detail, type_style),
    ])
}

// ── Helpers ───────────────────────────────────────────────

/// Center a rectangle of given size within a container area.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Truncate a string to at most `max_len` characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    // Take only the first line for display.
    let first_line = s.lines().next().unwrap_or(s);

    if first_line.len() <= max_len {
        first_line.to_string()
    } else if max_len > 3 {
        let mut result: String = first_line.chars().take(max_len - 3).collect();
        result.push_str("...");
        result
    } else {
        first_line.chars().take(max_len).collect()
    }
}

//! Rendering logic for the unified hive TUI.

use super::app::{
    App, ChatLine, DispatchPhase, DispatchState, DispatchStatus, Panel, PrDetailInfo, RepoStage,
    SidebarItem,
};
use super::markdown;
use crate::keeper::discovery::WorktreeInfo;
use crate::keeper::tui::theme;
use chrono::Local;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

/// Braille spinner animation frames.
const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Main draw entry point.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Background fill.
    frame.render_widget(Block::default().style(theme::overlay_bg()), area);

    // Overall layout: tab bar (1) + body.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // body
        ])
        .split(area);

    draw_tab_bar(frame, outer[0], app);
    draw_body(frame, outer[1], app);

    // Floating overlays (rendered on top of body).
    if let Some(ref pr) = app.pr_detail {
        draw_pr_detail_overlay(frame, area, pr);
    }
    if app.show_help {
        draw_help_overlay(frame, area);
    }
}

// ── Tab Bar ──────────────────────────────────────────────

fn draw_tab_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();

    for (i, ws) in app.workspaces.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", theme::muted()));
        }
        if i == app.active_tab {
            spans.push(Span::styled(format!(" {} ", ws.name), theme::highlight()));
        } else {
            spans.push(Span::styled(format!(" {} ", ws.name), theme::muted()));
        }
    }

    // Right-aligned status.
    let conn_indicator = if app.daemon_connected {
        "\u{25cf}"
    } else {
        "\u{25cb}"
    };
    let status_text = format!(" {conn_indicator} hive ui  ^b n/p:tab  q:quit ");
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let status_len = status_text.chars().count();
    let padding = (area.width as usize)
        .saturating_sub(used.min(area.width as usize))
        .saturating_sub(status_len.min(area.width as usize));
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.push(Span::styled(status_text, theme::subtitle()));

    let line = Line::from(spans);
    let bar = Paragraph::new(line);
    frame.render_widget(bar, area);
}

// ── Body ─────────────────────────────────────────────────

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    if app.zoomed {
        // Full-width content panel (no sidebar).
        draw_content_panel(frame, area, app);
        return;
    }

    // Responsive split: wider sidebar for 3-line rows.
    let left_pct = if area.width > 120 {
        20
    } else if area.width > 80 {
        24
    } else {
        30
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Percentage(100 - left_pct),
        ])
        .split(area);

    draw_sidebar(frame, columns[0], app);
    draw_content_panel(frame, columns[1], app);
}

// ── Sidebar (borderless, swarm-style) ────────────────────

fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // list
            Constraint::Length(1), // divider
            Constraint::Length(1), // status bar
        ])
        .split(area);

    draw_sidebar_list(frame, chunks[0], app);
    draw_sidebar_divider(frame, chunks[1]);
    draw_sidebar_status_bar(frame, chunks[2], app);
}

fn draw_sidebar_divider(frame: &mut Frame, area: Rect) {
    let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let divider = Paragraph::new("\u{2500}".repeat(inner.width as usize))
        .style(Style::default().fg(theme::WAX));
    frame.render_widget(divider, inner);
}

fn draw_sidebar_list(frame: &mut Frame, area: Rect, app: &App) {
    let workers = app.flat_workers();
    let has_dispatch = app.active_dispatch.is_some();
    let viewport = area.height as usize;

    // Build item heights: Chat=2, Dispatch=2 (optional), each Worker=3
    let mut heights: Vec<usize> = vec![2]; // Chat item
    if has_dispatch {
        heights.push(2); // Dispatch item
    }
    if workers.is_empty() {
        heights.push(1); // "No workers" line
    } else {
        heights.extend(std::iter::repeat_n(3, workers.len()));
    }

    // Cumulative top positions
    let mut tops = Vec::with_capacity(heights.len());
    let mut cumulative = 0usize;
    for &h in &heights {
        tops.push(cumulative);
        cumulative += h;
    }
    let total_height = cumulative;

    // Find selected item index in the heights array
    let dispatch_offset = usize::from(has_dispatch);
    let selected_item = match app.sidebar_selection {
        SidebarItem::Chat => 0,
        SidebarItem::Dispatch => 1, // only valid when has_dispatch
        SidebarItem::Worker(i) => {
            if workers.is_empty() {
                0
            } else {
                1 + dispatch_offset + i
            }
        }
    };

    // Adjust scroll to keep selected item visible
    let mut scroll = app.sidebar_scroll.get();
    if selected_item < heights.len() {
        let sel_top = tops[selected_item];
        let sel_bottom = sel_top + heights[selected_item];

        if sel_top < scroll {
            scroll = sel_top;
        } else if sel_bottom > scroll + viewport {
            scroll = sel_bottom.saturating_sub(viewport);
        }
    }

    if total_height <= viewport {
        scroll = 0;
    } else {
        scroll = scroll.min(total_height - viewport);
    }
    app.sidebar_scroll.set(scroll);

    // Render visible items
    let mut render_y = area.y;
    let viewport_bottom = area.y + area.height;

    // Generic render helper for an item in the heights array.
    let mut render_item =
        |item_idx: usize, render_y: &mut u16, draw_fn: &dyn Fn(&mut Frame, Rect)| {
            let item_top = tops[item_idx];
            let item_bottom = item_top + heights[item_idx];
            if item_bottom > scroll && *render_y < viewport_bottom {
                let skip_top = scroll.saturating_sub(item_top);
                let available = (viewport_bottom - *render_y) as usize;
                let render_h = (heights[item_idx] - skip_top).min(available);
                if render_h > 0 {
                    let rect = Rect::new(area.x, *render_y, area.width, render_h as u16);
                    *render_y += render_h as u16;
                    draw_fn(frame, rect);
                }
            }
        };

    // Item 0: Chat
    render_item(0, &mut render_y, &|f, r| draw_chat_sidebar_item(f, r, app));

    // Item 1 (optional): Dispatch
    if has_dispatch {
        render_item(1, &mut render_y, &|f, r| {
            draw_dispatch_sidebar_item(f, r, app)
        });
    }

    // Workers
    let worker_start = 1 + dispatch_offset;
    if workers.is_empty() {
        // "No workers" placeholder
        if render_y < viewport_bottom {
            let rect = Rect::new(area.x, render_y, area.width, 1);
            let line = Line::from(Span::styled("  No active workers", theme::muted()));
            frame.render_widget(Paragraph::new(line), rect);
        }
    } else {
        for (i, (_, wt)) in workers.iter().enumerate() {
            let item_idx = worker_start + i;
            let item_top = tops[item_idx];
            let item_bottom = item_top + heights[item_idx];

            if item_bottom <= scroll {
                continue;
            }
            if render_y >= viewport_bottom {
                break;
            }

            let skip_top = scroll.saturating_sub(item_top);
            let available = (viewport_bottom - render_y) as usize;
            let render_h = (heights[item_idx] - skip_top).min(available);

            if render_h == 0 {
                continue;
            }

            let rect = Rect::new(area.x, render_y, area.width, render_h as u16);
            render_y += render_h as u16;

            let is_selected = app.sidebar_selection == SidebarItem::Worker(i);
            draw_worker_sidebar_row(frame, rect, wt, is_selected, i);
        }
    }
}

/// 2-line chat entry in the sidebar.
fn draw_chat_sidebar_item(frame: &mut Frame, area: Rect, app: &App) {
    let is_selected = app.sidebar_selection == SidebarItem::Chat;

    let row_style = if is_selected {
        Style::default().bg(ratatui::style::Color::Rgb(58, 50, 42))
    } else {
        Style::default().bg(theme::COMB)
    };
    frame.render_widget(Paragraph::new("").style(row_style), area);

    let selector = if is_selected { "\u{25b8}" } else { " " };
    let text_style = if is_selected {
        theme::selected()
    } else {
        theme::text()
    };

    let icon = if app.streaming {
        Span::styled("\u{25c9} ", theme::status_running())
    } else {
        Span::styled("\u{25c8} ", theme::accent())
    };

    // Line 1: " ▸ ◈ Coordinator"
    if area.height >= 1 {
        let line1 = Line::from(vec![
            Span::styled(format!(" {selector}"), text_style),
            Span::styled(" ", Style::default()),
            icon,
            Span::styled("Coordinator", text_style),
        ]);
        frame.render_widget(
            Paragraph::new(line1).style(row_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    // Line 2: "     ready|streaming...|daemon"
    if area.height >= 2 {
        let status_text = if app.streaming {
            "streaming..."
        } else if app.daemon_connected {
            "daemon"
        } else {
            "local"
        };
        let line2 = Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                status_text,
                Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90)),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line2).style(row_style),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

/// 2-line dispatch entry in the sidebar.
fn draw_dispatch_sidebar_item(frame: &mut Frame, area: Rect, app: &App) {
    let is_selected = app.sidebar_selection == SidebarItem::Dispatch;

    let row_style = if is_selected {
        Style::default().bg(ratatui::style::Color::Rgb(58, 50, 42))
    } else {
        Style::default().bg(theme::COMB)
    };
    frame.render_widget(Paragraph::new("").style(row_style), area);

    let selector = if is_selected { "\u{25b8}" } else { " " };
    let text_style = if is_selected {
        theme::selected()
    } else {
        theme::text()
    };

    // Line 1: " ▸ ⚙ Dispatch"
    if area.height >= 1 {
        let line1 = Line::from(vec![
            Span::styled(format!(" {selector}"), text_style),
            Span::styled(" ", Style::default()),
            Span::styled("\u{2699} ", theme::accent()),
            Span::styled("Dispatch", text_style),
        ]);
        frame.render_widget(
            Paragraph::new(line1).style(row_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    // Line 2: status text with spinner for active stages
    if area.height >= 2 {
        let (status_text, status_style) = if let Some(ref d) = app.active_dispatch {
            match d.status {
                DispatchStatus::Refining => {
                    let spinner = SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()];
                    (
                        format!("{spinner} Refining..."),
                        Style::default().fg(theme::HONEY),
                    )
                }
                DispatchStatus::Running => {
                    let spinner = SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()];
                    // Find the first repo that isn't done yet.
                    let active = d
                        .per_repo
                        .iter()
                        .find(|r| r.stage != RepoStage::Done)
                        .map(|r| {
                            let stage = match r.stage {
                                RepoStage::Context => "context",
                                RepoStage::Planning => "plan",
                                RepoStage::Pending => "pending",
                                RepoStage::Done => "done",
                            };
                            format!("{}: {stage}", r.repo)
                        })
                        .unwrap_or_else(|| "running".into());
                    (
                        format!("{spinner} {active}"),
                        Style::default().fg(theme::HONEY),
                    )
                }
                DispatchStatus::Ready => {
                    let count = d.per_repo.len();
                    if count == 0 {
                        (
                            "no results".to_string(),
                            Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90)),
                        )
                    } else {
                        (
                            format!(
                                "\u{2713} {count} repo{} ready",
                                if count == 1 { "" } else { "s" }
                            ),
                            Style::default().fg(theme::POLLEN),
                        )
                    }
                }
            }
        } else {
            (
                String::new(),
                Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90)),
            )
        };

        let line2 = Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(status_text, status_style),
        ]);
        frame.render_widget(
            Paragraph::new(line2).style(row_style),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

/// 3-line worker row matching swarm's style.
fn draw_worker_sidebar_row(
    frame: &mut Frame,
    area: Rect,
    wt: &WorktreeInfo,
    selected: bool,
    idx: usize,
) {
    let wt_color = theme::SIDEBAR_COLORS[idx % theme::SIDEBAR_COLORS.len()];
    let bar = Span::styled("\u{258c}", Style::default().fg(wt_color));

    let row_style = if selected {
        Style::default().bg(ratatui::style::Color::Rgb(58, 50, 42))
    } else {
        Style::default().bg(theme::COMB)
    };

    // Background fill
    frame.render_widget(Paragraph::new("").style(row_style), area);

    let status_icon = match wt.phase.as_deref() {
        Some("creating") | Some("starting") => {
            Span::styled("\u{25cc} ", theme::status_waiting()) // ◌ amber
        }
        Some("running") => Span::styled("\u{25cf} ", theme::status_running()), // ● green
        Some("waiting") => Span::styled("\u{25cf} ", theme::status_waiting()), // ● amber
        Some("completed") => Span::styled("\u{25c6} ", theme::status_done()),  // ◆ grey
        Some("failed") => {
            Span::styled(
                "\u{2717} ",
                Style::default().fg(ratatui::style::Color::Rgb(200, 60, 60)),
            )
            // ✗ red
        }
        _ => {
            // Fallback for old state.json without phase field
            if wt.agent_alive {
                if wt.agent_session_status.as_deref() == Some("waiting") {
                    Span::styled("\u{25cf} ", theme::status_waiting())
                } else {
                    Span::styled("\u{25cf} ", theme::status_running())
                }
            } else if wt.summary.is_some() {
                Span::styled("\u{25c6} ", theme::status_done())
            } else {
                Span::styled("\u{25cb} ", theme::status_idle())
            }
        }
    };

    let selector = if selected { "\u{25b8}" } else { " " };
    let selector_style = if selected {
        theme::selected()
    } else {
        theme::muted()
    };
    let id_style = if selected {
        theme::selected()
    } else {
        theme::text()
    };

    // Line 1: ▌▸ ● worker-id  #PR
    if area.height >= 1 {
        let id_max = (area.width as usize).saturating_sub(10);
        let mut line1_spans = vec![
            bar.clone(),
            Span::styled(selector, selector_style),
            Span::styled(" ", Style::default()),
            status_icon,
            Span::styled(truncate(&wt.id, id_max), id_style),
        ];

        if let Some(ref pr) = wt.pr {
            let pr_badge = format!(" #{}", pr.number);
            let pr_style = match pr.state.as_str() {
                "MERGED" => theme::success(),
                "OPEN" => Style::default().fg(theme::MINT),
                _ => theme::muted(),
            };
            line1_spans.push(Span::styled(pr_badge, pr_style));
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1_spans)).style(row_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    // Line 2: ▌  agent-kind · 5m
    if area.height >= 2 {
        let ago = format_time_ago(wt);
        let line2 = Line::from(vec![
            bar.clone(),
            Span::styled("  ", Style::default()),
            Span::styled(&wt.agent_kind, theme::agent_color()),
            Span::styled(
                format!(" \u{00b7} {ago}"),
                Style::default().fg(ratatui::style::Color::Rgb(80, 77, 70)),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line2).style(row_style),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }

    // Line 3: ▌  summary/prompt text
    if area.height >= 3 {
        let max_len = (area.width as usize).saturating_sub(3);
        let (text, style) = if let Some(ref summary) = wt.summary {
            (
                truncate(summary, max_len),
                Style::default().fg(ratatui::style::Color::Rgb(140, 137, 130)),
            )
        } else if !wt.prompt.is_empty() {
            // Show first line of prompt as fallback
            let first = wt.prompt.lines().next().unwrap_or(&wt.prompt);
            (
                truncate(first, max_len),
                Style::default().fg(ratatui::style::Color::Rgb(90, 87, 80)),
            )
        } else {
            (String::new(), theme::muted())
        };

        let line3 = Line::from(vec![
            bar,
            Span::styled("  ", Style::default()),
            Span::styled(text, style),
        ]);
        frame.render_widget(
            Paragraph::new(line3).style(row_style),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
}

fn draw_sidebar_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    // Pending confirmation takes priority.
    if let Some(ref action) = app.pending_action {
        let msg = match action {
            super::app::PendingAction::CloseWorker(id) => format!(" Close {}? y/n", id),
        };
        let line = Line::from(Span::styled(
            msg,
            Style::default()
                .fg(theme::NECTAR)
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // Flash message.
    if let Some(ref flash) = app.flash_message {
        let line = Line::from(Span::styled(&flash.text[..], theme::accent()));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // Context-sensitive hints.
    let hints = if let Some(ref d) = app.dispatch {
        match d.phase {
            super::app::DispatchPhase::SelectRepos => Line::from(vec![
                Span::styled(" j", theme::key_hint()),
                Span::styled("/", theme::key_desc()),
                Span::styled("k", theme::key_hint()),
                Span::styled(" nav  ", theme::key_desc()),
                Span::styled("⎵", theme::key_hint()),
                Span::styled(" toggle  ", theme::key_desc()),
                Span::styled("↵", theme::key_hint()),
                Span::styled(" next  ", theme::key_desc()),
                Span::styled("esc", theme::key_hint()),
                Span::styled(" cancel", theme::key_desc()),
            ]),
            super::app::DispatchPhase::EnterPrompt => Line::from(vec![
                Span::styled(" ↵", theme::key_hint()),
                Span::styled(" submit  ", theme::key_desc()),
                Span::styled("alt+↵", theme::key_hint()),
                Span::styled(" newline  ", theme::key_desc()),
                Span::styled("esc", theme::key_hint()),
                Span::styled(" back", theme::key_desc()),
            ]),
        }
    } else if app.is_dispatch_selected() {
        Line::from(vec![
            Span::styled(" j", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("k", theme::key_hint()),
            Span::styled(" nav  ", theme::key_desc()),
            Span::styled("\u{21b5}", theme::key_hint()),
            Span::styled(" toggle  ", theme::key_desc()),
            Span::styled("c", theme::key_hint()),
            Span::styled(" collapse  ", theme::key_desc()),
            Span::styled("y", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("n", theme::key_hint()),
            Span::styled(" confirm  ", theme::key_desc()),
            Span::styled("?", theme::key_hint()),
            Span::styled(" help", theme::key_desc()),
        ])
    } else if app.is_chat_selected() {
        Line::from(vec![
            Span::styled(" j", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("k", theme::key_hint()),
            Span::styled(" nav  ", theme::key_desc()),
            Span::styled("l", theme::key_hint()),
            Span::styled(" focus  ", theme::key_desc()),
            Span::styled("q", theme::key_hint()),
            Span::styled(" quit  ", theme::key_desc()),
            Span::styled("?", theme::key_hint()),
            Span::styled(" help", theme::key_desc()),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ↵", theme::key_hint()),
            Span::styled(" enter  ", theme::key_desc()),
            Span::styled("x", theme::key_hint()),
            Span::styled(" close  ", theme::key_desc()),
            Span::styled("p", theme::key_hint()),
            Span::styled(" PR  ", theme::key_desc()),
            Span::styled("d", theme::key_hint()),
            Span::styled(" dispatch  ", theme::key_desc()),
            Span::styled("?", theme::key_hint()),
            Span::styled(" help", theme::key_desc()),
        ])
    };
    frame.render_widget(Paragraph::new(hints), area);
}

// ── Content Panel (contextual) ───────────────────────────

fn draw_content_panel(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(ref dispatch) = app.dispatch {
        draw_dispatch_overlay(frame, area, dispatch);
        return;
    }

    match app.sidebar_selection {
        SidebarItem::Chat => {
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),    // chat
                    Constraint::Length(3), // input bar
                ])
                .split(area);

            draw_chat_panel(frame, right[0], app);
            draw_input_bar(frame, right[1], app, "> ");
        }
        SidebarItem::Dispatch => {
            draw_dispatch_review(frame, area, app);
        }
        SidebarItem::Worker(_) => {
            draw_worker_output(frame, area, app);
        }
    }
}

// ── Dispatch Overlay ─────────────────────────────────────

fn draw_dispatch_overlay(frame: &mut Frame, area: Rect, dispatch: &DispatchState) {
    match dispatch.phase {
        DispatchPhase::SelectRepos => {
            let block = Block::default()
                .title(Span::styled(" Dispatch ", theme::title()))
                .borders(Borders::ALL)
                .border_style(theme::border_active());

            let inner = block.inner(area);
            frame.render_widget(block, area);

            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Select repos (Space to toggle):",
                theme::text(),
            )));
            lines.push(Line::from(""));

            for (i, repo) in dispatch.repos.iter().enumerate() {
                let check = if dispatch.selected[i] { "x" } else { " " };
                let cursor = if i == dispatch.cursor {
                    "\u{25b8}"
                } else {
                    " "
                };
                let style = if i == dispatch.cursor {
                    theme::selected()
                } else {
                    theme::text()
                };
                lines.push(Line::from(Span::styled(
                    format!("  {cursor} [{check}] {repo}"),
                    style,
                )));
            }

            let paragraph = Paragraph::new(lines);
            frame.render_widget(paragraph, inner);
        }
        DispatchPhase::EnterPrompt => {
            let selected: Vec<&str> = dispatch
                .repos
                .iter()
                .zip(dispatch.selected.iter())
                .filter(|&(_, sel)| *sel)
                .map(|(name, _)| name.as_str())
                .collect();
            let title = format!(" Dispatch to: {} ", selected.join(", "));

            let block = Block::default()
                .title(Span::styled(title, theme::title()))
                .borders(Borders::ALL)
                .border_style(theme::border_active());

            let inner = block.inner(area);
            frame.render_widget(block, area);

            // Calculate cursor position in multiline text.
            let buf_lines: Vec<&str> = dispatch.prompt.split('\n').collect();
            let mut cursor_line = 0usize;
            let mut cursor_col = 0usize;
            let mut pos = 0usize;
            for (i, line) in buf_lines.iter().enumerate() {
                let line_chars = line.chars().count();
                if pos + line_chars >= dispatch.prompt_cursor && i < buf_lines.len() {
                    cursor_line = i;
                    cursor_col = dispatch.prompt_cursor - pos;
                    break;
                }
                pos += line_chars + 1; // +1 for \n
            }

            // Build styled lines with cursor highlight.
            let mut styled_lines: Vec<Line> = Vec::new();
            for (i, line_str) in buf_lines.iter().enumerate() {
                let prefix = if i == 0 { " \u{25b8} " } else { "   " };
                let prefix_style = if i == 0 {
                    theme::accent()
                } else {
                    theme::muted()
                };

                if i == cursor_line {
                    let before: String = line_str.chars().take(cursor_col).collect();
                    let cursor_char = line_str.chars().nth(cursor_col).unwrap_or(' ');
                    let after: String = line_str.chars().skip(cursor_col + 1).collect();
                    styled_lines.push(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::styled(before, theme::text()),
                        Span::styled(
                            cursor_char.to_string(),
                            Style::default()
                                .fg(ratatui::style::Color::Rgb(30, 28, 25))
                                .bg(theme::HONEY),
                        ),
                        Span::styled(after, theme::text()),
                    ]));
                } else {
                    styled_lines.push(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::styled(line_str.to_string(), theme::text()),
                    ]));
                }
            }

            // Input area: leave 1 row for hint at the bottom.
            let input_height = inner.height.saturating_sub(1);
            let input_area = Rect::new(inner.x, inner.y, inner.width, input_height);

            let text = Text::from(styled_lines);
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), input_area);

            // Hint at bottom.
            let hint = Line::from(vec![
                Span::styled("\u{21b5}", theme::key_hint()),
                Span::styled(" submit  ", theme::key_desc()),
                Span::styled("alt+\u{21b5}", theme::key_hint()),
                Span::styled(" newline  ", theme::key_desc()),
                Span::styled("esc", theme::key_hint()),
                Span::styled(" back", theme::key_desc()),
            ]);
            let hint_area = Rect::new(
                inner.x + 1,
                inner.y + inner.height - 1,
                inner.width.saturating_sub(2),
                1,
            );
            frame.render_widget(Paragraph::new(hint), hint_area);
        }
    }
}

// ── Dispatch Review Panel ────────────────────────────────

fn draw_dispatch_review(frame: &mut Frame, area: Rect, app: &App) {
    let Some(ref dispatch) = app.active_dispatch else {
        return;
    };

    let border_style = if app.focus == Panel::Chat {
        theme::border_active()
    } else {
        theme::border()
    };

    let spinner = SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()];

    match dispatch.status {
        DispatchStatus::Refining | DispatchStatus::Running => {
            let title_text = if dispatch.status == DispatchStatus::Refining {
                format!(" Dispatch \u{2014} {spinner} refining... ")
            } else {
                " Dispatch ".to_string()
            };
            let block = Block::default()
                .title(Span::styled(title_text, theme::title()))
                .borders(Borders::ALL)
                .border_style(border_style);

            let inner = block.inner(area);
            frame.render_widget(block, area);

            let mut lines: Vec<Line> = Vec::new();

            // Task description
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                if dispatch.title.is_empty() {
                    format!("  {}", dispatch.task)
                } else {
                    format!("  {}", dispatch.title)
                },
                theme::accent(),
            )));
            lines.push(Line::from(""));

            // Refine status
            if dispatch.status == DispatchStatus::Refining {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {spinner} "), Style::default().fg(theme::HONEY)),
                    Span::styled("Refining task...", Style::default().fg(theme::HONEY)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("  \u{2713} ", Style::default().fg(theme::POLLEN)),
                    Span::styled("Refined", Style::default().fg(theme::POLLEN)),
                ]));
            }
            lines.push(Line::from(""));

            // Per-repo stage stepper (only in Running)
            if dispatch.status == DispatchStatus::Running {
                for r in &dispatch.per_repo {
                    // Repo header
                    let header = format!(
                        "  \u{2500}\u{2500} {} {}",
                        r.repo,
                        "\u{2500}".repeat(
                            (inner.width as usize)
                                .saturating_sub(6 + r.repo.len())
                                .min(40)
                        )
                    );
                    lines.push(Line::from(Span::styled(
                        header,
                        Style::default().fg(theme::WAX),
                    )));

                    // Context line
                    let (ctx_icon, ctx_style) = match r.stage {
                        RepoStage::Pending => {
                            ("\u{25CB}".to_string(), Style::default().fg(theme::SMOKE))
                        }
                        RepoStage::Context => {
                            (format!("{spinner}"), Style::default().fg(theme::HONEY))
                        }
                        RepoStage::Planning | RepoStage::Done => {
                            ("\u{2713}".to_string(), Style::default().fg(theme::POLLEN))
                        }
                    };
                    let ctx_detail = if let Some(file_count) = r.file_count
                        && matches!(r.stage, RepoStage::Planning | RepoStage::Done)
                    {
                        format!("  {} files", file_count)
                    } else if r.stage == RepoStage::Context {
                        "...".to_string()
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {ctx_icon} "), ctx_style),
                        Span::styled("Context", ctx_style),
                        Span::styled(
                            ctx_detail,
                            Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90)),
                        ),
                    ]));

                    // Plan line
                    let (plan_icon, plan_style) = match r.stage {
                        RepoStage::Pending | RepoStage::Context => {
                            ("\u{25CB}".to_string(), Style::default().fg(theme::SMOKE))
                        }
                        RepoStage::Planning => {
                            (format!("{spinner}"), Style::default().fg(theme::HONEY))
                        }
                        RepoStage::Done => {
                            ("\u{2713}".to_string(), Style::default().fg(theme::POLLEN))
                        }
                    };
                    let plan_detail = if let Some(step_count) = r.step_count
                        && r.stage == RepoStage::Done
                    {
                        format!("  {} steps", step_count)
                    } else if r.stage == RepoStage::Planning {
                        "...".to_string()
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {plan_icon} "), plan_style),
                        Span::styled("Plan", plan_style),
                        Span::styled(
                            plan_detail,
                            Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90)),
                        ),
                    ]));

                    lines.push(Line::from(""));
                }
            }

            let visible_height = inner.height as usize;
            let visible_lines: Vec<Line> = lines.into_iter().take(visible_height).collect();
            let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
        }
        DispatchStatus::Ready => {
            if dispatch.per_repo.is_empty() {
                let block = Block::default()
                    .title(Span::styled(" Dispatch Review ", theme::title()))
                    .borders(Borders::ALL)
                    .border_style(border_style);

                let inner = block.inner(area);
                frame.render_widget(block, area);

                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No pipeline results were generated.",
                        Style::default().fg(ratatui::style::Color::Rgb(200, 60, 60)),
                    )),
                    Line::from(Span::styled(
                        "  Try rephrasing your task or selecting different repos.",
                        theme::muted(),
                    )),
                    Line::from(""),
                    Line::from(Span::styled("  Press n to dismiss.", theme::muted())),
                ];
                frame.render_widget(Paragraph::new(lines), inner);
                return;
            }

            let hint = " y confirm  n cancel  j/k nav  \u{21b5} toggle  c collapse  u/d scroll ";
            let block = Block::default()
                .title(Span::styled(" Dispatch Review ", theme::title()))
                .title_bottom(Line::from(Span::styled(hint, theme::subtitle())).centered())
                .borders(Borders::ALL)
                .border_style(border_style);

            let inner = block.inner(area);
            frame.render_widget(block, area);

            let heading_style = Style::default().fg(theme::POLLEN);
            let dim_style = Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90));

            let mut lines: Vec<Line> = Vec::new();
            let mut focused_line: usize = 0; // line index of the focused section header

            // ── Section 0: Task ──
            let task_focused = dispatch.focused_section == 0;
            let task_collapsed = dispatch
                .sections
                .first()
                .map(|s| s.collapsed)
                .unwrap_or(false);
            let task_icon = if task_collapsed {
                "\u{25b6}"
            } else {
                "\u{25bc}"
            };
            let task_summary = format!("scope: {}", dispatch.repos.join(", "));
            let task_header_style = if task_focused {
                Style::default()
                    .fg(theme::HONEY)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::NECTAR)
                    .add_modifier(Modifier::BOLD)
            };

            if task_focused {
                focused_line = lines.len();
            }
            let task_cursor = if task_focused { "\u{25b8}" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(format!("{task_cursor}{task_icon} "), task_header_style),
                Span::styled("Task", task_header_style),
                Span::styled(format!("  {task_summary}"), dim_style),
            ]));

            if !task_collapsed && !dispatch.task_md.is_empty() {
                // Show Description + Scope + Acceptance Criteria
                let mut in_section = false;
                let mut blank_run = 0u8;
                for md_line in dispatch.task_md.lines() {
                    let trimmed = md_line.trim();
                    if trimmed.starts_with("## ") {
                        let section_name = trimmed.trim_start_matches('#').trim();
                        match section_name {
                            "Description" | "Scope" | "Acceptance Criteria" => {
                                in_section = true;
                                lines.push(Line::from(""));
                                lines.push(Line::from(Span::styled(
                                    format!("    {section_name}"),
                                    heading_style,
                                )));
                                blank_run = 0;
                                continue;
                            }
                            _ => {
                                in_section = false;
                                continue;
                            }
                        }
                    }
                    if trimmed.starts_with("# ") {
                        let title = trimmed.trim_start_matches('#').trim();
                        lines.push(Line::from(Span::styled(
                            format!("    {title}"),
                            Style::default()
                                .fg(theme::HONEY)
                                .add_modifier(Modifier::BOLD),
                        )));
                        continue;
                    }
                    if in_section {
                        if trimmed.is_empty() {
                            blank_run += 1;
                            if blank_run <= 1 {
                                lines.push(Line::from(""));
                            }
                        } else {
                            blank_run = 0;
                            lines.push(Line::from(Span::styled(
                                format!("    {trimmed}"),
                                theme::text(),
                            )));
                        }
                    }
                }
                lines.push(Line::from(""));
            }

            // ── Sections 1..N: per-repo ──
            for (i, r) in dispatch.per_repo.iter().enumerate() {
                let section_idx = i + 1;
                let focused = dispatch.focused_section == section_idx;
                let collapsed = dispatch
                    .sections
                    .get(section_idx)
                    .map(|s| s.collapsed)
                    .unwrap_or(false);
                let icon = if collapsed { "\u{25b6}" } else { "\u{25bc}" };

                let counts = match (r.file_count, r.step_count) {
                    (Some(f), Some(s)) => format!("  {f} files, {s} steps"),
                    (Some(f), None) => format!("  {f} files"),
                    (None, Some(s)) => format!("  {s} steps"),
                    (None, None) => String::new(),
                };

                let header_style = if focused {
                    Style::default()
                        .fg(theme::HONEY)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme::NECTAR)
                        .add_modifier(Modifier::BOLD)
                };

                if focused {
                    focused_line = lines.len();
                }
                let cursor = if focused { "\u{25b8}" } else { " " };
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor}{icon} "), header_style),
                    Span::styled(&r.repo, header_style),
                    Span::styled(counts, dim_style),
                ]));

                if collapsed {
                    continue;
                }

                // Context: relevant files
                if let Some(ref ctx) = r.context_md {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled("    Files", heading_style)));
                    let mut in_files = false;
                    for ctx_line in ctx.lines() {
                        let trimmed = ctx_line.trim();
                        if trimmed.starts_with("## Relevant") {
                            in_files = true;
                            continue;
                        }
                        if trimmed.starts_with("## ") && in_files {
                            break;
                        }
                        if in_files && trimmed.starts_with("- ") {
                            let content = trimmed.trim_start_matches("- ");
                            if let (Some(start), Some(end)) =
                                (content.find('`'), content.rfind('`'))
                            {
                                if end > start {
                                    let path = &content[start + 1..end];
                                    let desc = content[end + 1..]
                                        .trim()
                                        .trim_start_matches('\u{2014}')
                                        .trim_start_matches("—")
                                        .trim_start_matches('-')
                                        .trim();
                                    lines.push(Line::from(vec![
                                        Span::styled(
                                            format!("    {path}"),
                                            Style::default().fg(theme::HONEY),
                                        ),
                                        Span::styled(
                                            if desc.is_empty() {
                                                String::new()
                                            } else {
                                                format!("  {desc}")
                                            },
                                            dim_style,
                                        ),
                                    ]));
                                }
                            } else {
                                lines.push(Line::from(Span::styled(
                                    format!("    {content}"),
                                    theme::text(),
                                )));
                            }
                        }
                    }

                    // Key patterns
                    let mut in_patterns = false;
                    for ctx_line in ctx.lines() {
                        let trimmed = ctx_line.trim();
                        if trimmed.starts_with("## Key Patterns") {
                            in_patterns = true;
                            lines.push(Line::from(""));
                            lines.push(Line::from(Span::styled("    Patterns", heading_style)));
                            continue;
                        }
                        if trimmed.starts_with("## ") && in_patterns {
                            break;
                        }
                        if in_patterns && trimmed.starts_with("- ") {
                            let content = trimmed.trim_start_matches("- ");
                            lines.push(Line::from(Span::styled(
                                format!("    {content}"),
                                dim_style,
                            )));
                        }
                    }
                }

                // Plan: approach + steps + testing
                if let Some(ref plan) = r.plan_md {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled("    Plan", heading_style)));

                    let mut in_approach = false;
                    for plan_line in plan.lines() {
                        let trimmed = plan_line.trim();
                        if trimmed.starts_with("## Approach") {
                            in_approach = true;
                            continue;
                        }
                        if trimmed.starts_with("## ") && in_approach {
                            break;
                        }
                        if in_approach && !trimmed.is_empty() {
                            lines.push(Line::from(Span::styled(
                                format!("    {trimmed}"),
                                dim_style,
                            )));
                        }
                    }

                    lines.push(Line::from(""));
                    let mut in_steps = false;
                    for plan_line in plan.lines() {
                        let trimmed = plan_line.trim();
                        if trimmed.starts_with("## Steps") {
                            in_steps = true;
                            continue;
                        }
                        if trimmed.starts_with("## ") && in_steps {
                            break;
                        }
                        if !in_steps {
                            continue;
                        }
                        let is_step = trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
                            && trimmed.contains('.');
                        if is_step {
                            let clean = trimmed.replace("**", "");
                            lines.push(Line::from(Span::styled(
                                format!("    {clean}"),
                                theme::text(),
                            )));
                        } else if !trimmed.is_empty() {
                            lines.push(Line::from(Span::styled(
                                format!("      {trimmed}"),
                                dim_style,
                            )));
                        }
                    }

                    let mut in_testing = false;
                    for plan_line in plan.lines() {
                        let trimmed = plan_line.trim();
                        if trimmed.starts_with("## Testing") {
                            in_testing = true;
                            lines.push(Line::from(""));
                            lines.push(Line::from(Span::styled("    Testing", heading_style)));
                            continue;
                        }
                        if trimmed.starts_with("## ") && in_testing {
                            break;
                        }
                        if in_testing && trimmed.starts_with("- ") {
                            let content = trimmed.trim_start_matches("- ");
                            lines.push(Line::from(Span::styled(
                                format!("    {content}"),
                                dim_style,
                            )));
                        }
                    }
                }

                lines.push(Line::from(""));
            }

            // Scroll: use stored offset, but auto-adjust if focused section is out of view.
            let visible_height = inner.height as usize;
            let mut scroll = dispatch.scroll as usize;
            if focused_line < scroll {
                scroll = focused_line;
            } else if focused_line >= scroll + visible_height {
                scroll = focused_line.saturating_sub(visible_height / 3);
            }
            scroll = scroll.min(lines.len().saturating_sub(visible_height.min(lines.len())));

            let visible_lines: Vec<Line> = lines
                .into_iter()
                .skip(scroll)
                .take(visible_height)
                .collect();
            let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
        }
    }
}

// ── Chat Panel ───────────────────────────────────────────

/// Deduplicate timestamps: return empty string if same as previous.
fn dedup_timestamp<'a>(ts: &'a str, prev: &mut Option<&'a str>) -> &'a str {
    if prev.as_ref() == Some(&ts) {
        ""
    } else {
        *prev = Some(ts);
        ts
    }
}

fn draw_chat_panel(frame: &mut Frame, area: Rect, app: &App) {
    let border_style = if app.focus == Panel::Chat {
        theme::border_active()
    } else {
        theme::border()
    };

    // Build lines first to calculate scroll indicator.
    let mut lines: Vec<Line> = Vec::new();
    let mut prev_ts: Option<&str> = None;
    let inner_width = area.width.saturating_sub(2) as usize;
    let divider_line = "\u{2500}".repeat(inner_width.saturating_sub(4));

    for (i, entry) in app.chat_history.iter().enumerate() {
        match entry {
            ChatLine::User(text, ts) => {
                // Divider between messages (skip before first)
                if i > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("  {divider_line}"),
                        Style::default().fg(theme::STEEL),
                    )));
                }
                let display_ts = dedup_timestamp(ts, &mut prev_ts);
                let mut label_spans = vec![Span::styled(
                    "  You:",
                    Style::default()
                        .fg(theme::HONEY)
                        .bg(theme::FOCUS_BG)
                        .add_modifier(Modifier::BOLD),
                )];
                if !display_ts.is_empty() {
                    label_spans.push(Span::styled(
                        format!("  {display_ts}"),
                        Style::default().fg(theme::SMOKE),
                    ));
                }
                lines.push(Line::from(label_spans));
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(theme::HONEY).bg(theme::FOCUS_BG),
                    )));
                }
                lines.push(Line::from(""));
            }
            ChatLine::Assistant(text, ts) => {
                if i > 0
                    && !matches!(
                        app.chat_history.get(i.wrapping_sub(1)),
                        Some(ChatLine::User(..))
                    )
                {
                    lines.push(Line::from(Span::styled(
                        format!("  {divider_line}"),
                        Style::default().fg(theme::STEEL),
                    )));
                }
                let display_ts = dedup_timestamp(ts, &mut prev_ts);
                let mut label_spans = vec![Span::styled(
                    "  Coordinator:",
                    Style::default()
                        .fg(theme::FROST)
                        .add_modifier(Modifier::BOLD),
                )];
                if !display_ts.is_empty() {
                    label_spans.push(Span::styled(
                        format!("  {display_ts}"),
                        Style::default().fg(theme::SMOKE),
                    ));
                }
                lines.push(Line::from(label_spans));
                // Render assistant text as markdown.
                let md_lines = markdown::render_markdown(text);
                lines.extend(md_lines);
                // Streaming cursor.
                if app.streaming && i == app.chat_history.len() - 1 {
                    lines.push(Line::from(Span::styled(
                        "  \u{258c}",
                        Style::default().fg(theme::HONEY),
                    )));
                }
            }
            ChatLine::System(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  {text}"),
                    theme::subtitle(),
                )));
                lines.push(Line::from(""));
            }
        }
    }

    // Calculate scroll indicator for title.
    let inner_height = area.height.saturating_sub(2) as usize; // borders top+bottom
    let total = lines.len();
    let title = if app.chat_scroll > 0 && total > inner_height {
        let max_scroll = total.saturating_sub(inner_height);
        let scroll_clamped = (app.chat_scroll as usize).min(max_scroll);
        let pct = ((max_scroll - scroll_clamped) * 100) / max_scroll.max(1);
        if pct == 0 {
            " Coordinator  Top ".to_string()
        } else {
            format!(" Coordinator  {pct}% ")
        }
    } else if app.streaming {
        " Coordinator (streaming...) ".to_string()
    } else {
        " Coordinator ".to_string()
    };

    let block = Block::default()
        .title(Span::styled(title, theme::title()))
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.chat_history.is_empty() {
        return;
    }

    // Apply scroll from bottom.
    let visible_height = inner.height as usize;
    let skip = if total > visible_height {
        total
            .saturating_sub(visible_height)
            .saturating_sub((app.chat_scroll as usize).min(total.saturating_sub(visible_height)))
    } else {
        0
    };

    let visible_lines: Vec<Line> = lines.into_iter().skip(skip).take(visible_height).collect();

    let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ── Input Bar ────────────────────────────────────────────

fn draw_input_bar(frame: &mut Frame, area: Rect, app: &App, prompt: &str) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::border());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_spans = vec![
        Span::styled(prompt, theme::accent()),
        Span::styled(app.input.as_str(), theme::text()),
    ];

    // Focus hint on the right.
    let hint = if app.focus == Panel::Chat {
        "[h: sidebar]"
    } else {
        "[l: focus]"
    };

    let input_line = Line::from(prompt_spans);
    let hint_line = Line::from(Span::styled(hint, theme::subtitle()));

    // Split inner into input area + hint.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length((hint.len() as u16).saturating_add(1)),
        ])
        .split(inner);

    let input_widget = Paragraph::new(input_line);
    frame.render_widget(input_widget, cols[0]);

    let hint_widget = Paragraph::new(hint_line);
    frame.render_widget(hint_widget, cols[1]);

    // Place the cursor.
    if app.focus == Panel::Chat {
        let prompt_width = prompt.len() as u16;
        let cursor_x = cols[0].x + prompt_width + app.input_cursor as u16;
        let cursor_y = cols[0].y;
        frame.set_cursor_position((cursor_x.min(cols[0].right().saturating_sub(1)), cursor_y));
    }
}

// ── Worker Output ─────────────────────────────────────────

fn draw_worker_output(frame: &mut Frame, area: Rect, app: &App) {
    let border_style = if app.focus == Panel::Chat {
        theme::border_active()
    } else {
        theme::border()
    };

    let workers = app.flat_workers();
    let worker_idx = match app.sidebar_selection {
        SidebarItem::Worker(i) => i,
        _ => return,
    };

    let Some((_, wt)) = workers.get(worker_idx) else {
        return;
    };

    // Build title: id + status (prefer phase when available).
    let status_str = match wt.phase.as_deref() {
        Some("creating") => "creating",
        Some("starting") => "starting",
        Some("running") => "running",
        Some("waiting") => "waiting",
        Some("completed") => "completed",
        Some("failed") => "failed",
        _ => {
            // Fallback: infer from agent liveness.
            if wt.agent_alive {
                if wt.agent_session_status.as_deref() == Some("waiting") {
                    "waiting"
                } else {
                    "running"
                }
            } else {
                "stopped"
            }
        }
    };
    let title_parts = format!(" {}  {} ", wt.id, status_str);

    let block = Block::default()
        .title(Span::styled(title_parts, theme::title()))
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Parse ANSI-colored pane content into styled lines.
    let all_lines: Vec<Line> = app
        .pane_content
        .split('\n')
        .map(|l| parse_ansi_line(l))
        .collect();

    // Trim trailing empty lines.
    let trimmed_len = all_lines
        .iter()
        .rposition(|l| l.width() > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    let lines = &all_lines[..trimmed_len];

    let visible_height = inner.height as usize;
    let total = lines.len();

    // Always pinned to bottom (keys are forwarded to the pane directly).
    let skip = total.saturating_sub(visible_height);

    let visible_lines: Vec<Line> = lines
        .iter()
        .skip(skip)
        .take(visible_height)
        .cloned()
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, inner);
}

// ── ANSI parsing ─────────────────────────────────────────

/// Parse a line with ANSI escape sequences into a ratatui Line with styles.
fn parse_ansi_line(input: &str) -> Line<'static> {
    use ratatui::style::Color;

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_style = Style::default();
    let mut buf = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            // Flush buffered text.
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), current_style));
            }
            chars.next(); // consume '['
            // Read CSI parameters.
            let mut params = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == ';' {
                    params.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            // Read final byte.
            let final_byte = chars.next().unwrap_or('m');
            if final_byte == 'm' {
                // SGR — apply styles.
                let codes: Vec<u32> = params
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                let mut i = 0;
                while i < codes.len() {
                    match codes[i] {
                        0 => current_style = Style::default(),
                        1 => current_style = current_style.add_modifier(Modifier::BOLD),
                        3 => current_style = current_style.add_modifier(Modifier::ITALIC),
                        4 => current_style = current_style.add_modifier(Modifier::UNDERLINED),
                        7 => current_style = current_style.add_modifier(Modifier::REVERSED),
                        22 => current_style = current_style.remove_modifier(Modifier::BOLD),
                        23 => current_style = current_style.remove_modifier(Modifier::ITALIC),
                        24 => current_style = current_style.remove_modifier(Modifier::UNDERLINED),
                        27 => current_style = current_style.remove_modifier(Modifier::REVERSED),
                        // Standard foreground colors.
                        30 => current_style = current_style.fg(Color::Black),
                        31 => current_style = current_style.fg(Color::Red),
                        32 => current_style = current_style.fg(Color::Green),
                        33 => current_style = current_style.fg(Color::Yellow),
                        34 => current_style = current_style.fg(Color::Blue),
                        35 => current_style = current_style.fg(Color::Magenta),
                        36 => current_style = current_style.fg(Color::Cyan),
                        37 => current_style = current_style.fg(Color::White),
                        39 => current_style.fg = None,
                        // Standard background colors.
                        40 => current_style = current_style.bg(Color::Black),
                        41 => current_style = current_style.bg(Color::Red),
                        42 => current_style = current_style.bg(Color::Green),
                        43 => current_style = current_style.bg(Color::Yellow),
                        44 => current_style = current_style.bg(Color::Blue),
                        45 => current_style = current_style.bg(Color::Magenta),
                        46 => current_style = current_style.bg(Color::Cyan),
                        47 => current_style = current_style.bg(Color::White),
                        49 => current_style.bg = None,
                        // 256-color and RGB.
                        38 if i + 1 < codes.len() => {
                            if codes[i + 1] == 5 && i + 2 < codes.len() {
                                current_style =
                                    current_style.fg(Color::Indexed(codes[i + 2] as u8));
                                i += 2;
                            } else if codes[i + 1] == 2 && i + 4 < codes.len() {
                                current_style = current_style.fg(Color::Rgb(
                                    codes[i + 2] as u8,
                                    codes[i + 3] as u8,
                                    codes[i + 4] as u8,
                                ));
                                i += 4;
                            }
                        }
                        48 if i + 1 < codes.len() => {
                            if codes[i + 1] == 5 && i + 2 < codes.len() {
                                current_style =
                                    current_style.bg(Color::Indexed(codes[i + 2] as u8));
                                i += 2;
                            } else if codes[i + 1] == 2 && i + 4 < codes.len() {
                                current_style = current_style.bg(Color::Rgb(
                                    codes[i + 2] as u8,
                                    codes[i + 3] as u8,
                                    codes[i + 4] as u8,
                                ));
                                i += 4;
                            }
                        }
                        // Bright foreground.
                        90..=97 => {
                            current_style =
                                current_style.fg(Color::Indexed((codes[i] - 90 + 8) as u8));
                        }
                        // Bright background.
                        100..=107 => {
                            current_style =
                                current_style.bg(Color::Indexed((codes[i] - 100 + 8) as u8));
                        }
                        _ => {} // ignore unknown
                    }
                    i += 1;
                }
                if codes.is_empty() {
                    // "\x1b[m" with no params = reset.
                    current_style = Style::default();
                }
            }
        } else {
            buf.push(ch);
        }
    }

    // Flush remaining text.
    if !buf.is_empty() {
        spans.push(Span::styled(buf, current_style));
    }

    if spans.is_empty() {
        Line::from("")
    } else {
        Line::from(spans)
    }
}

// ── Centered Rect Helper ─────────────────────────────────

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

// ── Help Overlay ─────────────────────────────────────────

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(54, 32, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(" help ", theme::title()))
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .style(Style::default().bg(theme::COMB));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let section = |title: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!(" ── {title} ──"),
            Style::default()
                .fg(theme::HONEY)
                .add_modifier(Modifier::BOLD),
        ))
    };

    let key_line = |key: &str, desc: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {key:<14}"), theme::key_hint()),
            Span::styled(desc.to_string(), theme::key_desc()),
        ])
    };

    let lines: Vec<Line> = vec![
        section("Sidebar"),
        key_line("j / k", "Navigate workers"),
        key_line("Tab / l / ↵", "Focus content"),
        key_line("d", "Dispatch"),
        key_line("x", "Close worker"),
        key_line("p", "PR detail overlay"),
        key_line("c", "Copy PR URL"),
        key_line("o", "Jump to agent"),
        key_line("g / G", "First / last worker"),
        key_line("z", "Toggle zoom"),
        key_line("?", "Help"),
        key_line("q", "Quit"),
        Line::from(""),
        section("Chat"),
        key_line("↵", "Send message"),
        key_line("j / k", "Scroll 1 line"),
        key_line("u / d", "Half-page up/down"),
        key_line("PgUp / PgDn", "Full page"),
        key_line("Home", "Scroll to top"),
        key_line("G / End", "Scroll to bottom"),
        key_line("h / Tab", "Return to sidebar"),
        key_line("z", "Toggle zoom"),
        Line::from(""),
        section("Dispatch Review"),
        key_line("j / k", "Navigate sections"),
        key_line("↵", "Toggle section"),
        key_line("c", "Collapse all"),
        key_line("u / d", "Scroll"),
        key_line("y / n", "Confirm / cancel"),
        Line::from(""),
        section("Tabs"),
        key_line("^b n / p", "Next / prev tab"),
        key_line("^b 1-9", "Direct tab"),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ── PR Detail Overlay ────────────────────────────────────

fn draw_pr_detail_overlay(frame: &mut Frame, area: Rect, pr: &PrDetailInfo) {
    let popup = centered_rect(58, 11, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(format!(" PR #{} ", pr.number), theme::title()))
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .style(Style::default().bg(theme::COMB));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let state_style = match pr.state.as_str() {
        "OPEN" => Style::default().fg(theme::MINT),
        "MERGED" => Style::default().fg(theme::ROYAL),
        "CLOSED" => Style::default().fg(theme::EMBER),
        _ => theme::muted(),
    };

    let lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Title:  ", theme::muted()),
            Span::styled(pr.title.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("  State:  ", theme::muted()),
            Span::styled(pr.state.clone(), state_style),
        ]),
        Line::from(vec![
            Span::styled("  URL:    ", theme::muted()),
            Span::styled(pr.url.clone(), Style::default().fg(theme::FROST)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  o", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("↵", theme::key_hint()),
            Span::styled(" open  ", theme::key_desc()),
            Span::styled("c", theme::key_hint()),
            Span::styled(" copy  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("p", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("q", theme::key_hint()),
            Span::styled(" dismiss", theme::key_desc()),
        ]),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ── Helpers ──────────────────────────────────────────────

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Format elapsed time as "now/5m/2h/3d".
fn format_time_ago(wt: &WorktreeInfo) -> String {
    let Some(created) = wt.created_at else {
        return String::new();
    };
    let elapsed = Local::now().signed_duration_since(created);
    if elapsed.num_minutes() < 1 {
        "now".to_string()
    } else if elapsed.num_minutes() < 60 {
        format!("{}m", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h", elapsed.num_hours())
    } else {
        format!("{}d", elapsed.num_days())
    }
}

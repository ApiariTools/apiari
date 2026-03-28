//! All ratatui rendering for the apiari TUI.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use apiari_tui::conversation;
use unicode_width::UnicodeWidthChar;

use super::app::{self, App, ChatLine, Mode, Panel, PendingAction, View};
use super::theme;

const SPINNER: &[&str] = &["|", "/", "-", "\\"];

/// Build the display string for the input box with cursor indicator.
/// Returns the rendered string with `_` at the cursor position.
/// `cursor_pos` is a byte offset into `text`, clamped and snapped to
/// the nearest char boundary for safety.
fn build_input_display(text: &str, cursor_pos: usize) -> String {
    // Clamp to text length, then snap back to a char boundary.
    let mut pos = cursor_pos.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push(' ');
    out.push_str(&text[..pos]);
    out.push('_');
    out.push_str(&text[pos..]);
    out
}

/// Calculate the number of visual rows needed for input text with wrapping.
/// `text` is the raw input (may contain newlines), `width` is the available
/// display width. Builds the exact rendered string with cursor and uses
/// display width for correct Unicode/emoji handling.
fn visual_input_rows(text: &str, cursor_pos: usize, width: u16) -> u16 {
    let w = width as usize;
    if w == 0 {
        return 1;
    }
    let rendered = build_input_display(text, cursor_pos);
    let mut total: usize = 0;
    for segment in rendered.split('\n') {
        let display_w = Line::from(segment).width();
        total += display_w.max(1).div_ceil(w);
    }
    total.max(1) as u16
}

// ── Main draw ────────────────────────────────────────────

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // 5-row layout: tab bar (1) + gap (1) + body (rest) + status bar (1) + bottom pad (1)
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

    draw_tab_bar(frame, app, outer[0]);

    // Paint an explicit background on every cell that no widget would
    // otherwise cover: the breathing-room gap (outer[1]), the body area
    // (outer[2]), and the bottom pad (outer[4]).  Without this, those cells
    // stay at Color::Reset — identical to the cleared previous buffer — so
    // the ratatui diff never outputs them.  If the terminal's alternate-
    // screen buffer was not fully blanked, ghost content from a prior
    // session persists in those unwritten cells and becomes visible when the
    // dashboard layout shifts (e.g. during daemon auto-start).
    let bg = Block::default().style(Style::default().bg(theme::COMB));
    frame.render_widget(bg.clone(), outer[1]);
    frame.render_widget(bg.clone(), outer[2]);
    frame.render_widget(bg, outer[4]);

    match &app.view {
        View::Dashboard => draw_dashboard(frame, app, outer[2]),
        View::WorkerDetail(i) => draw_worker_detail(frame, app, outer[2], *i),
        View::WorkerChat(i) => draw_worker_chat(frame, app, outer[2], *i),
        View::SignalDetail(i) => draw_signal_detail(frame, app, outer[2], *i),
        View::SignalList => draw_signal_list(frame, app, outer[2]),
        View::ReviewList => draw_review_list(frame, app, outer[2]),
        View::PrList => draw_pr_list(frame, app, outer[2]),
    }

    draw_status_bar(frame, app, outer[3]);

    // During onboarding, dim the status bar and tab bar
    if app.onboarding.active {
        onboarding_dim_area(frame, outer[0]); // tab bar
        onboarding_dim_area(frame, outer[3]); // status bar
    }

    // Overlays
    match app.mode {
        Mode::Help => draw_help_overlay(frame, size),
        Mode::Confirm => {
            if let Some(ref action) = app.pending_action {
                draw_confirm_overlay(frame, size, action, app);
            }
        }
        _ => {}
    }

    // Review comment input overlay
    if app.review_comment_active {
        draw_review_comment_input(frame, size, app);
    }

    // Shell name input overlay
    if app.shell_input_active {
        draw_shell_name_input(frame, size, app);
    }
}

// ── Tab bar ──────────────────────────────────────────────

fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![
        Span::styled(" * ", theme::logo()),
        Span::styled("apiari ", theme::title()),
        Span::styled("--- ", theme::border()),
    ];

    for (i, ws) in app.workspaces.iter().enumerate() {
        let style = if i == app.active_tab {
            theme::highlight()
        } else {
            theme::muted()
        };
        spans.push(Span::styled(format!(" {} ", ws.name), style));
        spans.push(Span::raw(" "));
    }

    // "+" tab for adding a new workspace (hidden during setup)
    if app.setup.is_none() {
        spans.push(Span::styled(" + ", theme::muted()));
        spans.push(Span::raw(" "));
    }

    // Right-aligned hints
    let hints = " ^b n/p  q:quit ";
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let hints_width = Span::from(hints).width();
    let padding = (area.width as usize)
        .saturating_sub(used)
        .saturating_sub(hints_width);
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.push(Span::styled(hints, theme::key_desc()));

    let line = Line::from(spans);
    let bar = Paragraph::new(line).style(Style::default().bg(theme::COMB));
    frame.render_widget(bar, area);
}

// ── Dashboard ────────────────────────────────────────────

fn draw_dashboard(frame: &mut Frame, app: &App, area: Rect) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };

    // Zoomed panel (or auto-zoom on narrow terminals)
    let zoomed = if area.width < 50 {
        Some(app.zoomed_panel.unwrap_or(app.focused_panel))
    } else {
        app.zoomed_panel
    };

    if let Some(panel) = zoomed {
        match panel {
            Panel::Home => draw_home_panel(frame, app, ws, area),
            Panel::Workers => draw_workers_panel(frame, app, ws, area),
            Panel::Shells => draw_shells_panel(frame, app, ws, area),
            Panel::Signals => draw_signals_card(frame, app, ws, area),
            Panel::Reviews => draw_reviews_pane(frame, app, ws, area),
            Panel::Feed => draw_feed_panel(frame, app, ws, area),
            Panel::Chat => draw_chat_panel(frame, app, ws, area),
        }
        return;
    }

    // Kanban strip + Chat (+optional Triage sidebar) layout
    let kanban_h = app::compute_kanban_height(ws);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(kanban_h), // Kanban strip
            Constraint::Min(5),           // Chat (gets everything else)
        ])
        .split(area);

    // Record actual allocated height so navigation can stay within visible cards.
    app.kanban_allocated_height.set(rows[0].height);

    draw_kanban_strip(frame, app, ws, rows[0]);

    // Bottom area: Chat + optional Triage sidebar
    if ws.triage_sidebar_open {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(rows[1]);
        draw_chat_panel(frame, app, ws, cols[0]);
        draw_triage_sidebar(frame, ws, cols[1]);
    } else {
        draw_chat_panel(frame, app, ws, rows[1]);
    }

    // Onboarding: dim kanban area if Home panel not revealed
    if app.onboarding.active && !app.onboarding.is_revealed(Panel::Home) {
        onboarding_dim_area(frame, rows[0]);
    }
}

// ── Home panel (full-screen zoom) ────────────────────────

fn draw_home_panel(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let actions = build_action_summary(app, ws);
    let action_h: u16 = if actions.is_empty() { 0 } else { 1 };

    let has_thoughts = !ws.thoughts.is_empty();
    let thoughts_h: u16 = if has_thoughts { 2 } else { 0 };

    // KPI gets most of the space, action banner + thoughts at bottom
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),             // KPI cards (fill)
            Constraint::Length(action_h),   // Action banner
            Constraint::Length(thoughts_h), // Thoughts
        ])
        .split(area);

    draw_kpi_strip(frame, app, ws, rows[0]);

    if !actions.is_empty() {
        draw_action_banner(frame, &actions, rows[1]);
    }

    if has_thoughts {
        draw_thoughts_strip(frame, app, ws, rows[2]);
    }
}

// ── KPI strip (single inline bar) ────────────────────────

fn draw_kpi_strip(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let kpi_border = Style::default().fg(theme::STEEL);
    let kpi_title = Style::default().fg(theme::POLLEN);
    let kpi_bg = Style::default().bg(theme::COMB);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .spacing(1)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // ── Watchers card ──
    {
        let healthy_count = ws.watcher_health.iter().filter(|w| w.healthy).count();
        let total_count = ws.watcher_health.len();
        let title = format!("Watchers ({healthy_count}/{total_count})");
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(kpi_border)
            .style(kpi_bg)
            .title(Span::styled(format!(" {title} "), kpi_title));
        let inner = block.inner(cols[0]);
        frame.render_widget(block, cols[0]);

        // Count open signals per source for watcher context
        let mut signals_by_source: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for sig in &ws.signals {
            *signals_by_source.entry(sig.source.as_str()).or_default() += 1;
        }

        if ws.watcher_health.is_empty() {
            let lines = vec![Line::from(vec![
                Span::raw(" "),
                Span::styled("No watchers configured", theme::muted()),
            ])];
            frame.render_widget(Paragraph::new(lines), inner);
        } else {
            let mut lines: Vec<Line> = Vec::new();
            for watcher in &ws.watcher_health {
                let (dot, dot_style) = if watcher.healthy {
                    ("\u{25cf}", Style::default().fg(theme::MINT))
                } else {
                    ("\u{25cb}", Style::default().fg(theme::EMBER))
                };

                // Relative time since last check
                let ago = if watcher.last_check_secs < 0 {
                    " never ".to_string()
                } else if watcher.last_check_secs < 60 {
                    format!(" {:>3}s ago ", watcher.last_check_secs)
                } else if watcher.last_check_secs < 3600 {
                    format!(" {:>3}m ago ", watcher.last_check_secs / 60)
                } else {
                    format!(" {:>3}h ago ", watcher.last_check_secs / 3600)
                };

                // What this watcher is seeing
                let sig_count = signals_by_source
                    .get(watcher.name.as_str())
                    .copied()
                    .unwrap_or(0);
                let finding = if sig_count > 0 {
                    Span::styled(
                        format!("{sig_count} signal{}", if sig_count > 1 { "s" } else { "" }),
                        Style::default().fg(theme::NECTAR),
                    )
                } else {
                    Span::styled("all clear", Style::default().fg(theme::SMOKE))
                };

                lines.push(Line::from(vec![
                    Span::styled(format!(" {dot} "), dot_style),
                    Span::styled(
                        format!("{:<8}", watcher.name),
                        Style::default().fg(theme::FROST),
                    ),
                    Span::styled(ago, theme::muted()),
                    Span::styled("\u{00b7} ", Style::default().fg(theme::WAX)),
                    finding,
                ]));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    // ── Today + PRs card ──
    {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(kpi_border)
            .style(kpi_bg)
            .title(Span::styled(" Today ", kpi_title));
        let inner = block.inner(cols[1]);
        frame.render_widget(block, cols[1]);

        let running = ws
            .workers
            .iter()
            .filter(|w| app::phase_display(w) == "running")
            .count();
        let waiting = ws
            .workers
            .iter()
            .filter(|w| app::phase_display(w) == "waiting")
            .count();
        let done = ws
            .workers
            .iter()
            .filter(|w| {
                let p = app::phase_display(w);
                p == "completed" || p == "closed"
            })
            .count();
        let open_prs = ws
            .workers
            .iter()
            .filter(|w| w.pr.as_ref().is_some_and(|p| p.state == "OPEN"))
            .count();
        let merged_prs = ws
            .workers
            .iter()
            .filter(|w| w.pr.as_ref().is_some_and(|p| p.state == "MERGED"))
            .count();

        let crit = ws
            .signals
            .iter()
            .filter(|s| {
                matches!(
                    s.severity,
                    crate::buzz::signal::Severity::Critical | crate::buzz::signal::Severity::Error
                )
            })
            .count();

        let mut lines: Vec<Line> = Vec::new();

        // Workers summary
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} workers", ws.workers.len()),
                Style::default().fg(theme::FROST),
            ),
            Span::styled(format!("  \u{25cf}{running}"), theme::status_running()),
            Span::styled(
                format!(" \u{25cb}{waiting}"),
                Style::default().fg(theme::POLLEN),
            ),
            Span::styled(format!(" \u{2713}{done}"), theme::status_done()),
        ]));

        // Signals summary (filtered in normal mode, total in debug)
        let sig_display_count = if app.signals_debug_mode {
            ws.signals.len()
        } else {
            ws.signals
                .iter()
                .filter(|s| !app::is_noise_signal(s))
                .count()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} signals", sig_display_count),
                Style::default().fg(theme::FROST),
            ),
            if crit > 0 {
                Span::styled(format!("  !!{crit}"), theme::error())
            } else {
                Span::styled("  all clear", Style::default().fg(theme::SMOKE))
            },
        ]));

        // PRs summary
        if open_prs > 0 || merged_prs > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} PRs", open_prs + merged_prs),
                    Style::default().fg(theme::FROST),
                ),
                Span::styled(
                    format!("  \u{27f3}{open_prs} open"),
                    Style::default().fg(theme::MINT),
                ),
                Span::styled(format!(" \u{2713}{merged_prs}"), theme::status_done()),
            ]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                " 0 PRs",
                Style::default().fg(theme::FROST),
            )]));
        }

        frame.render_widget(Paragraph::new(lines), inner);
    }
}

// ── Kanban strip ─────────────────────────────────────────

/// Draw the kanban strip with columns for each stage.
fn draw_kanban_strip(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    // Build status line for the header
    let healthy = ws.watcher_health.iter().filter(|w| w.healthy).count();
    let total = ws.watcher_health.len();
    // Most recent poll across all watchers (min non-negative last_check_secs)
    let last_poll = {
        let most_recent = ws
            .watcher_health
            .iter()
            .filter(|w| w.last_check_secs >= 0)
            .map(|w| w.last_check_secs)
            .min();
        match most_recent {
            Some(secs) if secs < 60 => format!("{secs}s ago"),
            Some(secs) => format!("{}m ago", secs / 60),
            None if ws.watcher_health.is_empty() => "n/a".to_string(),
            None => "never".to_string(), // all watchers have -1
        }
    };

    let health_icon = if total == 0 || healthy == total {
        "\u{2705}"
    } else {
        "\u{26a0}\u{fe0f}"
    };
    let open_signals = ws.kanban_cards.len();
    let title = format!(
        " Kanban \u{00b7} Watchers: {healthy}/{total} {health_icon} \u{00b7} Last poll: {last_poll} \u{00b7} Open: {open_signals} signals "
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(theme::STEEL))
        .style(Style::default().bg(theme::COMB))
        .title(Span::styled(title, Style::default().fg(theme::POLLEN)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Always show all 3 columns — empty columns show a placeholder
    let stages = [
        app::KanbanStage::InProgress,
        app::KanbanStage::InReview,
        app::KanbanStage::MergeReady,
    ];

    let columns: Vec<(app::KanbanStage, Vec<&app::KanbanCard>)> = stages
        .iter()
        .map(|&stage| {
            let cards: Vec<&app::KanbanCard> = ws
                .kanban_cards
                .iter()
                .filter(|c| c.stage == stage)
                .collect();
            (stage, cards)
        })
        .collect();

    // Build constraints with 1-char gaps for dividers between columns
    let num_cols = columns.len() as u32;
    let mut constraints: Vec<Constraint> = Vec::new();
    for (i, _) in columns.iter().enumerate() {
        if i > 0 {
            constraints.push(Constraint::Length(1)); // divider
        }
        constraints.push(Constraint::Ratio(1, num_cols));
    }
    let col_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(inner);

    let is_focused = app.focused_panel == app::Panel::Home;

    for (i, (stage, cards)) in columns.iter().enumerate() {
        let area_idx = i * 2; // skip divider slots
        let col_area = col_areas[area_idx];

        // Determine selected card index within this column
        let selected_idx = if is_focused {
            ws.kanban_selected.and_then(|(sel_stage, sel_idx)| {
                if sel_stage == *stage {
                    Some(sel_idx)
                } else {
                    None
                }
            })
        } else {
            None
        };

        draw_kanban_column(frame, *stage, cards, col_area, selected_idx);

        // Draw divider after this column (before the next)
        if i + 1 < columns.len() {
            let div_area = col_areas[area_idx + 1];
            let divider_lines: Vec<Line> = (0..div_area.height)
                .map(|_| Line::from(Span::styled("│", Style::default().fg(theme::STEEL))))
                .collect();
            frame.render_widget(Paragraph::new(divider_lines), div_area);
        }
    }
}

fn draw_kanban_column(
    frame: &mut Frame,
    stage: app::KanbanStage,
    cards: &[&app::KanbanCard],
    area: Rect,
    selected: Option<usize>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let (header_text, header_style) = match stage {
        app::KanbanStage::InProgress => (
            "IN PROGRESS",
            Style::default()
                .fg(theme::MINT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        app::KanbanStage::InReview => (
            "AI REVIEW",
            Style::default()
                .fg(theme::POLLEN)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        app::KanbanStage::MergeReady => (
            "MERGE READY",
            Style::default()
                .fg(theme::HONEY)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
    };

    let mut lines: Vec<Line> = Vec::new();

    // Header line
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(header_text, header_style),
    ]));

    // Empty column placeholder
    if cards.is_empty() {
        lines.push(Line::from(Span::styled(
            "  \u{2014}",
            Style::default()
                .fg(theme::STEEL)
                .add_modifier(Modifier::DIM),
        )));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    // How many cards can we fit? Each card = 2 lines, reserve 1 for header
    let available = area.height.saturating_sub(1) as usize;
    let cards_fit = available / 2;
    let show_count = cards_fit.min(cards.len());
    let overflow = cards.len().saturating_sub(show_count);

    let is_merge_ready = stage == app::KanbanStage::MergeReady;
    let card_style = if is_merge_ready {
        Style::default()
            .fg(theme::HONEY)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::FROST)
    };
    let subtitle_style = if is_merge_ready {
        Style::default().fg(theme::HONEY)
    } else {
        Style::default().fg(theme::SMOKE)
    };

    // Clamp selected index to the visible range; if the selected card is
    // scrolled off (e.g. narrow terminal fits fewer cards), suppress highlight.
    let visible_selected = selected.filter(|&i| i < show_count);

    for (card_idx, card) in cards.iter().take(show_count).enumerate() {
        let is_selected = visible_selected == Some(card_idx);

        let effective_card_style = if is_selected {
            card_style.add_modifier(Modifier::REVERSED)
        } else {
            card_style
        };
        let effective_subtitle_style = if is_selected {
            subtitle_style.add_modifier(Modifier::REVERSED)
        } else {
            subtitle_style
        };

        // Line 1: icon + title + action hint for MergeReady
        let mut title_spans = vec![
            Span::raw(" "),
            Span::styled(&card.icon, effective_card_style),
            Span::raw(" "),
            Span::styled(&card.title, effective_card_style),
        ];
        if is_merge_ready {
            title_spans.push(Span::styled(
                " \u{2192}",
                if is_selected {
                    Style::default()
                        .fg(theme::HONEY)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default()
                        .fg(theme::HONEY)
                        .add_modifier(Modifier::BOLD)
                },
            ));
        }
        lines.push(Line::from(title_spans));

        // Line 2: subtitle (indented)
        let sub_line = Line::from(vec![
            Span::raw("   "),
            Span::styled(&card.subtitle, effective_subtitle_style),
        ]);
        lines.push(sub_line);
    }

    if overflow > 0 {
        lines.push(Line::from(Span::styled(
            format!("  +{overflow} more"),
            Style::default()
                .fg(theme::SMOKE)
                .add_modifier(Modifier::DIM),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_triage_sidebar(frame: &mut Frame, ws: &app::WorkspaceState, area: Rect) {
    use crate::buzz::signal::Severity;

    let items = app::triage_items(ws);

    let count = items.len();
    let title = format!(" Triage ({count}) ");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(theme::STEEL))
        .style(Style::default().bg(theme::COMB))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme::FROST)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if items.is_empty() {
        let line = Line::from(Span::styled(
            " All clear",
            Style::default().fg(theme::SMOKE),
        ));
        frame.render_widget(Paragraph::new(vec![line]), inner);
        return;
    }

    // 4 lines per item: 3 content + 1 blank spacer
    const LINES_PER_ITEM: usize = 4;
    let visible = (inner.height as usize / LINES_PER_ITEM).min(items.len());
    let scroll = ws.triage_scroll.min(items.len().saturating_sub(visible));

    let mut y_offset = 0u16;

    for (i, item) in items.iter().skip(scroll).take(visible).enumerate() {
        if y_offset + 3 > inner.height {
            break;
        }

        let actual_idx = i + scroll;
        let is_selected = actual_idx == ws.triage_selected;
        let bar_color = theme::SIDEBAR_COLORS[actual_idx % theme::SIDEBAR_COLORS.len()];

        let row_bg = if is_selected {
            Style::default().bg(Color::Rgb(58, 50, 42))
        } else {
            Style::default().bg(theme::COMB)
        };

        let age_str = format_age(&item.age);
        let selector = if is_selected { "\u{25b8}" } else { " " };

        let (sev_dot, sev_style) = match item.severity {
            Severity::Critical | Severity::Error => {
                ("●", Style::default().fg(Color::Rgb(200, 60, 60)))
            }
            Severity::Warning => ("●", Style::default().fg(theme::HONEY)),
            Severity::Info => ("○", Style::default().fg(theme::SMOKE)),
        };

        let title_style = if is_selected {
            Style::default()
                .fg(theme::HONEY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FROST)
        };
        let selector_style = if is_selected {
            theme::selected()
        } else {
            theme::muted()
        };

        // Line 1: ▌ selector dot title
        let line1_area = Rect {
            x: inner.x,
            y: inner.y + y_offset,
            width: inner.width,
            height: 1,
        };
        let title_max = (inner.width as usize).saturating_sub(5);
        let line1 = Line::from(vec![
            Span::styled("\u{258c}", Style::default().fg(bar_color)),
            Span::styled(selector, selector_style),
            Span::styled(format!("{sev_dot} "), sev_style),
            Span::styled(truncate_to_width(&item.title, title_max), title_style),
        ]);
        frame.render_widget(Paragraph::new(line1).style(row_bg), line1_area);

        // Line 2: ▌   source_label · age
        // Reserve width for " · {age}" so the age is never pushed off-screen.
        let line2_area = Rect {
            x: inner.x,
            y: inner.y + y_offset + 1,
            width: inner.width,
            height: 1,
        };
        // prefix "▌  " = 3 columns; suffix " · {age}" reserves separator + age
        let age_suffix = format!(" \u{b7} {age_str}");
        let age_suffix_width: usize = age_suffix.chars().map(|c| c.width().unwrap_or(0)).sum();
        let label_max = (inner.width as usize)
            .saturating_sub(3) // bar + two spaces
            .saturating_sub(age_suffix_width);
        let truncated_label = truncate_to_width(&item.source_label, label_max);
        let line2 = Line::from(vec![
            Span::styled("\u{258c}", Style::default().fg(bar_color)),
            Span::raw("  "),
            Span::styled(truncated_label, Style::default().fg(theme::MINT)),
            Span::styled(age_suffix, Style::default().fg(Color::Rgb(80, 77, 70))),
        ]);
        frame.render_widget(Paragraph::new(line2).style(row_bg), line2_area);

        // Line 3: ▌   subtitle (muted description)
        let line3_area = Rect {
            x: inner.x,
            y: inner.y + y_offset + 2,
            width: inner.width,
            height: 1,
        };
        let sub_max = (inner.width as usize).saturating_sub(3);
        let line3 = Line::from(vec![
            Span::styled("\u{258c}", Style::default().fg(bar_color)),
            Span::raw("  "),
            Span::styled(
                truncate_to_width(&item.subtitle, sub_max),
                Style::default().fg(Color::Rgb(90, 87, 80)),
            ),
        ]);
        frame.render_widget(Paragraph::new(line3).style(row_bg), line3_area);

        y_offset += LINES_PER_ITEM as u16;
    }

    // "+N more" indicator at the bottom
    if items.len() > visible + scroll {
        let more = items.len() - visible - scroll;
        let more_y = inner.y + inner.height.saturating_sub(1);
        let more_area = Rect {
            x: inner.x,
            y: more_y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" +{more} more"),
                Style::default()
                    .fg(theme::STEEL)
                    .add_modifier(Modifier::DIM),
            ))),
            more_area,
        );
    }
}

fn format_age(dur: &chrono::Duration) -> String {
    let mins = dur.num_minutes();
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else if mins < 1440 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / 1440)
    }
}

fn truncate_to_width(s: &str, max_width: usize) -> String {
    let display_width: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if display_width <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let budget = max_width - 1; // reserve 1 column for "…"
    let mut used = 0usize;
    let mut truncated = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        truncated.push(c);
        used += w;
    }
    format!("{truncated}…")
}

// ── Action banner ────────────────────────────────────────

/// Build a list of action-needed items from the current workspace state.
/// Each item is narrative — tells you WHAT needs attention, not just a count.
fn build_action_summary(_app: &App, ws: &app::WorkspaceState) -> Vec<(Style, String)> {
    let mut items: Vec<(Style, String)> = Vec::new();

    // Group signals by source for narrative descriptions
    let mut by_source: std::collections::HashMap<&str, (usize, usize, usize)> =
        std::collections::HashMap::new();
    for sig in &ws.signals {
        let entry = by_source.entry(sig.source.as_str()).or_default();
        match sig.severity {
            crate::buzz::signal::Severity::Critical | crate::buzz::signal::Severity::Error => {
                entry.0 += 1
            }
            crate::buzz::signal::Severity::Warning => entry.1 += 1,
            crate::buzz::signal::Severity::Info => entry.2 += 1,
        }
    }

    // Narrative signal summaries per source (sorted for stable order)
    let mut sources: Vec<_> = by_source.iter().collect();
    sources.sort_by_key(|(name, _)| *name);
    for (source, (crit_err, warn, _info)) in sources {
        if *crit_err > 0 {
            items.push((
                theme::error(),
                format!(
                    "{crit_err} {source} error{}",
                    if *crit_err > 1 { "s" } else { "" }
                ),
            ));
        } else if *warn > 0 {
            items.push((
                Style::default().fg(theme::POLLEN),
                format!(
                    "{warn} {source} warning{}",
                    if *warn > 1 { "s" } else { "" }
                ),
            ));
        }
    }

    // Workers waiting for input — name them
    let waiting: Vec<&str> = ws
        .workers
        .iter()
        .filter(|w| app::phase_display(w) == "waiting")
        .map(|w| w.id.as_str())
        .collect();
    if !waiting.is_empty() {
        let names = if waiting.len() <= 2 {
            waiting.join(", ")
        } else {
            format!("{} +{} more", waiting[0], waiting.len() - 1)
        };
        items.push((
            Style::default().fg(theme::POLLEN),
            format!("{names} waiting for input"),
        ));
    }

    // Open PRs — name them
    let open_prs: Vec<String> = ws
        .workers
        .iter()
        .filter_map(|w| {
            let pr = w.pr.as_ref()?;
            if pr.state == "OPEN" {
                Some(format!("#{}", pr.number))
            } else {
                None
            }
        })
        .collect();
    if !open_prs.is_empty() {
        items.push((
            Style::default().fg(theme::MINT),
            format!("PR {} ready for review", open_prs.join(", ")),
        ));
    }

    // Unhealthy watchers — name them
    let unhealthy: Vec<&str> = ws
        .watcher_health
        .iter()
        .filter(|w| !w.healthy)
        .map(|w| w.name.as_str())
        .collect();
    if !unhealthy.is_empty() {
        items.push((
            theme::error(),
            format!("{} watcher stale", unhealthy.join(", ")),
        ));
    }

    items
}

/// Render a 1-line action banner showing what needs attention.
fn draw_action_banner(frame: &mut Frame, actions: &[(Style, String)], area: Rect) {
    let mut spans: Vec<Span> = vec![Span::styled(
        " \u{25b6} ",
        Style::default()
            .fg(theme::HONEY)
            .add_modifier(Modifier::BOLD),
    )];

    for (i, (style, text)) in actions.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "  \u{00b7}  ",
                Style::default().fg(theme::WAX),
            ));
        }
        spans.push(Span::styled(text.clone(), *style));
    }

    let line = Line::from(spans);
    let p = Paragraph::new(line).style(Style::default().bg(theme::COMB));
    frame.render_widget(p, area);
}

// ── Helpers ──────────────────────────────────────────────

/// Dim all content in an area (for unfocused panels).
fn dim_area(frame: &mut Frame, area: Rect) {
    frame
        .buffer_mut()
        .set_style(area, Style::default().add_modifier(Modifier::DIM));
}

/// Heavy dim for onboarding — unrevealed panels are barely visible.
/// Resets fg, bg, and modifiers so content appears ghost-like.
fn onboarding_dim_area(frame: &mut Frame, area: Rect) {
    let dim_style = Style::default()
        .fg(ratatui::style::Color::Rgb(50, 48, 42))
        .bg(ratatui::style::Color::Rgb(30, 28, 25));
    // set_style merges, so we need to reset per-cell to clear BOLD/DIM/etc.
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(dim_style);
                // Explicitly clear modifiers that set_style merges additively
                cell.modifier = Modifier::empty();
            }
        }
    }
}

// ── Panel block helper ───────────────────────────────────

/// Build a rounded-border panel block with focus-aware styling.
/// Title is rendered as a styled span. Optional right-aligned text in title bar.
fn panel_block<'a>(title: &'a str, focused: bool, right_text: Option<&'a str>) -> Block<'a> {
    let border_style = if focused {
        Style::default().fg(theme::HONEY)
    } else {
        Style::default().fg(theme::STEEL)
    };
    let title_style = if focused {
        Style::default()
            .fg(theme::HONEY)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::POLLEN)
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(border_style)
        .style(Style::default().bg(theme::COMB))
        .title(Span::styled(format!(" {title} "), title_style));

    if let Some(right) = right_text {
        let right_style = if focused {
            Style::default().fg(theme::SMOKE)
        } else {
            Style::default()
                .fg(theme::STEEL)
                .add_modifier(Modifier::DIM)
        };
        block = block.title_bottom(
            Line::from(vec![Span::styled(format!(" {right} "), right_style)]).right_aligned(),
        );
    }

    block
}

// ── Bee character ────────────────────────────────────────

/// Returns (animated_bee, status_text) for the coordinator bee.
/// The bee flies back and forth with wing flutter and a little trail.
fn bee_status(app: &App, ws: &app::WorkspaceState) -> (String, String) {
    let t = app.spinner_tick;

    if !app.daemon_alive {
        // Sleeping bee, gentle z's
        let zs = match t % 3 {
            0 => "z",
            1 => "zz",
            _ => "zzz",
        };
        return (format!("(-_-){zs}"), "offline".into());
    }

    // Thinking: fast buzzing, doesn't patrol — vibrates in place
    if ws.streaming {
        let face = match t % 6 {
            0 => " ~(*v*)~  ",
            1 => "  \\(*v*)/  ",
            2 => "   ~(*v*)~ ",
            3 => "  ~(*v*)~  ",
            4 => " /(*v*)\\  ",
            _ => "~(*v*)~   ",
        };
        let dots = match t % 4 {
            0 => "thinking",
            1 => "thinking.",
            2 => "thinking..",
            _ => "thinking...",
        };
        return (face.into(), dots.into());
    }

    // Alert: bee is agitated, bouncing with !
    let crit = ws
        .signals
        .iter()
        .filter(|s| {
            matches!(
                s.severity,
                crate::buzz::signal::Severity::Critical | crate::buzz::signal::Severity::Error
            )
        })
        .count();
    if crit > 0 {
        let face = match t % 4 {
            0 => "~(*!*)~",
            1 => "\\(*!*)/",
            2 => " (*!*) ",
            _ => "/(*!*)\\",
        };
        return (
            face.into(),
            format!("{crit} alert{}", if crit > 1 { "s" } else { "" }),
        );
    }

    // Workers waiting: curious bee
    let waiting = ws
        .workers
        .iter()
        .filter(|w| app::phase_display(w) == "waiting")
        .count();
    if waiting > 0 {
        let face = match t % 4 {
            0 => "~(*?*)~",
            1 => " (*?*) ",
            2 => "~(*?*)~",
            _ => " (*?*) ",
        };
        return (face.into(), format!("{waiting} waiting"));
    }

    // Unread response: bee winks to get your attention
    if ws.has_unread_response {
        let face = match t % 4 {
            0 => "~(*^*)~",
            1 => " (*^*) ",
            2 => "~(*^*)~",
            _ => " (*^*) ",
        };
        return (face.into(), "new reply!".into());
    }

    // Happy idle: bee patrols back and forth with cycling moods
    let mood = match (t / 48) % 6 {
        // Change mood every ~12 seconds
        0 => "listening...",
        1 => "watching the hive",
        2 => "all quiet",
        3 => "ready",
        4 => "keeping watch",
        _ => "humming along",
    };

    // 12-frame patrol cycle (3s at 250ms tick)
    let patrol_pos = match t % 12 {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 5,
        6 => 5,
        7 => 4,
        8 => 3,
        9 => 2,
        10 => 1,
        _ => 0,
    };
    let wings = match t % 4 {
        0 => ("~", "~"),
        1 => ("-", "-"),
        2 => ("~", "~"),
        _ => ("-", "-"),
    };
    let pad = " ".repeat(patrol_pos);
    let trail = if patrol_pos > 0 { "\u{00b7}" } else { " " };
    let face = format!("{trail}{pad}{}(*v*){}", wings.0, wings.1);
    (face, mood.into())
}

// ── Chat panel ───────────────────────────────────────────

fn draw_chat_panel(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let chat_highlighted = app.focused_panel == Panel::Chat || app.chat_focused;

    // Input height (inside the bordered panel)
    let input_h = if app.chat_focused {
        // Account for visual line wrapping, not just explicit newlines.
        // area.width - 2 accounts for the panel's left+right borders.
        let avail_w = area.width.saturating_sub(2);
        let rows = visual_input_rows(&ws.input, ws.cursor_pos, avail_w);
        rows.clamp(1, 6) + 1 // +1 to include the top-border separator row
    } else {
        1 // just the hint line
    };

    let (face, status) = bee_status(app, ws);
    let title = format!("{face:<13} {status}");
    let block = panel_block(&title, chat_highlighted, None);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner into messages + input
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(input_h)])
        .split(inner);

    // Chat messages
    let msg_block = Block::default();

    if ws.chat_history.is_empty() && !ws.streaming {
        let msg_inner = msg_block.inner(layout[0]);
        frame.render_widget(msg_block, layout[0]);
        let name = &ws.config.coordinator.name;
        let greeting = match app.spinner_tick / 60 % 4 {
            0 => format!("Hey! I'm {name}, your coordinator."),
            1 => "I keep an eye on signals, workers, and PRs.".to_string(),
            2 => "Ask me anything about the hive.".to_string(),
            _ => "Press c to start a conversation.".to_string(),
        };
        let lines = vec![
            Line::from(""),
            Line::from(""),
            Line::from(vec![Span::styled(
                "        ~(*v*)~",
                Style::default().fg(theme::HONEY),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::raw("    "),
                Span::styled(greeting, Style::default().fg(theme::SMOKE)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("    "),
                Span::styled("c", theme::key_hint()),
                Span::styled(" to chat", theme::key_desc()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), msg_inner);
    } else {
        // Convert ChatLine → ConversationEntry so both coordinator chat
        // and worker detail use the same rendering path.
        let entries: Vec<conversation::ConversationEntry> = ws
            .chat_history
            .iter()
            .map(|msg| match msg {
                ChatLine::User(text, ts, source) => {
                    let display_text = if matches!(source, Some(app::MessageSource::Telegram)) {
                        format!("[TG] {text}")
                    } else {
                        text.clone()
                    };
                    conversation::ConversationEntry::User {
                        text: display_text,
                        timestamp: ts.clone(),
                    }
                }
                ChatLine::Assistant(text, ts, _source) => {
                    // TODO: map to ConversationEntry::Question when assistant message ends with "?"
                    // and contains actionable content (merge, close, dispatch, etc.)
                    conversation::ConversationEntry::AssistantText {
                        text: text.clone(),
                        timestamp: ts.clone(),
                    }
                }
                ChatLine::System(text) => {
                    conversation::ConversationEntry::Status { text: text.clone() }
                }
            })
            .collect();

        let mut all_lines: Vec<Line> = Vec::new();
        conversation::render_conversation(
            &mut all_lines,
            &entries,
            None,
            Some(&ws.config.coordinator.name),
        );

        if ws.streaming {
            let spin = SPINNER[app.spinner_tick % SPINNER.len()];
            all_lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{spin} thinking..."), theme::muted()),
            ]));
        }

        apiari_tui::scroll::render_scrollable(
            frame,
            layout[0],
            all_lines,
            &ws.chat_scroll,
            msg_block,
        );
    }

    // Input area
    if app.chat_focused {
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme::WAX).add_modifier(Modifier::DIM));
        let input_inner = input_block.inner(layout[1]);
        frame.render_widget(input_block, layout[1]);

        let input_text = build_input_display(&ws.input, ws.cursor_pos);
        let input_p = Paragraph::new(input_text)
            .style(theme::text())
            .wrap(Wrap { trim: false });
        frame.render_widget(input_p, input_inner);
    } else {
        let hint = Line::from(vec![
            Span::raw(" "),
            Span::styled("c", theme::key_hint()),
            Span::styled(":chat", theme::key_desc()),
        ]);
        frame.render_widget(Paragraph::new(hint), layout[1]);
    }

    if !chat_highlighted {
        dim_area(frame, inner);
    }
}

// ── Workers panel (sidebar) ──────────────────────────────

fn draw_workers_panel(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let panel_focused = app.focused_panel == Panel::Workers;

    let title = format!("Workers ({})", ws.workers.len());
    let block = panel_block(&title, panel_focused, None);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if ws.workers.is_empty() {
        let lines = vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled("No active workers", theme::muted()),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled("Use swarm to create workers", theme::key_desc()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (i, worker) in ws.workers.iter().enumerate() {
        let is_sel = panel_focused && app.worker_selection == i;
        render_worker_item(&mut lines, worker, is_sel, i, inner.width as usize);
    }
    frame.render_widget(Paragraph::new(lines), inner);

    if !panel_focused {
        dim_area(frame, inner);
    }
}

// ── Shells panel ──────────────────────────────────────────

fn draw_shells_panel(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let panel_focused = app.focused_panel == Panel::Shells;
    let tmux_available = ws.tmux.as_ref().is_some_and(|tmux| tmux.is_available());

    let hint = if panel_focused && tmux_available {
        Some("n:new  enter:attach  d:kill")
    } else {
        None
    };
    let title = format!("Shells ({})", ws.shell_windows.len());
    let block = panel_block(&title, panel_focused, hint);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !tmux_available {
        let lines = vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled("tmux not found", Style::default().fg(theme::NECTAR)),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled("Install tmux to manage shells", theme::muted()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
        if !panel_focused {
            dim_area(frame, inner);
        }
        return;
    }

    if ws.shell_windows.is_empty() {
        let lines = vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled("No shell windows", theme::muted()),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled("Press n to create one", theme::key_desc()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
        if !panel_focused {
            dim_area(frame, inner);
        }
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (i, shell) in ws.shell_windows.iter().enumerate() {
        let is_sel = panel_focused && app.shell_selection == i;
        let marker = if is_sel { "> " } else { "  " };
        let name_style = if is_sel {
            theme::highlight()
        } else {
            Style::default().fg(theme::POLLEN)
        };
        let preview_style = theme::muted();

        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled(&shell.name, name_style),
        ]));

        // Show preview line (trimmed to fit using display column width)
        if !shell.preview.is_empty() {
            let max_w = inner.width.saturating_sub(4) as usize;
            let mut width = 0;
            let mut end_byte = shell.preview.len();
            for (i, ch) in shell.preview.char_indices() {
                let cw = ch.width().unwrap_or(0);
                if width + cw > max_w {
                    end_byte = i;
                    break;
                }
                width += cw;
            }
            let preview = &shell.preview[..end_byte];
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(preview, preview_style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);

    if !panel_focused {
        dim_area(frame, inner);
    }
}

// ── Signals card (horizontal carousel) ───────────────────

fn draw_signals_card(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let panel_focused = app.focused_panel == Panel::Signals;
    let debug = app.signals_debug_mode;

    // Filter out review queue signals — those go in the Reviews pane.
    // In normal mode, also hide noise signals (merged PRs, CI pass).
    let filtered: Vec<&crate::buzz::signal::SignalRecord> = ws
        .signals
        .iter()
        .filter(|s| s.source != "github_review_queue")
        .filter(|s| debug || !app::is_noise_signal(s))
        .collect();
    let total = filtered.len();

    // Title with navigation indicator + debug badge
    let label = if debug { "Signals [debug]" } else { "Signals" };
    let title = if total == 0 {
        format!("{label} ({total})")
    } else {
        let idx = app.signal_selection.min(total.saturating_sub(1));
        let signal = filtered[idx];
        let icon = app::severity_icon(&signal.severity);
        if panel_focused {
            format!(
                "{label} ({total})  {icon} \u{25c0} {}/{} \u{25b6}",
                idx + 1,
                total
            )
        } else {
            format!("{label} ({total})  {icon} {}/{}", idx + 1, total)
        }
    };

    let block = panel_block(&title, panel_focused, None);
    let content_area = block.inner(area);
    frame.render_widget(block, area);

    let content_h = content_area.height as usize;
    if content_h == 0 {
        return;
    }

    if total == 0 {
        // Check if there are hidden noise signals that debug mode would reveal
        let hidden_noise = if debug {
            0
        } else {
            ws.signals
                .iter()
                .filter(|s| s.source != "github_review_queue")
                .filter(|s| app::is_noise_signal(s))
                .count()
        };
        let msg = if hidden_noise > 0 {
            Cow::Owned(format!(
                "No actionable signals ({hidden_noise} hidden, d=debug)"
            ))
        } else {
            Cow::Borrowed("No open signals")
        };
        let lines = vec![Line::from(vec![
            Span::raw(" "),
            Span::styled(msg.into_owned(), theme::muted()),
        ])];
        frame.render_widget(Paragraph::new(lines), content_area);
        return;
    }

    // In debug mode, batch consecutive merged-PR-only entries
    if debug {
        let idx = app.signal_selection.min(total.saturating_sub(1));

        // Check if the selected signal is part of a batch of consecutive noise signals
        let mut batch_start = idx;
        let mut batch_end = idx;
        if app::is_noise_signal(filtered[idx]) {
            // Expand backward
            while batch_start > 0 && app::is_noise_signal(filtered[batch_start - 1]) {
                batch_start -= 1;
            }
            // Expand forward
            while batch_end + 1 < total && app::is_noise_signal(filtered[batch_end + 1]) {
                batch_end += 1;
            }
            let batch_count = batch_end - batch_start + 1;
            if batch_count > 1 {
                let mut lines: Vec<Line> = Vec::new();
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{batch_count} noise signals"), theme::muted()),
                    Span::styled(format!("  ({}/{})", idx + 1, total), theme::muted()),
                ]));
                let p = Paragraph::new(lines);
                frame.render_widget(p, content_area);
                if !panel_focused {
                    dim_area(frame, content_area);
                }
                return;
            }
        }
    }

    let idx = app.signal_selection.min(total.saturating_sub(1));
    let signal = filtered[idx];
    let icon = app::severity_icon(&signal.severity);
    let sev_style = severity_style(&signal.severity);
    let ago = time_ago(&signal.updated_at);
    let clean_title = strip_repo_prefix(&signal.title);
    let content_lines = content_h;

    let mut lines: Vec<Line> = Vec::new();

    // Line 1: severity icon + title
    lines.push(Line::from(vec![
        Span::styled(format!("  {icon} "), sev_style),
        Span::styled(
            clean_title.to_string(),
            Style::default()
                .fg(theme::FROST)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Line 2: repo (extracted from external_id) + source + time
    let repo = extract_repo(&signal.external_id);
    let mut meta_spans = vec![Span::raw("  ")];
    if let Some(r) = repo {
        meta_spans.push(Span::styled(r.to_string(), Style::default().fg(theme::ICE)));
        meta_spans.push(Span::styled(" \u{00b7} ", theme::muted()));
    }
    meta_spans.push(Span::styled(signal.source.clone(), theme::muted()));
    meta_spans.push(Span::styled(format!(" \u{00b7} {ago} ago"), theme::muted()));
    lines.push(Line::from(meta_spans));

    // Body lines (only if they add info beyond the title)
    if content_lines > 4
        && let Some(ref body) = signal.body
    {
        let body_adds_info = !body
            .lines()
            .next()
            .is_some_and(|first| clean_title.contains(first.trim()));
        if body_adds_info {
            lines.push(Line::from(""));
            let max_body = content_lines.saturating_sub(5); // room for title+meta+url+blank
            for line in body.lines().take(max_body) {
                let clean = strip_repo_prefix(line);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(clean.to_string(), theme::muted()),
                ]));
            }
        }
    }

    // URL at the bottom (truncated for terminal click-detection)
    if let Some(ref url) = signal.url
        && lines.len() < content_lines
    {
        // Push URL to the bottom with spacing
        let gap = content_lines.saturating_sub(lines.len() + 1);
        if gap > 0 {
            lines.push(Line::from(""));
        }
        let max_url = (content_area.width as usize).saturating_sub(3);
        let display_url = if url.len() > max_url {
            &url[..url.floor_char_boundary(max_url)]
        } else {
            url.as_str()
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(display_url.to_string(), Style::default().fg(theme::HONEY)),
        ]));
    }

    // No Wrap — keeps URLs on one line so terminal click-detection works
    let p = Paragraph::new(lines);
    frame.render_widget(p, content_area);

    if !panel_focused {
        dim_area(frame, content_area);
    }
}

// ── Reviews pane (dedicated review queue card) ───────────

fn draw_reviews_pane(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let panel_focused = app.focused_panel == Panel::Reviews;

    let filtered: Vec<&crate::buzz::signal::SignalRecord> = ws
        .signals
        .iter()
        .filter(|s| s.source == "github_review_queue")
        .collect();
    let total = filtered.len();

    let title = if total == 0 {
        format!("Reviews ({total})")
    } else {
        let idx = app.review_selection.min(total.saturating_sub(1));
        let signal = filtered[idx];
        let icon = app::severity_icon(&signal.severity);
        if panel_focused {
            format!(
                "Reviews ({total})  {icon} \u{25c0} {}/{} \u{25b6}",
                idx + 1,
                total
            )
        } else {
            format!("Reviews ({total})  {icon} {}/{}", idx + 1, total)
        }
    };

    let block = panel_block(&title, panel_focused, None);
    let content_area = block.inner(area);
    frame.render_widget(block, area);

    let content_h = content_area.height as usize;
    if content_h == 0 {
        return;
    }

    if total == 0 {
        let lines = vec![Line::from(vec![
            Span::raw(" "),
            Span::styled("No review queue items", theme::muted()),
        ])];
        frame.render_widget(Paragraph::new(lines), content_area);
        if !panel_focused {
            dim_area(frame, content_area);
        }
        return;
    }

    let idx = app.review_selection.min(total.saturating_sub(1));
    let signal = filtered[idx];
    let icon = app::severity_icon(&signal.severity);
    let sev_style = severity_style(&signal.severity);
    let ago = time_ago(&signal.updated_at);
    let clean_title = strip_repo_prefix(&signal.title);
    let content_lines = content_h;

    let mut lines: Vec<Line> = Vec::new();

    // Show query_name from metadata if available
    if let Some(ref meta) = signal.metadata
        && let Ok(meta_val) = serde_json::from_str::<serde_json::Value>(meta)
        && let Some(qname) = meta_val.get("query_name").and_then(|v| v.as_str())
    {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                qname.to_string(),
                Style::default()
                    .fg(theme::POLLEN)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Line 1: severity icon + title
    lines.push(Line::from(vec![
        Span::styled(format!("  {icon} "), sev_style),
        Span::styled(
            clean_title.to_string(),
            Style::default()
                .fg(theme::FROST)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Line 2: repo + source + time
    let repo = extract_repo(&signal.external_id);
    let mut meta_spans = vec![Span::raw("  ")];
    if let Some(r) = repo {
        meta_spans.push(Span::styled(r.to_string(), Style::default().fg(theme::ICE)));
        meta_spans.push(Span::styled(" \u{00b7} ", theme::muted()));
    }
    meta_spans.push(Span::styled(signal.source.clone(), theme::muted()));
    meta_spans.push(Span::styled(format!(" \u{00b7} {ago} ago"), theme::muted()));
    lines.push(Line::from(meta_spans));

    // Body lines
    if content_lines > 4
        && let Some(ref body) = signal.body
    {
        let body_adds_info = !body
            .lines()
            .next()
            .is_some_and(|first| clean_title.contains(first.trim()));
        if body_adds_info {
            lines.push(Line::from(""));
            let max_body = content_lines.saturating_sub(5);
            for line in body.lines().take(max_body) {
                let clean = strip_repo_prefix(line);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(clean.to_string(), theme::muted()),
                ]));
            }
        }
    }

    // URL at the bottom
    if let Some(ref url) = signal.url
        && lines.len() < content_lines
    {
        let gap = content_lines.saturating_sub(lines.len() + 1);
        if gap > 0 {
            lines.push(Line::from(""));
        }
        let max_url = (content_area.width as usize).saturating_sub(3);
        let display_url = if url.len() > max_url {
            &url[..url.floor_char_boundary(max_url)]
        } else {
            url.as_str()
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(display_url.to_string(), Style::default().fg(theme::HONEY)),
        ]));
    }

    let p = Paragraph::new(lines);
    frame.render_widget(p, content_area);

    if !panel_focused {
        dim_area(frame, content_area);
    }
}

// ── Feed panel ───────────────────────────────────────────

fn draw_feed_panel(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    let panel_focused = app.focused_panel == Panel::Feed;

    let title = format!("Heartbeat ({})", ws.feed.len());
    let block = panel_block(&title, panel_focused, None);
    let content_area = block.inner(area);
    frame.render_widget(block, area);

    if ws.feed.is_empty() {
        let lines = vec![Line::from(vec![
            Span::raw(" "),
            Span::styled("No recent activity", theme::muted()),
        ])];
        frame.render_widget(Paragraph::new(lines), content_area);
        if !panel_focused {
            dim_area(frame, content_area);
        }
        return;
    }

    let now = chrono::Utc::now();
    let mut lines: Vec<Line> = Vec::new();
    for (i, item) in ws.feed.iter().enumerate() {
        let is_sel = panel_focused && app.feed_selection == i;
        let age = now.signed_duration_since(item.when);
        let age_minutes = age.num_minutes();

        // Relative timestamp
        let rel_ts = if age_minutes < 1 {
            "just now".to_string()
        } else if age_minutes < 60 {
            format!("{age_minutes}m ago")
        } else if age.num_hours() < 24 {
            format!("{}h ago", age.num_hours())
        } else {
            format!("{}d ago", age.num_days())
        };

        // Icon + color by kind
        let (icon, icon_style) = match item.kind {
            app::FeedKind::Signal => ("!", Style::default().fg(theme::NECTAR)),
            app::FeedKind::Worker => ("\u{25cf}", Style::default().fg(theme::MINT)),
            app::FeedKind::Heartbeat => ("\u{2665}", Style::default().fg(theme::STEEL)),
        };

        // Dim older items: full brightness < 5m, slightly dim < 30m, dim < 2h, very dim beyond
        let text_style = if is_sel {
            theme::selected()
        } else if age_minutes < 5 {
            theme::text()
        } else if age_minutes < 30 {
            Style::default().fg(theme::SMOKE)
        } else {
            Style::default()
                .fg(theme::STEEL)
                .add_modifier(Modifier::DIM)
        };
        let ts_style = if age_minutes < 5 {
            Style::default().fg(theme::POLLEN)
        } else {
            theme::muted()
        };

        let bg = if is_sel {
            Style::default().bg(theme::FOCUS_BG)
        } else {
            Style::default()
        };
        let max_text = (content_area.width as usize).saturating_sub(12 + rel_ts.len());
        let text = trunc(&item.text, max_text);

        lines.push(
            Line::from(vec![
                Span::styled(format!(" {:>8} ", rel_ts), ts_style),
                Span::styled(format!("{icon} "), icon_style),
                Span::styled(text, text_style),
            ])
            .style(bg),
        );
    }

    let feed_block = Block::default();
    apiari_tui::scroll::render_scrollable(frame, content_area, lines, &ws.feed_scroll, feed_block);

    if !panel_focused {
        dim_area(frame, content_area);
    }
}

// ── Thoughts strip ───────────────────────────────────────

fn draw_thoughts_strip(frame: &mut Frame, app: &App, ws: &app::WorkspaceState, area: Rect) {
    if ws.thoughts.is_empty() || area.height == 0 {
        return;
    }

    let category_icon = |cat: &str| -> &str {
        match cat {
            "observation" => "\u{25cb}", // ○
            "decision" => "\u{2713}",    // ✓
            "preference" => "\u{2605}",  // ★
            _ => "\u{00b7}",             // ·
        }
    };

    // Build scrolling text from thoughts
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let start = app.spinner_tick / 3 % ws.thoughts.len().max(1);
    let width = area.width as usize;

    let mut used = 1;
    let mut idx = start;
    let count = ws.thoughts.len();
    for _ in 0..count {
        let (cat, content) = &ws.thoughts[idx % count];
        let icon = category_icon(cat);
        let entry = format!("{icon} {content}");
        if used + entry.len() + 4 > width {
            break;
        }
        if used > 1 {
            spans.push(Span::styled(
                "  \u{00b7}  ",
                Style::default().fg(theme::WAX),
            ));
            used += 5;
        }
        spans.push(Span::styled(
            icon.to_string(),
            Style::default().fg(theme::POLLEN),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            content.clone(),
            Style::default().fg(theme::SMOKE),
        ));
        used += entry.len();
        idx += 1;
    }

    let line = Line::from(spans);
    let p = Paragraph::new(line).style(Style::default().bg(theme::COMB));
    frame.render_widget(p, area);
}

// ── Dashboard line renderers ─────────────────────────────

/// Render a 3-line worker item with `▌` color bar into the lines vec.
fn render_worker_item(
    lines: &mut Vec<Line<'static>>,
    worker: &app::WorkerInfo,
    selected: bool,
    idx: usize,
    width: usize,
) {
    let phase = app::phase_display(worker);

    let (status_icon, status_style) = match phase {
        "running" => ("\u{25cf}", theme::status_running()),
        "waiting" => ("\u{25cb}", Style::default().fg(theme::POLLEN)),
        "completed" => ("\u{2713}", theme::status_done()),
        "failed" => ("\u{2717}", theme::error()),
        _ => ("\u{25cb}", theme::status_idle()),
    };

    let elapsed = app::elapsed_display(&worker.created_at);
    let wt_color = theme::SIDEBAR_COLORS[idx % theme::SIDEBAR_COLORS.len()];
    let bar = Span::styled("\u{258c}", Style::default().fg(wt_color));
    let bg = if selected {
        Style::default().bg(theme::FOCUS_BG)
    } else {
        Style::default()
    };
    let selector = if selected { "\u{25b8}" } else { " " };

    // Line 1: ▌▸ ● worker-id  #PR
    let mut line1_spans: Vec<Span> = vec![
        bar.clone(),
        Span::styled(
            format!("{selector} "),
            if selected {
                theme::selected()
            } else {
                theme::text()
            },
        ),
        Span::styled(format!("{status_icon} "), status_style),
        Span::styled(
            worker.id.clone(),
            if selected {
                theme::selected()
            } else {
                theme::text()
            },
        ),
    ];
    if let Some(ref pr) = worker.pr {
        line1_spans.push(Span::styled(
            format!("  #{}", pr.number),
            Style::default().fg(theme::MINT),
        ));
    }
    lines.push(Line::from(line1_spans).style(bg));

    // Line 2: ▌   phase elapsed · branch
    let branch_max = width.saturating_sub(phase.len() + elapsed.len() + 8).max(4);
    let branch = trunc(&worker.branch, branch_max);
    let line2_spans: Vec<Span> = vec![
        bar.clone(),
        Span::raw("  "),
        Span::styled(format!("{phase} {elapsed}"), theme::muted()),
        Span::styled(" \u{00b7} ", Style::default().fg(theme::WAX)),
        Span::styled(branch, Style::default().fg(theme::ICE)),
    ];
    lines.push(Line::from(line2_spans).style(bg));

    // Line 3: ▌   truncated prompt...
    let prompt_max = width.saturating_sub(5).max(6);
    let prompt = trunc(&worker.prompt, prompt_max);
    let line3_spans: Vec<Span> = vec![bar, Span::raw("  "), Span::styled(prompt, theme::muted())];
    lines.push(Line::from(line3_spans).style(bg));
}

/// Render a single signal line inside a card.
fn render_signal_line(
    signal: &crate::buzz::signal::SignalRecord,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let icon = app::severity_icon(&signal.severity);
    let sev_style = severity_style(&signal.severity);
    let line_style = if selected {
        theme::selected()
    } else {
        theme::text()
    };
    let bg = if selected {
        Style::default().bg(theme::FOCUS_BG)
    } else {
        Style::default()
    };

    // Severity-colored bar
    let sev_color = match signal.severity {
        crate::buzz::signal::Severity::Critical | crate::buzz::signal::Severity::Error => {
            theme::EMBER
        }
        crate::buzz::signal::Severity::Warning => theme::NECTAR,
        crate::buzz::signal::Severity::Info => theme::SMOKE,
    };

    let ago = time_ago(&signal.updated_at);
    let is_snoozed = signal.snoozed_until.is_some_and(|t| t > chrono::Utc::now());
    let snooze_tag = if is_snoozed { " zz" } else { "" };
    let meta = format!("{snooze_tag} {:>8} {:>4}", signal.source, ago);
    let clean_title = strip_repo_prefix(&signal.title);
    let title_max = width.saturating_sub(meta.len() + 8);
    let title = trunc(clean_title, title_max);

    let mut spans = vec![
        Span::styled("\u{258c}", Style::default().fg(sev_color)),
        Span::styled(if selected { "\u{25b8}" } else { " " }, line_style),
        Span::styled(format!(" {icon} "), sev_style),
        Span::styled(title, line_style),
    ];
    if is_snoozed {
        spans.push(Span::styled(
            snooze_tag.to_string(),
            Style::default()
                .fg(theme::SMOKE)
                .add_modifier(Modifier::DIM),
        ));
        let rest_meta = format!(" {:>8} {:>4}", signal.source, ago);
        spans.push(Span::styled(rest_meta, theme::muted()));
    } else {
        spans.push(Span::styled(meta, theme::muted()));
    }

    Line::from(spans).style(bg)
}

// ── Worker detail (full-screen) ──────────────────────────

fn draw_worker_detail(frame: &mut Frame, app: &App, area: Rect, idx: usize) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };
    let worker = match ws.workers.get(idx) {
        Some(w) => w,
        None => return,
    };

    let phase = app::phase_display(worker);
    let elapsed = app::elapsed_display(&worker.created_at);

    // Status icon
    let (status_icon, status_style) = match phase {
        "running" => ("\u{25cf}", theme::status_running()),
        "waiting" => ("\u{25cb}", Style::default().fg(theme::POLLEN)),
        "completed" => ("\u{2713}", theme::status_done()),
        "failed" => ("\u{2717}", theme::error()),
        _ => ("\u{25cb}", theme::status_idle()),
    };

    // Input bar height — account for visual line wrapping
    let input_h: u16 = if app.worker_input_active {
        let avail_w = area.width;
        let rows = visual_input_rows(&app.worker_input, app.worker_input.len(), avail_w);
        rows.clamp(1, 6) + 1 // +1 for top border separator
    } else {
        0
    };

    // Layout: header (1) + body (fill) + input (input_h, 0 when inactive)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_h),
        ])
        .split(area);

    // ── Header bar (compact single line) ──
    let mut header_spans = vec![
        Span::styled(format!(" {status_icon} "), status_style),
        Span::styled(worker.id.clone(), theme::title()),
        Span::styled(format!("  {phase} {elapsed}  ",), theme::muted()),
        Span::styled(worker.branch.clone(), Style::default().fg(theme::ICE)),
    ];
    if let Some(ref pr) = worker.pr {
        header_spans.push(Span::styled(
            format!("  #{}", pr.number),
            Style::default().fg(theme::MINT),
        ));
    }

    let header_line = Line::from(header_spans);
    let header = Paragraph::new(header_line).style(Style::default().bg(theme::COMB));
    frame.render_widget(header, chunks[0]);

    // ── Split body: activity log (40%) | conversation (60%) ──
    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    // ── Left: Activity log ──
    let activity_block = if app.worker_activity_focused {
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(theme::border_active())
    } else {
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(theme::STEEL))
    };
    draw_activity_log_with_block(frame, worker, body_cols[0], activity_block);

    // ── Right: Conversation body ──
    let conv_border_style = if !app.worker_activity_focused {
        theme::border_active()
    } else {
        Style::default().fg(theme::STEEL)
    };
    let conv_block = Block::default()
        .borders(Borders::NONE)
        .border_style(conv_border_style);

    if worker.conversation.is_empty() {
        let inner = conv_block.inner(body_cols[1]);
        frame.render_widget(conv_block, body_cols[1]);
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("No conversation yet.", theme::muted()),
            ]),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("Waiting for agent to start...", theme::muted()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    } else {
        let mut lines: Vec<Line<'_>> = Vec::new();
        conversation::render_conversation(&mut lines, &worker.conversation, None, None);

        apiari_tui::scroll::render_scrollable(
            frame,
            body_cols[1],
            lines,
            &worker.conv_scroll,
            conv_block,
        );
    }

    // ── Input bar ──
    if app.worker_input_active {
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(theme::border_active())
            .title(Span::styled(
                " Send message (enter:send esc:cancel) ",
                theme::accent(),
            ));
        let input_inner = input_block.inner(chunks[2]);
        frame.render_widget(input_block, chunks[2]);

        let input_text = format!(" {}_", app.worker_input);
        let input_p = Paragraph::new(input_text)
            .style(theme::text())
            .wrap(Wrap { trim: false });
        frame.render_widget(input_p, input_inner);
    }
}

// ── Activity log rendering ──────────────────────────────

/// Render the activity log lines from worker events.
fn render_activity_lines<'a>(events: &'a [app::WorkerEvent]) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    lines.push(Line::from(""));

    for event in events {
        let ts_str = event
            .ts
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default();

        let (icon, icon_style) = match event.kind {
            app::WorkerEventKind::Dispatched => {
                ("\u{25cf}", Style::default().fg(theme::HONEY)) // ●
            }
            app::WorkerEventKind::BeeToWorker => {
                ("\u{2192}", Style::default().fg(theme::MINT)) // →
            }
            app::WorkerEventKind::UserToWorker => {
                ("\u{2192}", Style::default().fg(theme::ICE)) // →
            }
            app::WorkerEventKind::PrOpened => {
                ("\u{2705}", Style::default().fg(theme::MINT)) // ✅
            }
            app::WorkerEventKind::CiFailed => ("\u{2717}", theme::error()), // ✗
            app::WorkerEventKind::CiPassed => {
                ("\u{2713}", theme::status_done()) // ✓
            }
            app::WorkerEventKind::Merged => {
                ("\u{2713}", theme::status_done()) // ✓
            }
            app::WorkerEventKind::StatusChange => {
                ("\u{25cb}", Style::default().fg(theme::SMOKE)) // ○
            }
        };

        let kind_label: &str = match event.kind {
            app::WorkerEventKind::Dispatched => "Task dispatched",
            app::WorkerEventKind::BeeToWorker => "Bee \u{2192} worker",
            app::WorkerEventKind::UserToWorker => "You \u{2192} worker",
            app::WorkerEventKind::PrOpened => "PR opened",
            app::WorkerEventKind::CiFailed => "CI failed",
            app::WorkerEventKind::CiPassed => "CI passed",
            app::WorkerEventKind::Merged => "Merged",
            app::WorkerEventKind::StatusChange => "Status",
        };

        // Icon + timestamp + label line
        lines.push(Line::from(vec![
            Span::styled(format!(" {icon} "), icon_style),
            Span::styled(format!("{ts_str}  "), Style::default().fg(theme::SMOKE)),
            Span::styled(kind_label, theme::muted()),
        ]));

        // Event text on next line(s), indented
        for text_line in event.text.lines() {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(text_line.to_string(), theme::text()),
            ]));
        }

        lines.push(Line::from(""));
    }

    lines
}

/// Draw the activity log panel with a custom block (used in the left split of WorkerDetail).
fn draw_activity_log_with_block(
    frame: &mut Frame,
    worker: &app::WorkerInfo,
    area: Rect,
    block: Block<'_>,
) {
    if worker.activity.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("No activity yet.", theme::muted()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    } else {
        let lines = render_activity_lines(&worker.activity);
        apiari_tui::scroll::render_scrollable(frame, area, lines, &worker.activity_scroll, block);
    }
}

// ── Worker chat (full-screen activity view) ──────────────

fn draw_worker_chat(frame: &mut Frame, app: &App, area: Rect, idx: usize) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };
    let worker = match ws.workers.get(idx) {
        Some(w) => w,
        None => return,
    };

    let phase = app::phase_display(worker);

    // Status icon
    let (status_icon, status_style) = match phase {
        "running" => ("\u{25cf}", theme::status_running()),
        "waiting" => ("\u{25cb}", Style::default().fg(theme::POLLEN)),
        "completed" => ("\u{2713}", theme::status_done()),
        "failed" => ("\u{2717}", theme::error()),
        _ => ("\u{25cb}", theme::status_idle()),
    };

    // Layout: header (1) + chat body (fill)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // ── Header ──
    let mut header_spans = vec![
        Span::styled(format!(" {status_icon} "), status_style),
        Span::styled(format!("{} ", worker.id), theme::title()),
        Span::styled("Activity Log  ", theme::muted()),
    ];
    if let Some(ref pr) = worker.pr {
        header_spans.push(Span::styled(
            format!("PR #{} ", pr.number),
            Style::default().fg(theme::MINT),
        ));
        header_spans.push(Span::styled(
            pr.url.clone(),
            Style::default().fg(theme::ICE),
        ));
    }
    let header_line = Line::from(header_spans);
    let header = Paragraph::new(header_line).style(Style::default().bg(theme::COMB));
    frame.render_widget(header, chunks[0]);

    // ── Chat body: render activity events as conversation entries ──
    let entries: Vec<conversation::ConversationEntry> = worker
        .activity
        .iter()
        .map(|ev| match ev.kind {
            app::WorkerEventKind::Dispatched | app::WorkerEventKind::UserToWorker => {
                let ts = ev
                    .ts
                    .map(|t| t.format("%H:%M").to_string())
                    .unwrap_or_default();
                conversation::ConversationEntry::User {
                    text: ev.text.clone(),
                    timestamp: ts,
                }
            }
            app::WorkerEventKind::BeeToWorker => {
                let ts = ev
                    .ts
                    .map(|t| t.format("%H:%M").to_string())
                    .unwrap_or_default();
                conversation::ConversationEntry::AssistantText {
                    text: ev.text.clone(),
                    timestamp: ts,
                }
            }
            app::WorkerEventKind::PrOpened
            | app::WorkerEventKind::CiFailed
            | app::WorkerEventKind::CiPassed
            | app::WorkerEventKind::Merged
            | app::WorkerEventKind::StatusChange => conversation::ConversationEntry::Status {
                text: ev.text.clone(),
            },
        })
        .collect();

    let block = Block::default();

    if entries.is_empty() {
        let inner = block.inner(chunks[1]);
        frame.render_widget(block, chunks[1]);
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("No activity yet.", theme::muted()),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    } else {
        let mut lines: Vec<Line<'_>> = Vec::new();
        conversation::render_conversation(&mut lines, &entries, None, Some("Bee"));

        apiari_tui::scroll::render_scrollable(
            frame,
            chunks[1],
            lines,
            &worker.activity_scroll,
            block,
        );
    }
}

// ── Signal detail (full-screen) ──────────────────────────

fn draw_signal_detail(frame: &mut Frame, app: &App, area: Rect, idx: usize) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };
    let signal = match ws.signals.get(idx) {
        Some(s) => s,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();

    // Title
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Signal: ", theme::muted()),
        Span::styled(format!("[{}] ", signal.source), theme::accent()),
        Span::styled(strip_repo_prefix(&signal.title).to_string(), theme::title()),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "  {}",
            "\u{2500}".repeat((area.width as usize).saturating_sub(4))
        ),
        theme::border(),
    )));

    // Severity
    let icon = app::severity_icon(&signal.severity);
    let sev_style = severity_style(&signal.severity);
    lines.push(Line::from(vec![
        Span::styled("  Severity: ", theme::muted()),
        Span::styled(format!("{icon} {}", signal.severity), sev_style),
    ]));

    // Source
    lines.push(Line::from(vec![
        Span::styled("  Source:   ", theme::muted()),
        Span::styled(signal.source.clone(), theme::text()),
    ]));

    // Updated
    let ago = time_ago(&signal.updated_at);
    lines.push(Line::from(vec![
        Span::styled("  Updated:  ", theme::muted()),
        Span::styled(ago, theme::text()),
    ]));

    // URL
    if let Some(ref url) = signal.url {
        lines.push(Line::from(vec![
            Span::styled("  URL:      ", theme::muted()),
            Span::styled(url.clone(), Style::default().fg(theme::HONEY)),
        ]));
    }

    lines.push(Line::from(Span::styled(
        format!(
            "  {}",
            "\u{2500}".repeat((area.width as usize).saturating_sub(4))
        ),
        theme::border(),
    )));

    // Body
    if let Some(ref body) = signal.body {
        lines.push(Line::from(""));
        for line in body.lines() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_string(), theme::text()),
            ]));
        }
    }

    // Hints
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  o", theme::key_hint()),
        Span::styled(":open  ", theme::key_desc()),
        Span::styled("R", theme::key_hint()),
        Span::styled(":resolve  ", theme::key_desc()),
        Span::styled("z", theme::key_hint()),
        Span::styled(":snooze  ", theme::key_desc()),
        Span::styled("esc", theme::key_hint()),
        Span::styled(":back", theme::key_desc()),
    ]));

    // No Wrap — keeps URLs intact for terminal click-detection
    let paragraph = Paragraph::new(lines).scroll((app.content_scroll, 0));
    frame.render_widget(paragraph, area);
}

// ── Signal list (full-screen) ────────────────────────────

fn draw_signal_list(frame: &mut Frame, app: &App, area: Rect) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();
    let width = area.width as usize;

    // Header
    lines.push(Line::from(""));
    let header = format!(" All Signals ({}) ", ws.signals.len());
    let ruler_len = width.saturating_sub(header.len());
    lines.push(Line::from(vec![
        Span::styled(header, theme::subtitle()),
        Span::styled("\u{2500}".repeat(ruler_len), theme::border()),
    ]));
    lines.push(Line::from(""));

    for (i, signal) in ws.signals.iter().enumerate() {
        let is_sel = i == app.signal_list_selection;
        lines.push(render_signal_line(signal, is_sel, width));
    }

    if ws.signals.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("No open signals", theme::muted()),
        ]));
    }

    // Hints
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  j/k", theme::key_hint()),
        Span::styled(":nav  ", theme::key_desc()),
        Span::styled("enter", theme::key_hint()),
        Span::styled(":detail  ", theme::key_desc()),
        Span::styled("o", theme::key_hint()),
        Span::styled(":open  ", theme::key_desc()),
        Span::styled("R", theme::key_hint()),
        Span::styled(":resolve  ", theme::key_desc()),
        Span::styled("z", theme::key_hint()),
        Span::styled(":snooze  ", theme::key_desc()),
        Span::styled("esc", theme::key_hint()),
        Span::styled(":back", theme::key_desc()),
    ]));

    let paragraph = Paragraph::new(lines)
        .scroll((app.content_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

// ── Review list (full-screen) ─────────────────────────

fn draw_review_list(frame: &mut Frame, app: &App, area: Rect) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };

    let width = area.width as usize;

    // Collect review queue signals (any source ending with _review_queue)
    let reviews: Vec<&crate::buzz::signal::SignalRecord> = ws
        .signals
        .iter()
        .filter(|s| s.source.ends_with("_review_queue"))
        .collect();

    let mut lines: Vec<Line> = Vec::new();

    // Header
    lines.push(Line::from(""));
    let header = format!(" Reviews ({}) ", reviews.len());
    let ruler_len = width.saturating_sub(header.len());
    lines.push(Line::from(vec![
        Span::styled(header, theme::subtitle()),
        Span::styled("\u{2500}".repeat(ruler_len), theme::border()),
    ]));
    lines.push(Line::from(""));

    if reviews.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("No review queue items", theme::muted()),
        ]));
    } else {
        // Group by source when multiple sources are present
        let mut sources: Vec<String> = reviews.iter().map(|s| s.source.clone()).collect();
        sources.sort();
        sources.dedup();
        let multi_source = sources.len() > 1;

        let mut flat_idx = 0usize;
        for source in &sources {
            if multi_source {
                // Source header: extract short name (e.g. "github" from "github_review_queue")
                let short = source.strip_suffix("_review_queue").unwrap_or(source);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("\u{2500}\u{2500} {short} \u{2500}\u{2500}"),
                        theme::muted(),
                    ),
                ]));
            }

            for signal in reviews.iter().filter(|s| &s.source == source) {
                let is_sel = flat_idx == app.review_list_selection;

                // Build line with query_name prefix
                let qname = signal
                    .metadata
                    .as_ref()
                    .and_then(|meta| serde_json::from_str::<serde_json::Value>(meta).ok())
                    .and_then(|v| {
                        v.get("query_name")
                            .and_then(|q| q.as_str())
                            .map(String::from)
                    });

                let icon = app::severity_icon(&signal.severity);
                let sev_style = severity_style(&signal.severity);
                let sev_color = match signal.severity {
                    crate::buzz::signal::Severity::Critical
                    | crate::buzz::signal::Severity::Error => theme::EMBER,
                    crate::buzz::signal::Severity::Warning => theme::NECTAR,
                    crate::buzz::signal::Severity::Info => theme::SMOKE,
                };
                let line_style = if is_sel {
                    theme::selected()
                } else {
                    theme::text()
                };
                let bg = if is_sel {
                    Style::default().bg(theme::FOCUS_BG)
                } else {
                    Style::default()
                };

                let ago = time_ago(&signal.updated_at);
                let clean_title = strip_repo_prefix(&signal.title);

                let mut spans: Vec<Span> = vec![
                    Span::styled("\u{258c}", Style::default().fg(sev_color)),
                    Span::styled(if is_sel { "\u{25b8}" } else { " " }, line_style),
                    Span::styled(format!(" {icon} "), sev_style),
                ];

                // Add query_name as dim prefix before title
                if let Some(ref qn) = qname {
                    spans.push(Span::styled(
                        format!("{qn} "),
                        Style::default()
                            .fg(theme::POLLEN)
                            .add_modifier(Modifier::DIM),
                    ));
                }

                let meta_str = format!(" {:>4}", ago);
                let prefix_len: usize = spans.iter().map(|s| s.content.len()).sum();
                let title_max = width.saturating_sub(prefix_len + meta_str.len());
                let title = trunc(clean_title, title_max);

                spans.push(Span::styled(title, line_style));
                spans.push(Span::styled(meta_str, theme::muted()));

                lines.push(Line::from(spans).style(bg));
                flat_idx += 1;
            }
        }
    }

    // Hints
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  j/k", theme::key_hint()),
        Span::styled(":nav  ", theme::key_desc()),
        Span::styled("enter", theme::key_hint()),
        Span::styled(":detail  ", theme::key_desc()),
        Span::styled("a", theme::key_hint()),
        Span::styled(":approve  ", theme::key_desc()),
        Span::styled("c", theme::key_hint()),
        Span::styled(":comment  ", theme::key_desc()),
        Span::styled("o", theme::key_hint()),
        Span::styled(":open  ", theme::key_desc()),
        Span::styled("z", theme::key_hint()),
        Span::styled(":snooze  ", theme::key_desc()),
        Span::styled("esc", theme::key_hint()),
        Span::styled(":back", theme::key_desc()),
    ]));

    let paragraph = Paragraph::new(lines)
        .scroll((app.content_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

// ── PR list (full-screen) ─────────────────────────────

fn draw_pr_list(frame: &mut Frame, app: &App, area: Rect) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };
    let prs: Vec<(usize, &app::WorkerInfo)> = ws
        .workers
        .iter()
        .enumerate()
        .filter(|(_, w)| w.pr.is_some())
        .collect();

    let mut lines: Vec<Line> = Vec::new();
    let width = area.width as usize;

    // Header
    lines.push(Line::from(""));
    let header = format!(" Pull Requests ({}) ", prs.len());
    let ruler_len = width.saturating_sub(header.len());
    lines.push(Line::from(vec![
        Span::styled(header, theme::subtitle()),
        Span::styled("\u{2500}".repeat(ruler_len), theme::border()),
    ]));
    lines.push(Line::from(""));

    for (list_idx, (_, worker)) in prs.iter().enumerate() {
        let pr = worker.pr.as_ref().unwrap();
        let is_sel = list_idx == app.pr_list_selection;
        let line_style = if is_sel {
            theme::selected()
        } else {
            theme::text()
        };
        let bg = if is_sel {
            Style::default().bg(theme::FOCUS_BG)
        } else {
            Style::default()
        };

        let state_style = match pr.state.as_str() {
            "OPEN" => Style::default().fg(theme::MINT),
            "MERGED" => theme::status_done(),
            "CLOSED" => theme::error(),
            _ => theme::muted(),
        };

        let title_max = width.saturating_sub(30).max(10);
        let title = trunc(&pr.title, title_max);

        lines.push(
            Line::from(vec![
                Span::styled(if is_sel { " \u{25b6}" } else { "  " }, line_style),
                Span::styled(
                    format!(" #{:<5}", pr.number),
                    Style::default().fg(theme::MINT),
                ),
                Span::styled(format!(" {:>6} ", pr.state), state_style),
                Span::styled(title, line_style),
                Span::styled(format!("  ({})", worker.id), theme::muted()),
            ])
            .style(bg),
        );
        // URL on second line (cmd+clickable in terminals)
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(&pr.url, Style::default().fg(theme::FROST)),
        ]));
        lines.push(Line::from(""));
    }

    if prs.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("No pull requests", theme::muted()),
        ]));
    }

    // Hints
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  j/k", theme::key_hint()),
        Span::styled(":nav  ", theme::key_desc()),
        Span::styled("enter", theme::key_hint()),
        Span::styled(":detail  ", theme::key_desc()),
        Span::styled("o", theme::key_hint()),
        Span::styled(":open  ", theme::key_desc()),
        Span::styled("esc", theme::key_hint()),
        Span::styled(":back", theme::key_desc()),
    ]));

    let paragraph = Paragraph::new(lines)
        .scroll((app.content_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

// ── Status bar ───────────────────────────────────────────

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    // Flash message takes priority
    if let Some(ref flash) = app.flash {
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(flash.text.clone(), theme::accent()),
        ]);
        let bar = Paragraph::new(line).style(Style::default().bg(theme::COMB));
        frame.render_widget(bar, area);
        return;
    }

    // Context-sensitive hints
    let hints: Vec<Span> = match &app.view {
        View::Dashboard => {
            let zoomed = if area.width < 50 {
                Some(app.zoomed_panel.unwrap_or(app.focused_panel))
            } else {
                app.zoomed_panel
            };
            if app.review_comment_active {
                vec![
                    Span::raw(" "),
                    Span::styled("enter", theme::key_hint()),
                    Span::styled(":send  ", theme::key_desc()),
                    Span::styled("esc", theme::key_hint()),
                    Span::styled(":cancel", theme::key_desc()),
                ]
            } else if app.chat_focused {
                vec![
                    Span::raw(" "),
                    Span::styled("enter", theme::key_hint()),
                    Span::styled(":send  ", theme::key_desc()),
                    Span::styled("alt+enter", theme::key_hint()),
                    Span::styled(":newline  ", theme::key_desc()),
                    Span::styled("ctrl+u/d", theme::key_hint()),
                    Span::styled(":scroll  ", theme::key_desc()),
                    Span::styled("esc", theme::key_hint()),
                    Span::styled(":back to nav", theme::key_desc()),
                ]
            } else if zoomed.is_some() {
                // Panel indicator + zoom nav hints
                let panel_name = match app.focused_panel {
                    Panel::Home => "Home",
                    Panel::Workers => "Workers",
                    Panel::Shells => "Shells",
                    Panel::Signals => "Signals",
                    Panel::Reviews => "Reviews",
                    Panel::Feed => "Feed",
                    Panel::Chat => "Chat",
                };
                vec![
                    Span::raw(" "),
                    Span::styled(
                        panel_name,
                        Style::default()
                            .fg(theme::HONEY)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  ", theme::key_desc()),
                    Span::styled("h/l", theme::key_hint()),
                    Span::styled(":prev/next  ", theme::key_desc()),
                    Span::styled("z", theme::key_hint()),
                    Span::styled(":unzoom  ", theme::key_desc()),
                    Span::styled("c", theme::key_hint()),
                    Span::styled(":chat  ", theme::key_desc()),
                    Span::styled("q", theme::key_hint()),
                    Span::styled(":quit", theme::key_desc()),
                ]
            } else {
                let has_reviews = app.current_ws().is_some_and(|ws| {
                    ws.signals
                        .iter()
                        .any(|s| s.source.ends_with("_review_queue"))
                });
                let on_reviews_panel = app.focused_panel == Panel::Reviews;
                let github_review = on_reviews_panel
                    && app
                        .selected_signal()
                        .is_some_and(|s| s.source == "github_review_queue");
                let mut h = vec![
                    Span::raw(" "),
                    Span::styled("1-5", theme::key_hint()),
                    Span::styled(":jump  ", theme::key_desc()),
                    Span::styled("tab", theme::key_hint()),
                    Span::styled(":panel  ", theme::key_desc()),
                    Span::styled("j/k", theme::key_hint()),
                    Span::styled(":nav  ", theme::key_desc()),
                    Span::styled("enter", theme::key_hint()),
                    Span::styled(":detail  ", theme::key_desc()),
                ];
                if github_review {
                    h.push(Span::styled("a", theme::key_hint()));
                    h.push(Span::styled(":approve  ", theme::key_desc()));
                    h.push(Span::styled("c", theme::key_hint()));
                    h.push(Span::styled(":comment  ", theme::key_desc()));
                } else if !on_reviews_panel {
                    h.push(Span::styled("c", theme::key_hint()));
                    h.push(Span::styled(":chat  ", theme::key_desc()));
                }
                h.push(Span::styled("z", theme::key_hint()));
                h.push(Span::styled(":zoom  ", theme::key_desc()));
                h.push(Span::styled("p", theme::key_hint()));
                h.push(Span::styled(":prs  ", theme::key_desc()));
                if has_reviews {
                    h.push(Span::styled("r", theme::key_hint()));
                    h.push(Span::styled(":reviews  ", theme::key_desc()));
                }
                h.push(Span::styled("?", theme::key_hint()));
                h.push(Span::styled(":help  ", theme::key_desc()));
                h.push(Span::styled("q", theme::key_hint()));
                h.push(Span::styled(":quit", theme::key_desc()));
                h
            }
        }
        View::WorkerDetail(_) => {
            if app.worker_input_active {
                vec![
                    Span::raw(" "),
                    Span::styled("enter", theme::key_hint()),
                    Span::styled(":send  ", theme::key_desc()),
                    Span::styled("esc", theme::key_hint()),
                    Span::styled(":cancel", theme::key_desc()),
                ]
            } else {
                vec![
                    Span::raw(" "),
                    Span::styled("tab", theme::key_hint()),
                    Span::styled(":pane  ", theme::key_desc()),
                    Span::styled("c", theme::key_hint()),
                    Span::styled(":activity  ", theme::key_desc()),
                    Span::styled("m", theme::key_hint()),
                    Span::styled(":message  ", theme::key_desc()),
                    Span::styled("j/k", theme::key_hint()),
                    Span::styled(":scroll  ", theme::key_desc()),
                    Span::styled("o", theme::key_hint()),
                    Span::styled(":open PR  ", theme::key_desc()),
                    Span::styled("x", theme::key_hint()),
                    Span::styled(":close  ", theme::key_desc()),
                    Span::styled("esc", theme::key_hint()),
                    Span::styled(":back", theme::key_desc()),
                ]
            }
        }
        View::WorkerChat(_) => vec![
            Span::raw(" "),
            Span::styled("j/k", theme::key_hint()),
            Span::styled(":scroll  ", theme::key_desc()),
            Span::styled("o", theme::key_hint()),
            Span::styled(":open PR  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled(":back", theme::key_desc()),
        ],
        View::SignalDetail(_) => vec![
            Span::raw(" "),
            Span::styled("j/k", theme::key_hint()),
            Span::styled(":scroll  ", theme::key_desc()),
            Span::styled("o", theme::key_hint()),
            Span::styled(":open  ", theme::key_desc()),
            Span::styled("R", theme::key_hint()),
            Span::styled(":resolve  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled(":back", theme::key_desc()),
        ],
        View::PrList => vec![
            Span::raw(" "),
            Span::styled("j/k", theme::key_hint()),
            Span::styled(":nav  ", theme::key_desc()),
            Span::styled("enter", theme::key_hint()),
            Span::styled(":detail  ", theme::key_desc()),
            Span::styled("o", theme::key_hint()),
            Span::styled(":open  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled(":back", theme::key_desc()),
        ],
        View::SignalList => vec![
            Span::raw(" "),
            Span::styled("j/k", theme::key_hint()),
            Span::styled(":nav  ", theme::key_desc()),
            Span::styled("enter", theme::key_hint()),
            Span::styled(":detail  ", theme::key_desc()),
            Span::styled("o", theme::key_hint()),
            Span::styled(":open  ", theme::key_desc()),
            Span::styled("R", theme::key_hint()),
            Span::styled(":resolve  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled(":back", theme::key_desc()),
        ],
        View::ReviewList => {
            if app.review_comment_active {
                vec![
                    Span::raw(" "),
                    Span::styled("enter", theme::key_hint()),
                    Span::styled(":send  ", theme::key_desc()),
                    Span::styled("esc", theme::key_hint()),
                    Span::styled(":cancel", theme::key_desc()),
                ]
            } else {
                let is_github = app
                    .selected_signal()
                    .is_some_and(|s| s.source == "github_review_queue");
                let mut h = vec![
                    Span::raw(" "),
                    Span::styled("j/k", theme::key_hint()),
                    Span::styled(":nav  ", theme::key_desc()),
                    Span::styled("enter", theme::key_hint()),
                    Span::styled(":detail  ", theme::key_desc()),
                ];
                if is_github {
                    h.push(Span::styled("a", theme::key_hint()));
                    h.push(Span::styled(":approve  ", theme::key_desc()));
                    h.push(Span::styled("c", theme::key_hint()));
                    h.push(Span::styled(":comment  ", theme::key_desc()));
                }
                h.push(Span::styled("o", theme::key_hint()));
                h.push(Span::styled(":open  ", theme::key_desc()));
                h.push(Span::styled("esc", theme::key_hint()));
                h.push(Span::styled(":back", theme::key_desc()));
                h
            }
        }
    };

    // Right-aligned daemon status with ECG heartbeat trace
    let cur_ws = app.current_ws();
    let ecg = activity_graph(&app.activity_buf);
    // ECG color reflects signal severity
    let ecg_color = if !app.daemon_alive {
        theme::STEEL
    } else if let Some(ws) = app.current_ws() {
        let has_crit = ws.signals.iter().any(|s| {
            matches!(
                s.severity,
                crate::buzz::signal::Severity::Critical | crate::buzz::signal::Severity::Error
            )
        });
        let has_warn = ws
            .signals
            .iter()
            .any(|s| s.severity == crate::buzz::signal::Severity::Warning);
        if has_crit {
            theme::EMBER
        } else if has_warn {
            theme::NECTAR
        } else {
            theme::MINT
        }
    } else {
        theme::MINT
    };
    let conn_indicator = if app.daemon_connected {
        "\u{25cf}" // ● connected to daemon
    } else {
        "\u{25cb}" // ○ local fallback
    };
    let conn_label: Cow<'_, str> = if app.daemon_remote {
        match &app.remote_host {
            Some(host) => format!("remote ({host})").into(),
            None => "remote".into(),
        }
    } else if app.daemon_connected {
        "daemon".into()
    } else {
        "local".into()
    };
    let conn_style = if app.daemon_remote {
        Style::default().fg(theme::FROST) // cyan for remote
    } else {
        theme::muted()
    };

    // Live stats: workers, signals, turns, context usage
    let worker_count = cur_ws.map_or(0, |ws| ws.workers.len());
    let signal_count = cur_ws.map_or(0, |ws| ws.signals.len());
    let turn_count = cur_ws.map_or(0, |ws| ws.coordinator_turns);
    let mut stats_str =
        format!("workers:{worker_count}  signals:{signal_count}  turns:{turn_count}");

    // Append context usage if available
    let ctx_style = if let Some(ws) = cur_ws
        && ws.usage_context_window > 0
    {
        let pct = ws.usage_input_tokens as f64 / ws.usage_context_window as f64 * 100.0;
        stats_str.push_str(&format!(
            "  ctx:{}% ({}/{})",
            pct as u32,
            format_tokens(ws.usage_input_tokens),
            format_tokens(ws.usage_context_window),
        ));
        if let Some(cost) = ws.usage_cost_usd {
            stats_str.push_str(&format!("  ${cost:.3}"));
        }
        if pct > 80.0 {
            Some(Style::default().fg(theme::EMBER))
        } else if pct > 50.0 {
            Some(Style::default().fg(theme::HONEY))
        } else {
            None
        }
    } else {
        None
    };
    stats_str.push_str("  ");

    let stats_style = ctx_style.unwrap_or_else(theme::muted);

    let daemon_spans: Vec<Span> = if app.daemon_alive || app.daemon_remote {
        vec![
            Span::styled(stats_str, stats_style),
            Span::styled(ecg, Style::default().fg(ecg_color)),
            Span::styled(format!(" {conn_indicator} {conn_label} "), conn_style),
        ]
    } else {
        vec![
            Span::styled(stats_str, stats_style),
            Span::styled(ecg, Style::default().fg(ecg_color)),
            Span::styled(
                format!(" {conn_indicator} {conn_label} offline "),
                conn_style,
            ),
        ]
    };

    // On narrow terminals (mobile), skip key hints — just show daemon status
    let hints = if area.width < 120 {
        vec![Span::raw(" ")]
    } else {
        hints
    };

    // Calculate padding for right-alignment (use display width for Unicode correctness)
    let hints_len = Line::from(hints.clone()).width();
    let daemon_len = Line::from(daemon_spans.clone()).width();
    let padding = (area.width as usize)
        .saturating_sub(hints_len)
        .saturating_sub(daemon_len);

    let mut all_spans = hints;
    if padding > 0 {
        all_spans.push(Span::raw(" ".repeat(padding)));
    }
    all_spans.extend(daemon_spans);

    let line = Line::from(all_spans);
    let bar = Paragraph::new(line).style(Style::default().bg(theme::COMB));
    frame.render_widget(bar, area);
}

// ── Help overlay ─────────────────────────────────────────

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let w = 54u16.min(area.width.saturating_sub(4));
    let h = 32u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .title(Span::styled(" Keyboard Shortcuts ", theme::title()));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Dashboard",
            Style::default().fg(theme::ICE).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  Tab / S-Tab   ", theme::key_hint()),
            Span::styled("Switch panel (W/S/F/C)", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  j/k  up/dn    ", theme::key_hint()),
            Span::styled("Navigate in panel", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Enter         ", theme::key_hint()),
            Span::styled("Drill into selected", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  c             ", theme::key_hint()),
            Span::styled("Focus chat input", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  1-5           ", theme::key_hint()),
            Span::styled("Jump to panel", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  z             ", theme::key_hint()),
            Span::styled("Zoom focused panel", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  S             ", theme::key_hint()),
            Span::styled("Signal list (all)", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  p             ", theme::key_hint()),
            Span::styled("Pull requests list", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  r             ", theme::key_hint()),
            Span::styled("Review list", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  o             ", theme::key_hint()),
            Span::styled("Open URL in browser", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  x             ", theme::key_hint()),
            Span::styled("Close worker (confirm)", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  R             ", theme::key_hint()),
            Span::styled("Resolve signal (confirm)", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  d             ", theme::key_hint()),
            Span::styled("Toggle signal debug mode", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  q             ", theme::key_hint()),
            Span::styled("Quit", theme::key_desc()),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Chat (when focused)",
            Style::default().fg(theme::ICE).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  Enter         ", theme::key_hint()),
            Span::styled("Send message", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Alt+Enter     ", theme::key_hint()),
            Span::styled("Newline in input", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+U/D      ", theme::key_hint()),
            Span::styled("Scroll chat", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Esc           ", theme::key_hint()),
            Span::styled("Back to nav mode", theme::key_desc()),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Detail Views",
            Style::default().fg(theme::ICE).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  j/k           ", theme::key_hint()),
            Span::styled("Scroll content", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Esc           ", theme::key_hint()),
            Span::styled("Back to dashboard", theme::key_desc()),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Global",
            Style::default().fg(theme::ICE).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  Ctrl+B n/p    ", theme::key_hint()),
            Span::styled("Next/prev workspace", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+B 1-9    ", theme::key_hint()),
            Span::styled("Jump to workspace", theme::key_desc()),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+C        ", theme::key_hint()),
            Span::styled("Quit", theme::key_desc()),
        ]),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ── Confirm overlay ──────────────────────────────────────

fn draw_confirm_overlay(frame: &mut Frame, area: Rect, action: &PendingAction, app: &App) {
    match action {
        PendingAction::SnoozeSignal(_) => {
            draw_snooze_overlay(frame, area, app);
        }
        _ => {
            let message = match action {
                PendingAction::CloseWorker(id) => format!("Close worker '{id}'?"),
                PendingAction::ResolveSignal(id) => format!("Resolve signal #{id}?"),
                PendingAction::ApproveReview { repo, pr_number } => {
                    format!("Approve PR #{pr_number} in {repo}?")
                }
                PendingAction::KillShell(name) => format!("Kill shell '{name}'?"),
                PendingAction::SnoozeSignal(_) => unreachable!(),
            };

            let w = (message.len() as u16 + 8).min(area.width.saturating_sub(4));
            let h = 5u16;
            let x = (area.width.saturating_sub(w)) / 2;
            let y = (area.height.saturating_sub(h)) / 2;
            let popup = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_active())
                .title(Span::styled(" Confirm ", theme::title()));

            let inner = block.inner(popup);
            frame.render_widget(block, popup);

            let lines = vec![
                Line::from(""),
                Line::from(vec![Span::styled(format!("  {message}"), theme::text())]),
                Line::from(vec![
                    Span::styled("  y", theme::key_hint()),
                    Span::styled(":yes  ", theme::key_desc()),
                    Span::styled("n", theme::key_hint()),
                    Span::styled(":no", theme::key_desc()),
                ]),
            ];

            let paragraph = Paragraph::new(lines);
            frame.render_widget(paragraph, inner);
        }
    }
}

fn draw_snooze_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let w = 32u16.min(area.width.saturating_sub(4));
    let h = (app::SNOOZE_OPTIONS.len() as u16 + 5).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .title(Span::styled(" Snooze until... ", theme::title()));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    for (i, option) in app::SNOOZE_OPTIONS.iter().enumerate() {
        let selected = i == app.snooze_selection;
        let marker = if selected { "\u{25b8} " } else { "  " };
        let style = if selected {
            theme::selected()
        } else {
            theme::text()
        };
        let bg = if selected {
            Style::default().bg(theme::FOCUS_BG)
        } else {
            Style::default()
        };
        lines.push(
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{marker}{option}"), style),
            ])
            .style(bg),
        );
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  j/k", theme::key_hint()),
        Span::styled(":select  ", theme::key_desc()),
        Span::styled("enter", theme::key_hint()),
        Span::styled(":snooze  ", theme::key_desc()),
        Span::styled("esc", theme::key_hint()),
        Span::styled(":cancel", theme::key_desc()),
    ]));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn draw_review_comment_input(frame: &mut Frame, area: Rect, app: &App) {
    let title = format!(
        " Comment on PR #{} in {} ",
        app.review_comment_pr, app.review_comment_repo
    );
    let w = (area.width.saturating_sub(4)).min(60);
    let h = 5u16;
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .title(Span::styled(title, theme::accent()));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let input_text = format!(" {}_", app.review_comment_input);
    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(input_text, theme::text())]),
        Line::from(vec![
            Span::styled("  enter", theme::key_hint()),
            Span::styled(":send  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled(":cancel", theme::key_desc()),
        ]),
    ];

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn draw_shell_name_input(frame: &mut Frame, area: Rect, app: &App) {
    let w = (area.width.saturating_sub(4)).min(40);
    let h = 5u16;
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .title(Span::styled(" New Shell ", theme::accent()));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let input_text = format!(" {}_", app.shell_input);
    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(input_text, theme::text())]),
        Line::from(vec![
            Span::styled("  enter", theme::key_hint()),
            Span::styled(":create  ", theme::key_desc()),
            Span::styled("esc", theme::key_hint()),
            Span::styled(":cancel", theme::key_desc()),
        ]),
    ];

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ── Helpers ──────────────────────────────────────────────

/// Extract repo name from external_id.
/// e.g., "gh-ci-ApiariTools/swarm-66478932700" → "ApiariTools/swarm"
///       "gh-issue-org/repo-42" → "org/repo"
///       "sentry-12345" → None (no repo info)
fn extract_repo(external_id: &str) -> Option<&str> {
    // GitHub format: gh-{type}-{owner}/{repo}-{number}
    let rest = external_id.strip_prefix("gh-")?;
    let rest = rest.split_once('-')?.1; // skip type (ci, issue, review, etc.)
    // Repo is everything up to the last dash followed by digits
    let last_dash = rest.rfind('-')?;
    let after = &rest[last_dash + 1..];
    if after.chars().all(|c| c.is_ascii_digit()) {
        Some(&rest[..last_dash])
    } else {
        None
    }
}

/// Strip `[owner/repo]` prefix from signal titles.
fn strip_repo_prefix(title: &str) -> &str {
    if title.starts_with('[')
        && let Some(end) = title.find("] ")
    {
        return &title[end + 2..];
    }
    title
}

fn severity_style(severity: &crate::buzz::signal::Severity) -> Style {
    match severity {
        crate::buzz::signal::Severity::Critical => theme::error(),
        crate::buzz::signal::Severity::Error => Style::default().fg(theme::NECTAR),
        crate::buzz::signal::Severity::Warning => Style::default().fg(theme::POLLEN),
        crate::buzz::signal::Severity::Info => theme::muted(),
    }
}

/// Activity graph — just renders the buffer as block bars.
/// Data is pushed in from the left and scrolls right. The buffer IS the graph.
fn activity_graph(buf: &[u8]) -> String {
    const BLOCKS: &[char] = &[
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    buf.iter().map(|&v| BLOCKS[(v as usize).min(7)]).collect()
}

/// Truncate a string to `max` chars, appending "..." if truncated.
/// Safe for multi-byte unicode (emoji, CJK, etc).
fn trunc(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

fn time_ago(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_days() > 0 {
        format!("{}d", duration.num_days())
    } else if duration.num_hours() > 0 {
        format!("{}h", duration.num_hours())
    } else if duration.num_minutes() > 0 {
        format!("{}m", duration.num_minutes())
    } else {
        "now".to_string()
    }
}

/// Format a token count for display (e.g. 1500 → "1.5k", 200000 → "200.0k").
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(200_000), "200.0k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }
}

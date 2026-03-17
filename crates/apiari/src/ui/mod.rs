//! `apiari ui` — Unified TUI dashboard.

pub mod app;
pub mod daemon_client;
pub mod history;
pub mod render;
pub mod theme;

use app::{App, Mode, Panel, PendingAction, View, review_signal_target};
use color_eyre::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::prelude::*;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::buzz::coordinator::skills::{
    build_skills_prompt, default_coordinator_disallowed_tools, default_coordinator_tools,
};
use crate::buzz::coordinator::{Coordinator, CoordinatorEvent};
use crate::buzz::signal::store::SignalStore;

use crate::config;
use crate::git_safety::GitSafetyHooks;

// ── Channel types ────────────────────────────────────────

/// Messages from the TUI to the coordinator background task.
enum UserMessage {
    Chat {
        workspace_name: String,
        text: String,
    },
}

/// Messages from the coordinator back to the TUI.
enum CoordResponse {
    Token(String),
    Done,
    Error(String),
    /// Activity broadcast from daemon (Telegram or other TUI-originated).
    Activity {
        source: String,
        workspace: String,
        kind: String,
        text: String,
    },
}

// ── Key actions ──────────────────────────────────────────

enum KeyAction {
    None,
    Quit,
    SendChat(String),
    SendWorkerMessage {
        worker_id: String,
        text: String,
    },
    OpenUrl(String),
    CloseWorker(String),
    ResolveSignal(i64),
    ApproveReview {
        repo: String,
        pr_number: u64,
    },
    CommentReview {
        repo: String,
        pr_number: u64,
        body: String,
    },
    SnoozeSignal(i64, chrono::DateTime<chrono::Utc>),
    Redraw,
}

// ── Entry point ──────────────────────────────────────────

/// Launch the TUI.
pub async fn run(focus_workspace: Option<&str>) -> Result<()> {
    let workspaces = config::discover_workspaces()?;
    if workspaces.is_empty() {
        eprintln!(
            "No workspace configs found in {}",
            config::workspaces_dir().display()
        );
        eprintln!("Run `apiari init` in a project directory to get started.");
        eprintln!();
        eprintln!("Get a Telegram bot token from @BotFather (https://t.me/BotFather)");
        eprintln!("and your chat ID from @userinfobot (https://t.me/userinfobot).");
        eprintln!("Then edit the config and run: apiari daemon --background");
        return Ok(());
    }

    let mut app = App::new(workspaces, focus_workspace);

    // Coordinator channels
    let (user_tx, user_rx) = mpsc::channel::<UserMessage>(32);
    let (coord_tx, coord_rx) = mpsc::channel::<CoordResponse>(64);

    // Choose daemon client (shared session) or local coordinator (standalone)
    let use_daemon = daemon_client::socket_exists() && crate::daemon::is_daemon_running();
    app.daemon_connected = use_daemon;

    if use_daemon {
        // Spawn daemon client task (tokio task — the daemon handles coordinator)
        let coord_tx_clone = coord_tx.clone();
        tokio::spawn(daemon_client_task(user_rx, coord_tx_clone));
    } else {
        // Spawn coordinator on a dedicated thread (SignalStore is !Send).
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build coordinator runtime");
            rt.block_on(coordinator_task(user_rx, coord_tx));
        });
    }

    // Terminal setup
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(crossterm::event::EnableMouseCapture)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Install panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(crossterm::event::DisableMouseCapture);
        let _ = stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    let result = event_loop(&mut terminal, app, &user_tx, coord_rx).await;

    // Terminal teardown
    disable_raw_mode()?;
    stdout().execute(crossterm::event::DisableMouseCapture)?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

// ── Event loop ───────────────────────────────────────────

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
    user_tx: &mpsc::Sender<UserMessage>,
    mut coord_rx: mpsc::Receiver<CoordResponse>,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if app.needs_redraw {
            if let Ok(size) = crossterm::terminal::size() {
                app.terminal_width = size.0;
            }
            terminal.draw(|f| render::draw(f, &app))?;
            app.needs_redraw = false;
        }

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        let action = handle_key(&mut app, key);
                        if let Some(true) = handle_action(&mut app, action, user_tx).await {
                            break;
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        use crossterm::event::MouseEventKind;
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                if matches!(app.view, View::WorkerDetail(_)) {
                                    app.scroll_worker_conv_up(3);
                                } else {
                                    app.scroll_chat_up(3);
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                if matches!(app.view, View::WorkerDetail(_)) {
                                    app.scroll_worker_conv_down(3);
                                } else {
                                    app.scroll_chat_down(3);
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        app.needs_redraw = true;
                    }
                    _ => {}
                }
            }

            Some(msg) = coord_rx.recv() => {
                match msg {
                    CoordResponse::Token(text) => {
                        app.append_assistant_token(&text);
                    }
                    CoordResponse::Done => {
                        app.finish_assistant_message();
                    }
                    CoordResponse::Error(e) => {
                        app.push_system_message(format!("Error: {e}"));
                    }
                    CoordResponse::Activity { source, workspace, kind, text } => {
                        app.push_activity(&workspace, &source, &kind, &text);
                    }
                }
            }

            _ = tick.tick() => {
                app.spinner_tick = app.spinner_tick.wrapping_add(1);

                // Push activity value: streaming = rollercoaster, idle = heartbeat blip
                let streaming = app.current_ws().is_some_and(|ws| ws.streaming);
                let val = if streaming {
                    // Rollercoaster: use layered sine for organic tall values (3-7)
                    let t = app.spinner_tick as f64;
                    let w1 = ((t / 7.0) * std::f64::consts::TAU).sin();
                    let w2 = ((t / 3.0) * std::f64::consts::TAU).sin() * 0.6;
                    let combined = (w1 + w2 + 1.0) / 2.0; // ~0..1
                    (combined * 5.0) as u8 + 3 // 3-7 range
                } else if app.daemon_alive {
                    // Heartbeat: mostly 0, with a small bump every ~18 ticks
                    let phase = app.spinner_tick % 18;
                    match phase {
                        0 => 1,
                        1 => 2,
                        2 => 1,
                        _ => 0,
                    }
                } else {
                    0
                };
                app.push_activity_value(val);

                app.tick_flash();
                app.maybe_refresh();
                app.needs_redraw = true;
            }
        }
    }

    Ok(())
}

// ── Key handling (pure state) ────────────────────────────

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return KeyAction::Quit;
    }

    // Prefix mode (Ctrl+B, then command)
    if app.prefix_active {
        app.prefix_active = false;
        match key.code {
            KeyCode::Char('n') => {
                let next = (app.active_tab + 1) % app.workspaces.len().max(1);
                app.switch_tab(next);
            }
            KeyCode::Char('p') => {
                let prev = if app.active_tab == 0 {
                    app.workspaces.len().saturating_sub(1)
                } else {
                    app.active_tab - 1
                };
                app.switch_tab(prev);
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                app.switch_tab(idx);
            }
            KeyCode::Char('z') => app.toggle_zoom(),
            _ => {}
        }
        return KeyAction::Redraw;
    }

    // Ctrl+Z toggles zoom
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('z') {
        app.toggle_zoom();
        return KeyAction::Redraw;
    }

    // Ctrl+B activates prefix mode
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
        app.prefix_active = true;
        return KeyAction::None;
    }

    // ── Confirm overlay ──
    if app.mode == Mode::Confirm {
        // Snooze has its own key handling (j/k/enter/esc)
        if matches!(app.pending_action, Some(PendingAction::SnoozeSignal(_))) {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    if app.snooze_selection + 1 < app::SNOOZE_OPTIONS.len() {
                        app.snooze_selection += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.snooze_selection = app.snooze_selection.saturating_sub(1);
                }
                KeyCode::Enter => {
                    if let Some(PendingAction::SnoozeSignal(id)) = app.pending_action.take() {
                        app.mode = Mode::Normal;
                        let until = app::App::compute_snooze_until(app.snooze_selection);
                        return KeyAction::SnoozeSignal(id, until);
                    }
                }
                KeyCode::Esc => {
                    app.pending_action = None;
                    app.mode = Mode::Normal;
                    app.flash("Cancelled");
                }
                _ => {}
            }
            return KeyAction::Redraw;
        }

        match key.code {
            KeyCode::Char('y') => {
                if let Some(action) = app.pending_action.take() {
                    app.mode = Mode::Normal;
                    match action {
                        PendingAction::CloseWorker(id) => return KeyAction::CloseWorker(id),
                        PendingAction::ResolveSignal(id) => return KeyAction::ResolveSignal(id),
                        PendingAction::ApproveReview { repo, pr_number } => {
                            return KeyAction::ApproveReview { repo, pr_number };
                        }
                        PendingAction::SnoozeSignal(_) => unreachable!(),
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                app.pending_action = None;
                app.mode = Mode::Normal;
                app.flash("Cancelled");
            }
            _ => {}
        }
        return KeyAction::Redraw;
    }

    // ── Review comment input ──
    if app.review_comment_active {
        return handle_review_comment_key(app, key);
    }

    // ── Help overlay ──
    if app.mode == Mode::Help {
        app.mode = Mode::Normal;
        app.needs_redraw = true;
        return KeyAction::None;
    }

    // ── View-specific key handling ──
    match &app.view.clone() {
        View::Dashboard => handle_dashboard_key(app, key),
        View::WorkerDetail(i) => handle_worker_detail_key(app, key, *i),
        View::SignalDetail(i) => handle_signal_detail_key(app, key, *i),
        View::SignalList => handle_signal_list_key(app, key),
        View::ReviewList => handle_review_list_key(app, key),
        View::PrList => handle_pr_list_key(app, key),
    }
}

// ── Dashboard keys ───────────────────────────────────────

fn handle_dashboard_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    // When chat input is focused, keys go to the input
    if app.chat_focused {
        return handle_dashboard_chat_key(app, key);
    }

    // When a panel is zoomed full-screen, Tab cycles to the next panel (stays zoomed)
    if app.zoomed_panel.is_some() {
        match key.code {
            KeyCode::Esc => {
                app.zoomed_panel = None;
                app.needs_redraw = true;
                return KeyAction::Redraw;
            }
            KeyCode::Tab => {
                app.next_panel();
                app.zoomed_panel = Some(app.focused_panel);
                return KeyAction::Redraw;
            }
            KeyCode::BackTab => {
                app.prev_panel();
                app.zoomed_panel = Some(app.focused_panel);
                return KeyAction::Redraw;
            }
            _ => {} // fall through for j/k, Enter, o, x, etc.
        }
    }

    // Ctrl+U/D scrolls chat or feed when focused
    if matches!(app.focused_panel, Panel::Chat | Panel::Feed)
        && let KeyCode::Char(c) = key.code
    {
        if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'u' {
            if app.focused_panel == Panel::Chat {
                app.scroll_chat_up(5);
            } else if let Some(ws) = app.current_ws_mut() {
                ws.feed_scroll.scroll_up(5);
                app.needs_redraw = true;
            }
            return KeyAction::Redraw;
        } else if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'd' {
            if app.focused_panel == Panel::Chat {
                app.scroll_chat_down(5);
            } else if let Some(ws) = app.current_ws_mut() {
                ws.feed_scroll.scroll_down(5);
                app.needs_redraw = true;
            }
            return KeyAction::Redraw;
        }
    }

    match key.code {
        KeyCode::Tab => app.next_panel(),
        KeyCode::BackTab => app.prev_panel(),
        KeyCode::Char('j') | KeyCode::Down => app.select_next_in_panel(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev_in_panel(),
        KeyCode::Enter => app.drill_in(),
        KeyCode::Char('a') => {
            if app.focused_panel == Panel::Reviews
                && let Some(signal) = app.selected_signal()
            {
                if let Some((repo, pr_number)) = review_signal_target(signal) {
                    app.pending_action = Some(PendingAction::ApproveReview { repo, pr_number });
                    app.mode = Mode::Confirm;
                } else {
                    app.flash("Linear reviews are read-only");
                }
            }
        }
        KeyCode::Char('c') => {
            if app.focused_panel == Panel::Reviews {
                if let Some(signal) = app.selected_signal() {
                    if let Some((repo, pr_number)) = review_signal_target(signal) {
                        app.review_comment_active = true;
                        app.review_comment_input.clear();
                        app.review_comment_repo = repo;
                        app.review_comment_pr = pr_number;
                        app.needs_redraw = true;
                    } else {
                        app.flash("Linear reviews are read-only");
                    }
                }
            } else {
                app.focused_panel = Panel::Chat;
                app.chat_focused = true;
                if let Some(ws) = app.current_ws_mut() {
                    ws.has_unread_response = false;
                }
                app.needs_redraw = true;
            }
        }
        KeyCode::Char('h') | KeyCode::Left => {
            if app.zoomed_panel.is_some() || app.terminal_width < 50 {
                app.prev_panel();
                if app.zoomed_panel.is_some() {
                    app.zoomed_panel = Some(app.focused_panel);
                }
            }
        }
        KeyCode::Char('l') | KeyCode::Right => {
            if app.zoomed_panel.is_some() || app.terminal_width < 50 {
                app.next_panel();
                if app.zoomed_panel.is_some() {
                    app.zoomed_panel = Some(app.focused_panel);
                }
            }
        }
        KeyCode::Char('p') => app.enter_pr_list(),
        KeyCode::Char('S') => app.enter_signal_list(),
        KeyCode::Char('r') => {
            if app.has_review_queue() {
                app.enter_review_list();
            }
        }
        KeyCode::Char('o') => {
            if let Some(url) = app.selected_url() {
                return KeyAction::OpenUrl(url);
            }
        }
        KeyCode::Char('x') => {
            if let Some(worker) = app.selected_worker() {
                let id = worker.id.clone();
                app.pending_action = Some(PendingAction::CloseWorker(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('R') => {
            if let Some(signal) = app.selected_signal() {
                let id = signal.id;
                app.pending_action = Some(PendingAction::ResolveSignal(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('z') => {
            if matches!(app.focused_panel, Panel::Signals | Panel::Reviews) {
                if let Some(signal) = app.selected_signal() {
                    let id = signal.id;
                    app.snooze_selection = 0;
                    app.pending_action = Some(PendingAction::SnoozeSignal(id));
                    app.mode = Mode::Confirm;
                } else {
                    app.toggle_zoom();
                }
            } else {
                app.toggle_zoom();
            }
        }
        KeyCode::Char('G') => {
            if let Some(ws) = app.current_ws_mut() {
                ws.chat_scroll.scroll_to_bottom();
            }
        }
        KeyCode::Char('?') => {
            app.mode = Mode::Help;
        }
        KeyCode::Char('q') => {
            return KeyAction::Quit;
        }
        _ => {}
    }
    KeyAction::Redraw
}

fn handle_dashboard_chat_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::ALT) {
                app.insert_char('\n');
            } else {
                let input = app.take_input();
                if !input.trim().is_empty() {
                    return KeyAction::SendChat(input);
                }
            }
        }
        KeyCode::Esc => {
            app.chat_focused = false;
            app.needs_redraw = true;
        }
        KeyCode::Backspace => {
            app.backspace();
        }
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'u' {
                app.scroll_chat_up(5);
            } else if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'd' {
                app.scroll_chat_down(5);
            } else {
                app.insert_char(c);
            }
        }
        _ => {}
    }
    KeyAction::Redraw
}

// ── Worker detail keys ───────────────────────────────────

fn handle_worker_detail_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    idx: usize,
) -> KeyAction {
    // Input mode: keys go to the worker message input
    if app.worker_input_active {
        return handle_worker_input_key(app, key, idx);
    }

    match key.code {
        KeyCode::Esc => app.back_to_dashboard(),
        KeyCode::Tab => {
            cycle_fullscreen_next(app, idx);
        }
        KeyCode::BackTab => {
            cycle_fullscreen_prev(app, idx);
        }
        KeyCode::Char('c') => {
            app.worker_input_active = true;
            app.worker_input.clear();
            app.needs_redraw = true;
        }
        KeyCode::Char('j') | KeyCode::Down => app.scroll_worker_conv_up(3),
        KeyCode::Char('k') | KeyCode::Up => app.scroll_worker_conv_down(3),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_worker_conv_down(10);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_worker_conv_up(10);
        }
        KeyCode::Char('o') => {
            if let Some(url) = app.selected_url() {
                return KeyAction::OpenUrl(url);
            }
        }
        KeyCode::Char('x') => {
            if let Some(ws) = app.current_ws()
                && let Some(worker) = ws.workers.get(idx)
            {
                let id = worker.id.clone();
                app.pending_action = Some(PendingAction::CloseWorker(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('q') => return KeyAction::Quit,
        _ => {}
    }
    KeyAction::Redraw
}

fn handle_worker_input_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    idx: usize,
) -> KeyAction {
    match key.code {
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.worker_input);
            if !text.trim().is_empty()
                && let Some(ws) = app.current_ws()
                && let Some(worker) = ws.workers.get(idx)
            {
                let id = worker.id.clone();
                app.worker_input_active = false;
                app.needs_redraw = true;
                return KeyAction::SendWorkerMessage {
                    worker_id: id,
                    text,
                };
            }
            app.worker_input_active = false;
            app.needs_redraw = true;
        }
        KeyCode::Esc => {
            app.worker_input.clear();
            app.worker_input_active = false;
            app.needs_redraw = true;
        }
        KeyCode::Backspace => {
            app.worker_input.pop();
            app.needs_redraw = true;
        }
        KeyCode::Char(c) => {
            app.worker_input.push(c);
            app.needs_redraw = true;
        }
        _ => {}
    }
    KeyAction::Redraw
}

// ── Review comment input keys ────────────────────────────

fn handle_review_comment_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.review_comment_input);
            let repo = std::mem::take(&mut app.review_comment_repo);
            let pr = app.review_comment_pr;
            app.review_comment_active = false;
            app.needs_redraw = true;
            if !text.trim().is_empty() {
                return KeyAction::CommentReview {
                    repo,
                    pr_number: pr,
                    body: text,
                };
            }
        }
        KeyCode::Esc => {
            app.review_comment_input.clear();
            app.review_comment_active = false;
            app.needs_redraw = true;
        }
        KeyCode::Backspace => {
            app.review_comment_input.pop();
            app.needs_redraw = true;
        }
        KeyCode::Char(c) => {
            app.review_comment_input.push(c);
            app.needs_redraw = true;
        }
        _ => {}
    }
    KeyAction::Redraw
}

// ── Signal detail keys ───────────────────────────────────

fn handle_signal_detail_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    idx: usize,
) -> KeyAction {
    match key.code {
        KeyCode::Esc => app.back_to_dashboard(),
        KeyCode::Tab => {
            cycle_fullscreen_next(app, idx);
        }
        KeyCode::BackTab => {
            cycle_fullscreen_prev(app, idx);
        }
        KeyCode::Char('j') | KeyCode::Down => app.scroll_content_up(1),
        KeyCode::Char('k') | KeyCode::Up => app.scroll_content_down(1),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_content_down(10);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_content_up(10);
        }
        KeyCode::Char('o') => {
            if let Some(url) = app.selected_url() {
                return KeyAction::OpenUrl(url);
            }
        }
        KeyCode::Char('R') => {
            if let Some(ws) = app.current_ws()
                && let Some(signal) = ws.signals.get(idx)
            {
                let id = signal.id;
                app.pending_action = Some(PendingAction::ResolveSignal(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('z') => {
            if let Some(ws) = app.current_ws()
                && let Some(signal) = ws.signals.get(idx)
            {
                let id = signal.id;
                app.snooze_selection = 0;
                app.pending_action = Some(PendingAction::SnoozeSignal(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('q') => return KeyAction::Quit,
        _ => {}
    }
    KeyAction::Redraw
}

// ── Signal list keys ─────────────────────────────────────

fn handle_signal_list_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    let signal_count = app.current_ws().map_or(0, |ws| ws.signals.len());

    match key.code {
        KeyCode::Esc => app.back_to_dashboard(),
        KeyCode::Char('j') | KeyCode::Down => {
            if app.signal_list_selection + 1 < signal_count {
                app.signal_list_selection += 1;
                app.needs_redraw = true;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.signal_list_selection = app.signal_list_selection.saturating_sub(1);
            app.needs_redraw = true;
        }
        KeyCode::Char('g') => {
            app.signal_list_selection = 0;
            app.needs_redraw = true;
        }
        KeyCode::Char('G') => {
            app.signal_list_selection = signal_count.saturating_sub(1);
            app.needs_redraw = true;
        }
        KeyCode::Enter => {
            if app.signal_list_selection < signal_count {
                app.enter_signal_detail(app.signal_list_selection);
            }
        }
        KeyCode::Char('o') => {
            if let Some(url) = app.selected_url() {
                return KeyAction::OpenUrl(url);
            }
        }
        KeyCode::Char('R') => {
            if let Some(signal) = app.selected_signal() {
                let id = signal.id;
                app.pending_action = Some(PendingAction::ResolveSignal(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('z') => {
            if let Some(signal) = app.selected_signal() {
                let id = signal.id;
                app.snooze_selection = 0;
                app.pending_action = Some(PendingAction::SnoozeSignal(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('q') => return KeyAction::Quit,
        _ => {}
    }
    KeyAction::Redraw
}

// ── Review list keys ─────────────────────────────────────

fn handle_review_list_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    let review_count = app.current_ws().map_or(0, |ws| {
        ws.signals
            .iter()
            .filter(|s| s.source.ends_with("_review_queue"))
            .count()
    });

    match key.code {
        KeyCode::Esc => app.back_to_dashboard(),
        KeyCode::Char('j') | KeyCode::Down => {
            if app.review_list_selection + 1 < review_count {
                app.review_list_selection += 1;
                app.needs_redraw = true;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.review_list_selection = app.review_list_selection.saturating_sub(1);
            app.needs_redraw = true;
        }
        KeyCode::Char('g') => {
            app.review_list_selection = 0;
            app.needs_redraw = true;
        }
        KeyCode::Char('G') => {
            app.review_list_selection = review_count.saturating_sub(1);
            app.needs_redraw = true;
        }
        KeyCode::Enter => {
            if app.review_list_selection < review_count {
                // Find the original index in ws.signals for the nth review signal
                if let Some(orig_idx) = app.current_ws().and_then(|ws| {
                    ws.signals
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.source.ends_with("_review_queue"))
                        .map(|(i, _)| i)
                        .nth(app.review_list_selection)
                }) {
                    app.enter_signal_detail(orig_idx);
                }
            }
        }
        KeyCode::Char('a') => {
            if let Some(signal) = app.selected_signal() {
                if let Some((repo, pr_number)) = review_signal_target(signal) {
                    app.pending_action = Some(PendingAction::ApproveReview { repo, pr_number });
                    app.mode = Mode::Confirm;
                } else {
                    app.flash("Linear reviews are read-only");
                }
            }
        }
        KeyCode::Char('c') => {
            if let Some(signal) = app.selected_signal() {
                if let Some((repo, pr_number)) = review_signal_target(signal) {
                    app.review_comment_active = true;
                    app.review_comment_input.clear();
                    app.review_comment_repo = repo;
                    app.review_comment_pr = pr_number;
                    app.needs_redraw = true;
                } else {
                    app.flash("Linear reviews are read-only");
                }
            }
        }
        KeyCode::Char('o') => {
            if let Some(url) = app.selected_url() {
                return KeyAction::OpenUrl(url);
            }
        }
        KeyCode::Char('z') => {
            if let Some(signal) = app.selected_signal() {
                let id = signal.id;
                app.snooze_selection = 0;
                app.pending_action = Some(PendingAction::SnoozeSignal(id));
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('q') => return KeyAction::Quit,
        _ => {}
    }
    KeyAction::Redraw
}

// ── PR list keys ─────────────────────────────────────────

fn handle_pr_list_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyAction {
    let pr_count = app.workers_with_prs().len();

    match key.code {
        KeyCode::Esc => app.back_to_dashboard(),
        KeyCode::Char('j') | KeyCode::Down => {
            if app.pr_list_selection + 1 < pr_count {
                app.pr_list_selection += 1;
                app.needs_redraw = true;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.pr_list_selection = app.pr_list_selection.saturating_sub(1);
            app.needs_redraw = true;
        }
        KeyCode::Char('g') => {
            app.pr_list_selection = 0;
            app.needs_redraw = true;
        }
        KeyCode::Char('G') => {
            app.pr_list_selection = pr_count.saturating_sub(1);
            app.needs_redraw = true;
        }
        KeyCode::Enter => {
            let prs = app.workers_with_prs();
            if let Some((orig_idx, _)) = prs.get(app.pr_list_selection) {
                app.enter_worker_detail(*orig_idx);
            }
        }
        KeyCode::Char('o') => {
            if let Some(url) = app.selected_url() {
                return KeyAction::OpenUrl(url);
            }
        }
        KeyCode::Char('q') => return KeyAction::Quit,
        _ => {}
    }
    KeyAction::Redraw
}

// ── Full-screen cycling (Tab across workers + signals) ───

/// Cycle to the next full-screen item: workers first, then signals, wrapping.
fn cycle_fullscreen_next(app: &mut App, current_idx: usize) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };
    let worker_count = ws.workers.len();
    let signal_count = ws.signals.len();

    match app.view {
        View::WorkerDetail(_) => {
            if current_idx + 1 < worker_count {
                // Next worker
                app.worker_selection = current_idx + 1;
                app.enter_worker_detail(current_idx + 1);
            } else if signal_count > 0 {
                // Past last worker → first signal
                app.signal_selection = 0;
                app.enter_signal_detail(0);
            } else if worker_count > 0 {
                // No signals, wrap to first worker
                app.worker_selection = 0;
                app.enter_worker_detail(0);
            }
        }
        View::SignalDetail(_) => {
            if current_idx + 1 < signal_count {
                // Next signal
                app.signal_selection = current_idx + 1;
                app.enter_signal_detail(current_idx + 1);
            } else if worker_count > 0 {
                // Past last signal → first worker
                app.worker_selection = 0;
                app.enter_worker_detail(0);
            } else if signal_count > 0 {
                // No workers, wrap to first signal
                app.signal_selection = 0;
                app.enter_signal_detail(0);
            }
        }
        _ => {}
    }
}

/// Cycle to the previous full-screen item: signals then workers, wrapping.
fn cycle_fullscreen_prev(app: &mut App, current_idx: usize) {
    let ws = match app.current_ws() {
        Some(ws) => ws,
        None => return,
    };
    let worker_count = ws.workers.len();
    let signal_count = ws.signals.len();

    match app.view {
        View::WorkerDetail(_) => {
            if current_idx > 0 {
                // Previous worker
                app.worker_selection = current_idx - 1;
                app.enter_worker_detail(current_idx - 1);
            } else if signal_count > 0 {
                // Before first worker → last signal
                let last = signal_count - 1;
                app.signal_selection = last;
                app.enter_signal_detail(last);
            } else if worker_count > 0 {
                // No signals, wrap to last worker
                let last = worker_count - 1;
                app.worker_selection = last;
                app.enter_worker_detail(last);
            }
        }
        View::SignalDetail(_) => {
            if current_idx > 0 {
                // Previous signal
                app.signal_selection = current_idx - 1;
                app.enter_signal_detail(current_idx - 1);
            } else if worker_count > 0 {
                // Before first signal → last worker
                let last = worker_count - 1;
                app.worker_selection = last;
                app.enter_worker_detail(last);
            } else if signal_count > 0 {
                // No workers, wrap to last signal
                let last = signal_count - 1;
                app.signal_selection = last;
                app.enter_signal_detail(last);
            }
        }
        _ => {}
    }
}

// ── Action handling (async side effects) ─────────────────

async fn handle_action(
    app: &mut App,
    action: KeyAction,
    user_tx: &mpsc::Sender<UserMessage>,
) -> Option<bool> {
    match action {
        KeyAction::Quit => return Some(true),
        KeyAction::SendChat(text) => {
            let ws_name = app.current_ws().map(|ws| ws.name.clone());
            if let Some(name) = ws_name {
                app.push_user_message(text.clone());
                let _ = user_tx
                    .send(UserMessage::Chat {
                        workspace_name: name,
                        text,
                    })
                    .await;
            }
        }
        KeyAction::SendWorkerMessage { worker_id, text } => {
            if let Some(ws) = app.current_ws() {
                let root = ws.config.root.clone();
                let id = worker_id.clone();
                let msg = text.clone();
                tokio::spawn(async move {
                    let _ = tokio::process::Command::new("swarm")
                        .args(["--dir", &root.to_string_lossy(), "send", &id, &msg])
                        .output()
                        .await;
                });
                app.flash(format!("Message sent to {worker_id}"));
            }
        }
        KeyAction::OpenUrl(url) => {
            let _ = std::process::Command::new("open").arg(&url).spawn();
            app.flash("Opened in browser");
        }
        KeyAction::CloseWorker(id) => {
            if let Some(ws) = app.current_ws() {
                let root = ws.config.root.clone();
                let id_clone = id.clone();
                tokio::spawn(async move {
                    let _ = tokio::process::Command::new("swarm")
                        .args(["--dir", &root.to_string_lossy(), "close", &id_clone])
                        .output()
                        .await;
                });
                app.flash(format!("Closing worker {id}..."));
            }
        }
        KeyAction::ApproveReview { repo, pr_number } => {
            let r = repo.clone();
            let pr = pr_number;
            tokio::spawn(async move {
                let _ = tokio::process::Command::new("gh")
                    .args(["pr", "review", &pr.to_string(), "--approve", "--repo", &r])
                    .output()
                    .await;
            });
            app.flash(format!("Approving PR #{pr_number} in {repo}..."));
        }
        KeyAction::CommentReview {
            repo,
            pr_number,
            body,
        } => {
            let r = repo.clone();
            let pr = pr_number;
            let b = body.clone();
            tokio::spawn(async move {
                let _ = tokio::process::Command::new("gh")
                    .args([
                        "pr",
                        "review",
                        &pr.to_string(),
                        "--comment",
                        "--body",
                        &b,
                        "--repo",
                        &r,
                    ])
                    .output()
                    .await;
            });
            app.flash(format!("Sending comment on PR #{pr_number} in {repo}..."));
        }
        KeyAction::ResolveSignal(id) => {
            let db = config::db_path();
            if let Some(ws) = app.current_ws()
                && let Ok(store) = SignalStore::open(&db, &ws.name)
            {
                match store.resolve_signal(id) {
                    Ok(()) => {
                        app.flash(format!("Signal #{id} resolved"));
                        app.refresh_signals();
                    }
                    Err(e) => {
                        app.flash(format!("Failed to resolve: {e}"));
                    }
                }
            }
        }
        KeyAction::SnoozeSignal(id, until) => {
            let db = config::db_path();
            if let Some(ws) = app.current_ws()
                && let Ok(store) = SignalStore::open(&db, &ws.name)
            {
                match store.snooze_signal(id, until) {
                    Ok(()) => {
                        let label = app::SNOOZE_OPTIONS
                            .get(app.snooze_selection)
                            .unwrap_or(&"snoozed");
                        app.flash(format!("Signal #{id} snoozed until {label}"));
                        app.refresh_signals();
                    }
                    Err(e) => {
                        app.flash(format!("Failed to snooze: {e}"));
                    }
                }
            }
        }
        KeyAction::None | KeyAction::Redraw => {}
    }
    app.needs_redraw = true;
    None
}

// ── Daemon client task ───────────────────────────────────

/// Runs when TUI is connected to the daemon via Unix socket.
/// Forwards user messages to daemon, receives Token/Done/Error/Activity back.
async fn daemon_client_task(
    mut user_rx: mpsc::Receiver<UserMessage>,
    coord_tx: mpsc::Sender<CoordResponse>,
) {
    let socket_path = crate::config::socket_path();
    let mut client = match daemon_client::DaemonClient::connect(&socket_path).await {
        Ok(c) => c,
        Err(e) => {
            let _ = coord_tx
                .send(CoordResponse::Error(format!(
                    "Failed to connect to daemon: {e}"
                )))
                .await;
            return;
        }
    };

    loop {
        tokio::select! {
            Some(msg) = user_rx.recv() => {
                match msg {
                    UserMessage::Chat { workspace_name, text } => {
                        if let Err(e) = client.send_chat(&workspace_name, &text).await {
                            let _ = coord_tx
                                .send(CoordResponse::Error(format!("Socket send error: {e}")))
                                .await;
                        }
                    }
                }
            }

            resp = client.next_response() => {
                match resp {
                    Ok(Some(crate::daemon::socket::DaemonResponse::Token { text })) => {
                        let _ = coord_tx.send(CoordResponse::Token(text)).await;
                    }
                    Ok(Some(crate::daemon::socket::DaemonResponse::Done)) => {
                        let _ = coord_tx.send(CoordResponse::Done).await;
                    }
                    Ok(Some(crate::daemon::socket::DaemonResponse::Error { text })) => {
                        let _ = coord_tx.send(CoordResponse::Error(text)).await;
                    }
                    Ok(Some(crate::daemon::socket::DaemonResponse::Activity { source, workspace, kind, text })) => {
                        // Skip our own echoed messages — we already have them
                        // (user message pushed locally, assistant via Token stream)
                        if source == "tui" {
                            continue;
                        }
                        let _ = coord_tx
                            .send(CoordResponse::Activity { source, workspace, kind, text })
                            .await;
                    }
                    Ok(None) => {
                        // Daemon disconnected
                        let _ = coord_tx
                            .send(CoordResponse::Error("Daemon disconnected".into()))
                            .await;
                        break;
                    }
                    Err(e) => {
                        let _ = coord_tx
                            .send(CoordResponse::Error(format!("Socket read error: {e}")))
                            .await;
                        break;
                    }
                }
            }
        }
    }
}

// ── Coordinator task ─────────────────────────────────────

async fn coordinator_task(
    mut user_rx: mpsc::Receiver<UserMessage>,
    coord_tx: mpsc::Sender<CoordResponse>,
) {
    let db = config::db_path();
    let _ = std::fs::create_dir_all(db.parent().unwrap());

    // Cache coordinators per workspace
    let mut coordinators: std::collections::HashMap<String, Coordinator> =
        std::collections::HashMap::new();

    while let Some(msg) = user_rx.recv().await {
        match msg {
            UserMessage::Chat {
                workspace_name,
                text,
            } => {
                // Lazy-init coordinator for this workspace
                if !coordinators.contains_key(&workspace_name) {
                    if let Some(coord) = build_coordinator(&workspace_name) {
                        coordinators.insert(workspace_name.clone(), coord);
                    } else {
                        let _ = coord_tx
                            .send(CoordResponse::Error(
                                "Failed to initialize coordinator".to_string(),
                            ))
                            .await;
                        continue;
                    }
                }

                let coordinator = coordinators.get_mut(&workspace_name).unwrap();

                let store = match SignalStore::open(&db, &workspace_name) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = coord_tx
                            .send(CoordResponse::Error(format!("DB error: {e}")))
                            .await;
                        continue;
                    }
                };

                let token_tx = coord_tx.clone();
                match coordinator
                    .handle_message(&text, &store, move |event| match event {
                        CoordinatorEvent::Token(t) => {
                            let _ = token_tx.try_send(CoordResponse::Token(t));
                        }
                        CoordinatorEvent::FilesModified { files } => {
                            let file_list: Vec<String> = files
                                .iter()
                                .map(|(repo, file)| format!("{repo}/{file}"))
                                .collect();
                            let alert = format!(
                                "Warning: coordinator modified workspace files:\n{}",
                                file_list
                                    .iter()
                                    .map(|f| format!("  - {f}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            );
                            let _ = token_tx.try_send(CoordResponse::Error(alert));
                        }
                        CoordinatorEvent::BashAudit {
                            command,
                            matched_pattern,
                        } => {
                            let _ = token_tx.try_send(CoordResponse::Error(format!(
                                "Bash audit ({matched_pattern}): {command}"
                            )));
                        }
                    })
                    .await
                {
                    Ok(_) => {
                        let _ = coord_tx.send(CoordResponse::Done).await;
                    }
                    Err(e) => {
                        let _ = coord_tx.send(CoordResponse::Error(format!("{e}"))).await;
                    }
                }
            }
        }
    }
}

/// Build a Coordinator for a workspace with safety hooks pre-configured.
fn build_coordinator(workspace_name: &str) -> Option<Coordinator> {
    let workspaces = config::discover_workspaces().ok()?;
    let ws = workspaces.iter().find(|w| w.name == workspace_name)?;

    let mut coordinator = Coordinator::new(
        &ws.config.coordinator.model,
        ws.config.coordinator.max_turns,
    );
    coordinator.set_name(ws.config.coordinator.name.clone());

    let skill_ctx = config::build_skill_context(workspace_name, &ws.config);
    coordinator.set_extra_context(build_skills_prompt(&skill_ctx));
    if let Some(ref preamble) = skill_ctx.prompt_preamble {
        coordinator.set_prompt_preamble(preamble.clone());
    }
    coordinator.set_tools(default_coordinator_tools());
    coordinator.set_disallowed_tools(default_coordinator_disallowed_tools());
    coordinator.set_working_dir(ws.config.root.clone());
    if let Some(settings) = config::coordinator_settings_json() {
        coordinator.set_settings(settings);
    }
    coordinator.set_safety_hooks(Box::new(GitSafetyHooks {
        workspace_root: ws.config.root.clone(),
    }));

    Some(coordinator)
}

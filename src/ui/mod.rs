//! `apiari ui` — Unified TUI dashboard.

pub mod app;
pub mod history;
pub mod render;
pub mod theme;

use app::{App, Mode, Panel, PendingAction, View};
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

use buzz::coordinator::Coordinator;
use buzz::coordinator::skills::{SkillContext, build_skills_prompt, default_coordinator_tools};
use buzz::signal::store::SignalStore;

use crate::config::{self, WorkspaceConfig};

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
}

// ── Key actions ──────────────────────────────────────────

enum KeyAction {
    None,
    Quit,
    SendChat(String),
    SendWorkerMessage { worker_id: String, text: String },
    OpenUrl(String),
    CloseWorker(String),
    ResolveSignal(i64),
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
        eprintln!("Run `apiari init` in a project directory to create one.");
        return Ok(());
    }

    let app = App::new(workspaces, focus_workspace);

    // Coordinator channels
    let (user_tx, user_rx) = mpsc::channel::<UserMessage>(32);
    let (coord_tx, coord_rx) = mpsc::channel::<CoordResponse>(64);

    // Spawn coordinator on a dedicated thread (SignalStore is !Send).
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build coordinator runtime");
        rt.block_on(coordinator_task(user_rx, coord_tx));
    });

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
                }
            }

            _ = tick.tick() => {
                app.spinner_tick = app.spinner_tick.wrapping_add(1);
                app.tick_flash();
                app.maybe_refresh();
                app.needs_redraw = true; // spinner animation
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
            _ => {}
        }
        return KeyAction::Redraw;
    }

    // Ctrl+B activates prefix mode
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
        app.prefix_active = true;
        return KeyAction::None;
    }

    // ── Confirm overlay ──
    if app.mode == Mode::Confirm {
        match key.code {
            KeyCode::Char('y') => {
                if let Some(action) = app.pending_action.take() {
                    app.mode = Mode::Normal;
                    match action {
                        PendingAction::CloseWorker(id) => return KeyAction::CloseWorker(id),
                        PendingAction::ResolveSignal(id) => return KeyAction::ResolveSignal(id),
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
        KeyCode::Char('c') => {
            app.focused_panel = Panel::Chat;
            app.chat_focused = true;
            if let Some(ws) = app.current_ws_mut() {
                ws.has_unread_response = false;
            }
            app.needs_redraw = true;
        }
        KeyCode::Char('z') => app.toggle_zoom(),
        KeyCode::Char('p') => app.enter_pr_list(),
        KeyCode::Char('s') => app.enter_signal_list(),
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
        KeyAction::None | KeyAction::Redraw => {}
    }
    app.needs_redraw = true;
    None
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

                // Use streaming to get real-time token delivery
                let token_tx = coord_tx.clone();
                match coordinator
                    .handle_message_streaming(&text, &store, move |token| {
                        let _ = token_tx.try_send(CoordResponse::Token(token.to_string()));
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

/// Build a Coordinator for a workspace.
fn build_coordinator(workspace_name: &str) -> Option<Coordinator> {
    let workspaces = config::discover_workspaces().ok()?;
    let ws = workspaces.iter().find(|w| w.name == workspace_name)?;

    let mut coordinator = Coordinator::new(
        &ws.config.coordinator.model,
        ws.config.coordinator.max_turns,
    );
    coordinator.set_name(ws.config.coordinator.name.clone());

    let skill_ctx = build_skill_context(workspace_name, &ws.config);
    coordinator.set_extra_context(build_skills_prompt(&skill_ctx));
    coordinator.set_tools(default_coordinator_tools());
    coordinator.set_working_dir(ws.config.root.clone());

    Some(coordinator)
}

/// Build a SkillContext from workspace config.
fn build_skill_context(workspace_name: &str, config: &WorkspaceConfig) -> SkillContext {
    SkillContext {
        workspace_name: workspace_name.to_string(),
        workspace_root: config.root.clone(),
        config_path: config::workspaces_dir().join(format!("{workspace_name}.toml")),
        repos: config.repos.clone(),
        has_sentry: config.watchers.sentry.is_some(),
        has_swarm: config.watchers.swarm.is_some(),
    }
}

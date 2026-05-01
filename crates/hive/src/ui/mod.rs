//! Unified hive TUI — workers panel + coordinator chat + workspace tabs.

pub mod app;
pub mod daemon_client;
pub mod history;
pub mod inbox;
pub mod markdown;
pub mod render;

use app::{
    ActiveDispatch, App, ChatLine, DispatchStatus, Panel, PendingAction, PrDetailInfo, SidebarItem,
};
use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use daemon_client::DaemonClient;
use ratatui::prelude::*;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::daemon::tui_socket::{RepoDispatchInfo, TuiResponse};
use crate::presence;
use crate::workspace::{self, load_workspace};

/// Messages from the TUI event loop to the background coordinator/daemon task.
enum UserMessage {
    /// Regular chat message.
    Chat(String),
    /// Submit a multi-repo dispatch.
    SubmitDispatch { repos: Vec<String>, task: String },
    /// Confirm the pending dispatch.
    ConfirmDispatch,
    /// Cancel the pending dispatch.
    CancelDispatch,
}

/// Messages sent from the coordinator background task back to the TUI.
enum CoordResponse {
    /// Streaming text chunk.
    Token(String),
    /// Session finished.
    Done,
    /// Error from coordinator.
    Error(String),
    /// Notification from daemon (swarm events, buzz signals).
    Notification(inbox::UiEvent),
    /// Dispatch state update from daemon (sent on connect + state changes).
    DispatchStateUpdate {
        task: String,
        repos: Vec<String>,
        status: String,
        task_md: String,
        title: String,
        per_repo: Vec<RepoDispatchInfo>,
    },
    /// Dispatch cleared by daemon.
    DispatchCleared,
}

/// Async action results sent back to the event loop.
enum ActionResponse {
    Success(String),
    Error(String),
}

/// Launch the unified TUI.
pub async fn run(cwd: &Path) -> Result<()> {
    // Load workspace registry for tabs.
    let mut workspaces = workspace::load_registry();
    if workspaces.is_empty() {
        // Fallback: use current workspace.
        if let Ok(ws) = load_workspace(cwd) {
            workspaces.push(workspace::RegistryEntry {
                name: ws.name.clone(),
                path: ws.root.clone(),
            });
        }
    }

    // Find which tab matches the current directory.
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let active_tab = workspaces
        .iter()
        .position(|e| {
            std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone()) == canonical_cwd
        })
        .unwrap_or(0);

    let workspace_root = canonical_cwd.clone();
    let mut app = App::new(workspaces, active_tab, workspace_root.clone());
    app.refresh_workers();

    // Channels for coordinator communication.
    let (user_tx, user_rx) = mpsc::channel::<UserMessage>(32);
    let (coord_tx, coord_rx) = mpsc::channel::<CoordResponse>(64);

    // Decide which coordinator to use: daemon socket or local fallback.
    // Probe the connection first (without consuming user_rx).
    let use_daemon = daemon_client::can_connect(&workspace_root).await
        || (try_auto_start_daemon(&workspace_root).await
            && daemon_client::can_connect(&workspace_root).await);

    if use_daemon {
        // Spawn daemon client task.
        let root = workspace_root.clone();
        tokio::spawn(daemon_client_task(root, user_rx, coord_tx));
        app.daemon_connected = true;
    } else {
        // Spawn local coordinator fallback.
        let coord_cwd = cwd.to_path_buf();
        tokio::spawn(coordinator_task(coord_cwd, user_rx, coord_tx));
    }

    // Spawn presence heartbeat — touch "ui" channel every 5 seconds.
    let heartbeat_root = workspace_root.clone();
    let heartbeat_cancel = tokio_util::sync::CancellationToken::new();
    let heartbeat_token = heartbeat_cancel.clone();
    tokio::spawn(async move {
        loop {
            let _ = presence::touch_channel(&heartbeat_root, "ui");
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = heartbeat_token.cancelled() => break,
            }
        }
    });

    // Terminal setup.
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let result = event_loop(&mut terminal, &mut app, &user_tx, coord_rx).await;

    // Stop heartbeat.
    heartbeat_cancel.cancel();

    // Terminal teardown.
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

/// Try to auto-start the daemon if it's not running.
///
/// Returns true if the daemon was successfully started (or was already running).
async fn try_auto_start_daemon(workspace_root: &Path) -> bool {
    // Check if daemon.toml exists (required for daemon).
    if !workspace_root.join(".hive/daemon.toml").exists() {
        return false;
    }

    // Check if already running.
    if let Some(pid) = crate::daemon::read_pid()
        && crate::daemon::is_process_alive(pid)
    {
        // Daemon is running but socket might not be ready yet.
        // Wait for socket to appear.
        for _ in 0..50 {
            if daemon_client::daemon_sock_exists(workspace_root) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        return false;
    }

    // Try starting the daemon.
    tracing::info!("Auto-starting daemon...");
    match crate::daemon::start(false).await {
        Ok(()) => {
            // Wait for socket to appear (up to 5s).
            for _ in 0..50 {
                if daemon_client::daemon_sock_exists(workspace_root)
                    && daemon_client::can_connect(workspace_root).await
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            tracing::warn!("Daemon started but socket not ready");
            false
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to auto-start daemon");
            false
        }
    }
}

/// Background task that connects to the daemon socket and relays messages.
async fn daemon_client_task(
    workspace_root: PathBuf,
    mut user_rx: mpsc::Receiver<UserMessage>,
    coord_tx: mpsc::Sender<CoordResponse>,
) {
    let mut client = match DaemonClient::connect(&workspace_root).await {
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

    // On connect, request current dispatch state for sync.
    if let Err(e) = client.send_get_dispatch_state().await {
        tracing::warn!(error = %e, "Failed to request dispatch state");
    }

    loop {
        tokio::select! {
            // User sends a message.
            Some(message) = user_rx.recv() => {
                let result = match message {
                    UserMessage::Chat(text) => client.send_message(&text).await,
                    UserMessage::SubmitDispatch { repos, task } => {
                        client.send_submit_dispatch(&repos, &task).await
                    }
                    UserMessage::ConfirmDispatch => client.send_confirm_dispatch().await,
                    UserMessage::CancelDispatch => client.send_cancel_dispatch().await,
                };
                if let Err(e) = result {
                    let _ = coord_tx
                        .send(CoordResponse::Error(format!("Daemon send error: {e}")))
                        .await;
                    break;
                }
            }

            // Read responses from daemon.
            result = client.next_response() => {
                match result {
                    Ok(Some(response)) => {
                        let coord_msg = match response {
                            TuiResponse::Token { text } => CoordResponse::Token(text),
                            TuiResponse::Done => CoordResponse::Done,
                            TuiResponse::Error { text } => CoordResponse::Error(text),
                            TuiResponse::Notification { event } => {
                                CoordResponse::Notification(event)
                            }
                            TuiResponse::DispatchUpdate {
                                task,
                                repos,
                                status,
                                task_md,
                                title,
                                per_repo,
                                ..
                            } => CoordResponse::DispatchStateUpdate {
                                task,
                                repos,
                                status,
                                task_md,
                                title,
                                per_repo,
                            },
                            TuiResponse::DispatchCleared => CoordResponse::DispatchCleared,
                        };
                        if coord_tx.send(coord_msg).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = coord_tx
                            .send(CoordResponse::Error("Daemon disconnected".into()))
                            .await;
                        break;
                    }
                    Err(e) => {
                        let _ = coord_tx
                            .send(CoordResponse::Error(format!("Daemon error: {e}")))
                            .await;
                        break;
                    }
                }
            }
        }
    }
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    user_tx: &mpsc::Sender<UserMessage>,
    mut coord_rx: mpsc::Receiver<CoordResponse>,
) -> Result<()> {
    // Channel for async action results (swarm send, close, etc.).
    let (action_tx, mut action_rx) = mpsc::channel::<ActionResponse>(16);

    loop {
        // Capture terminal height for page scroll calculations.
        let viewport_height = terminal.size()?.height.saturating_sub(6); // approx content area

        app.spinner_tick = app.spinner_tick.wrapping_add(1);
        terminal.draw(|frame| render::draw(frame, app))?;

        // Check for coordinator responses (non-blocking).
        while let Ok(msg) = coord_rx.try_recv() {
            match msg {
                CoordResponse::Token(text) => {
                    if let Some(ChatLine::Assistant(s, _)) = app.chat_history.last_mut() {
                        s.push_str(&text);
                    } else {
                        app.chat_history
                            .push(ChatLine::Assistant(text, app::now_ts()));
                    }
                    app.chat_scroll = 0;
                }
                CoordResponse::Done => {
                    if let Some(ChatLine::Assistant(s, _)) = app.chat_history.last() {
                        let _ = history::save_message(
                            &app.workspace_root,
                            &history::ChatMessage {
                                role: "assistant".into(),
                                content: s.clone(),
                                ts: chrono::Utc::now(),
                            },
                        );
                    }
                    app.streaming = false;
                }
                CoordResponse::Error(e) => {
                    if app.active_dispatch.is_some() {
                        // Error during dispatch — mark ready so user can dismiss.
                        if let Some(ref mut d) = app.active_dispatch {
                            d.status = DispatchStatus::Ready;
                        }
                    }
                    app.chat_history
                        .push(ChatLine::System(format!("Error: {e}")));
                    app.streaming = false;
                }
                CoordResponse::Notification(event) => {
                    app.chat_history.push(ChatLine::System(event.display()));
                    app.chat_scroll = 0;
                }
                CoordResponse::DispatchStateUpdate {
                    task,
                    repos,
                    status,
                    task_md,
                    title,
                    per_repo,
                } => {
                    let ds = match status.as_str() {
                        "ready" => DispatchStatus::Ready,
                        "running" => DispatchStatus::Running,
                        _ => DispatchStatus::Refining,
                    };
                    let artifacts: Vec<app::RepoDispatchArtifacts> = per_repo
                        .into_iter()
                        .map(|r| {
                            let stage = match r.stage.as_str() {
                                "context" => app::RepoStage::Context,
                                "planning" => app::RepoStage::Planning,
                                "done" => app::RepoStage::Done,
                                _ => app::RepoStage::Pending,
                            };
                            app::RepoDispatchArtifacts {
                                repo: r.repo,
                                stage,
                                context_md: r.context_md,
                                plan_md: r.plan_md,
                                file_count: r.file_count,
                                step_count: r.step_count,
                            }
                        })
                        .collect();
                    // Preserve existing section collapsed state across incremental updates.
                    let section_count = 1 + artifacts.len();
                    let (sections, focused_section) =
                        if let Some(ref existing) = app.active_dispatch {
                            if existing.sections.len() == section_count {
                                (
                                    existing.sections.clone(),
                                    existing
                                        .focused_section
                                        .min(section_count.saturating_sub(1)),
                                )
                            } else {
                                (make_default_sections(section_count), 0)
                            }
                        } else {
                            (make_default_sections(section_count), 0)
                        };
                    app.active_dispatch = Some(ActiveDispatch {
                        task,
                        repos,
                        status: ds,
                        task_md,
                        title,
                        per_repo: artifacts,
                        scroll: app.active_dispatch.as_ref().map(|d| d.scroll).unwrap_or(0),
                        sections,
                        focused_section,
                    });
                    app.sidebar_selection = SidebarItem::Dispatch;
                    app.streaming = false;
                }
                CoordResponse::DispatchCleared => {
                    app.active_dispatch = None;
                    app.clamp_sidebar_selection();
                    app.streaming = false;
                }
            }
        }

        // Check for action responses (non-blocking).
        while let Ok(resp) = action_rx.try_recv() {
            match resp {
                ActionResponse::Success(msg) => app.flash(msg),
                ActionResponse::Error(msg) => app.flash(format!("Error: {msg}")),
            }
        }

        // Poll keyboard events with 100ms timeout.
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            // Ctrl+C always quits.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }

            // ── Prefix mode (Ctrl+B, then command) ──
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
                    _ => {} // unknown prefix command — ignore
                }
                continue;
            }

            // Ctrl+B activates prefix mode.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
                app.prefix_active = true;
                continue;
            }

            // ── Confirmation mode ──
            if app.pending_action.is_some() {
                match key.code {
                    KeyCode::Char('y') => {
                        let action = app.pending_action.take().unwrap();
                        match action {
                            PendingAction::CloseWorker(id) => {
                                let root = app.workspace_root.clone();
                                let tx = action_tx.clone();
                                spawn_close_worker(root, id, tx);
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        app.pending_action = None;
                        app.flash("Cancelled");
                    }
                    _ => {} // ignore other keys during confirm
                }
                continue; // skip normal key handling
            }

            // ── Dispatch mode ──
            if let Some(ref dispatch) = app.dispatch {
                use app::DispatchPhase;
                let phase = dispatch.phase.clone();
                match phase {
                    DispatchPhase::SelectRepos => match key.code {
                        KeyCode::Esc => app.dispatch_cancel(),
                        KeyCode::Char('j') | KeyCode::Down => app.dispatch_next(),
                        KeyCode::Char('k') | KeyCode::Up => app.dispatch_prev(),
                        KeyCode::Char(' ') => app.dispatch_toggle_repo(),
                        KeyCode::Enter => app.dispatch_advance_phase(),
                        _ => {}
                    },
                    DispatchPhase::EnterPrompt => {
                        // Enter submits; Alt+Enter inserts newline.
                        match key.code {
                            KeyCode::Enter => {
                                if key.modifiers.contains(KeyModifiers::ALT) {
                                    // Alt+Enter inserts newline.
                                    app.dispatch_insert_char('\n');
                                } else {
                                    // Plain Enter submits.
                                    let prompt = app
                                        .dispatch
                                        .as_ref()
                                        .map(|d| d.prompt.clone())
                                        .unwrap_or_default();
                                    if !prompt.is_empty() {
                                        let repos = app.dispatch_selected_repos();
                                        app.dispatch = None;

                                        app.active_dispatch = Some(ActiveDispatch {
                                            task: prompt.clone(),
                                            repos: repos.clone(),
                                            status: DispatchStatus::Refining,
                                            task_md: String::new(),
                                            title: String::new(),
                                            per_repo: Vec::new(),
                                            scroll: 0,
                                            sections: Vec::new(),
                                            focused_section: 0,
                                        });
                                        app.sidebar_selection = SidebarItem::Dispatch;

                                        let _ = user_tx
                                            .send(UserMessage::SubmitDispatch {
                                                repos,
                                                task: prompt,
                                            })
                                            .await;
                                    }
                                }
                            }
                            KeyCode::Esc => app.dispatch_cancel(),
                            KeyCode::Backspace => app.dispatch_backspace(),
                            KeyCode::Char(c) => {
                                if key.modifiers.contains(KeyModifiers::ALT) && c == '\r' {
                                    app.dispatch_insert_char('\n');
                                } else {
                                    app.dispatch_insert_char(c);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                continue;
            }

            // ── Help overlay ──
            if app.show_help {
                app.show_help = false;
                continue;
            }

            // ── PR detail overlay ──
            if app.pr_detail.is_some() {
                match key.code {
                    KeyCode::Char('o') | KeyCode::Enter => {
                        if let Some(ref pr) = app.pr_detail {
                            let _ = std::process::Command::new("open").arg(&pr.url).spawn();
                            app.flash("Opened PR in browser");
                        }
                        app.pr_detail = None;
                    }
                    KeyCode::Char('c') => {
                        if let Some(ref pr) = app.pr_detail {
                            copy_to_clipboard(&pr.url);
                            app.flash("PR URL copied");
                        }
                        app.pr_detail = None;
                    }
                    KeyCode::Esc | KeyCode::Char('p') | KeyCode::Char('q') => {
                        app.pr_detail = None;
                    }
                    _ => {}
                }
                continue;
            }

            // ── Global keybindings ──
            let handled = match key.code {
                // ? opens help from any panel (except when typing in chat input).
                KeyCode::Char('?')
                    if app.focus == Panel::Workers
                        || (app.focus == Panel::Chat && !app.is_chat_selected()) =>
                {
                    app.show_help = true;
                    true
                }
                KeyCode::Char(c @ '1'..='9')
                    if key.modifiers.contains(KeyModifiers::ALT) || app.focus == Panel::Workers =>
                {
                    let idx = (c as usize) - ('1' as usize);
                    app.switch_tab(idx);
                    true
                }
                _ => false,
            };

            if !handled {
                match app.focus {
                    Panel::Chat => {
                        if app.is_dispatch_selected() {
                            // ── Content: Dispatch review ──
                            match key.code {
                                KeyCode::Char('y') => {
                                    if app
                                        .active_dispatch
                                        .as_ref()
                                        .is_some_and(|d| d.status == DispatchStatus::Ready)
                                    {
                                        if app.daemon_connected {
                                            // Daemon executes the dispatch.
                                            let _ =
                                                user_tx.send(UserMessage::ConfirmDispatch).await;
                                            app.flash("Confirming dispatch...");
                                        } else {
                                            // Local fallback: dispatch using pipeline artifacts.
                                            let dispatch = app.active_dispatch.take().unwrap();
                                            let count = dispatch.per_repo.len();
                                            app.sidebar_selection = SidebarItem::Chat;
                                            if count > 0 {
                                                app.flash(format!(
                                                    "Dispatching {count} worker(s)..."
                                                ));
                                                let ws = app.workspace_root.clone();
                                                let atx = action_tx.clone();
                                                let title = dispatch.title.clone();
                                                let task_md = dispatch.task_md.clone();
                                                for r in dispatch.per_repo {
                                                    spawn_pipeline_dispatch(
                                                        ws.clone(),
                                                        title.clone(),
                                                        task_md.clone(),
                                                        r,
                                                        atx.clone(),
                                                    );
                                                }
                                            } else {
                                                app.flash("No pipeline results to confirm");
                                            }
                                        }
                                    }
                                }
                                KeyCode::Char('z') => {
                                    app.zoomed = !app.zoomed;
                                }
                                KeyCode::Char('n') => {
                                    if app.daemon_connected {
                                        let _ = user_tx.send(UserMessage::CancelDispatch).await;
                                    }
                                    app.active_dispatch = None;
                                    app.sidebar_selection = SidebarItem::Chat;
                                    app.flash("Dispatch cancelled");
                                }
                                KeyCode::Esc => {
                                    if app.zoomed {
                                        app.zoomed = false;
                                    } else {
                                        if app.daemon_connected {
                                            let _ = user_tx.send(UserMessage::CancelDispatch).await;
                                        }
                                        app.active_dispatch = None;
                                        app.sidebar_selection = SidebarItem::Chat;
                                        app.flash("Dispatch cancelled");
                                    }
                                }
                                // Section navigation.
                                KeyCode::Char('k') => app.dispatch_prev_section(),
                                KeyCode::Char('j') => app.dispatch_next_section(),
                                // Half-page scroll.
                                KeyCode::Char('u') => app.scroll_dispatch_up(viewport_height),
                                KeyCode::Char('d') => app.scroll_dispatch_down(viewport_height),
                                KeyCode::Enter => {
                                    if app
                                        .active_dispatch
                                        .as_ref()
                                        .is_some_and(|d| d.status == DispatchStatus::Ready)
                                    {
                                        app.dispatch_toggle_section();
                                    }
                                }
                                KeyCode::Char('c') => app.dispatch_toggle_all_sections(),
                                KeyCode::Tab | KeyCode::BackTab => app.focus = Panel::Workers,
                                _ => {}
                            }
                        } else if app.is_chat_selected() {
                            // ── Content: Chat mode ──
                            // Ctrl modifiers first (before generic Char match).
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                match key.code {
                                    KeyCode::Char('u') => app.scroll_chat_half_up(viewport_height),
                                    KeyCode::Char('d') => {
                                        app.scroll_chat_half_down(viewport_height)
                                    }
                                    KeyCode::Char('b') => app.scroll_chat_page_up(viewport_height),
                                    KeyCode::Char('f') => {
                                        app.scroll_chat_page_down(viewport_height)
                                    }
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::Char('z') => {
                                        app.zoomed = !app.zoomed;
                                    }
                                    KeyCode::Char('?') => {
                                        app.show_help = true;
                                    }
                                    KeyCode::Char('h') | KeyCode::BackTab | KeyCode::Left => {
                                        app.focus = Panel::Workers
                                    }
                                    KeyCode::Tab => app.focus = Panel::Workers,
                                    KeyCode::Esc => {
                                        if app.zoomed {
                                            app.zoomed = false;
                                        } else {
                                            app.focus = Panel::Workers;
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if !app.input.is_empty() && !app.streaming {
                                            let text = app.take_input();
                                            let _ = history::save_message(
                                                &app.workspace_root,
                                                &history::ChatMessage {
                                                    role: "user".into(),
                                                    content: text.clone(),
                                                    ts: chrono::Utc::now(),
                                                },
                                            );
                                            app.chat_history
                                                .push(ChatLine::User(text.clone(), app::now_ts()));
                                            app.streaming = true;
                                            let send_text = if app.daemon_connected {
                                                text
                                            } else {
                                                let recent =
                                                    history::load_history(&app.workspace_root, 20);
                                                build_prompt_with_history(&recent, &text)
                                            };
                                            let _ =
                                                user_tx.send(UserMessage::Chat(send_text)).await;
                                        }
                                    }
                                    KeyCode::Backspace => app.backspace(),
                                    // Vim-style scroll
                                    KeyCode::Char('j') | KeyCode::Down => {
                                        app.scroll_chat_lines(1, false)
                                    }
                                    KeyCode::Char('k') | KeyCode::Up => {
                                        app.scroll_chat_lines(1, true)
                                    }
                                    KeyCode::Char('u') => app.scroll_chat_half_up(viewport_height),
                                    KeyCode::Char('d') => {
                                        app.scroll_chat_half_down(viewport_height)
                                    }
                                    KeyCode::Char('G') | KeyCode::End => {
                                        app.scroll_chat_to_bottom()
                                    }
                                    KeyCode::Home => {
                                        // We use a large value; render will clamp.
                                        app.scroll_chat_to_top(u16::MAX, viewport_height)
                                    }
                                    KeyCode::PageUp => app.scroll_chat_page_up(viewport_height),
                                    KeyCode::PageDown => app.scroll_chat_page_down(viewport_height),
                                    KeyCode::Char(c) => app.insert_char(c),
                                    _ => {}
                                }
                            }
                        } else {
                            // ── Content: Worker pane (forward keys to agent) ──
                            // Esc / Tab / BackTab return focus to sidebar.
                            // Everything else is forwarded to the agent pane.
                            match key.code {
                                KeyCode::Char('z') => {
                                    app.zoomed = !app.zoomed;
                                }
                                KeyCode::Esc => {
                                    if app.zoomed {
                                        app.zoomed = false;
                                    } else {
                                        app.focus = Panel::Workers;
                                    }
                                }
                                KeyCode::Tab | KeyCode::BackTab => {
                                    app.focus = Panel::Workers;
                                }
                                _ => {
                                    if let Some((_, wt)) = app.selected_worker()
                                        && let Some(ref pane_id) = wt.agent_pane_id
                                    {
                                        send_key_to_pane(pane_id, key.code, key.modifiers);
                                    }
                                }
                            }
                        }
                    }
                    Panel::Workers => {
                        // ── Sidebar mode ──
                        // When dispatch is selected and Ready, route section/scroll keys.
                        if app.is_dispatch_selected()
                            && app
                                .active_dispatch
                                .as_ref()
                                .is_some_and(|d| d.status == DispatchStatus::Ready)
                        {
                            match key.code {
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.dispatch_next_section();
                                    continue;
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.dispatch_prev_section();
                                    continue;
                                }
                                KeyCode::Enter => {
                                    app.dispatch_toggle_section();
                                    continue;
                                }
                                KeyCode::Char('c') => {
                                    app.dispatch_toggle_all_sections();
                                    continue;
                                }
                                KeyCode::Char('u') => {
                                    app.scroll_dispatch_up(viewport_height);
                                    continue;
                                }
                                KeyCode::Char('d') => {
                                    app.scroll_dispatch_down(viewport_height);
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        match key.code {
                            KeyCode::Char('z') => {
                                app.zoomed = true;
                                app.focus = Panel::Chat;
                            }
                            KeyCode::Char('l') | KeyCode::Tab | KeyCode::Right => {
                                app.focus = Panel::Chat
                            }
                            KeyCode::BackTab => app.focus = Panel::Chat,
                            KeyCode::Char('q') => break,
                            KeyCode::Char('j') | KeyCode::Down => app.select_next(),
                            KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
                            KeyCode::Char('g') => app.select_first(),
                            KeyCode::Char('G') => app.select_last(),
                            KeyCode::Esc => app.focus = Panel::Chat,
                            KeyCode::Enter => {
                                // Focus the content panel (chat or worker input).
                                app.focus = Panel::Chat;
                            }
                            KeyCode::Char('o') => {
                                // Jump directly to agent pane for selected worker.
                                if let Some((_, wt)) = app.selected_worker()
                                    && let Some(ref pane_id) = wt.agent_pane_id
                                {
                                    let _ = jump_to_worker_pane(pane_id);
                                }
                            }
                            KeyCode::Char('x') => {
                                // Close worker (with confirmation).
                                if let Some((id, _)) = app.selected_worker() {
                                    app.pending_action =
                                        Some(PendingAction::CloseWorker(id.to_string()));
                                }
                            }
                            KeyCode::Char('p') => {
                                // Show PR detail overlay.
                                if let Some((_, wt)) = app.selected_worker() {
                                    if let Some(ref pr) = wt.pr {
                                        app.pr_detail = Some(PrDetailInfo {
                                            number: pr.number.to_string(),
                                            title: pr.title.clone(),
                                            state: pr.state.clone(),
                                            url: pr.url.clone(),
                                        });
                                    } else {
                                        app.flash("No PR for this worker");
                                    }
                                }
                            }
                            KeyCode::Char('c') => {
                                // Copy PR URL to clipboard.
                                if let Some((_, wt)) = app.selected_worker() {
                                    if let Some(ref pr) = wt.pr {
                                        copy_to_clipboard(&pr.url);
                                        app.flash("PR URL copied");
                                    } else {
                                        app.flash("No PR to copy");
                                    }
                                }
                            }
                            KeyCode::Char('d') => {
                                if app.active_dispatch.is_some() {
                                    // Jump to dispatch review.
                                    app.sidebar_selection = SidebarItem::Dispatch;
                                    app.focus = Panel::Chat;
                                } else {
                                    // Open dispatch overlay.
                                    app.enter_dispatch();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Periodic refresh (workers).
        app.tick();

        // Poll UI inbox for daemon-routed notifications (fallback when not socket-connected).
        if !app.daemon_connected {
            let events = inbox::poll_events(&app.workspace_root, &mut app.inbox_pos);
            for event in events {
                app.chat_history.push(ChatLine::System(event.display()));
                app.chat_scroll = 0;
            }
        }
    }

    Ok(())
}

// ── Async action helpers ─────────────────────────────────

/// Spawn a pipeline dispatch — uses Worker::dispatch_pipeline_task with structured artifacts.
fn spawn_pipeline_dispatch(
    workspace_root: PathBuf,
    title: String,
    task_md: String,
    repo_result: app::RepoDispatchArtifacts,
    tx: mpsc::Sender<ActionResponse>,
) {
    tokio::spawn(async move {
        let prompt =
            crate::pipeline::dispatch::build_dispatch_prompt(&title, Some(&repo_result.repo));
        let worker = crate::worker::Worker::new();
        match worker
            .dispatch_pipeline_task(
                &workspace_root,
                &prompt,
                Some(&repo_result.repo),
                "default",
                &task_md,
                repo_result.context_md.as_deref(),
                repo_result.plan_md.as_deref(),
            )
            .await
        {
            Ok(worktree_id) => {
                let _ = tx
                    .send(ActionResponse::Success(format!(
                        "Worker created for `{}`: {worktree_id}",
                        repo_result.repo
                    )))
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(ActionResponse::Error(format!(
                        "dispatch `{}`: {e}",
                        repo_result.repo
                    )))
                    .await;
            }
        }
    });
}

/// Spawn a task to close a worker via `swarm close`.
fn spawn_close_worker(workspace_root: PathBuf, id: String, tx: mpsc::Sender<ActionResponse>) {
    tokio::spawn(async move {
        let result = tokio::process::Command::new("swarm")
            .args(["--dir", &workspace_root.to_string_lossy(), "close", &id])
            .output()
            .await;

        match result {
            Ok(output) if output.status.success() => {
                let _ = tx
                    .send(ActionResponse::Success(format!("Closed {id}")))
                    .await;
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = tx
                    .send(ActionResponse::Error(format!("swarm close: {stderr}")))
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(ActionResponse::Error(format!("swarm close: {e}")))
                    .await;
            }
        }
    });
}

/// Forward a key event to an agent pane (legacy tmux send-keys).
fn send_key_to_pane(pane_id: &str, code: KeyCode, modifiers: KeyModifiers) {
    let key_str = if modifiers.contains(KeyModifiers::CONTROL) {
        // Ctrl+<key> → "C-<key>"
        match code {
            KeyCode::Char(c) => format!("C-{c}"),
            _ => return,
        }
    } else if modifiers.contains(KeyModifiers::ALT) {
        match code {
            KeyCode::Char(c) => format!("M-{c}"),
            KeyCode::Enter => "M-Enter".to_string(),
            _ => return,
        }
    } else {
        match code {
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Backspace => "BSpace".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Delete => "DC".to_string(),
            KeyCode::Char(c) => {
                // Use -l (literal) for printable characters.
                let _ = std::process::Command::new("tmux")
                    .args(["send-keys", "-t", pane_id, "-l", &c.to_string()])
                    .output();
                return;
            }
            _ => return,
        }
    };

    let _ = std::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, &key_str])
        .output();
}

/// Jump to a worker's agent pane (legacy tmux select-pane).
fn jump_to_worker_pane(pane_id: &str) -> std::io::Result<()> {
    std::process::Command::new("tmux")
        .args(["select-pane", "-t", pane_id])
        .status()?;
    Ok(())
}

/// Copy text to the system clipboard (macOS).
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Background task that receives user messages and calls the coordinator (local fallback).
async fn coordinator_task(
    cwd: std::path::PathBuf,
    mut user_rx: mpsc::Receiver<UserMessage>,
    coord_tx: mpsc::Sender<CoordResponse>,
) {
    // Build the coordinator system prompt.
    let workspace = match load_workspace(&cwd) {
        Ok(ws) => ws,
        Err(e) => {
            let _ = coord_tx
                .send(CoordResponse::Error(format!(
                    "Failed to load workspace: {e}"
                )))
                .await;
            return;
        }
    };

    let system_prompt = build_system_prompt(&workspace);

    while let Some(msg) = user_rx.recv().await {
        match msg {
            UserMessage::Chat(text) => {
                // Spawn an ephemeral session for each message.
                let response = run_coordinator_message(&cwd, &system_prompt, &text).await;

                match response {
                    Ok(text) => {
                        let _ = coord_tx.send(CoordResponse::Token(text)).await;
                        let _ = coord_tx.send(CoordResponse::Done).await;
                    }
                    Err(e) => {
                        let _ = coord_tx.send(CoordResponse::Error(format!("{e}"))).await;
                    }
                }
            }
            UserMessage::SubmitDispatch { repos, task } => {
                // Run the pipeline (refine → context → plan) locally.
                let repo_refs: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
                let conventions = workspace.conventions.as_deref().map(|s| s.to_owned());

                // Create progress channel for incremental updates.
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<crate::pipeline::PipelineEvent>();

                // Spawn forwarding task that converts progress events to DispatchStateUpdate.
                let fwd_tx = coord_tx.clone();
                let fwd_repos = repos.clone();
                let fwd_task = task.clone();
                tokio::spawn(async move {
                    // Track state incrementally for building updates.
                    let mut title = String::new();
                    let task_md = String::new();
                    let mut status = "refining";
                    let mut repo_infos: Vec<RepoDispatchInfo> = fwd_repos
                        .iter()
                        .map(|r| RepoDispatchInfo {
                            repo: r.clone(),
                            stage: "pending".into(),
                            context_md: None,
                            plan_md: None,
                            file_count: None,
                            step_count: None,
                        })
                        .collect();

                    while let Some(event) = progress_rx.recv().await {
                        use crate::pipeline::PipelineEvent;
                        match event {
                            PipelineEvent::RefineComplete { title: t } => {
                                title = t;
                                status = "running";
                            }
                            PipelineEvent::RepoContextStarted { repo } => {
                                if let Some(r) = repo_infos.iter_mut().find(|r| r.repo == repo) {
                                    r.stage = "context".into();
                                }
                            }
                            PipelineEvent::RepoContextComplete { repo, file_count } => {
                                if let Some(r) = repo_infos.iter_mut().find(|r| r.repo == repo) {
                                    r.file_count = Some(file_count);
                                }
                            }
                            PipelineEvent::RepoPlanStarted { repo } => {
                                if let Some(r) = repo_infos.iter_mut().find(|r| r.repo == repo) {
                                    r.stage = "planning".into();
                                }
                            }
                            PipelineEvent::RepoPlanComplete { repo, step_count } => {
                                if let Some(r) = repo_infos.iter_mut().find(|r| r.repo == repo) {
                                    r.stage = "done".into();
                                    r.step_count = Some(step_count);
                                }
                            }
                        }
                        let _ = fwd_tx
                            .send(CoordResponse::DispatchStateUpdate {
                                task: fwd_task.clone(),
                                repos: fwd_repos.clone(),
                                status: status.into(),
                                task_md: task_md.clone(),
                                title: title.clone(),
                                per_repo: repo_infos.clone(),
                            })
                            .await;
                    }
                });

                let result =
                    crate::pipeline::run_pipeline_multi(crate::pipeline::MultiPipelineOptions {
                        raw_prompt: &task,
                        workspace_root: &cwd,
                        conventions: conventions.as_deref(),
                        repos: repo_refs,
                        profile: "default",
                        until: None,
                        dry_run: true,
                        on_progress: Some(progress_tx),
                    })
                    .await;

                match result {
                    Ok(pipeline_result) => {
                        let per_repo: Vec<RepoDispatchInfo> = pipeline_result
                            .per_repo
                            .iter()
                            .map(|r| RepoDispatchInfo {
                                repo: r.repo.clone(),
                                stage: "done".into(),
                                context_md: r.context_md.clone(),
                                plan_md: r.plan_md.clone(),
                                file_count: r
                                    .context_md
                                    .as_ref()
                                    .map(|c| c.lines().filter(|l| l.starts_with("- `")).count()),
                                step_count: r.plan_md.as_ref().map(|p| {
                                    p.lines()
                                        .filter(|l| {
                                            l.starts_with("- ")
                                                || l.starts_with("1.")
                                                || l.starts_with("## Step")
                                        })
                                        .count()
                                }),
                            })
                            .collect();

                        let _ = coord_tx
                            .send(CoordResponse::DispatchStateUpdate {
                                task: task.clone(),
                                repos,
                                status: "ready".into(),
                                task_md: pipeline_result.task_md,
                                title: pipeline_result.title,
                                per_repo,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = coord_tx
                            .send(CoordResponse::Error(format!("Pipeline error: {e}")))
                            .await;
                    }
                }
            }
            UserMessage::ConfirmDispatch | UserMessage::CancelDispatch => {
                // Confirm/cancel handled directly in the event loop.
                continue;
            }
        }
    }
}

/// Run a single coordinator message and return the response (local fallback).
async fn run_coordinator_message(
    cwd: &Path,
    system_prompt: &str,
    user_message: &str,
) -> Result<String> {
    use apiari_claude_sdk::types::ContentBlock;
    use apiari_claude_sdk::{ClaudeClient, Event as SdkEvent, SessionOptions};

    let opts = SessionOptions {
        system_prompt: Some(system_prompt.to_owned()),
        model: Some("sonnet".into()),
        max_turns: Some(20),
        no_session_persistence: true,
        working_dir: Some(cwd.to_path_buf()),
        allowed_tools: vec!["Bash".into(), "Read".into(), "Glob".into(), "Grep".into()],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;

    session
        .send_message(user_message)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    session.close_stdin();

    let mut text = String::new();
    loop {
        let event = session
            .next_event()
            .await
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        match event {
            Some(SdkEvent::Assistant { message, .. }) => {
                for block in &message.message.content {
                    if let ContentBlock::Text { text: t } = block {
                        text.push_str(t);
                    }
                }
                if message.message.stop_reason.as_deref() == Some("end_turn") {
                    break;
                }
            }
            Some(SdkEvent::Result(r)) => {
                if let Some(result_text) = &r.result
                    && text.is_empty()
                {
                    text = result_text.clone();
                }
                break;
            }
            Some(SdkEvent::RateLimit(_))
            | Some(SdkEvent::System(_))
            | Some(SdkEvent::User(_))
            | Some(SdkEvent::Stream { .. }) => {}
            None => break,
        }
    }

    Ok(text)
}

/// Build a user message with recent conversation history prepended as context (local fallback).
fn build_prompt_with_history(history: &[history::ChatMessage], user_input: &str) -> String {
    if history.is_empty() {
        return user_input.to_string();
    }
    let mut parts = vec!["[Recent conversation]".to_string()];
    for msg in history.iter().rev().take(20).rev() {
        parts.push(format!("{}: {}", msg.role, msg.content));
    }
    parts.push(format!("\nCurrent message: {user_input}"));
    parts.join("\n")
}

/// Build the coordinator system prompt from workspace config (local fallback).
fn build_system_prompt(workspace: &crate::workspace::Workspace) -> String {
    let mut prompt = format!(
        "You are a coordinator for the \"{}\" workspace. \
         Help the user understand project status, plan work, and manage agents.\n\n\
         The user is chatting with you from `hive ui` — the local terminal TUI (not Telegram, not Claude Code).\n",
        workspace.name
    );

    if let Some(ref soul) = workspace.soul {
        prompt.push_str("\n--- Soul ---\n");
        prompt.push_str(soul);
        prompt.push('\n');
    }

    if let Some(ref conventions) = workspace.conventions {
        prompt.push_str("\n--- Conventions ---\n");
        prompt.push_str(conventions);
        prompt.push('\n');
    }

    if !workspace.repos.is_empty() {
        prompt.push_str("\nManaged repositories: ");
        prompt.push_str(&workspace.repos.join(", "));
        prompt.push('\n');
    }

    prompt.push_str("\n## Multi-repo Dispatch\n");
    prompt.push_str("When you receive a message starting with [DISPATCH], produce a ```dispatch:<repo> block for each listed repo.\n");
    prompt.push_str("Tailor each prompt to the repo's role. Do NOT use tools — just produce dispatch blocks.\n\n");

    prompt
}

/// Create default sections: Task collapsed, repos expanded.
fn make_default_sections(count: usize) -> Vec<app::ReviewSection> {
    let mut sections = Vec::with_capacity(count);
    // Section 0: Task — start collapsed (user already knows what they typed).
    sections.push(app::ReviewSection { collapsed: true });
    // Sections 1..N: repos — start expanded so user sees the plan.
    for _ in 1..count {
        sections.push(app::ReviewSection { collapsed: false });
    }
    sections
}

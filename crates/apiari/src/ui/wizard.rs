//! TUI onboarding wizard — runs when no workspace config exists.
//!
//! Four-screen flow: Welcome → Telegram → Integrations → Done.

use color_eyre::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::io::stdout;
use std::path::PathBuf;
use std::time::Instant;

use super::theme;
use crate::config;

// ── Wizard state ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardStep {
    Welcome,
    Telegram,
    Integrations,
    Done,
}

impl WizardStep {
    fn index(&self) -> usize {
        match self {
            Self::Welcome => 1,
            Self::Telegram => 2,
            Self::Integrations => 3,
            Self::Done => 4,
        }
    }

    fn next(&self) -> Self {
        match self {
            Self::Welcome => Self::Telegram,
            Self::Telegram => Self::Integrations,
            Self::Integrations => Self::Done,
            Self::Done => Self::Done,
        }
    }

    fn prev(&self) -> Self {
        match self {
            Self::Welcome => Self::Welcome,
            Self::Telegram => Self::Welcome,
            Self::Integrations => Self::Telegram,
            Self::Done => Self::Integrations,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenStatus {
    Empty,
    Validating,
    Valid,
    Invalid,
}

#[derive(Debug, Clone)]
struct Integration {
    name: &'static str,
    description: &'static str,
    enabled: bool,
    /// Fields: (label, value)
    fields: Vec<(&'static str, String)>,
}

struct WizardState {
    step: WizardStep,
    // Welcome
    workspace_name: String,
    root_dir: String,
    welcome_field: usize, // 0 = name, 1 = root
    // Telegram
    bot_token: String,
    chat_id: String,
    topic_id: String,
    telegram_field: usize, // 0 = token, 1 = chat_id, 2 = topic_id
    token_status: TokenStatus,
    token_status_msg: String,
    last_token_change: Option<Instant>,
    // Integrations
    integrations: Vec<Integration>,
    integration_selection: usize,
    integration_field: usize,  // which sub-field is focused
    integration_editing: bool, // editing a sub-field
    // Common
    needs_redraw: bool,
    quit: bool,
    launch_ui: bool,
}

impl WizardState {
    fn new(initial_name: Option<&str>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let name = initial_name.map(|s| s.to_string()).unwrap_or_else(|| {
            cwd.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string()
        });
        let root = cwd.to_string_lossy().to_string();

        // Detect available tools
        let gh_available = std::process::Command::new("gh")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let swarm_available = std::process::Command::new("which")
            .arg("swarm")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        Self {
            step: WizardStep::Welcome,
            workspace_name: name,
            root_dir: root,
            welcome_field: 0,
            bot_token: String::new(),
            chat_id: String::new(),
            topic_id: String::new(),
            telegram_field: 0,
            token_status: TokenStatus::Empty,
            token_status_msg: String::new(),
            last_token_change: None,
            integrations: vec![
                Integration {
                    name: "GitHub",
                    description: "PR reviews, CI notifications",
                    enabled: gh_available,
                    fields: vec![],
                },
                Integration {
                    name: "Sentry",
                    description: "Error monitoring",
                    enabled: false,
                    fields: vec![
                        ("org", String::new()),
                        ("project", String::new()),
                        ("token", String::new()),
                    ],
                },
                Integration {
                    name: "Linear",
                    description: "Issues & assignments",
                    enabled: false,
                    fields: vec![("api_key", String::new()), ("name", String::new())],
                },
                Integration {
                    name: "Swarm",
                    description: "AI coding agents",
                    enabled: swarm_available,
                    fields: vec![],
                },
            ],
            integration_selection: 0,
            integration_field: 0,
            integration_editing: false,
            needs_redraw: true,
            quit: false,
            launch_ui: false,
        }
    }
}

// ── Public entry point ───────────────────────────────────

/// Result of the wizard: the name of the created workspace config, or None if quit.
pub struct WizardResult {
    pub workspace_name: Option<String>,
    pub launch_ui: bool,
}

/// Run the onboarding wizard TUI. Returns the workspace name if config was created.
/// `initial_name` is an optional override for the workspace name (from `--name` flag).
pub async fn run_wizard(initial_name: Option<&str>) -> Result<WizardResult> {
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(crossterm::event::EnableMouseCapture)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(crossterm::event::DisableMouseCapture);
        let _ = stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    let result = wizard_loop(&mut terminal, initial_name).await;

    disable_raw_mode()?;
    stdout().execute(crossterm::event::DisableMouseCapture)?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

async fn wizard_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    initial_name: Option<&str>,
) -> Result<WizardResult> {
    let mut state = WizardState::new(initial_name);
    let mut events = EventStream::new();
    let mut debounce_interval = tokio::time::interval(std::time::Duration::from_millis(200));
    debounce_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if state.needs_redraw {
            terminal.draw(|f| draw_wizard(f, &state))?;
            state.needs_redraw = false;
        }

        if state.quit {
            return Ok(WizardResult {
                workspace_name: None,
                launch_ui: false,
            });
        }

        if state.launch_ui {
            // Write config and return
            let name = write_workspace_config(&state)?;
            return Ok(WizardResult {
                workspace_name: Some(name),
                launch_ui: true,
            });
        }

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        handle_wizard_key(&mut state, key);
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        state.needs_redraw = true;
                    }
                    _ => {}
                }
            }
            _ = debounce_interval.tick() => {
                // Check if we need to fire a debounced token validation
                if let Some(changed_at) = state.last_token_change
                    && changed_at.elapsed() >= std::time::Duration::from_millis(500)
                    && state.token_status == TokenStatus::Validating
                {
                    state.last_token_change = None;
                    validate_bot_token(&mut state).await;
                    state.needs_redraw = true;
                }
            }
        }
    }
}

// ── Key handling ─────────────────────────────────────────

fn handle_wizard_key(state: &mut WizardState, key: crossterm::event::KeyEvent) {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.quit = true;
        state.needs_redraw = true;
        return;
    }

    match state.step {
        WizardStep::Welcome => handle_welcome_key(state, key),
        WizardStep::Telegram => handle_telegram_key(state, key),
        WizardStep::Integrations => handle_integrations_key(state, key),
        WizardStep::Done => handle_done_key(state, key),
    }

    state.needs_redraw = true;
}

fn handle_welcome_key(state: &mut WizardState, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Tab | KeyCode::Down => {
            state.welcome_field = (state.welcome_field + 1) % 2;
        }
        KeyCode::BackTab | KeyCode::Up => {
            state.welcome_field = if state.welcome_field == 0 { 1 } else { 0 };
        }
        KeyCode::Esc => {
            state.quit = true;
        }
        KeyCode::Enter | KeyCode::Right => {
            if !state.workspace_name.trim().is_empty() {
                state.step = state.step.next();
            }
        }
        KeyCode::Backspace => {
            let field = current_welcome_field(state);
            field.pop();
        }
        KeyCode::Char(c) => {
            let field = current_welcome_field(state);
            field.push(c);
        }
        _ => {}
    }
}

fn current_welcome_field(state: &mut WizardState) -> &mut String {
    match state.welcome_field {
        0 => &mut state.workspace_name,
        _ => &mut state.root_dir,
    }
}

fn handle_telegram_key(state: &mut WizardState, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Tab | KeyCode::Down => {
            state.telegram_field = (state.telegram_field + 1) % 3;
        }
        KeyCode::BackTab | KeyCode::Up => {
            state.telegram_field = if state.telegram_field == 0 {
                2
            } else {
                state.telegram_field - 1
            };
        }
        KeyCode::Enter | KeyCode::Right => {
            state.step = state.step.next();
        }
        KeyCode::Left | KeyCode::Esc => {
            state.step = state.step.prev();
        }
        KeyCode::Backspace => {
            let field = current_telegram_field(state);
            field.pop();
            if state.telegram_field == 0 {
                state.token_status = TokenStatus::Empty;
                state.token_status_msg.clear();
            }
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Skip telegram
            state.step = state.step.next();
        }
        KeyCode::Char(c) => {
            let field = current_telegram_field(state);
            field.push(c);
            // Debounce token validation: mark pending, actual call fires after 500ms idle
            if state.telegram_field == 0 && state.bot_token.len() > 10 {
                state.token_status = TokenStatus::Validating;
                state.token_status_msg = "validating...".into();
                state.last_token_change = Some(Instant::now());
            }
        }
        _ => {}
    }
}

fn current_telegram_field(state: &mut WizardState) -> &mut String {
    match state.telegram_field {
        0 => &mut state.bot_token,
        1 => &mut state.chat_id,
        _ => &mut state.topic_id,
    }
}

async fn validate_bot_token(state: &mut WizardState) {
    let token = state.bot_token.clone();
    state.token_status = TokenStatus::Validating;
    state.token_status_msg = "validating...".into();

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            state.token_status = TokenStatus::Invalid;
            state.token_status_msg = format!("error: {e}");
            return;
        }
    };

    let url = format!("https://api.telegram.org/bot{token}/getMe");
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>().await
                && body.get("ok").and_then(|v| v.as_bool()) == Some(true)
            {
                let bot_name = body
                    .get("result")
                    .and_then(|r| r.get("username"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("bot");
                state.token_status = TokenStatus::Valid;
                state.token_status_msg = format!("@{bot_name}");
                return;
            }
            state.token_status = TokenStatus::Invalid;
            state.token_status_msg = "invalid response".into();
        }
        Ok(resp) => {
            state.token_status = TokenStatus::Invalid;
            state.token_status_msg = format!("HTTP {}", resp.status());
        }
        Err(e) => {
            state.token_status = TokenStatus::Invalid;
            state.token_status_msg = format!("error: {e}");
        }
    }
}

fn handle_integrations_key(state: &mut WizardState, key: crossterm::event::KeyEvent) {
    let int_count = state.integrations.len();

    // If editing a sub-field, handle text input
    if state.integration_editing {
        match key.code {
            KeyCode::Esc => {
                state.integration_editing = false;
            }
            KeyCode::Tab | KeyCode::Down => {
                let fields_len = state.integrations[state.integration_selection].fields.len();
                if fields_len > 0 {
                    state.integration_field = (state.integration_field + 1) % fields_len;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                let fields_len = state.integrations[state.integration_selection].fields.len();
                if fields_len > 0 {
                    state.integration_field = if state.integration_field == 0 {
                        fields_len - 1
                    } else {
                        state.integration_field - 1
                    };
                }
            }
            KeyCode::Enter => {
                state.integration_editing = false;
            }
            KeyCode::Backspace => {
                let sel = state.integration_selection;
                let fi = state.integration_field;
                if fi < state.integrations[sel].fields.len() {
                    state.integrations[sel].fields[fi].1.pop();
                }
            }
            KeyCode::Char(c) => {
                let sel = state.integration_selection;
                let fi = state.integration_field;
                if fi < state.integrations[sel].fields.len() {
                    state.integrations[sel].fields[fi].1.push(c);
                }
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if int_count > 0 {
                state.integration_selection = (state.integration_selection + 1) % int_count;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if int_count > 0 {
                state.integration_selection = if state.integration_selection == 0 {
                    int_count - 1
                } else {
                    state.integration_selection - 1
                };
            }
        }
        KeyCode::Char(' ') => {
            let sel = state.integration_selection;
            state.integrations[sel].enabled = !state.integrations[sel].enabled;
            // If enabling and has fields, enter editing mode
            if state.integrations[sel].enabled && !state.integrations[sel].fields.is_empty() {
                state.integration_editing = true;
                state.integration_field = 0;
            }
        }
        KeyCode::Enter | KeyCode::Right => {
            // If current integration is enabled and has fields, edit them
            let sel = state.integration_selection;
            if state.integrations[sel].enabled && !state.integrations[sel].fields.is_empty() {
                state.integration_editing = true;
                state.integration_field = 0;
            } else {
                state.step = state.step.next();
            }
        }
        KeyCode::Tab => {
            state.step = state.step.next();
        }
        KeyCode::Left | KeyCode::Esc => {
            state.step = state.step.prev();
        }
        _ => {}
    }
}

fn handle_done_key(state: &mut WizardState, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            state.launch_ui = true;
        }
        KeyCode::Left | KeyCode::Esc => {
            state.step = state.step.prev();
        }
        KeyCode::Char('q') => {
            state.quit = true;
        }
        _ => {}
    }
}

// ── Validation helpers ───────────────────────────────────

/// Sanitize workspace name: reject path separators and dots-only names.
fn sanitize_workspace_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    // Reject names that are only dots (e.g. ".", "..")
    if name.chars().all(|c| c == '.') {
        return None;
    }
    Some(name)
}

// ── Config writing ───────────────────────────────────────

fn write_workspace_config(state: &WizardState) -> Result<String> {
    let name = match sanitize_workspace_name(&state.workspace_name) {
        Some(n) => n,
        None => {
            return Err(color_eyre::eyre::eyre!(
                "Invalid workspace name: must not contain path separators"
            ));
        }
    };
    let root = state.root_dir.trim();

    let dir = config::workspaces_dir();
    std::fs::create_dir_all(&dir)?;

    let config_path = dir.join(format!("{name}.toml"));

    // Build a WorkspaceConfig struct and serialize with toml
    let telegram = if !state.bot_token.trim().is_empty() && !state.chat_id.trim().is_empty() {
        let chat_id: i64 = state
            .chat_id
            .trim()
            .parse()
            .map_err(|_| color_eyre::eyre::eyre!("chat_id must be a number"))?;
        let topic_id = if state.topic_id.trim().is_empty() {
            None
        } else {
            Some(
                state
                    .topic_id
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| color_eyre::eyre::eyre!("topic_id must be a number"))?,
            )
        };
        Some(config::TelegramConfig {
            bot_token: state.bot_token.trim().to_string(),
            chat_id,
            topic_id,
            allowed_user_ids: vec![],
        })
    } else {
        None
    };

    // Build watchers from integrations
    let mut watchers = config::WatchersConfig::default();
    for int in &state.integrations {
        if !int.enabled {
            continue;
        }
        match int.name {
            "GitHub" => {
                watchers.github = Some(config::GithubWatcherConfig {
                    repos: vec![],
                    interval_secs: 120,
                    review_queue: vec![],
                });
            }
            "Sentry" => {
                let org = int
                    .fields
                    .iter()
                    .find(|f| f.0 == "org")
                    .map(|f| f.1.trim().to_string())
                    .unwrap_or_default();
                let project = int
                    .fields
                    .iter()
                    .find(|f| f.0 == "project")
                    .map(|f| f.1.trim().to_string())
                    .unwrap_or_default();
                let token = int
                    .fields
                    .iter()
                    .find(|f| f.0 == "token")
                    .map(|f| f.1.trim().to_string())
                    .unwrap_or_default();
                if !org.is_empty() && !project.is_empty() && !token.is_empty() {
                    watchers.sentry = Some(config::SentryWatcherConfig {
                        org,
                        project,
                        token,
                        interval_secs: 120,
                    });
                }
            }
            "Linear" => {
                let api_key = int
                    .fields
                    .iter()
                    .find(|f| f.0 == "api_key")
                    .map(|f| f.1.trim().to_string())
                    .unwrap_or_default();
                let lname = int
                    .fields
                    .iter()
                    .find(|f| f.0 == "name")
                    .map(|f| f.1.trim().to_string())
                    .unwrap_or_default();
                if !api_key.is_empty() {
                    watchers.linear = vec![config::LinearWatcherConfig {
                        name: lname,
                        api_key,
                        poll_interval_secs: 60,
                        review_queue: vec![],
                    }];
                }
            }
            "Swarm" => {
                let state_path = format!("{root}/.swarm/state.json");
                watchers.swarm = Some(config::SwarmWatcherConfig {
                    state_path: state_path.into(),
                    interval_secs: 15,
                });
            }
            _ => {}
        }
    }

    let coordinator_name = "Bee";
    let default_prompt = crate::buzz::coordinator::prompt::default_preamble(coordinator_name);

    let ws_config = config::WorkspaceConfig {
        root: root.into(),
        repos: vec![],
        telegram,
        coordinator: config::CoordinatorConfig {
            name: coordinator_name.to_string(),
            model: "sonnet".to_string(),
            max_turns: 20,
            prompt: Some(default_prompt),
            max_session_turns: 50,
            ..config::CoordinatorConfig::default()
        },
        watchers,
        pipeline: config::PipelineConfig::default(),
        commands: vec![],
        morning_brief: None,
        daemon_tcp_port: None,
        daemon_tcp_bind: None,
        daemon_host: None,
        daemon_port: None,
        daemon_endpoints: vec![],
    };

    let toml_str = toml::to_string_pretty(&ws_config)?;
    std::fs::write(&config_path, toml_str)?;
    Ok(name.to_string())
}

// ── Rendering ────────────────────────────────────────────

fn draw_wizard(frame: &mut Frame, state: &WizardState) {
    let size = frame.area();

    // Center content with max width ~70
    let max_w = 70u16.min(size.width.saturating_sub(4));
    let h_pad = (size.width.saturating_sub(max_w)) / 2;
    let content_area = Rect::new(h_pad, 0, max_w, size.height);

    // Vertical: header(3) + body(rest) + footer(2)
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(content_area);

    draw_wizard_header(frame, state, layout[0]);

    match state.step {
        WizardStep::Welcome => draw_welcome(frame, state, layout[1]),
        WizardStep::Telegram => draw_telegram(frame, state, layout[1]),
        WizardStep::Integrations => draw_integrations(frame, state, layout[1]),
        WizardStep::Done => draw_done(frame, state, layout[1]),
    }

    draw_wizard_footer(frame, state, layout[2]);
}

fn draw_wizard_header(frame: &mut Frame, state: &WizardState, area: Rect) {
    let step = state.step.index();
    let mut spans = vec![
        Span::styled(" * ", theme::logo()),
        Span::styled("apiari ", theme::title()),
        Span::styled("setup", theme::muted()),
    ];

    // Right-aligned step indicator
    let indicator = format!("[{step}/4]");
    let used: usize = spans.iter().map(|s| s.content.len()).sum();
    let padding = (area.width as usize)
        .saturating_sub(used)
        .saturating_sub(indicator.len());
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.push(Span::styled(indicator, theme::accent()));

    let header = Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::COMB));
    frame.render_widget(header, Rect::new(area.x, area.y, area.width, 1));
}

fn draw_wizard_footer(frame: &mut Frame, state: &WizardState, area: Rect) {
    let hints = match state.step {
        WizardStep::Welcome => "[Tab] next field  [Enter/\u{2192}] continue  [Esc] quit",
        WizardStep::Telegram => {
            "[Tab] next field  [Enter/\u{2192}] continue  [\u{2190}/Esc] back  [^S] skip"
        }
        WizardStep::Integrations => {
            if state.integration_editing {
                "[Tab] next field  [Enter/Esc] done editing  [\u{2190}] back"
            } else {
                "[j/k] select  [Space] toggle  [Tab/Enter] continue  [\u{2190}/Esc] back"
            }
        }
        WizardStep::Done => "[Enter] launch dashboard  [\u{2190}/Esc] back  [q] quit",
    };

    let footer = Paragraph::new(Line::from(Span::styled(hints, theme::key_desc())))
        .alignment(Alignment::Center);
    frame.render_widget(footer, Rect::new(area.x, area.y + 1, area.width, 1));
}

// ── Step 1: Welcome ──────────────────────────────────────

fn draw_welcome(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // top pad
            Constraint::Length(5), // description
            Constraint::Length(2), // spacing
            Constraint::Length(3), // workspace name field
            Constraint::Length(1), // spacing
            Constraint::Length(3), // root dir field
            Constraint::Min(0),    // fill
        ])
        .split(area);

    // Description
    let desc = Paragraph::new(vec![
        Line::from(Span::styled(
            "Welcome to Apiari",
            Style::default()
                .fg(theme::HONEY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Apiari is your AI ops coordinator. It watches your repos,",
            theme::text(),
        )),
        Line::from(Span::styled(
            "routes signals, and manages coding agents from one dashboard.",
            theme::text(),
        )),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(desc, chunks[1]);

    // Workspace name field
    let name_valid = sanitize_workspace_name(&state.workspace_name).is_some();
    let name_suffix = if !name_valid && !state.workspace_name.trim().is_empty() {
        " \u{2717} invalid name"
    } else {
        ""
    };
    draw_text_field_with_suffix(
        frame,
        chunks[3],
        "Workspace name",
        &state.workspace_name,
        state.welcome_field == 0,
        name_suffix,
        theme::error(),
    );

    // Root directory field
    draw_text_field(
        frame,
        chunks[5],
        "Root directory",
        &state.root_dir,
        state.welcome_field == 1,
    );
}

// ── Step 2: Telegram ─────────────────────────────────────

fn draw_telegram(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top pad
            Constraint::Length(3), // title + desc
            Constraint::Length(1), // spacing
            Constraint::Length(5), // instructions
            Constraint::Length(1), // spacing
            Constraint::Length(3), // bot_token
            Constraint::Length(1), // spacing
            Constraint::Length(3), // chat_id
            Constraint::Length(1), // spacing
            Constraint::Length(3), // topic_id
            Constraint::Min(0),    // fill
        ])
        .split(area);

    let desc = Paragraph::new(vec![
        Line::from(Span::styled("Telegram", theme::title())),
        Line::from(""),
        Line::from(Span::styled(
            "Bee sends notifications and responds to your messages via Telegram.",
            theme::text(),
        )),
    ]);
    frame.render_widget(desc, chunks[1]);

    let instructions = Paragraph::new(vec![
        Line::from(Span::styled(
            "  1. Message @BotFather on Telegram \u{2192} /newbot",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "  2. Copy the bot token below",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "  3. Get your chat ID from @userinfobot",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "  (optional \u{2014} press ^S to skip Telegram setup)",
            theme::muted(),
        )),
    ]);
    frame.render_widget(instructions, chunks[3]);

    // Token field with validation indicator
    let token_suffix = match state.token_status {
        TokenStatus::Empty => String::new(),
        TokenStatus::Validating => " \u{2026}".into(),
        TokenStatus::Valid => format!(" \u{2713} {}", state.token_status_msg),
        TokenStatus::Invalid => format!(" \u{2717} {}", state.token_status_msg),
    };
    let token_style = match state.token_status {
        TokenStatus::Valid => theme::success(),
        TokenStatus::Invalid => theme::error(),
        _ => theme::muted(),
    };
    draw_text_field_with_suffix(
        frame,
        chunks[5],
        "Bot token",
        &state.bot_token,
        state.telegram_field == 0,
        &token_suffix,
        token_style,
    );

    // Chat ID with inline validation
    let chat_id_valid =
        state.chat_id.trim().is_empty() || state.chat_id.trim().parse::<i64>().is_ok();
    let chat_suffix = if !chat_id_valid {
        " \u{2717} must be a number"
    } else {
        ""
    };
    draw_text_field_with_suffix(
        frame,
        chunks[7],
        "Chat ID",
        &state.chat_id,
        state.telegram_field == 1,
        chat_suffix,
        theme::error(),
    );

    // Topic ID with inline validation
    let topic_id_valid =
        state.topic_id.trim().is_empty() || state.topic_id.trim().parse::<i64>().is_ok();
    let topic_suffix = if !topic_id_valid {
        " \u{2717} must be a number"
    } else {
        ""
    };
    draw_text_field_with_suffix(
        frame,
        chunks[9],
        "Topic ID (optional)",
        &state.topic_id,
        state.telegram_field == 2,
        topic_suffix,
        theme::error(),
    );
}

// ── Step 3: Integrations ─────────────────────────────────

fn draw_integrations(frame: &mut Frame, state: &WizardState, area: Rect) {
    let mut constraints = vec![
        Constraint::Length(1), // top pad
        Constraint::Length(2), // title
        Constraint::Length(1), // spacing
    ];
    // Each integration: 2 lines (checkbox + desc) + sub-fields if enabled
    for int in &state.integrations {
        constraints.push(Constraint::Length(2)); // checkbox line
        if int.enabled && !int.fields.is_empty() {
            for _ in &int.fields {
                constraints.push(Constraint::Length(3)); // each sub-field
            }
            constraints.push(Constraint::Length(1)); // spacing after fields
        }
    }
    constraints.push(Constraint::Min(0));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let title = Paragraph::new(vec![
        Line::from(Span::styled("Integrations", theme::title())),
        Line::from(Span::styled(
            "Select which services to connect:",
            theme::text(),
        )),
    ]);
    frame.render_widget(title, chunks[1]);

    let mut chunk_idx = 3; // start after title + spacing
    for (i, int) in state.integrations.iter().enumerate() {
        let selected = i == state.integration_selection && !state.integration_editing;
        let check = if int.enabled { "\u{2713}" } else { " " };
        let marker = if selected { "\u{25b6} " } else { "  " };

        let style = if selected {
            theme::highlight()
        } else if int.enabled {
            theme::text()
        } else {
            theme::muted()
        };

        let line = Paragraph::new(vec![Line::from(vec![
            Span::styled(marker, style),
            Span::styled(format!("[{check}] "), style),
            Span::styled(
                int.name,
                if int.enabled {
                    Style::default()
                        .fg(theme::HONEY)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style
                },
            ),
            Span::styled(format!(" \u{2014} {}", int.description), theme::muted()),
        ])]);
        if chunk_idx < chunks.len() {
            frame.render_widget(line, chunks[chunk_idx]);
        }
        chunk_idx += 1;

        // Sub-fields if enabled
        if int.enabled && !int.fields.is_empty() {
            for (fi, (label, value)) in int.fields.iter().enumerate() {
                let focused = state.integration_editing
                    && i == state.integration_selection
                    && fi == state.integration_field;
                if chunk_idx < chunks.len() {
                    draw_text_field(
                        frame,
                        chunks[chunk_idx],
                        &format!("    {label}"),
                        value,
                        focused,
                    );
                }
                chunk_idx += 1;
            }
            chunk_idx += 1; // spacing
        }
    }
}

// ── Step 4: Done ─────────────────────────────────────────

fn draw_done(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),  // top pad
            Constraint::Length(2),  // title
            Constraint::Length(1),  // spacing
            Constraint::Length(12), // summary
            Constraint::Length(2),  // spacing
            Constraint::Length(3),  // launch button
            Constraint::Min(0),     // fill
        ])
        .split(area);

    let title = Paragraph::new(vec![
        Line::from(Span::styled("Setup complete!", theme::title())),
        Line::from(""),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[1]);

    // Summary
    let mut summary_lines = vec![
        Line::from(vec![
            Span::styled("  \u{2713} ", theme::success()),
            Span::styled("Workspace: ", theme::muted()),
            Span::styled(&state.workspace_name, theme::text()),
        ]),
        Line::from(vec![
            Span::styled("  \u{2713} ", theme::success()),
            Span::styled("Root: ", theme::muted()),
            Span::styled(&state.root_dir, theme::text()),
        ]),
    ];

    if !state.bot_token.trim().is_empty() {
        let status_icon = match state.token_status {
            TokenStatus::Valid => ("\u{2713} ", theme::success()),
            _ => ("~ ", theme::muted()),
        };
        summary_lines.push(Line::from(vec![
            Span::styled(format!("  {} ", status_icon.0), status_icon.1),
            Span::styled("Telegram: ", theme::muted()),
            Span::styled("configured", theme::text()),
        ]));
    } else {
        summary_lines.push(Line::from(vec![
            Span::styled("  - ", theme::muted()),
            Span::styled("Telegram: ", theme::muted()),
            Span::styled("skipped", theme::muted()),
        ]));
    }

    for int in &state.integrations {
        let (icon, style) = if int.enabled {
            ("\u{2713} ", theme::success())
        } else {
            ("- ", theme::muted())
        };
        summary_lines.push(Line::from(vec![
            Span::styled(format!("  {icon}"), style),
            Span::styled(format!("{}: ", int.name), theme::muted()),
            Span::styled(
                if int.enabled { "enabled" } else { "disabled" },
                if int.enabled {
                    theme::text()
                } else {
                    theme::muted()
                },
            ),
        ]));
    }

    let summary = Paragraph::new(summary_lines);
    frame.render_widget(summary, chunks[3]);

    // Launch button
    let button = Paragraph::new(Line::from(vec![Span::styled(
        "  [ Launch Dashboard ]  ",
        Style::default()
            .fg(theme::COMB)
            .bg(theme::HONEY)
            .add_modifier(Modifier::BOLD),
    )]))
    .alignment(Alignment::Center);
    frame.render_widget(button, chunks[5]);
}

// ── Field widgets ────────────────────────────────────────

fn draw_text_field(frame: &mut Frame, area: Rect, label: &str, value: &str, focused: bool) {
    draw_text_field_with_suffix(frame, area, label, value, focused, "", theme::muted());
}

fn draw_text_field_with_suffix(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    focused: bool,
    suffix: &str,
    suffix_style: Style,
) {
    let border_style = if focused {
        theme::border_active()
    } else {
        theme::border()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {label} "),
            if focused {
                theme::accent()
            } else {
                theme::muted()
            },
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cursor = if focused { "\u{2588}" } else { "" };
    let display = format!("{value}{cursor}");
    let mut spans = vec![Span::styled(display, theme::text())];
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, suffix_style));
    }

    let content = Paragraph::new(Line::from(spans));
    frame.render_widget(content, inner);
}

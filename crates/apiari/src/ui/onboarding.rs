//! Conversational onboarding — Bee introduces herself through chat and UI panels
//! progressively reveal as the user progresses.

use color_eyre::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::stdout;
use std::path::PathBuf;
use std::time::Instant;

use super::theme;
use crate::config;

// ── Onboarding stages ────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Greeting,        // full screen chat, workspace name prompt
    WorkersReveal,   // workers panel appeared, ask about telegram
    TelegramToken,   // waiting for bot token
    TelegramChatId,  // waiting for chat id
    TelegramTopicId, // waiting for topic id (optional)
    HeartbeatReveal, // heartbeat panel appeared, ask about github
    GithubConfirm,   // showing discovered repos
    Complete,        // full dashboard, write final config
}

// ── Chat message model ───────────────────────────────────

#[derive(Debug, Clone)]
enum ChatMsg {
    Bee(String),
    User(String),
}

// ── Token validation ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenStatus {
    Empty,
    Validating,
    Valid,
    Invalid,
}

// ── Onboarding state ─────────────────────────────────────

struct OnboardingState {
    stage: Stage,
    messages: Vec<ChatMsg>,
    input: String,
    // Config values collected
    workspace_name: String,
    root_dir: PathBuf,
    bot_token: String,
    chat_id: String,
    topic_id: String,
    token_status: TokenStatus,
    token_bot_name: String,
    last_token_change: Option<Instant>,
    github_repos: Vec<String>,
    telegram_skipped: bool,
    github_skipped: bool,
    // Common
    needs_redraw: bool,
    quit: bool,
    done: bool,
}

impl OnboardingState {
    fn new(initial_name: Option<&str>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let name = initial_name.map(|s| s.to_string()).unwrap_or_else(|| {
            cwd.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string()
        });

        let mut state = Self {
            stage: Stage::Greeting,
            messages: Vec::new(),
            input: String::new(),
            workspace_name: name.clone(),
            root_dir: cwd,
            bot_token: String::new(),
            chat_id: String::new(),
            topic_id: String::new(),
            token_status: TokenStatus::Empty,
            token_bot_name: String::new(),
            last_token_change: None,
            github_repos: Vec::new(),
            telegram_skipped: false,
            github_skipped: false,
            needs_redraw: true,
            quit: false,
            done: false,
        };

        // Initial greeting
        state.bee(format!(
            "Hey! I'm Bee \u{2014} your dev workspace coordinator.\n\
             I watch your GitHub, manage AI workers, and keep\n\
             you in the loop when you step away.\n\
             \n\
             Let's get you set up. What should I call this\n\
             workspace? (or press enter to use \"{}\")",
            name
        ));

        state
    }

    fn bee(&mut self, msg: impl Into<String>) {
        self.messages.push(ChatMsg::Bee(msg.into()));
    }

    fn user(&mut self, msg: impl Into<String>) {
        self.messages.push(ChatMsg::User(msg.into()));
    }

    /// Write config file with whatever has been collected so far.
    fn write_config(&self) -> Result<String> {
        let name = match sanitize_workspace_name(&self.workspace_name) {
            Some(n) => n,
            None => {
                return Err(color_eyre::eyre::eyre!(
                    "Invalid workspace name: must not contain path separators"
                ));
            }
        };
        let dir = config::workspaces_dir();
        std::fs::create_dir_all(&dir)?;
        let config_path = dir.join(format!("{name}.toml"));

        let telegram = if !self.telegram_skipped
            && !self.bot_token.trim().is_empty()
            && !self.chat_id.trim().is_empty()
        {
            let chat_id: i64 = self.chat_id.trim().parse().unwrap_or(0);
            let topic_id = if self.topic_id.trim().is_empty() {
                None
            } else {
                self.topic_id.trim().parse::<i64>().ok()
            };
            Some(config::TelegramConfig {
                bot_token: self.bot_token.trim().to_string(),
                chat_id,
                topic_id,
                allowed_user_ids: vec![],
            })
        } else {
            None
        };

        let github = if !self.github_skipped && !self.github_repos.is_empty() {
            Some(config::GithubWatcherConfig {
                repos: self.github_repos.clone(),
                interval_secs: 120,
                review_queue: vec![],
            })
        } else {
            None
        };

        let mut watchers = config::WatchersConfig {
            github,
            ..config::WatchersConfig::default()
        };

        // Auto-detect swarm
        let swarm_available = std::process::Command::new("which")
            .arg("swarm")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if swarm_available {
            let state_path = self.root_dir.join(".swarm/state.json");
            watchers.swarm = Some(config::SwarmWatcherConfig {
                state_path,
                interval_secs: 15,
            });
        }

        let coordinator_name = "Bee";
        let default_prompt = crate::buzz::coordinator::prompt::default_preamble(coordinator_name);

        let ws_config = config::WorkspaceConfig {
            root: self.root_dir.clone(),
            repos: self.github_repos.clone(),
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
}

// ── Validation helpers ───────────────────────────────────

fn sanitize_workspace_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    if name.chars().all(|c| c == '.') {
        return None;
    }
    Some(name)
}

fn is_yes(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "yes" | "y")
}

fn is_no(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "no" | "n" | "skip" | "s")
}

// ── Public entry point ───────────────────────────────────

pub struct OnboardingResult {
    pub workspace_name: Option<String>,
    pub launch_ui: bool,
}

pub async fn run_onboarding(initial_name: Option<&str>) -> Result<OnboardingResult> {
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

    let result = onboarding_loop(&mut terminal, initial_name).await;

    disable_raw_mode()?;
    stdout().execute(crossterm::event::DisableMouseCapture)?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

// ── Main loop ────────────────────────────────────────────

async fn onboarding_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    initial_name: Option<&str>,
) -> Result<OnboardingResult> {
    let mut state = OnboardingState::new(initial_name);
    let mut events = EventStream::new();
    let mut debounce_interval = tokio::time::interval(std::time::Duration::from_millis(200));
    debounce_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if state.needs_redraw {
            terminal.draw(|f| draw_onboarding(f, &state))?;
            state.needs_redraw = false;
        }

        if state.quit {
            return Ok(OnboardingResult {
                workspace_name: None,
                launch_ui: false,
            });
        }

        if state.done {
            let name = state.write_config()?;
            return Ok(OnboardingResult {
                workspace_name: Some(name),
                launch_ui: true,
            });
        }

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        handle_key(&mut state, key).await;
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

async fn handle_key(state: &mut OnboardingState, key: crossterm::event::KeyEvent) {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.quit = true;
        state.needs_redraw = true;
        return;
    }

    // Esc = skip everything, use defaults
    if key.code == KeyCode::Esc {
        skip_to_end(state);
        state.needs_redraw = true;
        return;
    }

    match key.code {
        KeyCode::Enter => {
            handle_enter(state).await;
        }
        KeyCode::Backspace => {
            state.input.pop();
        }
        KeyCode::Char(c) => {
            state.input.push(c);
            // Debounce token validation — sync input to bot_token so the
            // debounce tick validates the current value.
            if state.stage == Stage::TelegramToken && state.input.len() > 10 {
                state.bot_token = state.input.clone();
                state.token_status = TokenStatus::Validating;
                state.last_token_change = Some(Instant::now());
            }
        }
        _ => {}
    }
    state.needs_redraw = true;
}

async fn handle_enter(state: &mut OnboardingState) {
    let input = state.input.trim().to_string();
    state.input.clear();

    match state.stage {
        Stage::Greeting => {
            let name = if input.is_empty() {
                state.workspace_name.clone()
            } else {
                input.clone()
            };
            if sanitize_workspace_name(&name).is_none() {
                state.bee(
                    "That name won't work \u{2014} no slashes or special characters. Try again:",
                );
                return;
            }
            state.workspace_name = name.clone();
            if !input.is_empty() {
                state.user(input);
            } else {
                state.user(name.clone());
            }

            // Write initial config
            try_save(state);

            state.bee(
                "Nice. I've created your workspace config.\n\
                 On the left you'll see your Workers panel \u{2014}\n\
                 that's where swarm agents will appear when\n\
                 you dispatch coding tasks.\n\
                 \n\
                 Want to connect Telegram so I can reach you\n\
                 when you're away? (yes / skip)",
            );
            state.stage = Stage::WorkersReveal;
        }

        Stage::WorkersReveal => {
            if !input.is_empty() {
                state.user(input.clone());
            }
            if is_yes(&input) {
                state.bee(
                    "Message @BotFather on Telegram \u{2192} /newbot\n\
                     and paste your bot token here:",
                );
                state.stage = Stage::TelegramToken;
            } else if is_no(&input) {
                state.telegram_skipped = true;
                try_save(state);
                advance_to_heartbeat(state);
            } else {
                state.bee("Just type 'yes' or 'skip':");
            }
        }

        Stage::TelegramToken => {
            if input.is_empty() {
                state.bee("I need a bot token to continue. Paste it here, or type 'skip':");
                return;
            }
            if is_no(&input) {
                state.user(input);
                state.telegram_skipped = true;
                try_save(state);
                advance_to_heartbeat(state);
                return;
            }
            state.user(input.clone());
            state.bot_token = input;
            // Validate inline
            validate_bot_token(state).await;
            match state.token_status {
                TokenStatus::Valid => {
                    state.bee(format!(
                        "\u{2713} Connected! I'm @{}.\n\
                         Now your chat ID \u{2014} message @userinfobot\n\
                         and paste the number it gives you:",
                        state.token_bot_name
                    ));
                    state.stage = Stage::TelegramChatId;
                }
                _ => {
                    state.bee(
                        "\u{2717} That token didn't work. Try again,\n\
                         or type 'skip' to set it up later:",
                    );
                    state.bot_token.clear();
                }
            }
        }

        Stage::TelegramChatId => {
            if input.is_empty() {
                state.bee("I need a chat ID. Paste it here, or type 'skip':");
                return;
            }
            if is_no(&input) {
                state.user(input);
                state.telegram_skipped = true;
                state.bot_token.clear();
                try_save(state);
                advance_to_heartbeat(state);
                return;
            }
            state.user(input.clone());
            if input.trim().parse::<i64>().is_err() {
                state.bee("That doesn't look like a number. Try again:");
                return;
            }
            state.chat_id = input;
            state.bee(
                "Got it! Topic ID is optional \u{2014} use it if you want\n\
                 me in a specific forum thread.\n\
                 (paste it or press enter to skip)",
            );
            state.stage = Stage::TelegramTopicId;
        }

        Stage::TelegramTopicId => {
            if !input.is_empty() {
                state.user(input.clone());
                if input.trim().parse::<i64>().is_err() && !is_no(&input) {
                    state
                        .bee("That doesn't look like a number. Try again, or press enter to skip:");
                    return;
                }
                if !is_no(&input) {
                    state.topic_id = input;
                }
            }
            // Write telegram config
            try_save(state);
            advance_to_heartbeat(state);
        }

        Stage::HeartbeatReveal => {
            if !input.is_empty() {
                state.user(input.clone());
            }
            if is_yes(&input) {
                // Auto-discover repos
                let repos = config::resolve_repos(&config::WorkspaceConfig {
                    root: state.root_dir.clone(),
                    repos: vec![],
                    telegram: None,
                    coordinator: config::CoordinatorConfig::default(),
                    watchers: config::WatchersConfig::default(),
                    pipeline: config::PipelineConfig::default(),
                    commands: vec![],
                    morning_brief: None,
                    daemon_tcp_port: None,
                    daemon_tcp_bind: None,
                    daemon_host: None,
                    daemon_port: None,
                    daemon_endpoints: vec![],
                });
                state.github_repos = repos.clone();
                if repos.is_empty() {
                    state.bee(
                        "I didn't find any repos under your workspace root.\n\
                         You can add them later in your config.\n\
                         \n\
                         I'll watch for PR reviews, CI failures, and\n\
                         release completions. \u{2713}",
                    );
                } else {
                    let repo_list: Vec<&str> = repos
                        .iter()
                        .map(|r| r.rsplit('/').next().unwrap_or(r.as_str()))
                        .collect();
                    state.bee(format!(
                        "I'll auto-discover repos under your workspace\n\
                         root. I detected: {}.\n\
                         \n\
                         I'll watch for PR reviews, CI failures, and\n\
                         release completions. \u{2713}",
                        repo_list.join(", ")
                    ));
                }
                try_save(state);
                state.stage = Stage::GithubConfirm;
            } else if is_no(&input) {
                state.github_skipped = true;
                try_save(state);
                state.stage = Stage::GithubConfirm;
            } else {
                state.bee("Just type 'yes' or 'skip':");
            }
        }

        Stage::GithubConfirm => {
            if !input.is_empty() {
                state.user(input);
            }
            advance_to_complete(state);
        }

        Stage::Complete => {
            state.done = true;
        }
    }
}

fn advance_to_heartbeat(state: &mut OnboardingState) {
    state.bee(
        "This is your Heartbeat \u{2014} it shows recent\n\
         signals from GitHub, Sentry, Linear, and\n\
         anything else you connect.\n\
         \n\
         Want to connect GitHub? I can watch your\n\
         PRs and CI runs. (yes / skip)",
    );
    state.stage = Stage::HeartbeatReveal;
}

fn advance_to_complete(state: &mut OnboardingState) {
    state.bee(
        "You're all set. \u{1f41d}\n\
         \n\
         This is your full dashboard. Signals from\n\
         your watchers appear on the right, workers\n\
         on the left, and I'm always here in chat.\n\
         \n\
         Press enter to launch the dashboard.",
    );
    state.stage = Stage::Complete;
}

/// Try to write config; show error via Bee if it fails.
fn try_save(state: &mut OnboardingState) {
    if let Err(e) = state.write_config() {
        state.bee(format!("Warning: couldn't save config: {e}"));
    }
}

fn skip_to_end(state: &mut OnboardingState) {
    state.telegram_skipped = true;
    state.github_skipped = true;
    state.bee(
        "All set with defaults. Add integrations\n\
         anytime via /settings.",
    );
    try_save(state);
    state.done = true;
}

// ── Token validation ─────────────────────────────────────

async fn validate_bot_token(state: &mut OnboardingState) {
    let token = state.bot_token.clone();
    if token.trim().is_empty() {
        state.token_status = TokenStatus::Empty;
        return;
    }
    state.token_status = TokenStatus::Validating;

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            state.token_status = TokenStatus::Invalid;
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
                    .unwrap_or("bot")
                    .to_string();
                state.token_status = TokenStatus::Valid;
                state.token_bot_name = bot_name;
                return;
            }
            state.token_status = TokenStatus::Invalid;
        }
        _ => {
            state.token_status = TokenStatus::Invalid;
        }
    }
}

// ── Rendering ────────────────────────────────────────────

fn draw_onboarding(frame: &mut Frame, state: &OnboardingState) {
    let size = frame.area();

    match state.stage {
        // Full-screen chat
        Stage::Greeting => {
            draw_fullscreen_chat(frame, state, size);
        }
        // Workers panel + chat
        Stage::WorkersReveal
        | Stage::TelegramToken
        | Stage::TelegramChatId
        | Stage::TelegramTopicId => {
            draw_two_panel(frame, state, size);
        }
        // Workers + chat + heartbeat
        Stage::HeartbeatReveal | Stage::GithubConfirm | Stage::Complete => {
            draw_three_panel(frame, state, size);
        }
    }
}

// ── Full-screen chat layout ──────────────────────────────

fn draw_fullscreen_chat(frame: &mut Frame, state: &OnboardingState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // chat
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(frame, outer[0]);
    draw_chat_panel(frame, state, outer[1], true);
    draw_footer(frame, state, outer[2]);
}

// ── Two-panel layout (workers | chat) ────────────────────

fn draw_two_panel(frame: &mut Frame, state: &OnboardingState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(frame, outer[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30), // workers
            Constraint::Percentage(70), // chat
        ])
        .split(outer[1]);

    draw_workers_panel(frame, cols[0]);
    draw_chat_panel(frame, state, cols[1], true);
    draw_footer(frame, state, outer[2]);
}

// ── Three-panel layout (workers | chat | heartbeat) ──────

fn draw_three_panel(frame: &mut Frame, state: &OnboardingState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(frame, outer[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25), // workers
            Constraint::Percentage(50), // chat
            Constraint::Percentage(25), // heartbeat
        ])
        .split(outer[1]);

    draw_workers_panel(frame, cols[0]);
    draw_chat_panel(frame, state, cols[1], true);
    draw_heartbeat_panel(frame, cols[2]);
    draw_footer(frame, state, outer[2]);
}

// ── Header ───────────────────────────────────────────────

fn draw_header(frame: &mut Frame, area: Rect) {
    let spans = vec![
        Span::styled(" * ", theme::logo()),
        Span::styled("apiari", theme::title()),
    ];
    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::COMB));
    frame.render_widget(bar, area);
}

// ── Footer ───────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, state: &OnboardingState, area: Rect) {
    let hint = match state.stage {
        Stage::Complete => "[Enter] launch dashboard  [Esc] skip",
        _ => "[Enter] confirm  [Esc] skip all & use defaults",
    };
    let footer = Paragraph::new(Line::from(Span::styled(hint, theme::key_desc())))
        .alignment(Alignment::Center);
    frame.render_widget(footer, area);
}

// ── Workers panel (placeholder) ──────────────────────────

fn draw_workers_panel(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(" Workers ", theme::accent()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let placeholder = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled("  No workers yet", theme::muted())),
        Line::from(""),
        Line::from(Span::styled("  Workers will appear here", theme::muted())),
        Line::from(Span::styled("  when you dispatch tasks.", theme::muted())),
    ]);
    frame.render_widget(placeholder, inner);
}

// ── Heartbeat panel (placeholder) ────────────────────────

fn draw_heartbeat_panel(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(" Signals ", theme::accent()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let placeholder = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled("  No signals yet", theme::muted())),
        Line::from(""),
        Line::from(Span::styled("  Signals from GitHub,", theme::muted())),
        Line::from(Span::styled("  Sentry, and other", theme::muted())),
        Line::from(Span::styled("  watchers appear here.", theme::muted())),
    ]);
    frame.render_widget(placeholder, inner);
}

// ── Chat panel ───────────────────────────────────────────

fn draw_chat_panel(frame: &mut Frame, state: &OnboardingState, area: Rect, focused: bool) {
    let border_style = if focused {
        theme::border_active()
    } else {
        theme::border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(" Bee ", theme::accent()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner: messages area + input line
    let chat_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // messages
            Constraint::Length(1), // input
        ])
        .split(inner);

    // Render messages
    draw_messages(frame, state, chat_layout[0]);

    // Render input line
    draw_input(frame, state, chat_layout[1]);
}

fn draw_messages(frame: &mut Frame, state: &OnboardingState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &state.messages {
        match msg {
            ChatMsg::Bee(text) => {
                lines.push(Line::from(""));
                for line in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(line.to_string(), Style::default().fg(theme::HONEY)),
                    ]));
                }
            }
            ChatMsg::User(text) => {
                lines.push(Line::from(""));
                for line in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("  > ", theme::muted()),
                        Span::styled(line.to_string(), theme::text()),
                    ]));
                }
            }
        }
    }

    // Show token validation status inline during token entry
    if state.stage == Stage::TelegramToken && !state.input.is_empty() {
        match state.token_status {
            TokenStatus::Validating => {
                lines.push(Line::from(Span::styled(
                    "    validating...",
                    theme::muted(),
                )));
            }
            TokenStatus::Valid => {
                lines.push(Line::from(Span::styled(
                    format!("    \u{2713} @{}", state.token_bot_name),
                    theme::success(),
                )));
            }
            TokenStatus::Invalid => {
                lines.push(Line::from(Span::styled(
                    "    \u{2717} invalid token",
                    theme::error(),
                )));
            }
            TokenStatus::Empty => {}
        }
    }

    // Scroll: show last N lines that fit
    let available_height = area.height as usize;
    let total_lines = lines.len();
    let skip = total_lines.saturating_sub(available_height);
    let visible_lines: Vec<Line> = lines.into_iter().skip(skip).collect();

    let messages_widget = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
    frame.render_widget(messages_widget, area);
}

fn draw_input(frame: &mut Frame, state: &OnboardingState, area: Rect) {
    let cursor = "\u{2588}";
    let display = format!("  > {}{}", state.input, cursor);
    let input_widget = Paragraph::new(Line::from(vec![Span::styled(display, theme::text())]));
    frame.render_widget(input_widget, area);
}

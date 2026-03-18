//! In-TUI settings screen — editable workspace config form.
//!
//! Accessible via `/settings` chat command or `s` key when not typing.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::theme;
use crate::config::{self, WorkspaceConfig};

// ── Settings state ───────────────────────────────────────

/// Editable settings fields extracted from WorkspaceConfig.
pub struct SettingsState {
    pub active: bool,
    pub dirty: bool,
    pub focused_field: usize,
    pub confirm_save: bool,
    // Fields
    pub workspace_name: String,
    pub root: String,
    pub bot_token: String,
    pub chat_id: String,
    pub topic_id: String,
    pub coordinator_model: String,
    pub coordinator_max_turns: String,
    // GitHub
    #[allow(dead_code)]
    pub github_enabled: bool,
    pub github_repos: String,
    // Sentry
    #[allow(dead_code)]
    pub sentry_enabled: bool,
    pub sentry_org: String,
    pub sentry_project: String,
    pub sentry_token: String,
    // Linear
    #[allow(dead_code)]
    pub linear_enabled: bool,
    pub linear_name: String,
    pub linear_api_key: String,
    // Swarm
    #[allow(dead_code)]
    pub swarm_enabled: bool,
    pub swarm_state_path: String,
}

const FIELD_COUNT: usize = 17;

impl SettingsState {
    /// Create settings from current workspace config.
    pub fn from_workspace(name: &str, config: &WorkspaceConfig) -> Self {
        Self {
            active: true,
            dirty: false,
            focused_field: 0,
            confirm_save: false,
            workspace_name: name.to_string(),
            root: config.root.to_string_lossy().to_string(),
            bot_token: config
                .telegram
                .as_ref()
                .map(|t| t.bot_token.clone())
                .unwrap_or_default(),
            chat_id: config
                .telegram
                .as_ref()
                .map(|t| t.chat_id.to_string())
                .unwrap_or_default(),
            topic_id: config
                .telegram
                .as_ref()
                .and_then(|t| t.topic_id.map(|id| id.to_string()))
                .unwrap_or_default(),
            coordinator_model: config.coordinator.model.clone(),
            coordinator_max_turns: config.coordinator.max_turns.to_string(),
            github_enabled: config.watchers.github.is_some(),
            github_repos: config
                .watchers
                .github
                .as_ref()
                .map(|g| g.repos.join(", "))
                .unwrap_or_default(),
            sentry_enabled: config.watchers.sentry.is_some(),
            sentry_org: config
                .watchers
                .sentry
                .as_ref()
                .map(|s| s.org.clone())
                .unwrap_or_default(),
            sentry_project: config
                .watchers
                .sentry
                .as_ref()
                .map(|s| s.project.clone())
                .unwrap_or_default(),
            sentry_token: config
                .watchers
                .sentry
                .as_ref()
                .map(|s| s.token.clone())
                .unwrap_or_default(),
            linear_enabled: !config.watchers.linear.is_empty(),
            linear_name: config
                .watchers
                .linear
                .first()
                .map(|l| l.name.clone())
                .unwrap_or_default(),
            linear_api_key: config
                .watchers
                .linear
                .first()
                .map(|l| l.api_key.clone())
                .unwrap_or_default(),
            swarm_enabled: config.watchers.swarm.is_some(),
            swarm_state_path: config
                .watchers
                .swarm
                .as_ref()
                .map(|s| s.state_path.to_string_lossy().to_string())
                .unwrap_or_default(),
        }
    }

    /// Handle a key event. Returns true if the settings screen should close.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;

        if self.confirm_save {
            match key.code {
                KeyCode::Char('y') => {
                    if let Err(e) = self.save() {
                        tracing::error!("failed to save settings: {e}");
                    }
                    self.active = false;
                    return true;
                }
                KeyCode::Char('n') => {
                    self.active = false;
                    return true;
                }
                KeyCode::Esc => {
                    self.confirm_save = false;
                }
                _ => {}
            }
            return false;
        }

        match key.code {
            KeyCode::Esc => {
                if self.dirty {
                    self.confirm_save = true;
                } else {
                    self.active = false;
                    return true;
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                self.focused_field = (self.focused_field + 1) % FIELD_COUNT;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focused_field = if self.focused_field == 0 {
                    FIELD_COUNT - 1
                } else {
                    self.focused_field - 1
                };
            }
            KeyCode::Backspace => {
                let field = self.current_field_mut();
                field.pop();
                self.dirty = true;
            }
            KeyCode::Char(c) => {
                let field = self.current_field_mut();
                field.push(c);
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn current_field_mut(&mut self) -> &mut String {
        match self.focused_field {
            0 => &mut self.root,
            1 => &mut self.bot_token,
            2 => &mut self.chat_id,
            3 => &mut self.topic_id,
            4 => &mut self.coordinator_model,
            5 => &mut self.coordinator_max_turns,
            6 => &mut self.github_repos,
            7 => &mut self.sentry_org,
            8 => &mut self.sentry_project,
            9 => &mut self.sentry_token,
            10 => &mut self.linear_name,
            11 => &mut self.linear_api_key,
            12 => &mut self.swarm_state_path,
            _ => &mut self.root, // fallback
        }
    }

    /// Save settings back to TOML.
    fn save(&self) -> color_eyre::Result<()> {
        let config_path = config::workspaces_dir().join(format!("{}.toml", self.workspace_name));
        if !config_path.exists() {
            return Ok(());
        }

        // Read existing config, update fields, write back
        let contents = std::fs::read_to_string(&config_path)?;
        let mut config: WorkspaceConfig = toml::from_str(&contents)?;

        config.root = self.root.trim().into();

        // Telegram
        if !self.bot_token.trim().is_empty() && !self.chat_id.trim().is_empty() {
            config.telegram = Some(config::TelegramConfig {
                bot_token: self.bot_token.trim().to_string(),
                chat_id: self.chat_id.trim().parse().unwrap_or(0),
                topic_id: if self.topic_id.trim().is_empty() {
                    None
                } else {
                    self.topic_id.trim().parse().ok()
                },
                allowed_user_ids: config
                    .telegram
                    .as_ref()
                    .map(|t| t.allowed_user_ids.clone())
                    .unwrap_or_default(),
            });
        } else {
            config.telegram = None;
        }

        // Coordinator
        config.coordinator.model = self.coordinator_model.trim().to_string();
        if let Ok(turns) = self.coordinator_max_turns.trim().parse() {
            config.coordinator.max_turns = turns;
        }

        // Serialize with toml
        let toml_str = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, toml_str)?;

        Ok(())
    }
}

// ── Rendering ────────────────────────────────────────────

/// Draw the settings screen as a full-screen overlay.
pub fn draw_settings(frame: &mut Frame, state: &SettingsState, area: Rect) {
    // Center with max width
    let max_w = 70u16.min(area.width.saturating_sub(4));
    let h_pad = (area.width.saturating_sub(max_w)) / 2;
    let content_area = Rect::new(area.x + h_pad, area.y, max_w, area.height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_active())
        .title(Span::styled(
            format!(" Settings \u{2014} {} ", state.workspace_name),
            theme::title(),
        ));

    let inner = block.inner(content_area);
    frame.render_widget(ratatui::widgets::Clear, content_area);
    frame.render_widget(block, content_area);

    // Layout fields
    let fields: Vec<(&str, &str, usize)> = vec![
        ("Root directory", &state.root, 0),
        ("Telegram: bot_token", &state.bot_token, 1),
        ("Telegram: chat_id", &state.chat_id, 2),
        ("Telegram: topic_id", &state.topic_id, 3),
        ("Coordinator: model", &state.coordinator_model, 4),
        ("Coordinator: max_turns", &state.coordinator_max_turns, 5),
        ("GitHub: repos", &state.github_repos, 6),
        ("Sentry: org", &state.sentry_org, 7),
        ("Sentry: project", &state.sentry_project, 8),
        ("Sentry: token", &state.sentry_token, 9),
        ("Linear: name", &state.linear_name, 10),
        ("Linear: api_key", &state.linear_api_key, 11),
        ("Swarm: state_path", &state.swarm_state_path, 12),
    ];

    let mut constraints: Vec<Constraint> = vec![Constraint::Length(1)]; // top pad
    for _ in &fields {
        constraints.push(Constraint::Length(3));
    }
    constraints.push(Constraint::Length(1)); // spacing
    constraints.push(Constraint::Length(1)); // hint
    constraints.push(Constraint::Min(0));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, (label, value, field_idx)) in fields.iter().enumerate() {
        let focused = state.focused_field == *field_idx;
        draw_settings_field(frame, chunks[i + 1], label, value, focused);
    }

    // Hint at bottom
    let hint_idx = fields.len() + 2;
    if hint_idx < chunks.len() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "[Tab/\u{2191}\u{2193}] navigate  [Esc] close  Changes save on exit",
            theme::key_desc(),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(hint, chunks[hint_idx]);
    }

    // Save confirmation overlay
    if state.confirm_save {
        let overlay_w = 40u16.min(area.width);
        let overlay_h = 5;
        let ox = area.x + (area.width.saturating_sub(overlay_w)) / 2;
        let oy = area.y + (area.height.saturating_sub(overlay_h)) / 2;
        let overlay = Rect::new(ox, oy, overlay_w, overlay_h);

        frame.render_widget(ratatui::widgets::Clear, overlay);
        let confirm_block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_active())
            .title(Span::styled(" Unsaved changes ", theme::accent()));
        let confirm_inner = confirm_block.inner(overlay);
        frame.render_widget(confirm_block, overlay);

        let confirm_text = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "Save changes? [y]es  [n]o  [Esc] cancel",
                theme::text(),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(confirm_text, confirm_inner);
    }
}

fn draw_settings_field(frame: &mut Frame, area: Rect, label: &str, value: &str, focused: bool) {
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
    let content = Paragraph::new(Line::from(Span::styled(display, theme::text())));
    frame.render_widget(content, inner);
}

//! TUI event loop for the keeper dashboard.

pub mod app;
pub mod render;
pub mod theme;

use app::App;
use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use std::io::stdout;
use std::time::Duration;

use crate::keeper::discovery::DiscoveryResult;

/// Launch the TUI dashboard.
pub async fn run(result: DiscoveryResult) -> Result<()> {
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut app = App::new(result);
    let result = event_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render::draw(frame, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            // Ctrl+C always quits.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }

            // If an overlay is active, handle overlay-specific keys first.
            if app.has_overlay() {
                match key.code {
                    KeyCode::Char('?') => app.toggle_help(),
                    KeyCode::Char('e') => {
                        if app.overlay == app::Overlay::EventDetail {
                            app.dismiss_overlay();
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => app.dismiss_overlay(),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('j') | KeyCode::Down => app.select_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
                    KeyCode::Enter => app.jump_to_selected(),
                    KeyCode::Char('r') => app.refresh(),
                    KeyCode::Tab | KeyCode::BackTab => app.toggle_panel(),
                    KeyCode::Char('?') => app.toggle_help(),
                    KeyCode::Char('e') => app.toggle_event_detail(),
                    KeyCode::Esc => {
                        // Esc moves focus back to sessions panel
                        if app.panel == app::Panel::Worktrees {
                            app.panel = app::Panel::Sessions;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Periodic refresh (every 5s for sessions, 30s for PRs).
        app.tick();
    }

    Ok(())
}

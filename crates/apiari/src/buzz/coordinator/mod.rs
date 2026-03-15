//! Buzz coordinator — Claude SDK wrapper with signal awareness.
//!
//! The coordinator maintains a Claude session with a system prompt that
//! includes open signals and accumulated memory. It handles user messages
//! and can proactively notify about signal changes.

pub mod audit;
pub mod memory;
pub mod prompt;
pub mod skills;

use std::path::PathBuf;

use color_eyre::eyre::Result;
use tracing::{info, warn};

use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};

use crate::buzz::signal::store::SignalStore;

/// Structured events emitted by the coordinator during a turn.
#[derive(Debug, Clone)]
pub enum CoordinatorEvent {
    /// A chunk of assistant text (replaces on_token).
    Token(String),
    /// Coordinator invoked a Bash command classified as mutating.
    BashAudit {
        command: String,
        matched_pattern: String,
    },
    /// Post-turn: workspace files were newly modified.
    FilesModified { files: Vec<(String, String)> },
}

/// Lets callers inject workspace safety checks without buzz depending on git.
///
/// The coordinator calls `pre_turn()` before the event loop and `post_turn()`
/// after. The opaque `Box<dyn Any>` snapshot avoids infecting the coordinator
/// struct with a type parameter.
pub trait SafetyHooks: Send {
    /// Capture a snapshot before the coordinator turn.
    fn pre_turn(&self) -> Option<Box<dyn std::any::Any + Send>>;
    /// Compare current state against the snapshot; return newly modified (repo, file) pairs.
    fn post_turn(&self, snapshot: Box<dyn std::any::Any + Send>) -> Vec<(String, String)>;
}

/// The buzz coordinator — manages Claude sessions with signal context.
pub struct Coordinator {
    name: String,
    model: String,
    max_turns: u32,
    session_id: Option<String>,
    extra_context: Option<String>,
    prompt_preamble: Option<String>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    working_dir: Option<PathBuf>,
    settings: Option<String>,
    safety_hooks: Option<Box<dyn SafetyHooks>>,
}

impl Coordinator {
    pub fn new(model: &str, max_turns: u32) -> Self {
        Self {
            name: "Bee".to_string(),
            model: model.to_string(),
            max_turns,
            session_id: None,
            extra_context: None,
            prompt_preamble: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            working_dir: None,
            settings: None,
            safety_hooks: None,
        }
    }

    /// Set the coordinator's name (used in system prompt).
    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// Set extra context to include in the system prompt (e.g. skills prompt).
    pub fn set_extra_context(&mut self, context: String) {
        self.extra_context = Some(context);
    }

    /// Set a custom prompt preamble (replaces default identity/role sections).
    pub fn set_prompt_preamble(&mut self, preamble: String) {
        self.prompt_preamble = Some(preamble);
    }

    /// Set the tools the coordinator is allowed to use (auto-approve, not a restriction).
    pub fn set_tools(&mut self, tools: Vec<String>) {
        self.allowed_tools = tools;
    }

    /// Set tools that are explicitly blocked (hard enforcement).
    pub fn set_disallowed_tools(&mut self, tools: Vec<String>) {
        self.disallowed_tools = tools;
    }

    /// Set the working directory for the coordinator session.
    pub fn set_working_dir(&mut self, dir: PathBuf) {
        self.working_dir = Some(dir);
    }

    /// Set custom settings JSON (e.g. PreToolUse hooks).
    pub fn set_settings(&mut self, settings: String) {
        self.settings = Some(settings);
    }

    /// Install safety hooks for pre/post-turn workspace checks.
    pub fn set_safety_hooks(&mut self, hooks: Box<dyn SafetyHooks>) {
        self.safety_hooks = Some(hooks);
    }

    /// Whether the coordinator has been used (has a persistent session).
    pub fn has_session(&self) -> bool {
        self.session_id.is_some()
    }

    /// Get current max turns.
    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    /// Temporarily change max turns (e.g. for system notifications).
    pub fn set_max_turns(&mut self, turns: u32) {
        self.max_turns = turns;
    }

    /// Check if the Claude CLI is available.
    pub async fn is_available() -> bool {
        tokio::process::Command::new("which")
            .arg("claude")
            .output()
            .await
            .is_ok_and(|o| o.status.success())
    }

    /// Build session options with current signal context.
    pub fn build_options(&self, store: &SignalStore) -> Result<SessionOptions> {
        let signals = store.get_open_signals()?;

        let system_prompt = prompt::build_system_prompt(
            &signals,
            &[],
            self.extra_context.as_deref(),
            Some(&self.name),
            self.prompt_preamble.as_deref(),
        );

        let mut opts = SessionOptions {
            system_prompt: Some(system_prompt),
            max_turns: Some(self.max_turns as u64),
            model: Some(self.model.clone()),
            allowed_tools: self.allowed_tools.clone(),
            disallowed_tools: self.disallowed_tools.clone(),
            working_dir: self.working_dir.clone(),
            settings: self.settings.clone(),
            ..Default::default()
        };

        if let Some(ref session_id) = self.session_id {
            opts.resume = Some(session_id.clone());
        }

        Ok(opts)
    }

    /// Extract structured events from an SDK event.
    fn process_event(&mut self, event: &Event) -> Vec<CoordinatorEvent> {
        let mut events = Vec::new();

        match event {
            Event::Assistant { message, tool_uses } => {
                // Audit Bash tool uses
                for tool_use in tool_uses {
                    if tool_use.name == "Bash"
                        && let Some(command) =
                            tool_use.input.get("command").and_then(|v| v.as_str())
                    {
                        let classification = audit::classify_bash_command(command);
                        match &classification {
                            audit::BashClassification::ReadOnly => {
                                info!("[coordinator] bash (read-only): {}", truncate_cmd(command));
                            }
                            audit::BashClassification::PotentiallyMutating { matched_pattern } => {
                                warn!(
                                    "[coordinator] bash MUTATING ({}): {}",
                                    matched_pattern,
                                    truncate_cmd(command)
                                );
                                events.push(CoordinatorEvent::BashAudit {
                                    command: command.to_string(),
                                    matched_pattern: matched_pattern.clone(),
                                });
                            }
                        }
                    }
                }

                let mut text = String::new();
                for block in &message.message.content {
                    if let ContentBlock::Text { text: t } = block {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    events.push(CoordinatorEvent::Token(text));
                }
            }
            Event::Result(result) => {
                self.session_id = Some(result.session_id.clone());
            }
            _ => {}
        }

        events
    }

    /// Handle a user message with structured event callbacks.
    ///
    /// Emits `CoordinatorEvent::Token` for text chunks, `BashAudit` for mutating
    /// commands, and `FilesModified` (via safety hooks) after the turn completes.
    pub async fn handle_message<F>(
        &mut self,
        message: &str,
        store: &SignalStore,
        on_event: F,
    ) -> Result<String>
    where
        F: FnMut(CoordinatorEvent),
    {
        let opts = self.build_options(store)?;
        self.handle_message_with_options(message, opts, on_event)
            .await
    }

    /// Handle a message with pre-built session options.
    ///
    /// Use this instead of `handle_message` when the `&SignalStore` reference
    /// cannot be held across await points (e.g. in spawned tasks where
    /// `SignalStore` is not `Sync`).
    pub async fn handle_message_with_options<F>(
        &mut self,
        message: &str,
        opts: SessionOptions,
        mut on_event: F,
    ) -> Result<String>
    where
        F: FnMut(CoordinatorEvent),
    {
        let client = ClaudeClient::new();
        let mut session = client.spawn(opts).await?;
        session.send_message(message).await?;

        // Pre-turn snapshot
        let snapshot = self
            .safety_hooks
            .as_ref()
            .and_then(|hooks| hooks.pre_turn());

        let mut response = String::new();

        while let Ok(Some(event)) = session.next_event().await {
            for coord_event in self.process_event(&event) {
                if let CoordinatorEvent::Token(ref t) = coord_event {
                    response.push_str(t);
                }
                on_event(coord_event);
            }
            if event.is_result() {
                break;
            }
        }

        // Post-turn: check for file modifications via safety hooks
        if let Some(snapshot) = snapshot
            && let Some(hooks) = &self.safety_hooks
        {
            let modified = hooks.post_turn(snapshot);
            if !modified.is_empty() {
                on_event(CoordinatorEvent::FilesModified { files: modified });
            }
        }

        Ok(response)
    }

    /// Convenience wrapper: handle a message ignoring all events.
    pub async fn handle_message_simple(
        &mut self,
        message: &str,
        store: &SignalStore,
    ) -> Result<String> {
        self.handle_message(message, store, |_| {}).await
    }

    /// Reset the session (start fresh).
    pub fn reset_session(&mut self) {
        info!("coordinator session reset");
        self.session_id = None;
    }
}

/// Truncate a command string for logging.
fn truncate_cmd(cmd: &str) -> &str {
    let end = cmd.char_indices().nth(120).map_or(cmd.len(), |(i, _)| i);
    &cmd[..end]
}

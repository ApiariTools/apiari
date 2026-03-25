//! Buzz coordinator — Claude SDK wrapper with signal awareness.
//!
//! The coordinator maintains a Claude session with a system prompt that
//! includes open signals and accumulated memory. It handles user messages
//! and can proactively notify about signal changes.

pub mod audit;
pub mod devmode;
pub mod memory;
pub mod prompt;
pub mod skills;
pub mod swarm_client;

use std::path::PathBuf;

use color_eyre::eyre::Result;
use tracing::{info, warn};

use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};

use crate::buzz::conversation::SessionToken;
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
    provider: String,
    max_turns: u32,
    session_id: Option<String>,
    session_token: Option<SessionToken>,
    extra_context: Option<String>,
    prompt_preamble: Option<String>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    working_dir: Option<PathBuf>,
    settings: Option<String>,
    safety_hooks: Option<Box<dyn SafetyHooks>>,
    /// Number of turns used in the last completed session.
    last_num_turns: u64,
}

impl Coordinator {
    pub fn new(model: &str, max_turns: u32) -> Self {
        Self {
            name: "Bee".to_string(),
            model: model.to_string(),
            provider: "claude".to_string(),
            max_turns,
            session_id: None,
            session_token: None,
            extra_context: None,
            prompt_preamble: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            working_dir: None,
            settings: None,
            safety_hooks: None,
            last_num_turns: 0,
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

    /// Get the current session token (provider + resume token).
    pub fn session_token(&self) -> Option<&SessionToken> {
        self.session_token.as_ref()
    }

    /// Get the provider name (e.g. "claude").
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Restore a session from a persisted token.
    pub fn restore_session(&mut self, token: SessionToken) {
        info!(
            "restoring {} session (token: {}...)",
            token.provider,
            token.token.get(..12).unwrap_or(&token.token)
        );
        self.session_id = Some(token.token.clone());
        self.session_token = Some(token);
    }

    /// Get current disallowed tools.
    pub fn disallowed_tools(&self) -> &[String] {
        &self.disallowed_tools
    }

    /// Get the model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get current max turns.
    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    /// Temporarily change max turns (e.g. for system notifications).
    pub fn set_max_turns(&mut self, turns: u32) {
        self.max_turns = turns;
    }

    /// Number of turns used in the last completed session.
    pub fn last_num_turns(&self) -> u64 {
        self.last_num_turns
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
        self.build_options_with_playbooks(store, None)
    }

    /// Build session options with current signal context and optional hook-triggered playbooks.
    pub fn build_options_with_playbooks(
        &self,
        store: &SignalStore,
        hook_playbooks: Option<&str>,
    ) -> Result<SessionOptions> {
        let signals = store.get_open_signals()?;

        let system_prompt = prompt::build_system_prompt(
            &signals,
            &[],
            self.extra_context.as_deref(),
            Some(&self.name),
            self.prompt_preamble.as_deref(),
            hook_playbooks,
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
    ///
    /// Returns `Ok(events)` normally, or `Err(message)` if the result indicates
    /// a session error (e.g. stale resume token).
    fn process_event(
        &mut self,
        event: &Event,
    ) -> std::result::Result<Vec<CoordinatorEvent>, String> {
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
                if result.is_error {
                    // Session errored — extract error message and signal to caller.
                    // Common cause: stale resume token ("No conversation found with session ID: ...").
                    let error_detail = result
                        .result
                        .as_deref()
                        .unwrap_or("session error (no detail)");
                    warn!(
                        "[coordinator] session error (subtype={}): {}",
                        result.subtype, error_detail
                    );
                    // Don't store the broken session — caller will reset.
                    return Err(error_detail.to_string());
                }
                self.last_num_turns = result.num_turns;
                self.session_id = Some(result.session_id.clone());
                self.session_token = Some(SessionToken {
                    provider: self.provider.clone(),
                    token: result.session_id.clone(),
                });
            }
            _ => {}
        }

        Ok(events)
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
        self.last_num_turns = 0;
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
            match self.process_event(&event) {
                Ok(coord_events) => {
                    for coord_event in coord_events {
                        if let CoordinatorEvent::Token(ref t) = coord_event {
                            response.push_str(t);
                        }
                        on_event(coord_event);
                    }
                }
                Err(error_msg) => {
                    // Session error (e.g. stale resume token). Propagate to caller
                    // who will reset the session and notify the user.
                    return Err(color_eyre::eyre::eyre!("Claude session error: {error_msg}"));
                }
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
        self.session_token = None;
    }
}

/// Truncate a command string for logging.
fn truncate_cmd(cmd: &str) -> &str {
    let end = cmd.char_indices().nth(120).map_or(cmd.len(), |(i, _)| i);
    &cmd[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use apiari_claude_sdk::Event;
    use apiari_claude_sdk::types::{
        AssistantMessage, AssistantMessageContent, ContentBlock, ResultMessage,
    };
    use serde_json::Map;

    fn make_coordinator() -> Coordinator {
        Coordinator::new("sonnet", 20)
    }

    fn make_result_event(session_id: &str, is_error: bool, result_text: Option<&str>) -> Event {
        Event::Result(ResultMessage {
            subtype: if is_error {
                "error_during_execution".to_string()
            } else {
                "success".to_string()
            },
            duration_ms: 100,
            duration_api_ms: 90,
            is_error,
            num_turns: 1,
            session_id: session_id.to_string(),
            total_cost_usd: Some(0.01),
            usage: None,
            result: result_text.map(|s| s.to_string()),
            structured_output: None,
        })
    }

    fn make_assistant_event(content_blocks: Vec<ContentBlock>) -> Event {
        Event::Assistant {
            message: AssistantMessage {
                message: AssistantMessageContent {
                    model: "claude-sonnet-4-6".to_string(),
                    content: content_blocks,
                    id: Some("msg_test".to_string()),
                    role: Some("assistant".to_string()),
                    stop_reason: Some("end_turn".to_string()),
                    usage: None,
                    extra: Map::new(),
                },
                parent_tool_use_id: None,
                session_id: Some("test-session".to_string()),
                uuid: Some("test-uuid".to_string()),
            },
            tool_uses: vec![],
        }
    }

    // -- process_event: success result --

    #[test]
    fn test_process_event_success_result_stores_session() {
        let mut coord = make_coordinator();
        assert!(!coord.has_session());

        let event = make_result_event("new-session-id", false, Some("All done."));
        let result = coord.process_event(&event);

        assert!(result.is_ok());
        assert!(result.unwrap().is_empty()); // Result events emit no CoordinatorEvents
        assert!(coord.has_session());
        assert_eq!(coord.session_token().unwrap().token, "new-session-id");
    }

    // -- process_event: error result --

    #[test]
    fn test_process_event_error_result_returns_err() {
        let mut coord = make_coordinator();

        let event = make_result_event(
            "error-session-id",
            true,
            Some("No conversation found with session ID: abc-123"),
        );
        let result = coord.process_event(&event);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("No conversation found with session ID")
        );
    }

    #[test]
    fn test_process_event_error_result_does_not_store_session() {
        let mut coord = make_coordinator();
        assert!(!coord.has_session());

        let event = make_result_event("broken-session-id", true, Some("error"));
        let _ = coord.process_event(&event);

        // Session should NOT be stored for error results
        assert!(!coord.has_session());
    }

    #[test]
    fn test_process_event_error_result_no_detail() {
        let mut coord = make_coordinator();

        let event = make_result_event("err-session", true, None);
        let result = coord.process_event(&event);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("session error (no detail)"));
    }

    #[test]
    fn test_process_event_error_does_not_clobber_existing_session() {
        let mut coord = make_coordinator();
        // Simulate an existing valid session
        coord.session_id = Some("good-session".to_string());
        coord.session_token = Some(SessionToken {
            provider: "claude".to_string(),
            token: "good-session".to_string(),
        });

        let event = make_result_event("broken-session", true, Some("error"));
        let _ = coord.process_event(&event);

        // The existing session should be preserved (caller decides whether to reset)
        assert_eq!(coord.session_token().unwrap().token, "good-session");
    }

    // -- process_event: assistant text --

    #[test]
    fn test_process_event_assistant_text_emits_token() {
        let mut coord = make_coordinator();

        let event = make_assistant_event(vec![ContentBlock::Text {
            text: "Hello!".to_string(),
        }]);
        let result = coord.process_event(&event);

        let events = result.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            CoordinatorEvent::Token(t) => assert_eq!(t, "Hello!"),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn test_process_event_assistant_multiple_text_blocks() {
        let mut coord = make_coordinator();

        let event = make_assistant_event(vec![
            ContentBlock::Text {
                text: "Hello ".to_string(),
            },
            ContentBlock::Text {
                text: "world!".to_string(),
            },
        ]);
        let result = coord.process_event(&event);

        let events = result.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            CoordinatorEvent::Token(t) => assert_eq!(t, "Hello world!"),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn test_process_event_assistant_no_text_emits_nothing() {
        let mut coord = make_coordinator();

        // Thinking block only — no text
        let event = make_assistant_event(vec![ContentBlock::Thinking {
            thinking: "Let me think...".to_string(),
            signature: "sig".to_string(),
        }]);
        let result = coord.process_event(&event);

        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_process_event_assistant_tool_use_no_text() {
        let mut coord = make_coordinator();

        let event = make_assistant_event(vec![ContentBlock::ToolUse {
            id: "tool_1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/tmp/test"}),
        }]);
        let result = coord.process_event(&event);

        // No text content → no Token event
        assert!(result.unwrap().is_empty());
    }

    // -- process_event: bash audit --

    #[test]
    fn test_process_event_bash_mutating_emits_audit() {
        let mut coord = make_coordinator();

        let event = Event::Assistant {
            message: AssistantMessage {
                message: AssistantMessageContent {
                    model: "claude-sonnet-4-6".to_string(),
                    content: vec![],
                    id: None,
                    role: None,
                    stop_reason: None,
                    usage: None,
                    extra: Map::new(),
                },
                parent_tool_use_id: None,
                session_id: None,
                uuid: None,
            },
            tool_uses: vec![apiari_claude_sdk::ToolUse {
                id: "tool_1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "rm -rf src/"}),
            }],
        };
        let result = coord.process_event(&event);

        let events = result.unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, CoordinatorEvent::BashAudit { .. })),
            "expected BashAudit event for mutating command"
        );
    }

    #[test]
    fn test_process_event_bash_readonly_no_audit() {
        let mut coord = make_coordinator();

        let event = Event::Assistant {
            message: AssistantMessage {
                message: AssistantMessageContent {
                    model: "claude-sonnet-4-6".to_string(),
                    content: vec![],
                    id: None,
                    role: None,
                    stop_reason: None,
                    usage: None,
                    extra: Map::new(),
                },
                parent_tool_use_id: None,
                session_id: None,
                uuid: None,
            },
            tool_uses: vec![apiari_claude_sdk::ToolUse {
                id: "tool_1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            }],
        };
        let result = coord.process_event(&event);

        let events = result.unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, CoordinatorEvent::BashAudit { .. })),
            "read-only command should not emit BashAudit"
        );
    }

    // -- process_event: other event types --

    #[test]
    fn test_process_event_system_event_ignored() {
        let mut coord = make_coordinator();

        let event = Event::System(apiari_claude_sdk::types::SystemMessage {
            subtype: "init".to_string(),
            data: Map::new(),
        });
        let result = coord.process_event(&event);

        assert!(result.unwrap().is_empty());
        assert!(!coord.has_session());
    }

    #[test]
    fn test_process_event_rate_limit_ignored() {
        let mut coord = make_coordinator();

        let event = Event::RateLimit(apiari_claude_sdk::types::RateLimitEvent {
            rate_limit_info: None,
            uuid: None,
            session_id: None,
        });
        let result = coord.process_event(&event);

        assert!(result.unwrap().is_empty());
    }
}

//! Buzz coordinator — multi-provider LLM wrapper with signal awareness.
//!
//! Claude runs as a long-lived interactive session. Codex and Gemini are
//! resumable single-turn executions: each user message spawns a fresh process
//! and resumes provider state with a saved session token when available.
//!
//! Because those alt providers ingest the system prompt inline with the user
//! turn, we intentionally compact first-turn workspace context for them. The
//! goal is behavioral parity with Claude, not byte-for-byte prompt parity.
//! They should keep the same coordinator role and visible capabilities without
//! paying the latency cost of the full monorepo prompt on every first turn.

pub mod actions;
pub mod audit;
pub mod devmode;
pub mod memory;
pub mod prompt;
pub mod skills;
pub mod swarm_client;

use std::path::PathBuf;

use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions, types::ContentBlock};
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::buzz::{conversation::SessionToken, signal::store::SignalStore};

/// Unified token usage stats across all providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_cost_usd: Option<f64>,
}

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
    /// Token usage stats for the completed turn.
    Usage(UsageStats),
}

/// Derive the context window size from a model name.
pub fn max_context_tokens(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("gemini") {
        1_000_000
    } else {
        200_000
    }
}

/// Reduce first-turn workspace context for Codex/Gemini.
///
/// Claude can tolerate the full expanded skills prompt because it has a
/// dedicated long-lived system prompt channel. Codex and Gemini do not: their
/// first turn carries that context inline with the user message, and the full
/// monorepo prompt materially hurts latency. Keep the coordinator contract and
/// project identity, but drop the long operational manuals and workflow dump.
fn compact_alt_provider_context(extra_context: Option<&str>) -> Option<String> {
    let extra_context = extra_context?;
    let mut kept = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    let flush_section =
        |heading: &Option<String>, lines: &mut Vec<&str>, kept: &mut Vec<String>| {
            let Some(heading) = heading.as_deref() else {
                lines.clear();
                return;
            };
            let keep = matches!(
                heading,
                "## Workspace"
                    | "## Repos in this workspace"
                    | "## Communication Style"
                    | "## Project Context"
                    | "## Available Playbooks"
                    | "## Authority Level"
            );
            if keep {
                let mut section = String::new();
                section.push_str(heading);
                section.push('\n');
                let body = lines.join("\n").trim().to_string();
                if !body.is_empty() {
                    section.push_str(&body);
                    section.push('\n');
                }
                kept.push(section);
            }
            lines.clear();
        };

    for line in extra_context.lines() {
        if line.starts_with("## ") {
            flush_section(&current_heading, &mut current_lines, &mut kept);
            current_heading = Some(line.to_string());
        } else if current_heading.is_some() {
            current_lines.push(line);
        }
    }
    flush_section(&current_heading, &mut current_lines, &mut kept);

    if kept.is_empty() {
        Some(extra_context.to_string())
    } else {
        Some(kept.join("\n"))
    }
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

/// Pre-built dispatch data, produced by `prepare_dispatch` (sync) and consumed
/// by `dispatch_message` (async). Separating these avoids holding `&SignalStore`
/// (which is !Send) across await points.
pub enum DispatchBundle {
    /// Claude SDK session options.
    Claude(Box<SessionOptions>),
    /// For Codex/Gemini: optional system prompt (first message only).
    AltProvider { system_prompt: Option<String> },
}

/// The buzz coordinator — manages LLM sessions with signal context.
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
    execution_policy: crate::config::BeeExecutionPolicy,
    working_dir: Option<PathBuf>,
    settings: Option<String>,
    safety_hooks: Option<Box<dyn SafetyHooks>>,
    /// Number of turns used in the last completed session.
    last_num_turns: u64,
    /// Context to prepend to the next user message (e.g. /doctor output).
    /// Consumed on the next `take_pending_context()` call.
    pending_context: Option<String>,
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
            execution_policy: crate::config::BeeExecutionPolicy::Autonomous,
            working_dir: None,
            settings: None,
            safety_hooks: None,
            last_num_turns: 0,
            pending_context: None,
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

    /// Queue context to be prepended to the next user message dispatched to
    /// the coordinator. Used for built-in command output (e.g. /doctor) that
    /// the coordinator should see without triggering a separate LLM turn.
    pub fn set_pending_context(&mut self, context: String) {
        self.pending_context = Some(context);
    }

    /// Take (and clear) any pending context queued via `set_pending_context`.
    pub fn take_pending_context(&mut self) -> Option<String> {
        self.pending_context.take()
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

    pub fn set_execution_policy(&mut self, policy: crate::config::BeeExecutionPolicy) {
        self.execution_policy = policy;
    }

    pub fn execution_policy(&self) -> crate::config::BeeExecutionPolicy {
        self.execution_policy
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

    /// Set the LLM provider ("claude", "codex", or "gemini").
    pub fn set_provider(&mut self, provider: String) {
        self.provider = provider;
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

    /// Check if the CLI for the configured provider is available.
    pub async fn is_available(provider: &str) -> bool {
        let bin = match provider {
            "codex" => "codex",
            "gemini" => "gemini",
            _ => "claude",
        };
        tokio::process::Command::new("which")
            .arg(bin)
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

                // Extract usage stats from the result.
                let mut stats = UsageStats {
                    total_cost_usd: result.total_cost_usd,
                    ..Default::default()
                };
                if let Some(ref usage_val) = result.usage {
                    stats.input_tokens = usage_val
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    stats.output_tokens = usage_val
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    stats.cache_read_tokens = usage_val
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                }
                events.push(CoordinatorEvent::Usage(stats));
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

    /// Build a provider-agnostic dispatch bundle synchronously.
    ///
    /// Call this while you still have access to `&SignalStore` (which is !Send),
    /// then pass the result to `dispatch_message` across the await boundary.
    pub fn prepare_dispatch(&self, store: &SignalStore) -> Result<DispatchBundle> {
        match self.provider.as_str() {
            "codex" | "gemini" => {
                // Codex/Gemini do not have Claude's dedicated long-lived system
                // prompt channel, so we inline a compact first-turn prompt.
                let system_prompt = if self.session_id.is_none() {
                    let signals = store.get_open_signals().unwrap_or_default();
                    let compact_context =
                        compact_alt_provider_context(self.extra_context.as_deref());
                    Some(prompt::build_system_prompt(
                        &signals,
                        &[],
                        compact_context.as_deref(),
                        Some(&self.name),
                        self.prompt_preamble.as_deref(),
                        None,
                    ))
                } else {
                    None
                };
                Ok(DispatchBundle::AltProvider { system_prompt })
            }
            _ => {
                let opts = self.build_options(store)?;
                Ok(DispatchBundle::Claude(Box::new(opts)))
            }
        }
    }

    /// Like `prepare_dispatch` but supports hook-triggered playbooks.
    pub fn prepare_dispatch_with_playbooks(
        &self,
        store: &SignalStore,
        hook_playbooks: Option<&str>,
    ) -> Result<DispatchBundle> {
        match self.provider.as_str() {
            "codex" | "gemini" => {
                // Same compact first-turn prompt path, plus any hook-triggered
                // playbook content that must be present for this run.
                let signals = store.get_open_signals().unwrap_or_default();
                let compact_context = compact_alt_provider_context(self.extra_context.as_deref());
                let system_prompt = Some(prompt::build_system_prompt(
                    &signals,
                    &[],
                    compact_context.as_deref(),
                    Some(&self.name),
                    self.prompt_preamble.as_deref(),
                    hook_playbooks,
                ));
                Ok(DispatchBundle::AltProvider { system_prompt })
            }
            _ => {
                let opts = self.build_options_with_playbooks(store, hook_playbooks)?;
                Ok(DispatchBundle::Claude(Box::new(opts)))
            }
        }
    }

    /// Provider-agnostic message dispatch.
    ///
    /// Routes to Claude, Codex, or Gemini based on `self.provider`.
    /// Call `prepare_dispatch` first to get the bundle.
    pub async fn dispatch_message<F>(
        &mut self,
        message: &str,
        bundle: DispatchBundle,
        image_paths: &[PathBuf],
        on_event: F,
    ) -> Result<String>
    where
        F: FnMut(CoordinatorEvent),
    {
        match bundle {
            DispatchBundle::Claude(opts) => {
                self.handle_message_with_options(message, *opts, on_event)
                    .await
            }
            DispatchBundle::AltProvider { system_prompt } => {
                let prompt = if let Some(sys) = system_prompt {
                    format!("{sys}\n\n---\n\n{message}")
                } else {
                    message.to_string()
                };
                match self.provider.as_str() {
                    "codex" => self.run_codex(&prompt, image_paths, on_event).await,
                    "gemini" => self.run_gemini(&prompt, on_event).await,
                    _ => unreachable!(),
                }
            }
        }
    }

    /// Run a turn against the Codex CLI.
    async fn run_codex<F>(
        &mut self,
        prompt: &str,
        image_paths: &[PathBuf],
        mut on_event: F,
    ) -> Result<String>
    where
        F: FnMut(CoordinatorEvent),
    {
        use apiari_codex_sdk::{CodexClient, ExecOptions, ResumeOptions};

        let client = CodexClient::new();
        let model = (!self.model.trim().is_empty()).then(|| self.model.clone());

        let snapshot = self
            .safety_hooks
            .as_ref()
            .and_then(|hooks| hooks.pre_turn());

        let mut execution = if let Some(ref sid) = self.session_id
            && matches!(
                self.execution_policy,
                crate::config::BeeExecutionPolicy::Autonomous
            ) {
            client
                .exec_resume(
                    prompt,
                    ResumeOptions {
                        session_id: Some(sid.clone()),
                        model: model.clone(),
                        images: image_paths.to_vec(),
                        dangerously_bypass_sandbox: true,
                        working_dir: self.working_dir.clone(),
                        ..Default::default()
                    },
                )
                .await?
        } else {
            client
                .exec(
                    prompt,
                    ExecOptions {
                        model,
                        sandbox: Some(match self.execution_policy {
                            crate::config::BeeExecutionPolicy::Observe
                            | crate::config::BeeExecutionPolicy::DispatchOnly => {
                                apiari_codex_sdk::SandboxMode::ReadOnly
                            }
                            crate::config::BeeExecutionPolicy::Autonomous => {
                                apiari_codex_sdk::SandboxMode::WorkspaceWrite
                            }
                        }),
                        approval: Some(apiari_codex_sdk::ApprovalPolicy::Never),
                        images: image_paths.to_vec(),
                        dangerously_bypass_sandbox: matches!(
                            self.execution_policy,
                            crate::config::BeeExecutionPolicy::Autonomous
                        ),
                        working_dir: self.working_dir.clone(),
                        ..Default::default()
                    },
                )
                .await?
        };

        let mut response = String::new();

        while let Ok(Some(event)) = execution.next_event().await {
            match &event {
                apiari_codex_sdk::Event::ThreadStarted { thread_id } => {
                    self.session_id = Some(thread_id.clone());
                    self.session_token = Some(SessionToken {
                        provider: "codex".to_string(),
                        token: thread_id.clone(),
                    });
                }
                apiari_codex_sdk::Event::ItemCompleted { item } => {
                    if let Some(text) = codex_assistant_text(item) {
                        response = text.to_string();
                        on_event(CoordinatorEvent::Token(text.to_string()));
                    }
                }
                apiari_codex_sdk::Event::TurnCompleted { usage: Some(u) } => {
                    on_event(CoordinatorEvent::Usage(UsageStats {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        cache_read_tokens: u.cached_input_tokens,
                        total_cost_usd: None,
                    }));
                }
                apiari_codex_sdk::Event::TurnFailed { error, .. } => {
                    let msg = error
                        .as_ref()
                        .and_then(|e| e.message.as_deref())
                        .unwrap_or("codex turn failed");
                    return Err(color_eyre::eyre::eyre!("{msg}"));
                }
                apiari_codex_sdk::Event::Error { message } => {
                    let msg = message.as_deref().unwrap_or("codex error");
                    return Err(color_eyre::eyre::eyre!("{msg}"));
                }
                _ => {}
            }
        }

        // Post-turn safety hooks
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

    /// Run a turn against the Gemini CLI.
    async fn run_gemini<F>(&mut self, prompt: &str, mut on_event: F) -> Result<String>
    where
        F: FnMut(CoordinatorEvent),
    {
        use apiari_gemini_sdk::{Event as GeminiEvent, GeminiClient, GeminiOptions};

        let client = GeminiClient::new();

        let snapshot = self
            .safety_hooks
            .as_ref()
            .and_then(|hooks| hooks.pre_turn());

        let mut execution = if let Some(ref sid) = self.session_id {
            client
                .exec_resume(
                    prompt,
                    apiari_gemini_sdk::SessionOptions {
                        session_id: Some(sid.clone()),
                        model: Some(self.model.clone()),
                        working_dir: self.working_dir.clone(),
                        yolo: matches!(
                            self.execution_policy,
                            crate::config::BeeExecutionPolicy::Autonomous
                        ),
                    },
                )
                .await?
        } else {
            client
                .exec(
                    prompt,
                    GeminiOptions {
                        model: Some(self.model.clone()),
                        working_dir: self.working_dir.clone(),
                        yolo: matches!(
                            self.execution_policy,
                            crate::config::BeeExecutionPolicy::Autonomous
                        ),
                        ..Default::default()
                    },
                )
                .await?
        };

        let mut response = String::new();

        while let Some(event) = execution.next_event().await? {
            match &event {
                GeminiEvent::ThreadStarted { thread_id } => {
                    self.session_id = Some(thread_id.clone());
                    self.session_token = Some(SessionToken {
                        provider: "gemini".to_string(),
                        token: thread_id.clone(),
                    });
                }
                GeminiEvent::Init { session_id, .. } => {
                    self.session_id = Some(session_id.clone());
                    self.session_token = Some(SessionToken {
                        provider: "gemini".to_string(),
                        token: session_id.clone(),
                    });
                }
                GeminiEvent::Message { role, delta, .. } => {
                    if role.as_deref() != Some("assistant") {
                        continue;
                    }
                    if let Some(text) = event.text()
                        && !text.is_empty()
                    {
                        if delta.unwrap_or(false) {
                            response.push_str(&text);
                            on_event(CoordinatorEvent::Token(text));
                        } else {
                            response = text.clone();
                            on_event(CoordinatorEvent::Token(text));
                        }
                    }
                }
                GeminiEvent::ItemCompleted { item } => {
                    if let Some(text) = item.text()
                        && !text.is_empty()
                    {
                        response = text.to_string();
                        on_event(CoordinatorEvent::Token(text.to_string()));
                    }
                }
                GeminiEvent::JsonOutput {
                    session_id,
                    response: json_response,
                    stats,
                    ..
                } => {
                    if let Some(session_id) = session_id {
                        self.session_id = Some(session_id.clone());
                        self.session_token = Some(SessionToken {
                            provider: "gemini".to_string(),
                            token: session_id.clone(),
                        });
                    }
                    if let Some(text) = json_response.as_deref().map(str::trim)
                        && !text.is_empty()
                        && response.trim().is_empty()
                    {
                        response = text.to_string();
                        on_event(CoordinatorEvent::Token(text.to_string()));
                    }
                    if let Some(stats) = stats {
                        on_event(CoordinatorEvent::Usage(UsageStats {
                            input_tokens: stats.input_tokens.unwrap_or_default(),
                            output_tokens: stats.output_tokens.unwrap_or_default(),
                            cache_read_tokens: stats.cached.unwrap_or_default(),
                            total_cost_usd: None,
                        }));
                    }
                }
                GeminiEvent::TurnCompleted { usage: Some(u) } => {
                    on_event(CoordinatorEvent::Usage(UsageStats {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        cache_read_tokens: u.cached_input_tokens,
                        total_cost_usd: None,
                    }));
                }
                GeminiEvent::UsageEvent {
                    input_tokens,
                    output_tokens,
                    cached_tokens,
                    ..
                } => {
                    on_event(CoordinatorEvent::Usage(UsageStats {
                        input_tokens: input_tokens.unwrap_or_default(),
                        output_tokens: output_tokens.unwrap_or_default(),
                        cache_read_tokens: cached_tokens.unwrap_or_default(),
                        total_cost_usd: None,
                    }));
                }
                GeminiEvent::Result { status, stats, .. } => {
                    if matches!(status.as_deref(), Some("failed" | "error")) {
                        return Err(color_eyre::eyre::eyre!("gemini turn failed"));
                    }
                    if let Some(stats) = stats {
                        on_event(CoordinatorEvent::Usage(UsageStats {
                            input_tokens: stats.input_tokens.unwrap_or_default(),
                            output_tokens: stats.output_tokens.unwrap_or_default(),
                            cache_read_tokens: stats.cached.unwrap_or_default(),
                            total_cost_usd: None,
                        }));
                    }
                }
                GeminiEvent::TurnFailed { error, .. } => {
                    let msg = error
                        .as_ref()
                        .and_then(|e| e.message.as_deref())
                        .unwrap_or("gemini turn failed");
                    return Err(color_eyre::eyre::eyre!("{msg}"));
                }
                GeminiEvent::Error { message, .. } => {
                    let msg = message.as_deref().unwrap_or("gemini error");
                    return Err(color_eyre::eyre::eyre!("{msg}"));
                }
                _ => {}
            }
        }

        // If we somehow finished with no captured text but the execution learned
        // a resumable session, keep that state and return the empty response.
        if response.contains("[Thought: true]") {
            response = response.replace("[Thought: true]", "").trim().to_string();
        }

        // Post-turn safety hooks
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
        self.pending_context = None;
    }
}

fn codex_assistant_text(item: &apiari_codex_sdk::Item) -> Option<&str> {
    match item {
        apiari_codex_sdk::Item::AgentMessage { text, .. } => {
            text.as_deref().filter(|text| !text.is_empty())
        }
        _ => None,
    }
}

/// Truncate a command string for logging.
fn truncate_cmd(cmd: &str) -> &str {
    let end = cmd.char_indices().nth(120).map_or(cmd.len(), |(i, _)| i);
    &cmd[..end]
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{Mutex, OnceLock},
    };

    use apiari_claude_sdk::{
        Event,
        types::{AssistantMessage, AssistantMessageContent, ContentBlock, ResultMessage},
    };
    use serde_json::Map;

    use super::*;

    fn make_coordinator() -> Coordinator {
        Coordinator::new("sonnet", 20)
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct PathGuard {
        old_path: Option<std::ffi::OsString>,
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            match self.old_path.take() {
                Some(path) => unsafe { std::env::set_var("PATH", path) },
                None => unsafe { std::env::remove_var("PATH") },
            }
        }
    }

    fn install_fake_binary(dir: &Path, name: &str, body: &str) -> PathGuard {
        let bin_dir = dir.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let script = bin_dir.join(name);
        fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let old_path = std::env::var_os("PATH");
        let mut paths = vec![bin_dir];
        paths.extend(std::env::split_paths(&old_path.clone().unwrap_or_default()));
        let joined = std::env::join_paths(paths).unwrap();
        unsafe { std::env::set_var("PATH", joined) };

        PathGuard { old_path }
    }

    fn install_fake_codex(dir: &Path, log_path: &Path) -> PathGuard {
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = \"exec\" ] && [ \"$2\" = \"resume\" ]; then\n  printf '%s\\n' '{}'\n  printf '%s\\n' '{}'\n  printf '%s\\n' '{}'\nelse\n  printf '%s\\n' '{}'\n  printf '%s\\n' '{}'\n  printf '%s\\n' '{}'\nfi\n",
            log_path.display(),
            r#"{"type":"thread.started","thread_id":"codex-session-1"}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Second reply. [FOLLOWUP: 1h | Check PR status again]"}}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":9,"cached_input_tokens":2}}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"thread.started","thread_id":"codex-session-1"}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"First reply."}}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"turn.completed","usage":{"input_tokens":5,"output_tokens":2,"cached_input_tokens":0}}"#.replace('\'', "'\"'\"'"),
        );
        install_fake_binary(dir, "codex", &body)
    }

    fn install_fake_gemini(dir: &Path, log_path: &Path) -> PathGuard {
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \" $* \" in\n  *' --resume gemini-session-1 '*)\n    printf '%s\\n' '{}'\n    printf '%s\\n' '{}'\n    printf '%s\\n' '{}'\n    ;;\n  *)\n    printf '%s\\n' '{}'\n    printf '%s\\n' '{}'\n    printf '%s\\n' '{}'\n    ;;\nesac\n",
            log_path.display(),
            r#"{"type":"init","session_id":"gemini-session-1"}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"message","role":"assistant","content":"Second reply. [FOLLOWUP: 1h | Check PR status again]","delta":true}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"result","status":"success","stats":{"input_tokens":7,"output_tokens":9,"cached":2}}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"init","session_id":"gemini-session-1"}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"message","role":"assistant","content":"First reply.","delta":true}"#.replace('\'', "'\"'\"'"),
            r#"{"type":"result","status":"success","stats":{"input_tokens":5,"output_tokens":2,"cached":0}}"#.replace('\'', "'\"'\"'"),
        );
        install_fake_binary(dir, "gemini", &body)
    }

    #[derive(Debug, Clone)]
    struct TurnContract {
        response: String,
        emitted_token: bool,
        usage: UsageStats,
        parsed_actions: Vec<crate::buzz::coordinator::actions::BeeAction>,
    }

    #[derive(Debug, Clone)]
    struct ProviderScenarioContract {
        first_turn: TurnContract,
        second_turn: TurnContract,
        session_token: SessionToken,
    }

    async fn run_mock_provider_contract(
        provider: &str,
        shim_dir: &Path,
        log_path: &Path,
    ) -> ProviderScenarioContract {
        let _path_guard = match provider {
            "codex" => install_fake_codex(shim_dir, log_path),
            "gemini" => install_fake_gemini(shim_dir, log_path),
            other => panic!("unsupported provider for mocked contract: {other}"),
        };

        let store = SignalStore::open_memory("ws").unwrap();
        let mut coord = Coordinator::new(
            if provider == "gemini" {
                "gemini-2.5-flash"
            } else {
                "gpt-5.3-codex"
            },
            20,
        );
        coord.set_provider(provider.to_string());
        coord.set_name("Main".to_string());
        coord.set_extra_context(
            "## Workspace\nTest workspace\n\n## Available Playbooks\n- none\n".to_string(),
        );

        let bundle = coord.prepare_dispatch(&store).unwrap();
        let mut first_events = Vec::new();
        let first_response = coord
            .dispatch_message("Reply briefly.", bundle, &[], |event| {
                first_events.push(event)
            })
            .await
            .unwrap();

        let bundle = coord.prepare_dispatch(&store).unwrap();
        let mut second_events = Vec::new();
        let second_response = coord
            .dispatch_message("Schedule a follow-up if needed.", bundle, &[], |event| {
                second_events.push(event)
            })
            .await
            .unwrap();

        let session_token = coord.session_token().cloned().expect("session token");

        fn to_turn_contract(response: String, events: Vec<CoordinatorEvent>) -> TurnContract {
            let emitted_token = events
                .iter()
                .any(|event| matches!(event, CoordinatorEvent::Token(_)));
            let usage = events
                .iter()
                .find_map(|event| match event {
                    CoordinatorEvent::Usage(stats) => Some(stats.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let parsed_actions = crate::buzz::coordinator::actions::parse_actions(&response);
            TurnContract {
                response,
                emitted_token,
                usage,
                parsed_actions,
            }
        }

        ProviderScenarioContract {
            first_turn: to_turn_contract(first_response, first_events),
            second_turn: to_turn_contract(second_response, second_events),
            session_token,
        }
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
        let events = result.unwrap();
        // Result events emit a Usage event
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], CoordinatorEvent::Usage(_)));
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

    // -- Usage extraction --

    #[test]
    fn test_process_event_result_extracts_usage() {
        let mut coord = make_coordinator();

        let event = Event::Result(ResultMessage {
            subtype: "success".to_string(),
            duration_ms: 100,
            duration_api_ms: 90,
            is_error: false,
            num_turns: 1,
            session_id: "sess-1".to_string(),
            total_cost_usd: Some(0.042),
            usage: Some(serde_json::json!({
                "input_tokens": 1500,
                "output_tokens": 300,
                "cache_read_input_tokens": 800
            })),
            result: None,
            structured_output: None,
        });
        let events = coord.process_event(&event).unwrap();

        assert_eq!(events.len(), 1);
        if let CoordinatorEvent::Usage(stats) = &events[0] {
            assert_eq!(stats.input_tokens, 1500);
            assert_eq!(stats.output_tokens, 300);
            assert_eq!(stats.cache_read_tokens, 800);
            assert_eq!(stats.total_cost_usd, Some(0.042));
        } else {
            panic!("expected Usage event");
        }
    }

    #[test]
    fn test_process_event_error_result_skips_usage() {
        let mut coord = make_coordinator();

        let event = make_result_event("err", true, Some("broken"));
        let result = coord.process_event(&event);

        // Error results return Err, no Usage event
        assert!(result.is_err());
    }

    #[test]
    fn test_process_event_result_no_usage_field() {
        let mut coord = make_coordinator();

        // Result with no usage field → Usage event with zeros
        let event = make_result_event("sess-2", false, Some("done"));
        let events = coord.process_event(&event).unwrap();

        assert_eq!(events.len(), 1);
        if let CoordinatorEvent::Usage(stats) = &events[0] {
            assert_eq!(stats.input_tokens, 0);
            assert_eq!(stats.output_tokens, 0);
            assert_eq!(stats.total_cost_usd, Some(0.01)); // from make_result_event
        } else {
            panic!("expected Usage event");
        }
    }

    // -- max_context_tokens --

    #[test]
    fn test_max_context_tokens() {
        assert_eq!(max_context_tokens("sonnet"), 200_000);
        assert_eq!(max_context_tokens("opus"), 200_000);
        assert_eq!(max_context_tokens("o4-mini"), 200_000);
        assert_eq!(max_context_tokens("gemini-2.0-flash"), 1_000_000);
        assert_eq!(max_context_tokens("unknown-model"), 200_000);
    }

    // -- set_provider --

    #[test]
    fn test_set_provider() {
        let mut coord = make_coordinator();
        assert_eq!(coord.provider(), "claude");

        coord.set_provider("codex".to_string());
        assert_eq!(coord.provider(), "codex");

        coord.set_provider("gemini".to_string());
        assert_eq!(coord.provider(), "gemini");
    }

    // -- UsageStats serde --

    #[test]
    fn test_usage_stats_serde_roundtrip() {
        let stats = UsageStats {
            input_tokens: 1500,
            output_tokens: 300,
            cache_read_tokens: 800,
            total_cost_usd: Some(0.042),
        };
        let json = serde_json::to_string(&stats).unwrap();
        let parsed: UsageStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, parsed);
    }

    #[test]
    fn test_usage_stats_default() {
        let stats = UsageStats::default();
        assert_eq!(stats.input_tokens, 0);
        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.cache_read_tokens, 0);
        assert_eq!(stats.total_cost_usd, None);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_mocked_alt_provider_contracts_stay_in_parity() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let codex_log = temp.path().join("codex.log");
        let gemini_log = temp.path().join("gemini.log");

        let codex = run_mock_provider_contract("codex", temp.path(), &codex_log).await;
        let gemini = run_mock_provider_contract("gemini", temp.path(), &gemini_log).await;

        let expected = ProviderScenarioContract {
            first_turn: TurnContract {
                response: "First reply.".to_string(),
                emitted_token: true,
                usage: UsageStats {
                    input_tokens: 5,
                    output_tokens: 2,
                    cache_read_tokens: 0,
                    total_cost_usd: None,
                },
                parsed_actions: vec![],
            },
            second_turn: TurnContract {
                response: "Second reply. [FOLLOWUP: 1h | Check PR status again]".to_string(),
                emitted_token: true,
                usage: UsageStats {
                    input_tokens: 7,
                    output_tokens: 9,
                    cache_read_tokens: 2,
                    total_cost_usd: None,
                },
                parsed_actions: vec![crate::buzz::coordinator::actions::BeeAction::Followup {
                    when: "1h".to_string(),
                    action: "Check PR status again".to_string(),
                }],
            },
            session_token: SessionToken {
                provider: "codex".to_string(),
                token: "codex-session-1".to_string(),
            },
        };

        fn assert_turn(actual: &TurnContract, expected: &TurnContract) {
            assert_eq!(actual.response, expected.response);
            assert_eq!(actual.emitted_token, expected.emitted_token);
            assert_eq!(actual.usage.input_tokens, expected.usage.input_tokens);
            assert_eq!(actual.usage.output_tokens, expected.usage.output_tokens);
            assert_eq!(
                actual.usage.cache_read_tokens,
                expected.usage.cache_read_tokens
            );
            assert_eq!(actual.usage.total_cost_usd, expected.usage.total_cost_usd);
            assert_eq!(actual.parsed_actions, expected.parsed_actions);
        }

        assert_turn(&codex.first_turn, &expected.first_turn);
        assert_turn(&codex.second_turn, &expected.second_turn);
        assert_eq!(
            codex.session_token.provider,
            expected.session_token.provider
        );
        assert_eq!(codex.session_token.token, expected.session_token.token);

        assert_turn(&gemini.first_turn, &expected.first_turn);
        assert_turn(&gemini.second_turn, &expected.second_turn);
        assert_eq!(gemini.session_token.provider, "gemini");
        assert_eq!(gemini.session_token.token, "gemini-session-1");

        let codex_invocations = fs::read_to_string(&codex_log).unwrap();
        assert!(codex_invocations.lines().count() >= 2);
        assert!(codex_invocations.contains("codex-session-1"));

        let gemini_invocations = fs::read_to_string(&gemini_log).unwrap();
        assert!(
            gemini_invocations
                .lines()
                .next()
                .unwrap_or_default()
                .contains("--yolo")
        );
        assert!(gemini_invocations.lines().count() >= 2);
        assert!(gemini_invocations.contains("gemini-session-1"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_mocked_alt_provider_error_contracts_match() {
        let _env_guard = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let _path_guard = install_fake_binary(
            temp.path(),
            "codex",
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"turn.failed\",\"error\":{\"message\":\"provider auth failed\"}}'\n",
        );
        let _gemini_guard = install_fake_binary(
            temp.path(),
            "gemini",
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"error\",\"message\":\"provider auth failed\",\"fatal\":true}'\n",
        );

        async fn run_error(provider: &str) -> String {
            let store = SignalStore::open_memory("ws").unwrap();
            let mut coord = Coordinator::new("test-model", 20);
            coord.set_provider(provider.to_string());
            let bundle = coord.prepare_dispatch(&store).unwrap();
            let err = coord
                .dispatch_message("Reply briefly.", bundle, &[], |_| {})
                .await
                .expect_err("provider should fail");
            assert!(coord.session_token().is_none());
            err.to_string()
        }

        let codex_err = run_error("codex").await;
        let gemini_err = run_error("gemini").await;
        assert!(codex_err.contains("provider auth failed"));
        assert!(gemini_err.contains("provider auth failed"));
    }

    #[tokio::test]
    #[ignore]
    async fn test_real_provider_smoke_contract_opt_in() {
        if std::env::var("APIARI_REAL_PROVIDER_SMOKE").ok().as_deref() != Some("1") {
            return;
        }

        let providers = std::env::var("APIARI_REAL_PROVIDER_SMOKE_PROVIDERS")
            .unwrap_or_else(|_| "claude,codex,gemini".to_string());
        let store = SignalStore::open_memory("ws").unwrap();

        for provider in providers
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !Coordinator::is_available(provider).await {
                continue;
            }

            let mut coord = Coordinator::new(
                if provider == "gemini" {
                    "gemini-2.5-flash"
                } else if provider == "codex" {
                    "gpt-5.3-codex"
                } else {
                    "sonnet"
                },
                10,
            );
            coord.set_provider(provider.to_string());
            let first_bundle = coord.prepare_dispatch(&store).unwrap();
            let first_response = coord
                .dispatch_message("Reply with OK and nothing else.", first_bundle, &[], |_| {})
                .await
                .unwrap();
            let normalized = first_response
                .trim()
                .trim_matches(|c: char| c == '.' || c.is_whitespace());
            assert!(
                normalized.eq_ignore_ascii_case("ok")
                    || normalized.to_ascii_lowercase().contains("ok"),
                "provider {provider} returned unexpected first smoke response: {first_response}"
            );
            let first_token = coord
                .session_token()
                .cloned()
                .expect("session token after first turn");
            assert_eq!(first_token.provider, provider);

            let second_bundle = coord.prepare_dispatch(&store).unwrap();
            let second_response = coord
                .dispatch_message(
                    "Respond with exactly one follow-up marker and no prose: [FOLLOWUP: 1h | Check CI status again]",
                    second_bundle,
                    &[],
                    |_| {},
                )
                .await
                .unwrap();
            let actions = crate::buzz::coordinator::actions::parse_actions(&second_response);
            assert!(
                actions.iter().any(|action| matches!(
                    action,
                    crate::buzz::coordinator::actions::BeeAction::Followup { when, action }
                        if when == "1h" && action == "Check CI status again"
                )),
                "provider {provider} returned unexpected followup smoke response: {second_response}"
            );
            let second_token = coord
                .session_token()
                .cloned()
                .expect("session token after second turn");
            assert_eq!(second_token.provider, provider);
            assert_eq!(second_token.token, first_token.token);
        }
    }

    #[test]
    fn codex_assistant_text_ignores_reasoning_items() {
        let reasoning = apiari_codex_sdk::Item::Reasoning {
            id: Some("r_1".to_string()),
            text: Some("Let me think".to_string()),
        };
        let message = apiari_codex_sdk::Item::AgentMessage {
            id: Some("m_1".to_string()),
            text: Some("Final answer".to_string()),
        };

        assert_eq!(codex_assistant_text(&reasoning), None);
        assert_eq!(codex_assistant_text(&message), Some("Final answer"));
    }
}

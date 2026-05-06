//! Session configuration and CLI argument building.
//!
//! [`SessionOptions`] holds all the knobs for launching a Claude session:
//! model, system prompt, working directory, allowed tools, resume/continue
//! flags, and so on.  Its [`to_cli_args`](SessionOptions::to_cli_args)
//! method converts the options into `claude` CLI flags.

use std::path::PathBuf;

/// Options for creating a new Claude session.
///
/// These map 1-to-1 onto `claude` CLI flags. Only the fields you set will
/// produce flags; `None` / empty-vec fields are omitted.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    // -- Session identity --------------------------------------------------
    /// Resume a specific session by ID.
    pub resume: Option<String>,

    /// Continue the most recent conversation in the working directory.
    pub continue_conversation: bool,

    /// When resuming, fork to a new session ID.
    pub fork_session: bool,

    /// Use a specific session ID (must be a valid UUID).
    pub session_id: Option<String>,

    // -- Model / budget ----------------------------------------------------
    /// Model to use (e.g. `"sonnet"`, `"opus"`, or full model name).
    pub model: Option<String>,

    /// Fallback model when the primary is overloaded.
    pub fallback_model: Option<String>,

    /// Maximum dollar amount to spend on API calls.
    pub max_budget_usd: Option<f64>,

    /// Maximum number of agentic turns.
    pub max_turns: Option<u64>,

    /// Maximum number of output tokens per response (--max-tokens).
    pub max_tokens: Option<u32>,

    // -- System prompt -----------------------------------------------------
    /// Replace the entire system prompt.
    pub system_prompt: Option<String>,

    /// Append text to the default system prompt.
    pub append_system_prompt: Option<String>,

    // -- Tools & permissions -----------------------------------------------
    /// Restrict which built-in tools Claude can use (e.g. `["Bash", "Edit", "Read"]`).
    pub tools: Vec<String>,

    /// Tools that execute without prompting for permission.
    pub allowed_tools: Vec<String>,

    /// Tools that are explicitly denied.
    pub disallowed_tools: Vec<String>,

    /// Permission mode.
    pub permission_mode: Option<PermissionMode>,

    /// Skip all permission checks (dangerous!).
    pub dangerously_skip_permissions: bool,

    // -- MCP ---------------------------------------------------------------
    /// Path(s) to MCP config JSON files.
    pub mcp_config: Vec<String>,

    /// Only use MCP servers from `mcp_config`, ignoring all other configs.
    pub strict_mcp_config: bool,

    // -- Working directory & extra dirs ------------------------------------
    /// Working directory for the Claude session.
    pub working_dir: Option<PathBuf>,

    /// Additional directories to allow tool access to.
    pub add_dirs: Vec<PathBuf>,

    // -- Streaming ---------------------------------------------------------
    /// Include partial streaming events in output.
    pub include_partial_messages: bool,

    // -- Other -------------------------------------------------------------
    /// Effort level (`"low"`, `"medium"`, `"high"`).
    pub effort: Option<String>,

    /// Disable session persistence.
    pub no_session_persistence: bool,

    /// JSON schema for structured output validation.
    pub json_schema: Option<String>,

    /// Custom subagents defined as a JSON string.
    pub agents: Option<String>,

    /// Custom settings file or JSON string.
    pub settings: Option<String>,

    /// Setting sources to load.
    pub setting_sources: Vec<String>,

    /// Additional environment variables to set.
    pub env_vars: Vec<(String, String)>,
}

/// Permission modes for controlling tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Standard permission behavior.
    Default,
    /// Auto-accept file edits.
    AcceptEdits,
    /// Planning mode (no execution).
    Plan,
    /// Bypass all permission checks.
    BypassPermissions,
    /// Ask for permission (do not ask).
    DontAsk,
}

impl PermissionMode {
    /// Return the CLI flag value for this mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
            Self::DontAsk => "dontAsk",
        }
    }
}

impl SessionOptions {
    /// Convert these options into CLI arguments for the `claude` binary.
    ///
    /// This does **not** include the base arguments (`--print`,
    /// `--output-format stream-json`, etc.) — those are added by
    /// [`Transport::spawn`](crate::transport::Transport::spawn).
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        // Session identity.
        if let Some(ref id) = self.resume {
            args.extend(["--resume".to_owned(), id.clone()]);
        }
        if self.continue_conversation {
            args.push("--continue".to_owned());
        }
        if self.fork_session {
            args.push("--fork-session".to_owned());
        }
        if let Some(ref id) = self.session_id {
            args.extend(["--session-id".to_owned(), id.clone()]);
        }

        // Model / budget.
        if let Some(ref model) = self.model {
            args.extend(["--model".to_owned(), model.clone()]);
        }
        if let Some(ref model) = self.fallback_model {
            args.extend(["--fallback-model".to_owned(), model.clone()]);
        }
        if let Some(budget) = self.max_budget_usd {
            args.extend(["--max-budget-usd".to_owned(), budget.to_string()]);
        }
        if let Some(turns) = self.max_turns {
            args.extend(["--max-turns".to_owned(), turns.to_string()]);
        }
        if let Some(tokens) = self.max_tokens {
            args.extend(["--max-tokens".to_owned(), tokens.to_string()]);
        }

        // System prompt.
        if let Some(ref prompt) = self.system_prompt {
            args.extend(["--system-prompt".to_owned(), prompt.clone()]);
        }
        if let Some(ref prompt) = self.append_system_prompt {
            args.extend(["--append-system-prompt".to_owned(), prompt.clone()]);
        }

        // Tools & permissions.
        if !self.tools.is_empty() {
            args.extend(["--tools".to_owned(), self.tools.join(",")]);
        }
        if !self.allowed_tools.is_empty() {
            args.push("--allowedTools".to_owned());
            for tool in &self.allowed_tools {
                args.push(tool.clone());
            }
        }
        if !self.disallowed_tools.is_empty() {
            args.push("--disallowedTools".to_owned());
            for tool in &self.disallowed_tools {
                args.push(tool.clone());
            }
        }
        if let Some(mode) = self.permission_mode {
            args.extend(["--permission-mode".to_owned(), mode.as_str().to_owned()]);
        }
        if self.dangerously_skip_permissions {
            args.push("--dangerously-skip-permissions".to_owned());
        }

        // MCP.
        if !self.mcp_config.is_empty() {
            args.push("--mcp-config".to_owned());
            for cfg in &self.mcp_config {
                args.push(cfg.clone());
            }
        }
        if self.strict_mcp_config {
            args.push("--strict-mcp-config".to_owned());
        }

        // Additional directories.
        if !self.add_dirs.is_empty() {
            args.push("--add-dir".to_owned());
            for dir in &self.add_dirs {
                args.push(dir.display().to_string());
            }
        }

        // Streaming.
        if self.include_partial_messages {
            args.push("--include-partial-messages".to_owned());
        }

        // Other.
        if let Some(ref effort) = self.effort {
            args.extend(["--effort".to_owned(), effort.clone()]);
        }
        if self.no_session_persistence {
            args.push("--no-session-persistence".to_owned());
        }
        if let Some(ref schema) = self.json_schema {
            args.extend(["--json-schema".to_owned(), schema.clone()]);
        }
        if let Some(ref agents) = self.agents {
            args.extend(["--agents".to_owned(), agents.clone()]);
        }
        if let Some(ref settings) = self.settings {
            args.extend(["--settings".to_owned(), settings.clone()]);
        }
        if !self.setting_sources.is_empty() {
            args.extend([
                "--setting-sources".to_owned(),
                self.setting_sources.join(","),
            ]);
        }

        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_options_produce_no_args() {
        let opts = SessionOptions::default();
        assert!(opts.to_cli_args().is_empty());
    }

    #[test]
    fn model_and_tools_args() {
        let opts = SessionOptions {
            model: Some("sonnet".to_owned()),
            allowed_tools: vec!["Bash".to_owned(), "Read".to_owned()],
            max_turns: Some(5),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--model".to_owned()));
        assert!(args.contains(&"sonnet".to_owned()));
        assert!(args.contains(&"--allowedTools".to_owned()));
        assert!(args.contains(&"Bash".to_owned()));
        assert!(args.contains(&"Read".to_owned()));
        assert!(args.contains(&"--max-turns".to_owned()));
        assert!(args.contains(&"5".to_owned()));
    }

    #[test]
    fn resume_and_continue_flags() {
        let opts = SessionOptions {
            resume: Some("abc-123".to_owned()),
            fork_session: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--resume".to_owned()));
        assert!(args.contains(&"abc-123".to_owned()));
        assert!(args.contains(&"--fork-session".to_owned()));
    }

    #[test]
    fn permission_mode_flag() {
        let opts = SessionOptions {
            permission_mode: Some(PermissionMode::AcceptEdits),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--permission-mode".to_owned()));
        assert!(args.contains(&"acceptEdits".to_owned()));
    }

    #[test]
    fn max_tokens_flag() {
        let opts = SessionOptions {
            max_tokens: Some(4096),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--max-tokens".to_owned()));
        assert!(args.contains(&"4096".to_owned()));
    }

    #[test]
    fn max_tokens_absent_when_none() {
        let opts = SessionOptions::default();
        let args = opts.to_cli_args();
        assert!(!args.iter().any(|a| a == "--max-tokens"));
    }
}

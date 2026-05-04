//! Execution options and CLI argument building.
//!
//! [`ExecOptions`] and [`ResumeOptions`] hold all the knobs for launching a
//! codex execution. Their [`to_cli_args`](ExecOptions::to_cli_args) methods
//! convert options into `codex` CLI flags.

use std::path::PathBuf;

/// Options for `codex exec`.
///
/// These map onto `codex` CLI flags. Only fields that are set will produce
/// flags; `None` / empty-vec fields are omitted.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Model to use (e.g. `"o4-mini"`, `"codex-mini"`).
    pub model: Option<String>,

    /// Sandbox mode controlling file system access.
    pub sandbox: Option<SandboxMode>,

    /// Approval policy for tool execution.
    pub approval: Option<ApprovalPolicy>,

    /// Enable fully autonomous mode (auto-approve everything).
    pub full_auto: bool,

    /// Named profile to load.
    pub profile: Option<String>,

    /// Config key=value overrides (passed as `-c key=value`).
    pub config_overrides: Vec<(String, String)>,

    /// Working directory for the execution.
    pub working_dir: Option<PathBuf>,

    /// Run in ephemeral mode (no session persistence).
    pub ephemeral: bool,

    /// JSON schema for structured output validation.
    pub output_schema: Option<String>,

    /// Image file paths to include as context.
    pub images: Vec<PathBuf>,

    /// Additional environment variables to set.
    pub env_vars: Vec<(String, String)>,

    /// Skip all confirmation prompts and execute without sandboxing.
    /// Maps to `--dangerously-bypass-approvals-and-sandbox`.
    pub dangerously_bypass_sandbox: bool,
}

/// Options for resuming a previous `codex exec` session.
#[derive(Debug, Clone, Default)]
pub struct ResumeOptions {
    /// Resume a specific session by ID.
    pub session_id: Option<String>,

    /// Resume the most recent session.
    pub last: bool,

    /// Model to use for the resumed session.
    pub model: Option<String>,

    /// Image file paths to include as context.
    pub images: Vec<PathBuf>,

    /// Enable fully autonomous mode.
    pub full_auto: bool,

    /// Working directory for the execution.
    pub working_dir: Option<PathBuf>,

    /// Additional environment variables to set.
    pub env_vars: Vec<(String, String)>,

    /// Skip all confirmation prompts and execute without sandboxing.
    /// Maps to `--dangerously-bypass-approvals-and-sandbox`.
    pub dangerously_bypass_sandbox: bool,
}

/// Sandbox modes controlling file system access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Read-only access to the file system.
    ReadOnly,
    /// Can write within the workspace directory.
    WorkspaceWrite,
    /// Full file system access (dangerous).
    DangerFullAccess,
}

impl SandboxMode {
    /// Return the CLI flag value for this mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// Approval policies for tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Require approval for all tool use.
    Untrusted,
    /// Only require approval on failure.
    OnFailure,
    /// Only require approval when explicitly requested.
    OnRequest,
    /// Never require approval.
    Never,
}

impl ApprovalPolicy {
    /// Return the CLI flag value for this policy.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

impl ExecOptions {
    /// Convert these options into CLI arguments for the `codex` binary.
    ///
    /// This does **not** include `exec` or `--json` — those are added by
    /// [`ReadOnlyTransport::spawn`](crate::transport::ReadOnlyTransport::spawn).
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(ref model) = self.model {
            args.extend(["-m".to_owned(), model.clone()]);
        }
        if let Some(sandbox) = self.sandbox {
            args.extend(["--sandbox".to_owned(), sandbox.as_str().to_owned()]);
        }
        if let Some(approval) = self.approval {
            args.extend(["--approval-policy".to_owned(), approval.as_str().to_owned()]);
        }
        if self.full_auto {
            args.push("--full-auto".to_owned());
        }
        if let Some(ref profile) = self.profile {
            args.extend(["--profile".to_owned(), profile.clone()]);
        }
        for (key, value) in &self.config_overrides {
            args.extend(["-c".to_owned(), format!("{key}={value}")]);
        }
        if self.ephemeral {
            args.push("--ephemeral".to_owned());
        }
        if let Some(ref schema) = self.output_schema {
            args.extend(["--output-schema".to_owned(), schema.clone()]);
        }
        for image in &self.images {
            args.extend(["--image".to_owned(), image.display().to_string()]);
        }
        if self.dangerously_bypass_sandbox {
            args.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
        }

        args
    }
}

impl ResumeOptions {
    /// Convert these options into CLI arguments for `codex exec resume`.
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(ref id) = self.session_id {
            args.push(id.clone());
        }
        if self.last {
            args.push("--last".to_owned());
        }
        if let Some(ref model) = self.model {
            args.extend(["-m".to_owned(), model.clone()]);
        }
        for image in &self.images {
            args.extend(["--image".to_owned(), image.display().to_string()]);
        }
        if self.full_auto {
            args.push("--full-auto".to_owned());
        }
        if self.dangerously_bypass_sandbox {
            args.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
        }

        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_options_produce_no_args() {
        let opts = ExecOptions::default();
        assert!(opts.to_cli_args().is_empty());
    }

    #[test]
    fn model_and_full_auto() {
        let opts = ExecOptions {
            model: Some("o4-mini".to_owned()),
            full_auto: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["-m", "o4-mini", "--full-auto"]);
    }

    #[test]
    fn sandbox_and_approval() {
        let opts = ExecOptions {
            sandbox: Some(SandboxMode::ReadOnly),
            approval: Some(ApprovalPolicy::OnFailure),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--sandbox".to_owned()));
        assert!(args.contains(&"read-only".to_owned()));
        assert!(args.contains(&"--approval-policy".to_owned()));
        assert!(args.contains(&"on-failure".to_owned()));
    }

    #[test]
    fn config_overrides() {
        let opts = ExecOptions {
            config_overrides: vec![
                ("key1".to_owned(), "val1".to_owned()),
                ("key2".to_owned(), "val2".to_owned()),
            ],
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["-c", "key1=val1", "-c", "key2=val2"]);
    }

    #[test]
    fn images() {
        let opts = ExecOptions {
            images: vec![PathBuf::from("a.png"), PathBuf::from("b.jpg")],
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["--image", "a.png", "--image", "b.jpg"]);
    }

    #[test]
    fn resume_options() {
        let opts = ResumeOptions {
            session_id: Some("sess_123".to_owned()),
            model: Some("o4-mini".to_owned()),
            full_auto: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["sess_123", "-m", "o4-mini", "--full-auto"]);
    }

    #[test]
    fn resume_last() {
        let opts = ResumeOptions {
            last: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["--last"]);
    }

    #[test]
    fn profile_and_ephemeral() {
        let opts = ExecOptions {
            profile: Some("custom".to_owned()),
            ephemeral: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["--profile", "custom", "--ephemeral"]);
    }

    #[test]
    fn output_schema() {
        let opts = ExecOptions {
            output_schema: Some(r#"{"type":"object"}"#.to_owned()),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args, vec!["--output-schema", r#"{"type":"object"}"#]);
    }
}

//! PreToolUse hook — blocks mutating Bash commands before execution.
//!
//! Invoked by Claude Code as a PreToolUse hook. Reads the tool input JSON
//! from stdin, classifies the Bash command, and returns a JSON decision on
//! stdout:
//! - No output + exit 0: allow (read-only command)
//! - JSON `permissionDecision: "deny"` + exit 0: block (potentially mutating)
//!
//! Uses the structured JSON deny format so Claude Code treats blocks as
//! per-command denials (tool remains available) rather than hook errors
//! (which can deregister the tool for the session).

use crate::buzz::coordinator::audit::{self, BashClassification};
use std::io::Read;

/// The hook's verdict: either allow (no stdout) or deny (JSON on stdout).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// Command is safe — emit nothing on stdout, exit 0.
    Allow,
    /// Command is blocked — emit structured JSON deny on stdout, exit 0.
    Deny { reason: String },
}

/// Run the validate-bash hook.
///
/// Returns exit code 0 in all cases. Blocked commands are communicated via
/// a JSON `permissionDecision: "deny"` on stdout so the Bash tool stays
/// registered for subsequent (allowed) commands.
///
/// Expected stdin JSON: `{"tool_name":"Bash","tool_input":{"command":"..."}}`
pub fn run() -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("validate-bash: failed to read stdin");
        return 0; // allow on error (fail open)
    }

    match evaluate(&input) {
        Verdict::Allow => 0,
        Verdict::Deny { reason } => {
            // Stderr: informational logging for human operators (daemon logs,
            // `claude --debug`). Not part of the Claude Code hook contract —
            // only the structured JSON on stdout drives the block decision.
            eprintln!("{reason}");

            let deny = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason
                }
            });
            serde_json::to_writer(std::io::stdout(), &deny)
                .expect("validate-bash: failed to write JSON to stdout");
            0 // exit 0 — JSON carries the deny decision
        }
    }
}

/// Pure evaluation: parse the hook input and classify the command.
///
/// Separated from `run()` so tests can assert the verdict without capturing
/// stdout/stderr.
fn evaluate(input: &str) -> Verdict {
    let command = match extract_command(input) {
        Some(cmd) => cmd,
        None => return Verdict::Allow, // unparseable or non-Bash — fail open
    };

    match audit::classify_bash_command_with_devmode(&command) {
        BashClassification::ReadOnly => Verdict::Allow,
        BashClassification::PotentiallyMutating { matched_pattern } => Verdict::Deny {
            reason: format!(
                "BLOCKED: coordinator attempted mutating Bash command (matched: {matched_pattern})"
            ),
        },
    }
}

/// Extract the command string from the hook JSON input.
fn extract_command(input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    v.get("tool_input")?
        .get("command")?
        .as_str()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_command_valid() {
        let input = r#"{"tool_name":"Bash","tool_input":{"command":"git log --oneline"}}"#;
        assert_eq!(
            extract_command(input),
            Some("git log --oneline".to_string())
        );
    }

    #[test]
    fn test_extract_command_missing_tool_input() {
        let input = r#"{"tool_name":"Bash"}"#;
        assert_eq!(extract_command(input), None);
    }

    #[test]
    fn test_extract_command_missing_command() {
        let input = r#"{"tool_name":"Bash","tool_input":{}}"#;
        assert_eq!(extract_command(input), None);
    }

    #[test]
    fn test_extract_command_invalid_json() {
        assert_eq!(extract_command("not json"), None);
    }

    #[test]
    fn test_extract_command_ignores_extra_fields() {
        // Hook input may include extra fields (description, session_id, etc.).
        // extract_command must only return tool_input.command — never
        // conversation context from other fields.
        let input = r#"{
            "tool_name": "Bash",
            "tool_input": {
                "command": "git status",
                "description": "Do NOT run cargo install"
            },
            "session_id": "sess-123",
            "extra": "cargo install --path ."
        }"#;
        assert_eq!(
            extract_command(input),
            Some("git status".to_string()),
            "must extract only tool_input.command, ignoring other fields"
        );
    }

    // -- Verdict tests: assert that allow emits Allow and deny emits Deny --

    #[test]
    fn test_verdict_allow_for_read_only_command() {
        let input = r#"{"tool_name":"Bash","tool_input":{"command":"git status"}}"#;
        assert_eq!(evaluate(input), Verdict::Allow);
    }

    #[test]
    fn test_verdict_allow_for_unparseable_input() {
        assert_eq!(evaluate("not json"), Verdict::Allow);
    }

    #[test]
    fn test_verdict_allow_for_missing_command() {
        let input = r#"{"tool_name":"Bash","tool_input":{}}"#;
        assert_eq!(evaluate(input), Verdict::Allow);
    }

    #[test]
    fn test_verdict_deny_for_mutating_command() {
        let input = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#;
        let verdict = evaluate(input);
        match verdict {
            Verdict::Deny { reason } => {
                assert!(
                    reason.contains("matched: rm "),
                    "reason should include the matched pattern: {reason}"
                );
            }
            Verdict::Allow => panic!("expected Deny for mutating command"),
        }
    }

    #[test]
    fn test_verdict_deny_for_cargo_install() {
        let input = r#"{"tool_name":"Bash","tool_input":{"command":"cargo install --path ."}}"#;
        let verdict = evaluate(input);
        match verdict {
            Verdict::Deny { reason } => {
                assert!(
                    reason.contains("matched: cargo install"),
                    "reason should include the matched pattern: {reason}"
                );
            }
            Verdict::Allow => panic!("expected Deny for cargo install"),
        }
    }

    #[test]
    fn test_verdict_allow_when_cargo_install_only_in_context() {
        // "cargo install" appears in extra fields (conversation context) but
        // the actual command is safe — must Allow.
        let input = r#"{
            "tool_name": "Bash",
            "tool_input": {
                "command": "ls -la",
                "description": "Do NOT run cargo install"
            },
            "extra": "cargo install --path ."
        }"#;
        assert_eq!(
            evaluate(input),
            Verdict::Allow,
            "must allow when blocked pattern is only in context, not the command"
        );
    }

    #[test]
    fn test_deny_json_structure() {
        // Verify the JSON written to stdout matches Claude Code's expected
        // hookSpecificOutput schema.
        let input = r#"{"tool_name":"Bash","tool_input":{"command":"cargo install --path ."}}"#;
        let verdict = evaluate(input);
        let Verdict::Deny { reason } = verdict else {
            panic!("expected Deny");
        };

        let deny = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        });

        // Round-trip through to_writer to match what run() produces
        let mut buf = Vec::new();
        serde_json::to_writer(&mut buf, &deny).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();

        assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            parsed["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("cargo install")
        );
    }
}

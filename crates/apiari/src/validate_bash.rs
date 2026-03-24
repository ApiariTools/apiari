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

    let command = match extract_command(&input) {
        Some(cmd) => cmd,
        None => {
            // Not a Bash tool use or couldn't parse — allow
            return 0;
        }
    };

    match audit::classify_bash_command_with_devmode(&command) {
        BashClassification::ReadOnly => 0,
        BashClassification::PotentiallyMutating { matched_pattern } => {
            let reason = format!(
                "BLOCKED: coordinator attempted mutating Bash command (matched: {matched_pattern})"
            );
            eprintln!("{reason}");
            eprintln!("  command: {command}");

            // Emit structured JSON deny on stdout so Claude Code treats this
            // as a per-command block, not a hook error.
            let deny = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason
                }
            });
            println!("{deny}");
            0 // exit 0 — JSON carries the deny decision
        }
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
}

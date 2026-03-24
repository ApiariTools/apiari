//! PreToolUse hook — blocks mutating Bash commands before execution.
//!
//! Invoked by Claude Code as a PreToolUse hook. Reads the hook input JSON
//! from stdin, extracts **only** `tool_input.command`, classifies it, and
//! communicates the decision via structured JSON on stdout:
//!
//! - Allow: exit 0, no stdout (tool proceeds).
//! - Deny:  exit 0, stdout JSON with `permissionDecision: "deny"`.
//!
//! **Important:** Only the `tool_input.command` field is evaluated. All other
//! fields in the hook input (session context, description, transcript path,
//! etc.) are ignored to avoid false positives from conversation context.

use crate::buzz::coordinator::audit::{self, BashClassification};
use std::io::Read;

/// Run the validate-bash hook.
///
/// Always returns exit code 0. Blocking decisions are communicated via
/// structured JSON on stdout (`permissionDecision: "deny"`), not via
/// non-zero exit codes (which Claude Code treats as error feedback piped
/// back into conversation context).
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
            eprintln!("validate-bash: denied mutating command (matched: {matched_pattern})");
            // Emit structured deny decision on stdout so Claude Code blocks
            // the tool call cleanly without feeding error text into context.
            let reason = format!(
                "Blocked by coordinator policy: mutating Bash command (matched pattern: {matched_pattern})"
            );
            let output = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason
                }
            });
            println!("{output}");
            0
        }
    }
}

/// Extract the command string from the hook JSON input.
///
/// The hook input contains many fields (session_id, transcript_path, cwd,
/// tool_name, tool_input, tool_use_id, etc.). We deliberately extract
/// **only** `tool_input.command` and ignore everything else so that
/// conversation context, descriptions, or other metadata cannot trigger
/// false positives in the bash audit classifier.
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
    fn test_extract_command_full_hook_input() {
        // Claude Code sends a richer JSON payload; extract_command must still
        // return only the command string.
        let input = r#"{
            "session_id": "abc123",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/home/user/project",
            "permission_mode": "default",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {
                "command": "ls -la",
                "description": "List files to check cargo install status",
                "timeout": 120000,
                "run_in_background": false
            },
            "tool_use_id": "toolu_01ABC123"
        }"#;
        assert_eq!(extract_command(input), Some("ls -la".to_string()));
    }

    #[test]
    fn test_extract_command_ignores_context_with_blocked_patterns() {
        // Blocked patterns appearing in non-command fields must NOT trigger
        // the classifier.  extract_command must return only "ls -la".
        let input = r#"{
            "session_id": "sess-with-cargo-install-in-id",
            "transcript_path": "/tmp/cargo-install-transcript.jsonl",
            "cwd": "/home/user/cargo-install-test",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {
                "command": "ls -la",
                "description": "Do not run cargo install or rm -rf /"
            },
            "tool_use_id": "toolu_cargo_install"
        }"#;
        let cmd = extract_command(input).unwrap();
        assert_eq!(cmd, "ls -la");
        // Verify the classifier sees the command as safe
        let classification = audit::classify_bash_command(&cmd);
        assert_eq!(
            classification,
            BashClassification::ReadOnly,
            "command 'ls -la' must be ReadOnly regardless of surrounding context"
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
}

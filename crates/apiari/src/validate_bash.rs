//! PreToolUse hook — blocks mutating Bash commands before execution.
//!
//! Invoked by Claude Code as a PreToolUse hook. Reads the tool input JSON
//! from stdin, classifies the Bash command, and exits with:
//! - 0: allow (read-only command)
//! - 2: block (potentially mutating command)

use crate::buzz::coordinator::audit::{self, BashClassification};
use std::io::Read;

/// Run the validate-bash hook.
///
/// Returns exit code: 0 = allow, 2 = block.
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

    match audit::classify_bash_command(&command) {
        BashClassification::ReadOnly => 0,
        BashClassification::PotentiallyMutating { matched_pattern } => {
            eprintln!(
                "BLOCKED: coordinator attempted mutating Bash command (matched: {matched_pattern})"
            );
            eprintln!("  command: {command}");
            2
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
}

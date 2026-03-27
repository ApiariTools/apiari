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
use crate::config;
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
        BashClassification::PotentiallyMutating { matched_pattern } => {
            // Allow gh pr merge if the workspace has merge_prs capability enabled
            // AND the command is a single invocation (no chaining).
            if matched_pattern == "gh pr merge"
                && !contains_command_chaining(&command)
                && is_merge_allowed()
            {
                return Verdict::Allow;
            }
            Verdict::Deny {
                reason: format!(
                    "BLOCKED: coordinator attempted mutating Bash command (matched: {matched_pattern})"
                ),
            }
        }
    }
}

/// Check if the current workspace allows PR merging.
///
/// Scans `~/.config/apiari/workspaces/*.toml` for a workspace whose `root`
/// matches the current working directory (or is a parent of it). If found,
/// checks the `merge_prs` capability. Fails closed (returns false) if the
/// config can't be loaded or no matching workspace is found.
fn is_merge_allowed() -> bool {
    // Fail closed if home dir can't be determined — config_dir() falls back
    // to "." which could read arbitrary workspace configs from CWD.
    if dirs::home_dir().is_none() {
        return false;
    }

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let workspaces = match config::discover_workspaces() {
        Ok(ws) => ws,
        Err(_) => return false,
    };

    check_merge_allowed(&cwd, &workspaces)
}

/// Pure logic: check if merging is allowed for the given cwd and workspace list.
///
/// Separated from `is_merge_allowed()` so tests can inject workspaces directly
/// without mutating environment variables.
fn check_merge_allowed(cwd: &std::path::Path, workspaces: &[config::Workspace]) -> bool {
    // Find the most specific (longest root) matching workspace to handle
    // nested workspace roots correctly.
    let best = workspaces
        .iter()
        .filter(|ws| cwd.starts_with(&ws.config.root))
        .max_by_key(|ws| ws.config.root.as_os_str().len());

    if let Some(ws) = best {
        let caps = ws.config.capabilities.resolved(ws.config.authority);
        return match caps.merge_prs {
            config::MergePrsCapability::Bool(allowed) => allowed,
            // validate-bash can't determine the target branch from the command
            // line, so branch-scoped configs fail closed here. The coordinator
            // has full context and enforces branch-scoped merges itself.
            config::MergePrsCapability::Branches(_) => false,
        };
    }

    false
}

/// Check if a command contains shell chaining operators that could bypass
/// the allowlist (e.g. `gh pr merge 123 && git push`).
fn contains_command_chaining(command: &str) -> bool {
    // Check for shell operators that chain or compose commands.
    // This is intentionally conservative — reject anything suspicious.
    command.contains("&&")
        || command.contains("||")
        || command.contains('|')
        || command.contains(';')
        || command.contains('`')
        || command.contains("$(")
        || command.contains('&')
        || command.contains('\n')
        || command.contains('\r')
        || command.contains(">(")
        || command.contains("<(")
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
    use std::sync::Mutex;

    /// Tests that mutate HOME must hold this lock to avoid interfering with each other.
    /// Note: this only serializes tests within this module. Tests in other modules that
    /// read HOME could observe the temporary value. This is an accepted tradeoff — the
    /// pure-logic check_merge_allowed() tests cover most cases without env mutation;
    /// only the evaluate() end-to-end tests need HOME isolation.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that sets HOME on creation and restores (or removes) it on drop.
    /// Holds the HOME_LOCK for the duration, and handles panics via Drop.
    struct HomeGuard {
        orig: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        fn new(new_home: &std::path::Path) -> Self {
            let lock = HOME_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let orig = std::env::var("HOME").ok();
            // Safety: HOME_LOCK serializes all HOME-mutating tests in this module.
            unsafe { std::env::set_var("HOME", new_home) };
            Self { orig, _lock: lock }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.orig {
                Some(h) => unsafe { std::env::set_var("HOME", h) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    /// Build a test workspace by parsing a TOML config string.
    fn test_workspace(toml_content: &str) -> config::Workspace {
        let cfg: config::WorkspaceConfig = toml::from_str(toml_content).expect("invalid test TOML");
        config::Workspace {
            name: "test".to_string(),
            config: cfg,
        }
    }

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

    // -- evaluate() end-to-end tests for gh pr merge --

    #[test]
    fn test_evaluate_denies_gh_pr_merge_without_config() {
        // evaluate() should deny gh pr merge when no workspace config matches
        // (is_merge_allowed fails closed). Use empty temp HOME.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::new(tmp.path());

        let input = r#"{"tool_name":"Bash","tool_input":{"command":"gh pr merge 123 --repo Org/repo --squash --delete-branch"}}"#;
        match evaluate(input) {
            Verdict::Deny { reason } => {
                assert!(
                    reason.contains("gh pr merge"),
                    "reason should reference gh pr merge: {reason}"
                );
            }
            Verdict::Allow => panic!("expected Deny for gh pr merge without config"),
        }
    }

    #[test]
    fn test_evaluate_allows_gh_pr_merge_with_config() {
        // evaluate() should allow gh pr merge when workspace has merge_prs=true.
        let tmp = tempfile::tempdir().unwrap();
        let ws_dir = tmp.path().join(".config/apiari/workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let config_content = format!("root = {cwd:?}\n\n[capabilities]\nmerge_prs = true\n");
        std::fs::write(ws_dir.join("test.toml"), &config_content).unwrap();

        let _guard = HomeGuard::new(tmp.path());

        let input = r#"{"tool_name":"Bash","tool_input":{"command":"gh pr merge 123 --squash"}}"#;
        assert_eq!(
            evaluate(input),
            Verdict::Allow,
            "evaluate() should allow gh pr merge when merge_prs=true"
        );
    }

    #[test]
    fn test_evaluate_denies_gh_pr_merge_with_chaining() {
        // Command chaining must always be denied, even with merge_prs enabled.
        let tmp = tempfile::tempdir().unwrap();
        let ws_dir = tmp.path().join(".config/apiari/workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let config_content = format!("root = {cwd:?}\n\n[capabilities]\nmerge_prs = true\n");
        std::fs::write(ws_dir.join("test.toml"), &config_content).unwrap();

        let _guard = HomeGuard::new(tmp.path());

        let cases = &[
            r#"gh pr merge 123 --squash && git push"#,
            r#"gh pr merge 123 --squash; rm -rf /"#,
            r#"gh pr merge 123 --squash || echo fail"#,
            r#"gh pr merge 123 --squash | tee log"#,
            r#"gh pr merge `echo 123` --squash"#,
            r#"gh pr merge $(echo 123) --squash"#,
            "gh pr merge 123 --squash & rm -rf /",
            "gh pr merge 123 >(tee log) --squash",
            "gh pr merge 123 <(cat prs) --squash",
        ];

        // Newlines/carriage returns can't be embedded in JSON test strings,
        // so test contains_command_chaining directly for those.
        assert!(contains_command_chaining("gh pr merge 123\nrm -rf /"));
        assert!(contains_command_chaining("gh pr merge 123\rrm -rf /"));
        for cmd in cases {
            let input = format!(r#"{{"tool_name":"Bash","tool_input":{{"command":"{cmd}"}}}}"#,);
            match evaluate(&input) {
                Verdict::Deny { .. } => {} // expected
                Verdict::Allow => panic!("expected Deny for chained command: {cmd}"),
            }
        }
    }

    // -- check_merge_allowed tests: use injected workspaces, no env var mutation --

    #[test]
    fn test_merge_denied_when_no_workspaces() {
        let cwd = std::env::current_dir().unwrap();
        assert!(!check_merge_allowed(&cwd, &[]));
    }

    #[test]
    fn test_merge_allowed_when_capability_enabled() {
        let cwd = std::env::current_dir().unwrap();
        let toml = format!("root = {cwd:?}\n\n[capabilities]\nmerge_prs = true\n");
        let ws = test_workspace(&toml);
        assert!(check_merge_allowed(&cwd, &[ws]));
    }

    #[test]
    fn test_merge_denied_when_capability_disabled() {
        let cwd = std::env::current_dir().unwrap();
        let toml = format!("root = {cwd:?}\n\n[capabilities]\nmerge_prs = false\n");
        let ws = test_workspace(&toml);
        assert!(!check_merge_allowed(&cwd, &[ws]));
    }

    #[test]
    fn test_merge_denied_when_branch_scoped() {
        // validate-bash can't determine the target branch, so branch-scoped
        // configs fail closed. The coordinator enforces branch-scoped merges.
        let cwd = std::env::current_dir().unwrap();
        let toml = format!("root = {cwd:?}\n\n[capabilities]\nmerge_prs = [\"main\"]\n");
        let ws = test_workspace(&toml);
        assert!(!check_merge_allowed(&cwd, &[ws]));
    }

    #[test]
    fn test_merge_denied_when_observe_mode() {
        // Observe mode forces merge_prs off even if config says true.
        let cwd = std::env::current_dir().unwrap();
        let toml = format!(
            "root = {cwd:?}\nauthority = \"observe\"\n\n[capabilities]\nmerge_prs = true\n"
        );
        let ws = test_workspace(&toml);
        assert!(!check_merge_allowed(&cwd, &[ws]));
    }

    #[test]
    fn test_merge_uses_most_specific_workspace() {
        // Nested roots: inner workspace (merge_prs=false) should win over
        // outer workspace (merge_prs=true).
        let cwd = std::env::current_dir().unwrap();
        let parent = cwd.parent().unwrap();
        let outer_toml = format!("root = {parent:?}\n\n[capabilities]\nmerge_prs = true\n");
        let inner_toml = format!("root = {cwd:?}\n\n[capabilities]\nmerge_prs = false\n");
        let outer = test_workspace(&outer_toml);
        let inner = test_workspace(&inner_toml);
        // Order shouldn't matter — longest prefix wins.
        assert!(!check_merge_allowed(&cwd, &[outer, inner]));
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

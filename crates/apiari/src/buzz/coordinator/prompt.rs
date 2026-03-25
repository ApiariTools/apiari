//! System prompt builder for the buzz coordinator.
//!
//! Constructs a system prompt that includes open signals and recent memory,
//! so the coordinator always knows what's happening.

use crate::buzz::coordinator::memory::{MemoryCategory, MemoryEntry};
use crate::buzz::signal::SignalRecord;

/// The default coordinator prompt preamble (identity + role boundaries).
///
/// This is used when no custom `prompt_file` is configured.
pub fn default_preamble(name: &str) -> String {
    format!(
        "You are {name}, the coordinator for this workspace.\n\n\
         ## Identity\n\
         You are an ops coordinator — you plan work, monitor signals, triage issues, \
         answer questions about the workspace, and dispatch swarm workers to do coding tasks.\n\
         You are concise, proactive, and technically precise.\n\n\
         ## Role Boundaries\n\
         - You are NOT a coding assistant. NEVER write, edit, or generate code.\n\
         - When asked to implement, fix, build, or code anything: dispatch a swarm worker. \
           No exceptions — even for \"simple\" or \"quick\" changes.\n\
         - If swarm cannot dispatch to a repo (e.g. repo not recognized), STOP and tell the user. \
           Do NOT work around it by making changes yourself.\n\
         - You CAN use Bash freely for research and investigation (git log, gh pr view, \
           swarm status, curl APIs, sqlite3, ls, etc.).\n\
         - You must NEVER use Bash to modify the workspace: no creating/editing/deleting files, \
           no git add/commit/push, no curl -o/wget into repos, no echo/cat/sed writing to files. \
           The ONLY Bash writes allowed are to /tmp/ (for swarm --prompt-file), your persistent \
           memory file (see Persistent Memory section if present), and `.apiari/` (see below).\n\
         - You MAY use Write, Edit, or Bash to create and update files under `.apiari/`: \
           specifically `.apiari/context.md` and `.apiari/skills/*.md`. These are coordinator-owned \
           config files (project context and playbooks), NOT code. Do NOT write to any other \
           workspace paths — all code changes must go through swarm workers.\n\
         - You MAY also read and edit `~/.config/apiari/workspaces/{{workspace}}.toml` (the workspace \
           config file). After any edit, validate it with `apiari config validate --workspace <name>`. \
           If validation fails, fix the TOML before finishing.\n\
         - `/devmode on` temporarily unlocks file creation, `gh repo create`, `git clone`, \
           `git init`, and general file writes for 30 minutes. Use it when the user asks to \
           create a new repo or needs to write files. Always turn it off when done: `/devmode off`. \
           Check status with `/devmode` (no args) or `/devmode status`. \
           State file: `~/.local/state/apiari/.devmode` (JSON with `enabled_at` and `expires_at` UTC ISO 8601) — \
           you can also `cat` it directly. Intentionally outside `~/.config/apiari/` to prevent self-enabling.\n\
         - You CAN read code, investigate issues, check PR status, query signals, \
           and answer questions about the codebase.\n\
         - You already know your workspace context from this prompt. Do NOT use tools \
           to look up information that is already here — just answer directly.\n\
         - Only use tools when you need current data (PR status, file contents, etc.) \
           or when the user asks you to DO something.\n\
         - Keep responses short and direct.\n\n",
    )
}

/// Build the system prompt with signal and memory context.
///
/// `hook_playbooks` contains full playbook content triggered by signal hooks
/// for this specific session.
pub fn build_system_prompt(
    signals: &[SignalRecord],
    memory: &[MemoryEntry],
    extra_context: Option<&str>,
    name: Option<&str>,
    prompt_preamble: Option<&str>,
    hook_playbooks: Option<&str>,
) -> String {
    let name = name.unwrap_or("Bee");
    let mut prompt = String::new();

    // Custom prompt preamble replaces default identity/role sections
    if let Some(preamble) = prompt_preamble {
        prompt.push_str(preamble);
        if !preamble.ends_with('\n') {
            prompt.push('\n');
        }
        prompt.push('\n');
    } else {
        prompt.push_str(&default_preamble(name));
    }

    prompt.push_str(
        "## Signals\n\
         You have access to real-time signals from GitHub, Sentry, swarm workers, \
         Linear, email (IMAP), and Notion. \
         When signals arrive, you proactively notify the user about important events.\n\
         All review-queue sources (GitHub, Linear, email, Notion) produce signals \
         ending with `_review_queue` that auto-appear in the Reviews panel.\n\n",
    );

    // Separate CI signals from other signals for dedicated section
    let (ci_signals, other_signals): (Vec<&SignalRecord>, Vec<&SignalRecord>) =
        signals.iter().partition(|s| {
            s.source == "github"
                && (s.external_id.starts_with("ci-pass-")
                    || s.external_id.starts_with("ci-failure-"))
        });

    // Open signals section (non-CI)
    if other_signals.is_empty() {
        prompt.push_str("## Current Signals\nNo open signals.\n\n");
    } else {
        prompt.push_str("## Current Signals\n");
        for signal in &other_signals {
            prompt.push_str(&format!(
                "- [{severity}] [{source}] {title}",
                severity = signal.severity,
                source = signal.source,
                title = signal.title,
            ));
            if let Some(ref url) = signal.url {
                prompt.push_str(&format!(" ({url})"));
            }
            prompt.push('\n');
            if let Some(ref body) = signal.body {
                let truncated = if body.len() > 200 {
                    // Find a char boundary at or before byte 200
                    let end = body.floor_char_boundary(200);
                    format!("{}...", &body[..end])
                } else {
                    body.clone()
                };
                prompt.push_str(&format!("  {truncated}\n"));
            }
        }
        prompt.push('\n');
    }

    // Dedicated CI activity section
    if !ci_signals.is_empty() {
        prompt.push_str("## Recent CI Activity\n");
        prompt.push_str(
            "Proactively mention CI results to the user when relevant to PRs they are watching.\n",
        );
        for signal in &ci_signals {
            prompt.push_str(&format!("- {title}", title = signal.title));
            if let Some(ref url) = signal.url {
                prompt.push_str(&format!(" ({url})"));
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    // Memory section
    let observations: Vec<&MemoryEntry> = memory
        .iter()
        .filter(|e| e.category == MemoryCategory::Observation)
        .collect();
    let decisions: Vec<&MemoryEntry> = memory
        .iter()
        .filter(|e| e.category == MemoryCategory::Decision)
        .collect();
    let preferences: Vec<&MemoryEntry> = memory
        .iter()
        .filter(|e| e.category == MemoryCategory::Preference)
        .collect();

    if !memory.is_empty() {
        prompt.push_str("## Memory\n");
        if !preferences.is_empty() {
            prompt.push_str("**Preferences:**\n");
            for entry in &preferences {
                prompt.push_str(&format!("- {}\n", entry.content));
            }
        }
        if !decisions.is_empty() {
            prompt.push_str("**Recent decisions:**\n");
            for entry in decisions.iter().take(5) {
                prompt.push_str(&format!("- {}\n", entry.content));
            }
        }
        if !observations.is_empty() {
            prompt.push_str("**Observations:**\n");
            for entry in observations.iter().take(10) {
                prompt.push_str(&format!("- {}\n", entry.content));
            }
        }
        prompt.push('\n');
    }

    // Extra context (skills prompt — includes tool skills, context skill, playbook index, authority)
    if let Some(ctx) = extra_context {
        prompt.push_str("## Additional Context\n");
        prompt.push_str(ctx);
        prompt.push('\n');
    }

    // Hook-triggered playbook content (injected per-session when signal hooks fire)
    if let Some(playbooks) = hook_playbooks {
        prompt.push_str("\n## Active Playbooks\n");
        prompt.push_str("The following playbook instructions were activated for this session:\n\n");
        prompt.push_str(playbooks);
        if !playbooks.ends_with('\n') {
            prompt.push('\n');
        }
    }

    prompt
}

/// Build a concise signal summary for proactive notifications.
pub fn format_signal_summary(signals: &[SignalRecord]) -> String {
    if signals.is_empty() {
        return "No open signals.".to_string();
    }

    let mut summary = format!("{} open signal(s):\n", signals.len());
    for signal in signals.iter().take(10) {
        summary.push_str(&format!(
            "  [{severity}] [{source}] {title}\n",
            severity = signal.severity,
            source = signal.source,
            title = signal.title,
        ));
    }
    if signals.len() > 10 {
        summary.push_str(&format!("  ... and {} more\n", signals.len() - 10));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::{Severity, SignalStatus};
    use chrono::Utc;

    fn make_signal(source: &str, title: &str, severity: Severity) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.to_string(),
            external_id: "ext-1".to_string(),
            title: title.to_string(),
            body: Some("body text".to_string()),
            severity,
            status: SignalStatus::Open,
            url: Some("https://example.com".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
            snoozed_until: None,
        }
    }

    fn make_memory(category: MemoryCategory, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: 1,
            category,
            content: content.to_string(),
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_build_system_prompt_empty() {
        let prompt = build_system_prompt(&[], &[], None, None, None, None);
        assert!(prompt.contains("No open signals"));
        assert!(prompt.contains("Bee"));
    }

    #[test]
    fn test_build_system_prompt_with_signals() {
        let signals = vec![
            make_signal("sentry", "Server down", Severity::Critical),
            make_signal("github", "PR #42 failed CI", Severity::Warning),
        ];
        let prompt = build_system_prompt(&signals, &[], None, None, None, None);
        assert!(prompt.contains("Server down"));
        assert!(prompt.contains("PR #42 failed CI"));
        assert!(prompt.contains("[critical]"));
        assert!(prompt.contains("[warning]"));
    }

    #[test]
    fn test_build_system_prompt_with_memory() {
        let memory = vec![
            make_memory(MemoryCategory::Preference, "User prefers concise responses"),
            make_memory(
                MemoryCategory::Observation,
                "Sentry alerts spike on Mondays",
            ),
            make_memory(MemoryCategory::Decision, "Switched to WAL mode for SQLite"),
        ];
        let prompt = build_system_prompt(&[], &memory, None, None, None, None);
        assert!(prompt.contains("Preferences:"));
        assert!(prompt.contains("User prefers concise responses"));
        assert!(prompt.contains("Observations:"));
        assert!(prompt.contains("Recent decisions:"));
    }

    #[test]
    fn test_build_system_prompt_with_extra_context() {
        let prompt = build_system_prompt(&[], &[], Some("Running on macOS"), None, None, None);
        assert!(prompt.contains("Additional Context"));
        assert!(prompt.contains("Running on macOS"));
    }

    #[test]
    fn test_format_signal_summary_empty() {
        assert_eq!(format_signal_summary(&[]), "No open signals.");
    }

    #[test]
    fn test_format_signal_summary() {
        let signals = vec![make_signal("sentry", "Bug", Severity::Error)];
        let summary = format_signal_summary(&signals);
        assert!(summary.contains("1 open signal(s)"));
        assert!(summary.contains("Bug"));
    }

    #[test]
    fn test_default_preamble_safety_language() {
        let preamble = default_preamble("Bee");
        // Must contain all critical safety phrases
        assert!(
            preamble.contains("NEVER write, edit, or generate code"),
            "preamble missing 'NEVER write, edit, or generate code'"
        );
        assert!(
            preamble.contains("dispatch a swarm worker"),
            "preamble missing 'dispatch a swarm worker'"
        );
        assert!(
            preamble.contains("NEVER use Bash to modify the workspace"),
            "preamble missing 'NEVER use Bash to modify the workspace'"
        );
        assert!(
            preamble.contains("no git add/commit/push"),
            "preamble missing 'no git add/commit/push'"
        );
        assert!(
            preamble.contains("ONLY Bash writes allowed are to /tmp/"),
            "preamble missing '/tmp/ exception'"
        );
        assert!(
            preamble.contains(".apiari/context.md"),
            "preamble missing '.apiari/ write scope'"
        );
    }

    #[test]
    fn test_custom_preamble_replaces_default() {
        let prompt = build_system_prompt(&[], &[], None, None, Some("Custom identity."), None);
        // Custom preamble replaces default
        assert!(prompt.contains("Custom identity."));
        assert!(!prompt.contains("ops coordinator"));
        // But still has Signals section
        assert!(prompt.contains("## Signals"));
    }

    #[test]
    fn test_full_prompt_no_write_tool_instructions() {
        // With skills context that might mention tools
        let prompt = build_system_prompt(
            &[],
            &[],
            Some("## Swarm Workers\nUse swarm to dispatch."),
            None,
            None,
            None,
        );
        assert!(
            !prompt.contains("use the Write tool"),
            "prompt must not instruct to use Write tool"
        );
        assert!(
            !prompt.contains("use the Edit tool"),
            "prompt must not instruct to use Edit tool"
        );
    }

    #[test]
    fn test_ci_signals_in_dedicated_section() {
        let ci_pass = SignalRecord {
            id: 1,
            source: "github".to_string(),
            external_id: "ci-pass-30-12345".to_string(),
            title: "CI passed: add snooze (#30)".to_string(),
            body: None,
            severity: Severity::Info,
            status: SignalStatus::Open,
            url: Some("https://github.com/org/repo/actions/runs/12345".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            snoozed_until: None,
            metadata: None,
        };
        let ci_fail = SignalRecord {
            id: 2,
            source: "github".to_string(),
            external_id: "ci-failure-29-12346".to_string(),
            title: "CI failed: add morning brief (#29)".to_string(),
            body: None,
            severity: Severity::Error,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            snoozed_until: None,
            metadata: None,
        };
        let other = make_signal("sentry", "Server error spike", Severity::Warning);

        let prompt = build_system_prompt(&[ci_pass, ci_fail, other], &[], None, None, None, None);

        // CI signals appear in dedicated section with titles as-is (no redundant prefix)
        assert!(prompt.contains("## Recent CI Activity"));
        assert!(prompt.contains("- CI passed: add snooze (#30)"));
        assert!(prompt.contains("- CI failed: add morning brief (#29)"));

        // Non-CI signal appears in Current Signals, not in CI section
        assert!(prompt.contains("## Current Signals"));
        assert!(prompt.contains("Server error spike"));
    }

    #[test]
    fn test_no_ci_section_when_no_ci_signals() {
        let signals = vec![make_signal("sentry", "Bug", Severity::Error)];
        let prompt = build_system_prompt(&signals, &[], None, None, None, None);
        assert!(!prompt.contains("## Recent CI Activity"));
        assert!(prompt.contains("## Current Signals"));
        assert!(prompt.contains("Bug"));
    }

    #[test]
    fn test_hook_playbooks_included_when_present() {
        let playbook_content = "### Playbook: ci-triage\n\nStep 1: Check logs.\nStep 2: Fix it.";
        let prompt = build_system_prompt(&[], &[], None, None, None, Some(playbook_content));
        assert!(prompt.contains("## Active Playbooks"));
        assert!(prompt.contains("ci-triage"));
        assert!(prompt.contains("Step 1: Check logs."));
    }

    #[test]
    fn test_hook_playbooks_absent_when_none() {
        let prompt = build_system_prompt(&[], &[], None, None, None, None);
        assert!(!prompt.contains("## Active Playbooks"));
    }

    #[test]
    fn test_signal_body_truncation() {
        let long_body = "x".repeat(300);
        let signal = SignalRecord {
            body: Some(long_body),
            ..make_signal("sentry", "Bug", Severity::Error)
        };
        let prompt = build_system_prompt(&[signal], &[], None, None, None, None);
        assert!(prompt.contains("..."));
    }
}

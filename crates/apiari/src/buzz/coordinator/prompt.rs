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
           The ONLY writes allowed are to /tmp/ (for swarm --prompt-file) and your persistent \
           memory file (see Persistent Memory section if present).\n\
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
pub fn build_system_prompt(
    signals: &[SignalRecord],
    memory: &[MemoryEntry],
    extra_context: Option<&str>,
    name: Option<&str>,
    prompt_preamble: Option<&str>,
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
         You have access to real-time signals from GitHub, Sentry, and swarm workers. \
         When signals arrive, you proactively notify the user about important events.\n\n",
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
                    format!("{}...", &body[..200])
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
            let icon = if signal.external_id.starts_with("ci-pass-") {
                "passed"
            } else {
                "FAILED"
            };
            prompt.push_str(&format!("- CI {icon}: {title}", title = signal.title));
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

    // Extra context
    if let Some(ctx) = extra_context {
        prompt.push_str("## Additional Context\n");
        prompt.push_str(ctx);
        prompt.push('\n');
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
        let prompt = build_system_prompt(&[], &[], None, None, None);
        assert!(prompt.contains("No open signals"));
        assert!(prompt.contains("Bee"));
    }

    #[test]
    fn test_build_system_prompt_with_signals() {
        let signals = vec![
            make_signal("sentry", "Server down", Severity::Critical),
            make_signal("github", "PR #42 failed CI", Severity::Warning),
        ];
        let prompt = build_system_prompt(&signals, &[], None, None, None);
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
        let prompt = build_system_prompt(&[], &memory, None, None, None);
        assert!(prompt.contains("Preferences:"));
        assert!(prompt.contains("User prefers concise responses"));
        assert!(prompt.contains("Observations:"));
        assert!(prompt.contains("Recent decisions:"));
    }

    #[test]
    fn test_build_system_prompt_with_extra_context() {
        let prompt = build_system_prompt(&[], &[], Some("Running on macOS"), None, None);
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
            preamble.contains("ONLY writes allowed are to /tmp/"),
            "preamble missing '/tmp/ exception'"
        );
    }

    #[test]
    fn test_custom_preamble_replaces_default() {
        let prompt = build_system_prompt(&[], &[], None, None, Some("Custom identity."));
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
            metadata: None,
        };
        let other = make_signal("sentry", "Server error spike", Severity::Warning);

        let prompt = build_system_prompt(&[ci_pass, ci_fail, other], &[], None, None, None);

        // CI signals appear in dedicated section
        assert!(prompt.contains("## Recent CI Activity"));
        assert!(prompt.contains("CI passed: CI passed: add snooze (#30)"));
        assert!(prompt.contains("CI FAILED: CI failed: add morning brief (#29)"));

        // Non-CI signal appears in Current Signals, not in CI section
        assert!(prompt.contains("## Current Signals"));
        assert!(prompt.contains("Server error spike"));
    }

    #[test]
    fn test_no_ci_section_when_no_ci_signals() {
        let signals = vec![make_signal("sentry", "Bug", Severity::Error)];
        let prompt = build_system_prompt(&signals, &[], None, None, None);
        assert!(!prompt.contains("## Recent CI Activity"));
        assert!(prompt.contains("## Current Signals"));
        assert!(prompt.contains("Bug"));
    }

    #[test]
    fn test_signal_body_truncation() {
        let long_body = "x".repeat(300);
        let signal = SignalRecord {
            body: Some(long_body),
            ..make_signal("sentry", "Bug", Severity::Error)
        };
        let prompt = build_system_prompt(&[signal], &[], None, None, None);
        assert!(prompt.contains("..."));
    }
}

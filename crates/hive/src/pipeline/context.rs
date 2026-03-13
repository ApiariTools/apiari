//! Context stage — identifies relevant codebase files and patterns for a task.
//!
//! Takes a TASK.md and searches the codebase to produce a CONTEXT.md that
//! lists relevant files, key patterns, and explicit exclusions.

use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use color_eyre::eyre::Result;
use std::path::Path;

/// Result of the context stage.
pub struct TaskContext {
    /// The full CONTEXT.md content.
    pub context_md: String,
    /// Relevant file paths extracted from the output.
    pub relevant_files: Vec<String>,
}

/// Build the system prompt for the context identification stage.
pub fn build_context_prompt() -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a codebase analyst. Your job is to identify the files and patterns\n");
    prompt.push_str("relevant to implementing a given task specification.\n\n");
    prompt.push_str(
        "Use the tools available to you (Read, Glob, Grep, Bash) to search the codebase.\n",
    );
    prompt.push_str("Then output a structured CONTEXT.md in this exact format — no preamble:\n\n");
    prompt.push_str("```\n");
    prompt.push_str("# Context\n\n");
    prompt.push_str("## Relevant Files\n");
    prompt.push_str("- `path/to/file.rs` — brief description of why it's relevant\n");
    prompt.push_str("- `path/to/other.rs` — brief description\n");
    prompt.push_str("...\n\n");
    prompt.push_str("## Key Patterns\n");
    prompt.push_str("- Pattern name: brief description of the pattern and where it's used\n");
    prompt.push_str("...\n\n");
    prompt.push_str("## Not Relevant\n");
    prompt.push_str("- `path/to/skip.rs` — why this is NOT relevant (prevents wasted time)\n");
    prompt.push_str("```\n\n");
    prompt.push_str("Rules:\n");
    prompt.push_str("- List 5-15 relevant files, ordered by importance\n");
    prompt.push_str("- Include file paths relative to the repo root\n");
    prompt.push_str(
        "- Key Patterns: identify 2-5 architectural patterns the implementer needs to follow\n",
    );
    prompt.push_str(
        "- Not Relevant: list 2-4 files that look relevant but aren't (saves the agent time)\n",
    );
    prompt.push_str("- Output raw markdown, NOT wrapped in a code fence\n");
    prompt
}

/// Identify relevant codebase context for a task.
///
/// Spawns a Claude session that searches the codebase and produces CONTEXT.md.
pub async fn identify_context(task_md: &str, repo_path: Option<&Path>) -> Result<TaskContext> {
    let system_prompt = build_context_prompt();

    let opts = SessionOptions {
        system_prompt: Some(system_prompt),
        model: Some("sonnet".into()),
        max_turns: Some(7),
        working_dir: repo_path.map(|p| p.to_path_buf()),
        allowed_tools: vec!["Read".into(), "Glob".into(), "Grep".into(), "Bash".into()],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to spawn Claude session for context: {e}"))?;

    session
        .send_message(task_md)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to send task to Claude: {e}"))?;

    session.close_stdin();

    let mut response_text = String::new();
    loop {
        match session.next_event().await {
            Ok(Some(Event::Assistant { message, .. })) => {
                for block in &message.message.content {
                    if let ContentBlock::Text { text } = block {
                        response_text.push_str(text);
                    }
                }
            }
            Ok(Some(Event::Result(result))) => {
                if response_text.is_empty()
                    && let Some(text) = &result.result
                {
                    response_text = text.clone();
                }
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => {
                return Err(color_eyre::eyre::eyre!(
                    "error reading Claude response: {e}"
                ));
            }
        }
    }

    let context_md = response_text.trim().to_string();
    let relevant_files = parse_relevant_files(&context_md);

    Ok(TaskContext {
        context_md,
        relevant_files,
    })
}

/// Parse file paths from the `## Relevant Files` section of CONTEXT.md.
pub fn parse_relevant_files(context_md: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut in_section = false;

    for line in context_md.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("## Relevant Files") {
            in_section = true;
            continue;
        }
        if trimmed.starts_with("## ") && in_section {
            break;
        }

        if in_section {
            // Match `- \`path/to/file\` — description` or `- \`path/to/file\``
            if let Some(rest) = trimmed.strip_prefix("- `")
                && let Some(end) = rest.find('`')
            {
                let path = rest[..end].trim().to_string();
                if !path.is_empty() {
                    files.push(path);
                }
            }
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_context_prompt_content() {
        let prompt = build_context_prompt();
        assert!(prompt.contains("codebase analyst"));
        assert!(prompt.contains("Relevant Files"));
        assert!(prompt.contains("Key Patterns"));
        assert!(prompt.contains("Not Relevant"));
    }

    #[test]
    fn parse_relevant_files_standard() {
        let md = "\
# Context

## Relevant Files
- `src/main.rs` — entry point
- `src/auth/mod.rs` — auth module
- `src/auth/login.rs` — login handler

## Key Patterns
- MVC pattern
";
        let files = parse_relevant_files(md);
        assert_eq!(
            files,
            vec!["src/main.rs", "src/auth/mod.rs", "src/auth/login.rs",]
        );
    }

    #[test]
    fn parse_relevant_files_empty_section() {
        let md = "\
## Relevant Files

## Key Patterns
";
        let files = parse_relevant_files(md);
        assert!(files.is_empty());
    }

    #[test]
    fn parse_relevant_files_no_section() {
        let md = "# Context\n## Key Patterns\n- something";
        let files = parse_relevant_files(md);
        assert!(files.is_empty());
    }

    #[test]
    fn parse_relevant_files_at_end_of_file() {
        let md = "\
## Relevant Files
- `src/lib.rs` — library root
- `Cargo.toml` — dependencies
";
        let files = parse_relevant_files(md);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn parse_relevant_files_with_descriptions() {
        let md = "\
## Relevant Files
- `src/main.rs` — main entry point with CLI parsing
- `src/config.rs`
";
        let files = parse_relevant_files(md);
        assert_eq!(files, vec!["src/main.rs", "src/config.rs"]);
    }
}

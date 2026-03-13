//! Refine stage — transforms a raw prompt into a structured TASK.md.
//!
//! Uses a short Claude session (sonnet, 3 turns, read-only tools) to
//! expand a one-line task description into a well-structured specification
//! with scope, acceptance criteria, anti-goals, and notes.

use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use color_eyre::eyre::Result;

/// Result of the refine stage.
pub struct RefinedTask {
    /// The full TASK.md content.
    pub task_md: String,
    /// Short title extracted from the task.
    pub title: String,
    /// Acceptance criteria parsed from the markdown.
    pub acceptance_criteria: Vec<String>,
}

/// Build the system prompt for the refine stage.
pub fn build_refine_prompt(conventions: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a task specification writer for a software engineering team.\n\n");
    prompt.push_str(
        "Your job is to take a raw task description and produce a structured task specification.\n",
    );
    prompt.push_str("Output ONLY the task specification in this exact markdown format — no preamble, no commentary:\n\n");
    prompt.push_str("```\n");
    prompt.push_str("# Task: <concise title>\n\n");
    prompt.push_str(
        "## Description\n<2-4 sentence description of what needs to be done and why>\n\n",
    );
    prompt.push_str("## Scope\n<bullet list of what is in scope>\n\n");
    prompt.push_str("## Acceptance Criteria\n- [ ] <criterion 1>\n- [ ] <criterion 2>\n...\n\n");
    prompt.push_str("## Anti-Goals\n<bullet list of what is explicitly NOT in scope>\n\n");
    prompt.push_str("## Notes\n<any relevant technical notes, constraints, or context>\n");
    prompt.push_str("```\n\n");
    prompt.push_str("Rules:\n");
    prompt.push_str("- Keep the title under 60 characters\n");
    prompt.push_str("- 3-7 acceptance criteria, each testable and specific\n");
    prompt.push_str("- Anti-goals prevent scope creep — list 2-4 things the agent should NOT do\n");
    prompt.push_str("- Use the codebase context (if available) to make criteria specific\n");
    prompt.push_str("- Output raw markdown, NOT wrapped in a code fence\n");

    if let Some(conventions) = conventions {
        prompt.push_str("\n## Workspace Conventions\n");
        prompt.push_str(conventions);
        prompt.push('\n');
    }

    prompt
}

/// Refine a raw prompt into a structured TASK.md.
///
/// Spawns a short Claude session to transform the prompt.
/// Returns `Err` if Claude CLI is unavailable.
pub async fn refine(raw_prompt: &str, conventions: Option<&str>) -> Result<RefinedTask> {
    let system_prompt = build_refine_prompt(conventions);

    let opts = SessionOptions {
        system_prompt: Some(system_prompt),
        model: Some("sonnet".into()),
        max_turns: Some(3),
        allowed_tools: vec!["Read".into(), "Glob".into(), "Grep".into()],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to spawn Claude session for refine: {e}"))?;

    session
        .send_message(raw_prompt)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to send message to Claude: {e}"))?;

    // Close stdin so Claude knows we're done sending.
    session.close_stdin();

    // Collect the response text from assistant turns.
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
                // Capture result text as fallback if no assistant content was extracted.
                if response_text.is_empty()
                    && let Some(text) = &result.result
                {
                    response_text = text.clone();
                }
                break;
            }
            Ok(Some(_)) => {} // Skip system, user, stream events
            Ok(None) => break,
            Err(e) => {
                return Err(color_eyre::eyre::eyre!(
                    "error reading Claude response: {e}"
                ));
            }
        }
    }

    let task_md = response_text.trim().to_string();
    let title = parse_title(&task_md);
    let acceptance_criteria = parse_acceptance_criteria(&task_md);

    Ok(RefinedTask {
        task_md,
        title,
        acceptance_criteria,
    })
}

/// Extract the title from a TASK.md (the `# Task: <title>` line).
pub fn parse_title(task_md: &str) -> String {
    for line in task_md.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# Task:") {
            return rest.trim().to_string();
        }
        // Also handle `# <title>` without the "Task:" prefix
        if let Some(rest) = trimmed.strip_prefix("# ")
            && !rest.starts_with('#')
        {
            return rest.trim().to_string();
        }
    }
    "Untitled Task".to_string()
}

/// Parse acceptance criteria from TASK.md (checkbox items).
pub fn parse_acceptance_criteria(task_md: &str) -> Vec<String> {
    let mut criteria = Vec::new();
    let mut in_criteria_section = false;

    for line in task_md.lines() {
        let trimmed = line.trim();

        // Detect the acceptance criteria section header
        if trimmed.starts_with("## Acceptance Criteria") {
            in_criteria_section = true;
            continue;
        }

        // A new section header ends the criteria section
        if trimmed.starts_with("## ") && in_criteria_section {
            break;
        }

        if in_criteria_section {
            // Match checkbox items: `- [ ] criterion` or `- [x] criterion`
            if let Some(rest) = trimmed
                .strip_prefix("- [ ] ")
                .or_else(|| trimmed.strip_prefix("- [x] "))
            {
                let criterion = rest.trim().to_string();
                if !criterion.is_empty() {
                    criteria.push(criterion);
                }
            }
        }
    }

    criteria
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_refine_prompt_without_conventions() {
        let prompt = build_refine_prompt(None);
        assert!(prompt.contains("task specification writer"));
        assert!(prompt.contains("Acceptance Criteria"));
        assert!(!prompt.contains("Workspace Conventions"));
    }

    #[test]
    fn build_refine_prompt_with_conventions() {
        let prompt = build_refine_prompt(Some("Use Rust. Test everything."));
        assert!(prompt.contains("Workspace Conventions"));
        assert!(prompt.contains("Use Rust. Test everything."));
    }

    #[test]
    fn parse_title_standard() {
        let md = "# Task: Add rate limiting to API\n\n## Description\n...";
        assert_eq!(parse_title(md), "Add rate limiting to API");
    }

    #[test]
    fn parse_title_without_task_prefix() {
        let md = "# Add rate limiting to API\n\n## Description\n...";
        assert_eq!(parse_title(md), "Add rate limiting to API");
    }

    #[test]
    fn parse_title_missing() {
        let md = "## Description\nSome task";
        assert_eq!(parse_title(md), "Untitled Task");
    }

    #[test]
    fn parse_acceptance_criteria_standard() {
        let md = "\
# Task: Add auth

## Scope
- Login flow

## Acceptance Criteria
- [ ] Users can log in with email/password
- [ ] Failed login shows error message
- [ ] Session expires after 24 hours

## Anti-Goals
- No OAuth
";
        let criteria = parse_acceptance_criteria(md);
        assert_eq!(criteria.len(), 3);
        assert_eq!(criteria[0], "Users can log in with email/password");
        assert_eq!(criteria[1], "Failed login shows error message");
        assert_eq!(criteria[2], "Session expires after 24 hours");
    }

    #[test]
    fn parse_acceptance_criteria_with_checked_items() {
        let md = "\
## Acceptance Criteria
- [x] Already done
- [ ] Not done yet

## Notes
";
        let criteria = parse_acceptance_criteria(md);
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[0], "Already done");
        assert_eq!(criteria[1], "Not done yet");
    }

    #[test]
    fn parse_acceptance_criteria_empty_section() {
        let md = "\
## Acceptance Criteria

## Notes
Nothing here.
";
        let criteria = parse_acceptance_criteria(md);
        assert!(criteria.is_empty());
    }

    #[test]
    fn parse_acceptance_criteria_no_section() {
        let md = "# Task: Something\n## Description\nBlah";
        let criteria = parse_acceptance_criteria(md);
        assert!(criteria.is_empty());
    }

    #[test]
    fn parse_acceptance_criteria_at_end_of_file() {
        let md = "\
## Acceptance Criteria
- [ ] First
- [ ] Second
";
        let criteria = parse_acceptance_criteria(md);
        assert_eq!(criteria.len(), 2);
    }
}

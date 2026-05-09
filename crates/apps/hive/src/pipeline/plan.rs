//! Plan stage — produces a step-by-step implementation plan from TASK.md + CONTEXT.md.
//!
//! The plan stage takes the refined task and identified context, then asks
//! Claude to produce a concrete implementation plan with ordered steps,
//! testing strategy, and risk assessment.

use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use color_eyre::eyre::Result;

/// Result of the plan stage.
pub struct TaskPlan {
    /// The full PLAN.md content.
    pub plan_md: String,
    /// Numbered steps extracted from the plan.
    pub steps: Vec<String>,
}

/// Build the system prompt for the planning stage.
pub fn build_plan_prompt() -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a software implementation planner. Given a task specification and\n");
    prompt.push_str("codebase context, produce a detailed step-by-step implementation plan.\n\n");
    prompt.push_str("Output ONLY the plan in this exact markdown format — no preamble:\n\n");
    prompt.push_str("```\n");
    prompt.push_str("# Plan\n\n");
    prompt.push_str("## Approach\n");
    prompt.push_str("<1-3 sentences describing the overall approach>\n\n");
    prompt.push_str("## Steps\n");
    prompt.push_str("1. **<step title>** — <description with specific file paths>\n");
    prompt.push_str("2. **<step title>** — <description>\n");
    prompt.push_str("...\n\n");
    prompt.push_str("## Testing Strategy\n");
    prompt.push_str("- <how to verify each step works>\n\n");
    prompt.push_str("## Risks\n");
    prompt.push_str("- <potential issues and mitigations>\n");
    prompt.push_str("```\n\n");
    prompt.push_str("Rules:\n");
    prompt.push_str("- 3-10 steps, ordered by implementation dependency\n");
    prompt.push_str("- Each step should reference specific files to create/modify\n");
    prompt.push_str("- Steps should be small enough for an agent to complete in one go\n");
    prompt.push_str("- Include concrete file paths from the context\n");
    prompt.push_str("- Testing strategy should be actionable (specific commands to run)\n");
    prompt.push_str("- Output raw markdown, NOT wrapped in a code fence\n");
    prompt
}

/// Produce a step-by-step implementation plan from task + context.
pub async fn create_plan(task_md: &str, context_md: &str) -> Result<TaskPlan> {
    let system_prompt = build_plan_prompt();

    let input =
        format!("## Task Specification\n\n{task_md}\n\n---\n\n## Codebase Context\n\n{context_md}");

    let opts = SessionOptions {
        system_prompt: Some(system_prompt),
        model: Some("sonnet".into()),
        max_turns: Some(5),
        allowed_tools: vec!["Read".into(), "Glob".into(), "Grep".into()],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to spawn Claude session for plan: {e}"))?;

    session
        .send_message(&input)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to send task+context to Claude: {e}"))?;

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

    let plan_md = response_text.trim().to_string();
    let steps = parse_steps(&plan_md);

    Ok(TaskPlan { plan_md, steps })
}

/// Parse numbered steps from the `## Steps` section of PLAN.md.
pub fn parse_steps(plan_md: &str) -> Vec<String> {
    let mut steps = Vec::new();
    let mut in_section = false;

    for line in plan_md.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("## Steps") {
            in_section = true;
            continue;
        }
        if trimmed.starts_with("## ") && in_section {
            break;
        }

        if in_section {
            // Match `1. **title** — description` or `1. description`
            if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                // Skip the remaining digits and the `. ` separator
                let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                if let Some(rest) = rest.strip_prefix(". ") {
                    let step = rest.trim().to_string();
                    if !step.is_empty() {
                        steps.push(step);
                    }
                }
            }
        }
    }

    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plan_prompt_content() {
        let prompt = build_plan_prompt();
        assert!(prompt.contains("implementation planner"));
        assert!(prompt.contains("Steps"));
        assert!(prompt.contains("Testing Strategy"));
        assert!(prompt.contains("Risks"));
    }

    #[test]
    fn parse_steps_standard() {
        let md = "\
# Plan

## Approach
Add rate limiting middleware.

## Steps
1. **Create middleware** — Add `src/middleware/rate_limit.rs`
2. **Add config** — Update `src/config.rs` with rate limit settings
3. **Wire up** — Register middleware in `src/main.rs`
4. **Add tests** — Unit tests for the middleware

## Testing Strategy
- Run `cargo test`
";
        let steps = parse_steps(md);
        assert_eq!(steps.len(), 4);
        assert!(steps[0].contains("Create middleware"));
        assert!(steps[3].contains("Add tests"));
    }

    #[test]
    fn parse_steps_empty_section() {
        let md = "## Steps\n\n## Testing Strategy\n";
        let steps = parse_steps(md);
        assert!(steps.is_empty());
    }

    #[test]
    fn parse_steps_no_section() {
        let md = "# Plan\n## Approach\nDo stuff.";
        let steps = parse_steps(md);
        assert!(steps.is_empty());
    }

    #[test]
    fn parse_steps_at_end_of_file() {
        let md = "\
## Steps
1. First step
2. Second step
";
        let steps = parse_steps(md);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0], "First step");
        assert_eq!(steps[1], "Second step");
    }

    #[test]
    fn parse_steps_double_digit() {
        let md = "\
## Steps
1. Step one
10. Step ten
";
        let steps = parse_steps(md);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0], "Step one");
        assert_eq!(steps[1], "Step ten");
    }
}

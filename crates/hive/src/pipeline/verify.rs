//! Verify stage — checks agent output against TASK.md acceptance criteria.
//!
//! Reads `.task/TASK.md` from a worktree, inspects the diff/code/tests,
//! and reports per-criterion pass/fail with evidence.

use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use color_eyre::eyre::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Result of the verify stage.
pub struct VerifyResult {
    /// Whether all criteria passed.
    pub passed: bool,
    /// Per-criterion results.
    pub criteria: Vec<CriterionResult>,
    /// Overall summary from Claude.
    pub summary: String,
}

/// Result for a single acceptance criterion.
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
    pub evidence: String,
}

/// Build the system prompt for the verify stage.
pub fn build_verify_prompt() -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a code reviewer verifying that a task was completed correctly.\n\n");
    prompt
        .push_str("You will receive the task specification (TASK.md) with acceptance criteria.\n");
    prompt.push_str("Your job is to inspect the code changes, run tests if needed, and verify\n");
    prompt.push_str("each acceptance criterion.\n\n");
    prompt.push_str(
        "Use the tools available to you (Read, Glob, Grep, Bash) to inspect the code.\n\n",
    );
    prompt.push_str("Output your verification in this exact format — no preamble:\n\n");
    prompt.push_str("```\n");
    prompt.push_str("## Verification Results\n\n");
    prompt.push_str("### Criterion: <criterion text>\n");
    prompt.push_str("**Result:** PASS | FAIL\n");
    prompt.push_str("**Evidence:** <what you found that proves pass/fail>\n\n");
    prompt.push_str("### Criterion: <criterion text>\n");
    prompt.push_str("**Result:** PASS | FAIL\n");
    prompt.push_str("**Evidence:** <evidence>\n\n");
    prompt.push_str("...\n\n");
    prompt.push_str("## Summary\n");
    prompt.push_str("<1-3 sentence overall assessment>\n");
    prompt.push_str("```\n\n");
    prompt.push_str("Rules:\n");
    prompt.push_str("- Check EVERY acceptance criterion from TASK.md\n");
    prompt.push_str("- Run `cargo test` or equivalent to verify tests pass\n");
    prompt.push_str("- Check for common issues: missing error handling, untested edge cases\n");
    prompt.push_str("- Be specific in evidence — cite file paths and line numbers\n");
    prompt.push_str("- Output raw markdown, NOT wrapped in a code fence\n");
    prompt
}

/// Verify a completed worktree against its TASK.md acceptance criteria.
pub async fn verify(worktree_path: &Path) -> Result<VerifyResult> {
    let task_md_path = worktree_path.join(".task").join("TASK.md");
    let task_md = std::fs::read_to_string(&task_md_path).map_err(|e| {
        color_eyre::eyre::eyre!(
            "failed to read {}: {e} — is this a pipeline worktree?",
            task_md_path.display()
        )
    })?;

    let system_prompt = build_verify_prompt();

    let opts = SessionOptions {
        system_prompt: Some(system_prompt),
        model: Some("sonnet".into()),
        max_turns: Some(7),
        working_dir: Some(worktree_path.to_path_buf()),
        allowed_tools: vec!["Read".into(), "Glob".into(), "Grep".into(), "Bash".into()],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to spawn Claude session for verify: {e}"))?;

    let message = format!(
        "Verify that this task was completed correctly. Check each acceptance criterion.\n\n{task_md}"
    );
    session
        .send_message(&message)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to send verify request: {e}"))?;

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

    let response = response_text.trim().to_string();
    let criteria = parse_verification_results(&response);
    let passed = criteria.iter().all(|c| c.passed);
    let summary = parse_summary(&response);

    Ok(VerifyResult {
        passed,
        criteria,
        summary,
    })
}

/// Resolve a worktree path from its ID by reading `.swarm/state.json`.
pub fn resolve_worktree_path(workspace_root: &Path, worktree_id: &str) -> Result<PathBuf> {
    let state_path = workspace_root.join(".swarm").join("state.json");
    let content = std::fs::read_to_string(&state_path)
        .map_err(|e| color_eyre::eyre::eyre!("failed to read {}: {e}", state_path.display()))?;

    let state: SwarmState = serde_json::from_str(&content)
        .map_err(|e| color_eyre::eyre::eyre!("failed to parse swarm state: {e}"))?;

    for wt in &state.worktrees {
        if wt.id == worktree_id {
            return Ok(wt.worktree_path.clone());
        }
    }

    color_eyre::eyre::bail!(
        "worktree {worktree_id:?} not found in {}",
        state_path.display()
    )
}

/// Mirror type for deserializing `.swarm/state.json`.
#[derive(Debug, Deserialize)]
struct SwarmState {
    #[serde(default)]
    worktrees: Vec<WorktreeEntry>,
}

#[derive(Debug, Deserialize)]
struct WorktreeEntry {
    id: String,
    #[serde(default)]
    worktree_path: PathBuf,
}

/// Parse per-criterion results from the verification response.
pub fn parse_verification_results(response: &str) -> Vec<CriterionResult> {
    let mut results = Vec::new();
    let mut current_criterion = None;
    let mut current_passed = None;
    let mut current_evidence = None;

    for line in response.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("### Criterion:") {
            // Save the previous criterion if we have one
            if let (Some(criterion), Some(passed)) =
                (current_criterion.take(), current_passed.take())
            {
                results.push(CriterionResult {
                    criterion,
                    passed,
                    evidence: current_evidence.take().unwrap_or_default(),
                });
            }
            current_criterion = Some(rest.trim().to_string());
            current_passed = None;
            current_evidence = None;
        } else if let Some(rest) = trimmed.strip_prefix("**Result:**") {
            let result_text = rest.trim().to_uppercase();
            current_passed = Some(result_text.starts_with("PASS"));
        } else if let Some(rest) = trimmed.strip_prefix("**Evidence:**") {
            current_evidence = Some(rest.trim().to_string());
        }
    }

    // Save the last criterion
    if let (Some(criterion), Some(passed)) = (current_criterion.take(), current_passed.take()) {
        results.push(CriterionResult {
            criterion,
            passed,
            evidence: current_evidence.take().unwrap_or_default(),
        });
    }

    results
}

/// Parse the summary section from the verification response.
pub fn parse_summary(response: &str) -> String {
    let mut in_summary = false;
    let mut summary_lines = Vec::new();

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## Summary") {
            in_summary = true;
            continue;
        }
        if trimmed.starts_with("## ") && in_summary {
            break;
        }
        if in_summary && !trimmed.is_empty() {
            summary_lines.push(trimmed.to_string());
        }
    }

    summary_lines.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_verify_prompt_content() {
        let prompt = build_verify_prompt();
        assert!(prompt.contains("code reviewer"));
        assert!(prompt.contains("acceptance criterion"));
        assert!(prompt.contains("PASS | FAIL"));
    }

    #[test]
    fn parse_verification_results_standard() {
        let response = "\
## Verification Results

### Criterion: Users can log in with email/password
**Result:** PASS
**Evidence:** Login endpoint at src/auth/login.rs:45 accepts email/password.

### Criterion: Failed login shows error message
**Result:** FAIL
**Evidence:** No error message UI component found. Only a 401 status is returned.

### Criterion: Session expires after 24 hours
**Result:** PASS
**Evidence:** Session config in src/config.rs:12 sets TTL to 86400 seconds.

## Summary
2 of 3 criteria pass. The error message UI is missing.
";
        let results = parse_verification_results(response);
        assert_eq!(results.len(), 3);
        assert!(results[0].passed);
        assert_eq!(results[0].criterion, "Users can log in with email/password");
        assert!(!results[1].passed);
        assert!(results[1].evidence.contains("No error message"));
        assert!(results[2].passed);
    }

    #[test]
    fn parse_verification_results_empty() {
        let response = "## Verification Results\n\n## Summary\nNothing to verify.";
        let results = parse_verification_results(response);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_verification_results_all_pass() {
        let response = "\
### Criterion: Step 1
**Result:** PASS
**Evidence:** Done.

### Criterion: Step 2
**Result:** PASS
**Evidence:** Also done.
";
        let results = parse_verification_results(response);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|c| c.passed));
    }

    #[test]
    fn parse_summary_standard() {
        let response = "\
## Verification Results
...

## Summary
All criteria pass. The implementation is solid and well-tested.
";
        let summary = parse_summary(response);
        assert!(summary.contains("All criteria pass"));
    }

    #[test]
    fn parse_summary_empty() {
        let response = "## Verification Results\nSome stuff.";
        let summary = parse_summary(response);
        assert!(summary.is_empty());
    }

    #[test]
    fn resolve_worktree_path_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let swarm_dir = tmp.path().join(".swarm");
        std::fs::create_dir_all(&swarm_dir).unwrap();
        std::fs::write(
            swarm_dir.join("state.json"),
            r#"{"worktrees":[{"id":"hive-1","branch":"swarm/test","worktree_path":"/tmp/wt/hive-1"}]}"#,
        )
        .unwrap();

        let path = resolve_worktree_path(tmp.path(), "hive-1").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/wt/hive-1"));
    }

    #[test]
    fn resolve_worktree_path_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let swarm_dir = tmp.path().join(".swarm");
        std::fs::create_dir_all(&swarm_dir).unwrap();
        std::fs::write(swarm_dir.join("state.json"), r#"{"worktrees":[]}"#).unwrap();

        let result = resolve_worktree_path(tmp.path(), "hive-99");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn resolve_worktree_path_no_state_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = resolve_worktree_path(tmp.path(), "hive-1");
        assert!(result.is_err());
    }
}

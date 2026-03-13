//! Dispatch stage — creates a swarm worktree seeded with pipeline artifacts.
//!
//! Builds the agent prompt that instructs it to read `.task/` artifacts
//! and executes via the Worker subprocess interface.

/// Build the agent prompt that tells it to read `.task/` artifacts.
///
/// When `repo` is provided, the prompt clarifies which repo the agent is working in.
pub fn build_dispatch_prompt(task_title: &str, repo: Option<&str>) -> String {
    let repo_line = match repo {
        Some(r) => format!("\nTarget repo: {r}\n"),
        None => String::new(),
    };
    format!(
        "\
Task: {task_title}
{repo_line}
A `.task/` directory has been seeded in this worktree with structured artifacts:
- `.task/TASK.md` — Task definition with scope and acceptance criteria
- `.task/CONTEXT.md` — Relevant codebase files and patterns
- `.task/PLAN.md` — Step-by-step implementation plan

## Instructions

1. Read ALL `.task/` files before writing any code.
2. Follow the steps in PLAN.md exactly. Do not add, skip, or reorder steps.
3. TASK.md defines your scope. The **Anti-Goals** section lists things you must NOT do.
4. Do NOT refactor, reorganize, or \"improve\" code outside the task scope.
5. Do NOT modify files that are not mentioned in the plan unless strictly necessary.
6. Update `.task/PROGRESS.md` as you complete each step.
7. Do NOT commit the `.task/` directory. Add it to `.gitignore` if it is not already ignored.
8. When done, create a PR with `gh pr create`. The PR should only contain changes described in the plan.

IMPORTANT: Stay within scope. A good PR is small and focused. If you notice other things \
that could be improved, ignore them — they are out of scope."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dispatch_prompt_includes_title() {
        let prompt = build_dispatch_prompt("Add rate limiting", None);
        assert!(prompt.contains("Task: Add rate limiting"));
    }

    #[test]
    fn build_dispatch_prompt_includes_repo() {
        let prompt = build_dispatch_prompt("Add rate limiting", Some("hive"));
        assert!(prompt.contains("Target repo: hive"));
    }

    #[test]
    fn build_dispatch_prompt_no_repo_line_when_none() {
        let prompt = build_dispatch_prompt("Add rate limiting", None);
        assert!(!prompt.contains("Target repo:"));
    }

    #[test]
    fn build_dispatch_prompt_references_artifacts() {
        let prompt = build_dispatch_prompt("test", None);
        assert!(prompt.contains("TASK.md"));
        assert!(prompt.contains("CONTEXT.md"));
        assert!(prompt.contains("PLAN.md"));
        assert!(prompt.contains("PROGRESS.md"));
    }

    #[test]
    fn build_dispatch_prompt_includes_instructions() {
        let prompt = build_dispatch_prompt("test", None);
        assert!(prompt.contains("Read ALL `.task/` files"));
        assert!(prompt.contains("Follow the steps in PLAN.md exactly"));
        assert!(prompt.contains("Anti-Goals"));
        assert!(prompt.contains("Stay within scope"));
        assert!(prompt.contains("gh pr create"));
    }
}

//! GitHub CLI skill — teaches the coordinator how to use `gh`.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if ctx.repos.is_empty() {
        return None;
    }

    let repos_list = ctx.repos.join(", ");

    Some(format!(
        "## GitHub\n\
         Repos: {repos_list}\n\
         Use the `gh` CLI (already authenticated):\n\
         - List open PRs: `gh pr list --repo {{repo}}`\n\
         - PR details: `gh pr view {{number}} --repo {{repo}}`\n\
         - PR checks: `gh pr checks {{number}} --repo {{repo}}`\n\
         - List issues: `gh issue list --repo {{repo}}`\n\
         - Issue detail: `gh issue view {{number}} --repo {{repo}}`\n\
         - Create issue: `gh issue create --repo {{repo}} --title \"...\" --body \"...\"`\n",
    ))
}

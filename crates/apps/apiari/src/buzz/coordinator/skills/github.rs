//! GitHub CLI skill — teaches the coordinator how to use `gh`.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if ctx.repos.is_empty() {
        return None;
    }

    let repos_list = ctx.repos.join(", ");

    let mut prompt = format!(
        "## GitHub\n\
         Repos: {repos_list}\n\
         Use the `gh` CLI (already authenticated):\n\
         - List open PRs: `gh pr list --repo {{repo}}`\n\
         - PR details: `gh pr view {{number}} --repo {{repo}}`\n\
         - PR checks: `gh pr checks {{number}} --repo {{repo}}`\n\
         - List issues: `gh issue list --repo {{repo}}`\n\
         - Issue detail: `gh issue view {{number}} --repo {{repo}}`\n\
         - Create issue: `gh issue create --repo {{repo}} --title \"...\" --body \"...\"`\n",
    );

    if ctx.has_review_queue && !ctx.review_queue_names.is_empty() {
        let names = ctx.review_queue_names.join(", ");
        prompt.push_str(&format!(
            "\n### Review Queue\n\
             This workspace has a review queue with {count} named quer{plural}: {names}.\n\
             Review queue signals appear in the signal store with source `github_review_queue`.\n\
             Each signal's metadata includes `query_name` and `priority` (lower = higher priority).\n\
             When summarizing pending reviews, group by query name and mention priority order.\n",
            count = ctx.review_queue_names.len(),
            plural = if ctx.review_queue_names.len() == 1 { "y" } else { "ies" },
        ));
    }

    Some(prompt)
}

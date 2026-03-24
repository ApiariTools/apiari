//! Swarm worker management skill — teaches the coordinator how to use swarm
//! via MCP tools (no shell commands needed).

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_swarm {
        return None;
    }

    // Build repo list hint
    let repo_hint = if ctx.repos.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = ctx
            .repos
            .iter()
            .filter_map(|r| r.split('/').next_back())
            .collect();
        format!(" (available repos: {})", names.join(", "))
    };

    let agent_line = match ctx.default_agent.as_str() {
        "codex" => "Default agent is codex. Use `codex` for autonomous tasks, \
                    or `codex-tui` for persistent sessions."
            .to_string(),
        "gemini" => "Default agent is gemini. Use `gemini` for autonomous tasks, \
                     or `gemini-tui` for persistent sessions."
            .to_string(),
        "auto" => "Agent selection: auto. Use `claude`, `codex`, or `gemini` — \
                   whichever is available. For persistent sessions, append `-tui`."
            .to_string(),
        _ => "Default agent is claude. Use `claude` for autonomous tasks, \
              or `claude-tui` for persistent sessions."
            .to_string(),
    };

    Some(format!(
        "## Swarm Workers\n\
         You dispatch coding tasks to swarm workers. Workers run in their own git worktrees \
         with an LLM agent that writes code, commits, and opens PRs.\n\n\
         IMPORTANT: When the user asks you to implement, fix, build, or code anything, \
         use `swarm_create_worker` to dispatch it. Do NOT write code yourself — \
         not via Edit/Write tools, and not via Bash (no echo, sed, curl -o, etc.).\n\n\
         Tools (MCP — call directly, no Bash needed):\n\
         - `swarm_create_worker` — Spawn a new worker with a task prompt.{repo_hint}\n\
         - `swarm_send_message` — Send a message to a waiting worker.\n\
         - `swarm_close_worker` — Close and clean up a worker.\n\
         - `swarm_list_workers` — List workers with status, phase, and PR info.\n\n\
         {agent_line}\n\n\
         When dispatching, always include in the task prompt:\n\
         'Plan and implement this completely in one session — do not pause mid-task \
         for confirmation. Commit and open a PR when done.'\n\n\
         ## Multi-repo Dispatch\n\
         When a task spans multiple repos, dispatch separate workers for each.\n\
         Each worker prompt must be self-contained — workers cannot see other repos.\n\
         Include relevant context about shared API contracts or interfaces in each prompt.\n",
    ))
}

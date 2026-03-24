//! Swarm worker management skill — teaches the coordinator how to use `swarm`.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_swarm {
        return None;
    }

    let root = ctx.workspace_root.display();

    // Build repo list for the --repo flag hint
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
        "codex" => "Default agent is codex. Use `--agent codex` for autonomous tasks, \
                    or `--agent codex-tui` for persistent sessions."
            .to_string(),
        "gemini" => "Default agent is gemini. Use `--agent gemini` for autonomous tasks, \
                     or `--agent gemini-tui` for persistent sessions."
            .to_string(),
        "auto" => {
            "Agent selection: auto. Use `--agent claude`, `--agent codex`, or `--agent gemini` — \
                   whichever is available. For persistent sessions, append `-tui`."
                .to_string()
        }
        _ => "Default agent is claude. Use `--agent claude` for autonomous tasks, \
              or `--agent claude-tui` for persistent sessions."
            .to_string(),
    };

    Some(format!(
        "## Swarm Workers\n\
         You dispatch coding tasks to swarm workers. Workers run in their own git worktrees \
         with an LLM agent that writes code, commits, and opens PRs.\n\n\
         IMPORTANT: When the user asks you to implement, fix, build, or code anything, \
         use `swarm create` to dispatch it. Do NOT write code yourself — \
         not via Edit/Write tools, and not via Bash (no echo, sed, curl -o, etc.).\n\n\
         Commands (always use `--dir {root}`):\n\
         - List workers: `swarm --dir {root} status`\n\
         - Spawn worker: `swarm --dir {root} create --repo {{repo}} --prompt-file /tmp/task.txt`\n\
           (Write the task prompt to a file first, then pass --prompt-file. Never inline long prompts.){repo_hint}\n\
         - Send message: `swarm --dir {root} send {{worktree_id}} \"message\"`\n\
         - Close worker: `swarm --dir {root} close {{worktree_id}}`\n\n\
         {agent_line}\n\n\
         ## Daemon\n\
         The swarm daemon is managed automatically — it is started on launch and \
         monitored via a persistent socket connection. You should not need to start it \
         manually. If a swarm command fails, retry once — the daemon may be restarting.\n\n\
         When dispatching, always include in the task prompt:\n\
         'Plan and implement this completely in one session — do not pause mid-task \
         for confirmation. Commit and open a PR when done.'\n\n\
         ## Multi-repo Dispatch\n\
         When a task spans multiple repos, dispatch separate workers for each.\n\
         Each worker prompt must be self-contained — workers cannot see other repos.\n\
         Include relevant context about shared API contracts or interfaces in each prompt.\n",
    ))
}

//! Persistent memory skill — teaches the coordinator how to update MEMORY.md.

use super::SkillContext;

/// Always included — the coordinator should always know about persistent memory.
pub fn build_prompt(_ctx: &SkillContext) -> String {
    "\
## Persistent Memory

You have a persistent memory file at `~/.claude/projects/*/memory/MEMORY.md` \
(the exact path depends on the workspace directory). This file is loaded into \
your system prompt at the start of every session, so anything written there \
persists across conversations.

### How to Update

You can **append** to MEMORY.md via Bash. The validate-bash hook allows writes \
to `~/.claude/.../memory/` paths. Examples:

```bash
# Append a line
echo '- User prefers PRs squash-merged' >> ~/.claude/projects/-Users-josh-Developer-myproject/memory/MEMORY.md

# Append a block
cat >> ~/.claude/projects/-Users-josh-Developer-myproject/memory/MEMORY.md << 'EOF'

## Deploy process
- Always run tests before deploying
- Use `make deploy-prod` for production
EOF
```

To find the exact path, look for a `memory/` directory under `~/.claude/projects/`.

### What to Remember

Update MEMORY.md when you learn something important about the workspace that \
should be remembered long-term:
- User preferences for workflow, tools, and communication style
- Key architectural decisions and project conventions
- Repo-specific patterns (branch naming, CI quirks, deploy steps)
- Solutions to recurring problems

### What NOT to Remember

- Session-specific context (current task details, in-progress work)
- Information already in the workspace config or CLAUDE.md files
- Speculative or unverified conclusions

### Rules

- **Append only** — do not overwrite the file. Use `>>` or `cat >>`, never `>`.
- **Keep it concise** — lines after 200 are truncated in the system prompt.
- **No secrets** — never write tokens, passwords, or API keys to MEMORY.md.
"
    .to_string()
}

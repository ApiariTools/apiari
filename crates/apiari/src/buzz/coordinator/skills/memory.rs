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

This is different from the **Signal Store `memory` table** (SQLite). Use each for \
different purposes:
- **MEMORY.md** — human-readable notes loaded into your prompt automatically. \
Best for workspace conventions, user preferences, and lessons learned. \
Workspace-local and prompt-visible.
- **SQLite `memory` table** — structured records queryable via `sqlite3`. Best \
for programmatic storage (observations, decisions) that you query on demand \
rather than always having in context.

When in doubt: if you want to remember something that should influence every \
future conversation, put it in MEMORY.md. If it's a structured fact you'll \
query occasionally, use the SQLite memory table.

### How to Update

First, discover the exact path:
```bash
ls ~/.claude/projects/*/memory/MEMORY.md
```

Then **append** via Bash. The validate-bash hook allows writes to \
`~/.claude/.../memory/` paths. Examples:

```bash
# Find the path
MEMORY=$(ls ~/.claude/projects/*/memory/MEMORY.md 2>/dev/null | head -1)

# Append a line
echo '- User prefers PRs squash-merged' >> \"$MEMORY\"

# Append a block
cat >> \"$MEMORY\" << 'EOF'

## Deploy process
- Always run tests before deploying
- Use `make deploy-prod` for production
EOF
```

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
- **Keep it concise** — very long entries may be truncated in the system prompt.
- **No secrets** — never write tokens, passwords, or API keys to MEMORY.md.
"
    .to_string()
}

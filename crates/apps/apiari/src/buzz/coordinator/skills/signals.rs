//! Signal store query skill — teaches the coordinator about the signal database.

use super::SkillContext;

/// Always included — the coordinator should always know about the signal store.
pub fn build_prompt(ctx: &SkillContext) -> String {
    let ws = &ctx.workspace_name;
    let db_path = crate::config::db_path();
    let db_display = db_path.display();
    format!(
        "\
## Signal Store

The signal database is at `{db_display}` (SQLite, workspace-scoped).
Open signals are already listed above. For detailed queries use `apiari status {ws}` \
or query SQLite directly.

### Database Tables

**signals** — all watcher events
Columns: `id, workspace, source, external_id, title, body, severity, status, url, \
created_at, updated_at, resolved_at, metadata, snoozed_until`

**watcher_cursors** — polling state per watcher/workspace
Columns: `workspace, watcher, cursor_value, updated_at`

**memory** — coordinator long-term memory
Columns: `id, workspace, category, content, created_at`

**conversations** — chat history between user and coordinator
Columns: `id, workspace, role, content, source, provider, session_id, created_at`

### Example Queries

Recent signals:
```
sqlite3 {db_display} \"SELECT source, severity, title, created_at FROM signals WHERE workspace='{ws}' ORDER BY created_at DESC LIMIT 20\"
```

Unresolved signals:
```
sqlite3 {db_display} \"SELECT source, severity, title, created_at FROM signals WHERE workspace='{ws}' AND status IN ('open','updated') ORDER BY created_at DESC\"
```

Recent conversation history:
```
sqlite3 {db_display} \"SELECT role, substr(content,1,120), source, created_at FROM conversations WHERE workspace='{ws}' ORDER BY created_at DESC LIMIT 20\"
```

Watcher cursor state:
```
sqlite3 {db_display} \"SELECT watcher, cursor_value, updated_at FROM watcher_cursors WHERE workspace='{ws}'\"
```

Coordinator memories:
```
sqlite3 {db_display} \"SELECT category, content, created_at FROM memory WHERE workspace='{ws}' ORDER BY created_at DESC LIMIT 20\"
```

### When to Query

Proactively query the database when:
- The user asks \"what happened recently?\" or \"any issues?\" — check signals
- Debugging why a watcher fired or didn't fire — check watcher_cursors and recent signals
- The user asks about past responses or conversations — check conversations table
- Investigating signal state (snoozed, resolved) — query signals with appropriate filters
",
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn build_prompt_uses_resolved_db_path() {
        let ctx = SkillContext {
            workspace_name: "ws".to_string(),
            workspace_root: PathBuf::from("/tmp/ws"),
            config_path: PathBuf::from("/tmp/ws.toml"),
            repos: vec![],
            has_sentry: false,
            has_swarm_runtime: false,
            can_dispatch_workers: false,
            has_review_queue: false,
            review_queue_names: vec![],
            has_linear: false,
            linear_names: vec![],
            has_email: false,
            email_names: vec![],
            has_notion: false,
            notion_names: vec![],
            has_scripts: false,
            script_names: vec![],
            has_telegram: false,
            prompt_preamble: None,
            default_agent: "claude".to_string(),
            authority: crate::config::WorkspaceAuthority::Autonomous,
            capabilities: crate::config::WorkspaceCapabilities::default(),
        };

        let prompt = build_prompt(&ctx);
        let db_display = crate::config::db_path().display().to_string();
        assert!(prompt.contains(&db_display));
        assert!(!prompt.contains("~/.config/apiari/apiari.db"));
    }
}

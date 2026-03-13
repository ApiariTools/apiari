//! Signal store query skill — teaches the coordinator about the signal database.

use super::SkillContext;

/// Always included — the coordinator should always know about the signal store.
pub fn build_prompt(ctx: &SkillContext) -> String {
    format!(
        "## Signal Store\n\
         The signal database is at ~/.config/apiari/apiari.db \
         (SQLite, workspace=\"{}\").\n\
         Open signals are already listed above. For detailed queries:\n\
         - `apiari status {}`\n\
         Or query SQLite directly with `sqlite3` if needed.\n",
        ctx.workspace_name, ctx.workspace_name,
    )
}

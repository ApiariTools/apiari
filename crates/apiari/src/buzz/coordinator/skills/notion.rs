//! Notion skill — teaches the coordinator about Notion workspace monitoring.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_notion {
        return None;
    }

    let names = ctx.notion_names.join(", ");

    Some(format!(
        "## Notion\n\
         This workspace has {count} Notion watcher(s) configured: {names}.\n\
         Notion signals appear in the signal store with source `{{name}}_review_queue`.\n\n\
         Configuration is in {config} under `[[watchers.notion]]`:\n\
         ```toml\n\
         [[watchers.notion]]\n\
         name = \"notion\"\n\
         token = \"secret_xxxx\"\n\
         user_id = \"notion-user-id\"\n\
         poll_database_ids = [\"db-id-1\"]  # optional: databases to scan for assignments\n\
         ```\n\n\
         Notion watchers poll for mentions and assignments using the Notion API.\n\
         When `poll_database_ids` is set, the watcher also scans those databases\n\
         for pages assigned to the configured user.\n",
        count = ctx.notion_names.len(),
        config = ctx.config_path.display(),
    ))
}

//! Linear skill — teaches the coordinator about Linear issue tracking integration.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_linear {
        return None;
    }

    let names = ctx.linear_names.join(", ");

    Some(format!(
        "## Linear\n\
         This workspace has {count} Linear watcher(s) configured: {names}.\n\
         Linear signals appear in the signal store with source `linear_review_queue`.\n\n\
         Configuration is in {config} under `[[watchers.linear]]`:\n\
         ```toml\n\
         [[watchers.linear]]\n\
         name = \"linear\"\n\
         api_key = \"lin_api_xxxx\"\n\
         user_id = \"your-linear-user-id\"\n\
         query = \"assignee:me\"   # supports: assignee:me, mentions:me, subscriber:me, or custom filter\n\
         ```\n\n\
         Linear watchers poll for issues matching the configured query and create signals\n\
         in the review queue. Each signal includes issue title, state, priority, and URL.\n",
        count = ctx.linear_names.len(),
        config = ctx.config_path.display(),
    ))
}

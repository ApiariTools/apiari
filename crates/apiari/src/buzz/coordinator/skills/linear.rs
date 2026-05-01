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
         poll_interval_secs = 60\n\
         \n\
         # Recommended: unread notifications (mentions, assignments, comments)\n\
         [[watchers.linear.review_queue]]\n\
         name = \"Unread notifications\"\n\
         query = \"notifications:unread\"\n\
         \n\
         # Optional: explicitly assigned issues\n\
         [[watchers.linear.review_queue]]\n\
         name = \"Assigned to me\"\n\
         query = \"assignee:me\"\n\
         ```\n\n\
         The `notifications:unread` query mirrors the Linear notification bell — it surfaces\n\
         unread mentions, assignments, and comments — and is the most useful single query\n\
         for staying on top of what needs attention.\n\n\
         Linear watchers poll for issues matching the configured queries and create signals\n\
         in the review queue. Each signal includes issue title, state, priority, and URL.\n",
        count = ctx.linear_names.len(),
        config = ctx.config_path.display(),
    ))
}

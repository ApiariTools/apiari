//! Config awareness skill — teaches the coordinator about current workspace
//! configuration and how to guide users through setting up integrations.

use super::SkillContext;

/// Build the config awareness prompt. Always included.
pub fn build_prompt(ctx: &SkillContext) -> String {
    let mut prompt = String::new();

    // Current config summary
    prompt.push_str("## Current Workspace Config\n");

    if ctx.has_telegram {
        prompt.push_str("- Telegram: ✓ configured\n");
    } else {
        prompt.push_str("- Telegram: ✗ not configured\n");
    }

    if !ctx.repos.is_empty() {
        let github_label = if ctx.has_review_queue {
            format!(
                "✓ watching {} repo(s), {} review queue(s)",
                ctx.repos.len(),
                ctx.review_queue_names.len()
            )
        } else {
            format!("✓ {} repo(s), no review queue", ctx.repos.len())
        };
        prompt.push_str(&format!("- GitHub: {github_label}\n"));
    } else {
        prompt.push_str("- GitHub: ✗ no repos configured\n");
    }

    if ctx.has_swarm {
        prompt.push_str("- Swarm: ✓ connected\n");
    } else {
        prompt.push_str("- Swarm: ✗ not configured\n");
    }

    if ctx.has_sentry {
        prompt.push_str("- Sentry: ✓ configured\n");
    } else {
        prompt.push_str("- Sentry: ✗ not configured\n");
    }

    if ctx.has_linear {
        let names = ctx.linear_names.join(", ");
        prompt.push_str(&format!("- Linear: ✓ configured ({names})\n"));
    } else {
        prompt.push_str("- Linear: ✗ not configured\n");
    }

    if ctx.has_email {
        let names = ctx.email_names.join(", ");
        prompt.push_str(&format!("- Email: ✓ configured ({names})\n"));
    } else {
        prompt.push_str("- Email: ✗ not configured\n");
    }

    if ctx.has_notion {
        let names = ctx.notion_names.join(", ");
        prompt.push_str(&format!("- Notion: ✓ configured ({names})\n"));
    } else {
        prompt.push_str("- Notion: ✗ not configured\n");
    }

    prompt.push_str(&format!(
        "\nThe workspace config file is at `{}`.\n\
         Users edit this file directly — you can tell them exactly what to add and where.\n",
        ctx.config_path.display(),
    ));

    // Setup guides
    prompt.push_str(
        "\n## Integration Setup Guides\n\
         When a user asks how to set up an integration, give them the exact TOML snippet \
         to add to their config file. Here are the available integrations:\n\n\
         **Telegram setup:**\n\
         ```toml\n\
         [telegram]\n\
         bot_token = \"TOKEN_FROM_BOTFATHER\"\n\
         chat_id = 0        # get from @userinfobot\n\
         topic_id = 0        # optional: forum thread ID\n\
         allowed_user_ids = []  # empty = allow all\n\
         ```\n\n\
         **GitHub watcher setup:**\n\
         ```toml\n\
         [watchers.github]\n\
         interval_secs = 120\n\
         \n\
         [[watchers.github.review_queue]]\n\
         name = \"Review Requested\"\n\
         query = \"is:pr is:open review-requested:@me org:YourOrg\"\n\
         \n\
         [[watchers.github.review_queue]]\n\
         name = \"Open PRs\"\n\
         query = \"is:pr is:open author:@me org:YourOrg\"\n\
         ```\n\n\
         **Sentry setup:**\n\
         ```toml\n\
         [watchers.sentry]\n\
         org = \"your-org-slug\"\n\
         project = \"your-project-slug\"\n\
         token = \"sntrys_...\"\n\
         interval_secs = 120\n\
         ```\n\n\
         **Swarm setup:**\n\
         ```toml\n\
         [watchers.swarm]\n\
         state_path = \"/path/to/.swarm/state.json\"\n\
         interval_secs = 30\n\
         ```\n\n\
         **Linear setup:**\n\
         ```toml\n\
         [[watchers.linear]]\n\
         name = \"linear\"\n\
         api_key = \"lin_api_...\"\n\
         poll_interval_secs = 60\n\
         \n\
         [[watchers.linear.review_queue]]\n\
         name = \"Unread notifications\"\n\
         query = \"notifications:unread\"\n\
         \n\
         [[watchers.linear.review_queue]]\n\
         name = \"Assigned to me\"\n\
         query = \"assignee:me\"\n\
         ```\n\n\
         **Email (IMAP) setup:**\n\
         ```toml\n\
         [[watchers.email]]\n\
         name = \"gmail\"\n\
         host = \"imap.gmail.com\"\n\
         port = 993\n\
         tls = true\n\
         username = \"you@gmail.com\"\n\
         password = \"app-password\"\n\
         folder = \"INBOX\"\n\
         filter = \"UNSEEN\"\n\
         ```\n\n\
         **Notion setup:**\n\
         ```toml\n\
         [[watchers.notion]]\n\
         name = \"notion\"\n\
         token = \"secret_...\"\n\
         user_id = \"your-notion-user-id\"\n\
         poll_database_ids = [\"db-id-1\"]  # optional\n\
         ```\n\n\
         **Signal hooks setup:**\n\
         Signal hooks trigger coordinator follow-through when signals arrive:\n\
         ```toml\n\
         [[coordinator.signal_hooks]]\n\
         source = \"github_ci_failure\"\n\
         prompt = \"CI failed: {events}\"\n\
         ttl_secs = 300\n\
         ```\n\
         The default hook watches `swarm` signals. Add more hooks for sources like \
         `github_ci_failure`, `github_bot_review`, `sentry`, etc.\n",
    );

    prompt
}

/// Build a summary of the current workspace configuration for the `/config` command.
pub fn build_config_summary(ctx: &SkillContext) -> String {
    let mut text = format!("Workspace: {}\n\n", ctx.workspace_name);

    if ctx.has_telegram {
        text.push_str("✓ Telegram — configured\n");
    } else {
        text.push_str("✗ Telegram — not configured\n");
    }

    if !ctx.repos.is_empty() {
        let rq = if ctx.has_review_queue {
            format!(", {} review queue(s)", ctx.review_queue_names.len())
        } else {
            String::new()
        };
        text.push_str(&format!("✓ GitHub — {} repo(s){rq}\n", ctx.repos.len()));
    } else {
        text.push_str("✗ GitHub — no repos configured\n");
    }

    if ctx.has_swarm {
        text.push_str("✓ Swarm — connected\n");
    } else {
        text.push_str("✗ Swarm — not configured\n");
    }

    if ctx.has_sentry {
        text.push_str("✓ Sentry — configured\n");
    } else {
        text.push_str("✗ Sentry — not configured\n");
    }

    if ctx.has_linear {
        let names = ctx.linear_names.join(", ");
        text.push_str(&format!("✓ Linear — {names}\n"));
    } else {
        text.push_str("✗ Linear — not configured\n");
    }

    if ctx.has_email {
        let names = ctx.email_names.join(", ");
        text.push_str(&format!("✓ Email — {names}\n"));
    } else {
        text.push_str("✗ Email — not configured\n");
    }

    if ctx.has_notion {
        let names = ctx.notion_names.join(", ");
        text.push_str(&format!("✓ Notion — {names}\n"));
    } else {
        text.push_str("✗ Notion — not configured\n");
    }

    text.push_str(&format!("\nConfig file: {}", ctx.config_path.display()));

    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn full_ctx() -> SkillContext {
        SkillContext {
            workspace_name: "myproject".to_string(),
            workspace_root: PathBuf::from("/home/user/myproject"),
            config_path: PathBuf::from("/home/user/.config/apiari/workspaces/myproject.toml"),
            repos: vec!["org/repo".to_string()],
            has_sentry: true,
            has_swarm: true,
            has_review_queue: true,
            review_queue_names: vec!["Review Requested".to_string()],
            has_linear: true,
            linear_names: vec!["linear".to_string()],
            has_email: true,
            email_names: vec!["gmail".to_string()],
            has_notion: true,
            notion_names: vec!["notion".to_string()],
            has_telegram: true,
            prompt_preamble: None,
        }
    }

    fn empty_ctx() -> SkillContext {
        SkillContext {
            workspace_name: "empty".to_string(),
            workspace_root: PathBuf::from("/home/user/empty"),
            config_path: PathBuf::from("/home/user/.config/apiari/workspaces/empty.toml"),
            repos: vec![],
            has_sentry: false,
            has_swarm: false,
            has_review_queue: false,
            review_queue_names: vec![],
            has_linear: false,
            linear_names: vec![],
            has_email: false,
            email_names: vec![],
            has_notion: false,
            has_telegram: false,
            notion_names: vec![],
            prompt_preamble: None,
        }
    }

    #[test]
    fn test_prompt_includes_all_configured() {
        let prompt = build_prompt(&full_ctx());
        assert!(prompt.contains("Telegram: ✓"));
        assert!(prompt.contains("GitHub: ✓"));
        assert!(prompt.contains("Swarm: ✓"));
        assert!(prompt.contains("Sentry: ✓"));
        assert!(prompt.contains("Linear: ✓"));
        assert!(prompt.contains("Email: ✓"));
        assert!(prompt.contains("Notion: ✓"));
    }

    #[test]
    fn test_prompt_shows_not_configured() {
        let prompt = build_prompt(&empty_ctx());
        assert!(prompt.contains("Telegram: ✗"));
        assert!(prompt.contains("GitHub: ✗"));
        assert!(prompt.contains("Swarm: ✗"));
        assert!(prompt.contains("Sentry: ✗"));
        assert!(prompt.contains("Linear: ✗"));
        assert!(prompt.contains("Email: ✗"));
        assert!(prompt.contains("Notion: ✗"));
    }

    #[test]
    fn test_prompt_includes_setup_guides() {
        let prompt = build_prompt(&empty_ctx());
        assert!(prompt.contains("## Integration Setup Guides"));
        assert!(prompt.contains("**Telegram setup:**"));
        assert!(prompt.contains("[telegram]"));
        assert!(prompt.contains("**GitHub watcher setup:**"));
        assert!(prompt.contains("[watchers.github]"));
        assert!(prompt.contains("**Sentry setup:**"));
        assert!(prompt.contains("[watchers.sentry]"));
        assert!(prompt.contains("**Swarm setup:**"));
        assert!(prompt.contains("[watchers.swarm]"));
        assert!(prompt.contains("**Linear setup:**"));
        assert!(prompt.contains("[[watchers.linear]]"));
        assert!(prompt.contains("**Email (IMAP) setup:**"));
        assert!(prompt.contains("[[watchers.email]]"));
        assert!(prompt.contains("**Notion setup:**"));
        assert!(prompt.contains("[[watchers.notion]]"));
        assert!(prompt.contains("**Signal hooks setup:**"));
        assert!(prompt.contains("[[coordinator.signal_hooks]]"));
    }

    #[test]
    fn test_prompt_includes_config_path() {
        let prompt = build_prompt(&full_ctx());
        assert!(prompt.contains("myproject.toml"));
    }

    #[test]
    fn test_summary_configured() {
        let summary = build_config_summary(&full_ctx());
        assert!(summary.contains("Workspace: myproject"));
        assert!(summary.contains("✓ Telegram"));
        assert!(summary.contains("✓ GitHub"));
        assert!(summary.contains("✓ Swarm"));
        assert!(summary.contains("✓ Sentry"));
        assert!(summary.contains("✓ Linear"));
        assert!(summary.contains("✓ Email"));
        assert!(summary.contains("✓ Notion"));
        assert!(summary.contains("Config file:"));
    }

    #[test]
    fn test_summary_not_configured() {
        let summary = build_config_summary(&empty_ctx());
        assert!(summary.contains("✗ Telegram"));
        assert!(summary.contains("✗ GitHub"));
        assert!(summary.contains("✗ Swarm"));
        assert!(summary.contains("✗ Sentry"));
        assert!(summary.contains("✗ Linear"));
        assert!(summary.contains("✗ Email"));
        assert!(summary.contains("✗ Notion"));
    }
}

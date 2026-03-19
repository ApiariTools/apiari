//! Coordinator skills — structured instructions + tool permissions.
//!
//! Skills teach the coordinator what CLI tools and APIs are available
//! based on the workspace configuration. Each skill contributes a block
//! of prompt text; `build_skills_prompt()` aggregates them all.

pub mod config;
mod email;
mod github;
mod linear;
mod memory;
mod notion;
mod sentry;
mod signals;
mod swarm;

use std::path::PathBuf;

/// Context derived from workspace config, used to build skill prompts.
pub struct SkillContext {
    pub workspace_name: String,
    pub workspace_root: PathBuf,
    pub config_path: PathBuf,
    pub repos: Vec<String>,
    pub has_sentry: bool,
    pub has_swarm: bool,
    pub has_review_queue: bool,
    pub review_queue_names: Vec<String>,
    pub has_linear: bool,
    pub linear_names: Vec<String>,
    pub has_email: bool,
    pub email_names: Vec<String>,
    pub has_notion: bool,
    pub notion_names: Vec<String>,
    pub has_telegram: bool,
    /// Custom prompt preamble loaded from prompt_file.
    /// If set, replaces the default identity/role sections in the system prompt.
    pub prompt_preamble: Option<String>,
    /// Default swarm agent: "claude", "codex", or "auto".
    pub default_agent: String,
}

/// Build the combined skills prompt from workspace context.
///
/// Each skill checks whether it's applicable (e.g. Sentry skill only
/// included when `has_sentry` is true) and contributes instructions.
pub fn build_skills_prompt(ctx: &SkillContext) -> String {
    let mut sections = Vec::new();

    // Always-on skills
    sections.push(config::build_prompt(ctx));
    sections.push(signals::build_prompt(ctx));
    sections.push(memory::build_prompt(ctx));

    // Conditional skills
    if let Some(s) = github::build_prompt(ctx) {
        sections.push(s);
    }
    if let Some(s) = sentry::build_prompt(ctx) {
        sections.push(s);
    }
    if let Some(s) = swarm::build_prompt(ctx) {
        sections.push(s);
    }
    if let Some(s) = linear::build_prompt(ctx) {
        sections.push(s);
    }
    if let Some(s) = email::build_prompt(ctx) {
        sections.push(s);
    }
    if let Some(s) = notion::build_prompt(ctx) {
        sections.push(s);
    }

    let mut prompt = format!(
        "## Workspace\n\
         Name: {}\n\
         Root: {}\n\
         Config: {}\n",
        ctx.workspace_name,
        ctx.workspace_root.display(),
        ctx.config_path.display(),
    );

    if !ctx.repos.is_empty() {
        prompt.push_str(&format!(
            "\n## Repos in this workspace\n\
             This workspace contains {} git repo(s):\n",
            ctx.repos.len(),
        ));
        for repo in &ctx.repos {
            prompt.push_str(&format!("- {repo}\n"));
        }
        prompt.push_str(
            "These are the projects you manage. When asked about repos, projects, or codebases, refer to this list.\n",
        );
    }

    prompt.push_str("\n# Skills\nYou have the following tools and capabilities:\n\n");

    prompt.push_str(&sections.join("\n"));
    prompt
}

/// Default tools the coordinator is allowed to use (auto-approve).
///
/// No Write/Edit — code changes go through swarm workers.
pub fn default_coordinator_tools() -> Vec<String> {
    ["Bash", "Read", "Glob", "Grep", "WebSearch", "WebFetch"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Tools explicitly blocked for the coordinator (hard enforcement).
///
/// Even if the model tries to use these, Claude CLI will refuse.
pub fn default_coordinator_disallowed_tools() -> Vec<String> {
    ["Write", "Edit", "NotebookEdit", "Task"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> SkillContext {
        SkillContext {
            workspace_name: "myproject".to_string(),
            workspace_root: PathBuf::from("/home/user/myproject"),
            config_path: PathBuf::from("/home/user/.config/apiari/workspaces/myproject.toml"),
            repos: vec!["org/repo".to_string()],
            has_sentry: true,
            has_swarm: true,
            has_review_queue: false,
            review_queue_names: vec![],
            has_linear: false,
            linear_names: vec![],
            has_email: false,
            email_names: vec![],
            has_notion: false,
            notion_names: vec![],
            has_telegram: false,
            prompt_preamble: None,
            default_agent: "claude".to_string(),
        }
    }

    #[test]
    fn test_build_skills_prompt_includes_all() {
        let prompt = build_skills_prompt(&test_ctx());
        assert!(prompt.contains("## Workspace"));
        assert!(prompt.contains("myproject"));
        assert!(prompt.contains("## Current Workspace Config"));
        assert!(prompt.contains("## Integration Setup Guides"));
        assert!(prompt.contains("## Signal Store"));
        assert!(prompt.contains("## GitHub"));
        assert!(prompt.contains("## Sentry"));
        assert!(prompt.contains("## Swarm Workers"));
    }

    #[test]
    fn test_build_skills_prompt_no_sentry() {
        let mut ctx = test_ctx();
        ctx.has_sentry = false;
        let prompt = build_skills_prompt(&ctx);
        assert!(!prompt.contains("## Sentry"));
        assert!(prompt.contains("## GitHub"));
    }

    #[test]
    fn test_build_skills_prompt_no_swarm() {
        let mut ctx = test_ctx();
        ctx.has_swarm = false;
        let prompt = build_skills_prompt(&ctx);
        assert!(!prompt.contains("## Swarm Workers"));
    }

    #[test]
    fn test_build_skills_prompt_no_repos() {
        let mut ctx = test_ctx();
        ctx.repos.clear();
        let prompt = build_skills_prompt(&ctx);
        assert!(!prompt.contains("## GitHub"));
    }

    #[test]
    fn test_build_skills_prompt_with_linear() {
        let mut ctx = test_ctx();
        ctx.has_linear = true;
        ctx.linear_names = vec!["linear".to_string()];
        let prompt = build_skills_prompt(&ctx);
        assert!(prompt.contains("## Linear"));
    }

    #[test]
    fn test_build_skills_prompt_no_linear() {
        let ctx = test_ctx();
        let prompt = build_skills_prompt(&ctx);
        assert!(!prompt.contains("## Linear"));
    }

    #[test]
    fn test_build_skills_prompt_with_email() {
        let mut ctx = test_ctx();
        ctx.has_email = true;
        ctx.email_names = vec!["fastmail".to_string()];
        let prompt = build_skills_prompt(&ctx);
        assert!(prompt.contains("## Email"));
    }

    #[test]
    fn test_build_skills_prompt_no_email() {
        let ctx = test_ctx();
        let prompt = build_skills_prompt(&ctx);
        assert!(!prompt.contains("## Email"));
    }

    #[test]
    fn test_build_skills_prompt_with_notion() {
        let mut ctx = test_ctx();
        ctx.has_notion = true;
        ctx.notion_names = vec!["notion".to_string()];
        let prompt = build_skills_prompt(&ctx);
        assert!(prompt.contains("## Notion"));
    }

    #[test]
    fn test_build_skills_prompt_no_notion() {
        let ctx = test_ctx();
        let prompt = build_skills_prompt(&ctx);
        assert!(!prompt.contains("## Notion"));
    }

    #[test]
    fn test_build_skills_prompt_includes_memory() {
        let prompt = build_skills_prompt(&test_ctx());
        assert!(prompt.contains("## Persistent Memory"));
        assert!(prompt.contains("MEMORY.md"));
    }

    #[test]
    fn test_disallowed_tools_exact() {
        let tools = default_coordinator_disallowed_tools();
        let expected = vec!["Write", "Edit", "NotebookEdit", "Task"];
        assert_eq!(
            tools, expected,
            "disallowed tools must be exactly {expected:?}"
        );
    }

    #[test]
    fn test_allowed_tools_no_write_capable() {
        let tools = default_coordinator_tools();
        let write_capable = ["Write", "Edit", "NotebookEdit", "Task", "TodoWrite"];
        for tool in &write_capable {
            assert!(
                !tools.contains(&tool.to_string()),
                "allowed tools must not contain {tool}"
            );
        }
    }

    #[test]
    fn test_swarm_skill_reinforces_bash_restriction() {
        let ctx = test_ctx();
        let prompt = swarm::build_prompt(&ctx).expect("swarm skill should be enabled");
        assert!(
            prompt.contains("not via Bash"),
            "swarm skill must reinforce Bash restriction"
        );
    }

    #[test]
    fn test_default_coordinator_tools() {
        let tools = default_coordinator_tools();
        assert_eq!(tools.len(), 6);
        assert!(tools.contains(&"Bash".to_string()));
        assert!(tools.contains(&"Read".to_string()));
        assert!(!tools.contains(&"Write".to_string()));
        assert!(!tools.contains(&"Edit".to_string()));
    }

    #[test]
    fn test_default_coordinator_disallowed_tools() {
        let tools = default_coordinator_disallowed_tools();
        assert!(tools.contains(&"Write".to_string()));
        assert!(tools.contains(&"Edit".to_string()));
        assert!(tools.contains(&"NotebookEdit".to_string()));
        assert!(tools.contains(&"Task".to_string()));
        assert!(!tools.contains(&"Bash".to_string()));
    }
}

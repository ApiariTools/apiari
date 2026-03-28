//! Coordinator skills — structured instructions + tool permissions.
//!
//! Skills teach the coordinator what CLI tools and APIs are available
//! based on the workspace configuration. Each skill contributes a block
//! of prompt text; `build_skills_prompt()` aggregates them all.
//!
//! ## Skill Kinds
//!
//! - **Tool** — operational knowledge for external tools/CLIs (auto-detected from watcher config)
//! - **Soul** — communication style and personality (auto-loaded from `.apiari/soul.md`)
//! - **Context** — what this workspace/project is (auto-loaded from `.apiari/context.md`)
//! - **Playbook** — how to handle a situation (indexed from `.apiari/skills/*.md`)

mod apiari;
pub mod config;
mod email;
mod github;
mod linear;
mod memory;
mod notion;
mod scripts;
mod sentry;
mod signals;
mod swarm;

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::config::{WorkspaceAuthority, WorkspaceCapabilities};

/// Check if a playbook name is safe (no path traversal).
/// Only allows alphanumeric, hyphens, and underscores.
fn is_valid_playbook_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

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
    pub has_scripts: bool,
    pub script_names: Vec<String>,
    pub has_telegram: bool,
    /// Custom prompt preamble loaded from prompt_file.
    /// If set, replaces the default identity/role sections in the system prompt.
    pub prompt_preamble: Option<String>,
    /// Default swarm agent: "claude", "codex", "gemini", or "auto".
    pub default_agent: String,
    /// Workspace authority level.
    pub authority: WorkspaceAuthority,
    /// Resolved capabilities (already adjusted for authority level).
    pub capabilities: WorkspaceCapabilities,
}

/// A playbook entry indexed from `.apiari/skills/*.md`.
#[derive(Debug, Clone)]
pub struct PlaybookEntry {
    /// Filename stem (e.g. "ci-triage" from "ci-triage.md").
    pub name: String,
    /// First line of the file, stripped of leading `#` and whitespace.
    pub description: String,
}

/// Load the context skill from `.apiari/context.md` if it exists.
pub fn load_context_skill(workspace_root: &Path) -> Option<String> {
    let path = workspace_root.join(".apiari/context.md");
    std::fs::read_to_string(&path).ok()
}

/// Load the soul/personality from `.apiari/soul.md` if it exists.
pub fn load_soul_skill(workspace_root: &Path) -> Option<String> {
    let path = workspace_root.join(".apiari/soul.md");
    std::fs::read_to_string(&path).ok()
}

/// Index playbooks from `.apiari/skills/*.md`.
///
/// Returns a list of (name, first-line description) entries.
pub fn index_playbooks(workspace_root: &Path) -> Vec<PlaybookEntry> {
    let dir = workspace_root.join(".apiari/skills");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut playbooks = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        // Skip symlinks to avoid following links outside the skills directory
        if std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "md") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if !is_valid_playbook_name(&name) {
                continue;
            }
            // Read only the first line using BufReader instead of the full file
            let description = std::fs::File::open(&path)
                .ok()
                .and_then(|file| {
                    let mut reader = std::io::BufReader::new(file);
                    let mut first_line = String::new();
                    reader.read_line(&mut first_line).ok()?;
                    Some(first_line.trim_start_matches('#').trim().to_string())
                })
                .unwrap_or_default();
            playbooks.push(PlaybookEntry { name, description });
        }
    }
    playbooks.sort_by(|a, b| a.name.cmp(&b.name));
    playbooks
}

/// Load the full content of a named playbook.
///
/// Returns `None` if the name contains invalid characters (path traversal prevention)
/// or if the file doesn't exist.
pub fn load_playbook(workspace_root: &Path, name: &str) -> Option<String> {
    if !is_valid_playbook_name(name) {
        return None;
    }
    let path = workspace_root.join(format!(".apiari/skills/{name}.md"));
    std::fs::read_to_string(&path).ok()
}

/// Build the combined skills prompt from workspace context.
///
/// Prompt is assembled in this order:
/// 1. Workspace info + repos
/// 2. Tool skills (auto from watcher config)
/// 3. Soul / communication style (from `.apiari/soul.md`)
/// 4. Context skill (from `.apiari/context.md`)
/// 5. Playbook index (names + descriptions from `.apiari/skills/*.md`)
/// 6. Authority level statement
pub fn build_skills_prompt(ctx: &SkillContext) -> String {
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

    // Tool skills (auto-detected from watcher config) + always-on apiari system skill
    let mut tool_sections = vec![
        apiari::build_prompt(ctx),
        config::build_prompt(ctx),
        signals::build_prompt(ctx),
        memory::build_prompt(ctx),
        scripts::build_prompt(ctx),
    ];

    if let Some(s) = github::build_prompt(ctx) {
        tool_sections.push(s);
    }
    if let Some(s) = sentry::build_prompt(ctx) {
        tool_sections.push(s);
    }
    if let Some(s) = swarm::build_prompt(ctx) {
        tool_sections.push(s);
    }
    if let Some(s) = linear::build_prompt(ctx) {
        tool_sections.push(s);
    }
    if let Some(s) = email::build_prompt(ctx) {
        tool_sections.push(s);
    }
    if let Some(s) = notion::build_prompt(ctx) {
        tool_sections.push(s);
    }

    prompt.push_str("\n# Skills\nYou have the following tools and capabilities:\n\n");
    prompt.push_str(&tool_sections.join("\n"));

    // Soul / communication style (from .apiari/soul.md)
    if let Some(soul) = load_soul_skill(&ctx.workspace_root) {
        prompt.push_str("\n## Communication Style\n");
        prompt.push_str(&soul);
        if !soul.ends_with('\n') {
            prompt.push('\n');
        }
    }

    // Context skill (from .apiari/context.md)
    if let Some(context) = load_context_skill(&ctx.workspace_root) {
        prompt.push_str("\n## Project Context\n");
        prompt.push_str(&context);
        if !context.ends_with('\n') {
            prompt.push('\n');
        }
    }

    // Playbook index
    let playbooks = index_playbooks(&ctx.workspace_root);
    if !playbooks.is_empty() {
        prompt.push_str("\n## Available Playbooks\n");
        prompt.push_str(
            "The following playbooks are available and will be loaded when relevant signal hooks fire:\n",
        );
        for pb in &playbooks {
            if pb.description.is_empty() {
                prompt.push_str(&format!("- {}\n", pb.name));
            } else {
                prompt.push_str(&format!("- {} — {}\n", pb.name, pb.description));
            }
        }
    }

    // Authority level statement
    match ctx.authority {
        WorkspaceAuthority::Observe => {
            prompt.push_str(
                "\n## Authority Level: Observe\n\
                 You are in observe mode. You have read-only access to the workspace.\n\
                 You CANNOT execute Bash commands, dispatch swarm workers, or make any changes.\n\
                 Your tools are limited to: Read, Glob, Grep, WebSearch, WebFetch.\n",
            );
        }
        WorkspaceAuthority::Autonomous => {
            prompt.push_str(
                "\n## Authority Level: Autonomous\n\
                 You have full operational access to this workspace.\n",
            );
            if !ctx.capabilities.merge_prs.is_allowed() {
                prompt.push_str(
                    "Note: PR merging is disabled. Do NOT merge PRs — this capability must be \
                     explicitly enabled in the workspace config (`[workspace.capabilities] merge_prs = true`).\n",
                );
            }
        }
    }

    prompt
}

/// Default tools the coordinator is allowed to use (auto-approve).
///
/// Write/Edit are included for `.apiari/` config files only — the coordinator
/// prompt constrains their use to `.apiari/context.md` and `.apiari/skills/*.md`.
pub fn default_coordinator_tools() -> Vec<String> {
    [
        "Bash",
        "Read",
        "Write",
        "Edit",
        "Glob",
        "Grep",
        "WebSearch",
        "WebFetch",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Tools explicitly blocked for the coordinator (hard enforcement).
///
/// Even if the model tries to use these, Claude CLI will refuse.
pub fn default_coordinator_disallowed_tools() -> Vec<String> {
    ["NotebookEdit", "Task"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Tools for observe mode — read-only only. No Bash.
pub fn observe_coordinator_tools() -> Vec<String> {
    ["Read", "Glob", "Grep", "WebSearch", "WebFetch"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Disallowed tools for observe mode — everything mutating.
pub fn observe_coordinator_disallowed_tools() -> Vec<String> {
    ["Write", "Edit", "NotebookEdit", "Task", "Bash"]
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
            has_scripts: false,
            script_names: vec![],
            has_telegram: false,
            prompt_preamble: None,
            default_agent: "claude".to_string(),
            authority: WorkspaceAuthority::Autonomous,
            capabilities: WorkspaceCapabilities::default(),
        }
    }

    #[test]
    fn test_build_skills_prompt_includes_all() {
        let prompt = build_skills_prompt(&test_ctx());
        assert!(prompt.contains("## Workspace"));
        assert!(prompt.contains("myproject"));
        assert!(prompt.contains("## Apiari System"));
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
    fn test_apiari_skill_always_present() {
        // Even with a minimal context (no watchers), the apiari system skill is included
        let mut ctx = test_ctx();
        ctx.has_sentry = false;
        ctx.has_swarm = false;
        ctx.repos.clear();
        let prompt = build_skills_prompt(&ctx);
        assert!(prompt.contains("## Apiari System"));
        assert!(prompt.contains("authority"));
        assert!(prompt.contains(".apiari/context.md"));
        assert!(prompt.contains(".apiari/skills/"));
        assert!(prompt.contains("skills = ["));
        assert!(prompt.contains("apiari init"));
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
        let expected = vec!["NotebookEdit", "Task"];
        assert_eq!(
            tools, expected,
            "disallowed tools must be exactly {expected:?}"
        );
    }

    #[test]
    fn test_allowed_tools_include_write_edit() {
        let tools = default_coordinator_tools();
        // Write and Edit are allowed for .apiari/ config files
        assert!(
            tools.contains(&"Write".to_string()),
            "allowed tools must contain Write"
        );
        assert!(
            tools.contains(&"Edit".to_string()),
            "allowed tools must contain Edit"
        );
        // NotebookEdit and Task must NOT be allowed
        let still_blocked = ["NotebookEdit", "Task", "TodoWrite"];
        for tool in &still_blocked {
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
        assert_eq!(tools.len(), 8);
        assert!(tools.contains(&"Bash".to_string()));
        assert!(tools.contains(&"Read".to_string()));
        assert!(tools.contains(&"Write".to_string()));
        assert!(tools.contains(&"Edit".to_string()));
    }

    #[test]
    fn test_default_coordinator_disallowed_tools() {
        let tools = default_coordinator_disallowed_tools();
        assert!(!tools.contains(&"Write".to_string()));
        assert!(!tools.contains(&"Edit".to_string()));
        assert!(tools.contains(&"NotebookEdit".to_string()));
        assert!(tools.contains(&"Task".to_string()));
        assert!(!tools.contains(&"Bash".to_string()));
    }

    #[test]
    fn test_load_context_skill_returns_content() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(
            apiari_dir.join("context.md"),
            "# My Project\nA cool project.",
        )
        .unwrap();
        let content = load_context_skill(dir.path());
        assert!(content.is_some());
        assert!(content.unwrap().contains("My Project"));
    }

    #[test]
    fn test_load_context_skill_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_context_skill(dir.path()).is_none());
    }

    #[test]
    fn test_load_soul_skill_returns_content() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(apiari_dir.join("soul.md"), "Be concise and direct.").unwrap();
        let content = load_soul_skill(dir.path());
        assert!(content.is_some());
        assert!(content.unwrap().contains("Be concise and direct."));
    }

    #[test]
    fn test_load_soul_skill_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_soul_skill(dir.path()).is_none());
    }

    #[test]
    fn test_build_skills_prompt_includes_soul_before_context() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(apiari_dir.join("skills")).unwrap();
        std::fs::write(apiari_dir.join("soul.md"), "Be terse and opinionated.").unwrap();
        std::fs::write(apiari_dir.join("context.md"), "# My Project\nA Rust CLI.").unwrap();

        let ctx = SkillContext {
            workspace_root: dir.path().to_path_buf(),
            ..test_ctx()
        };
        let prompt = build_skills_prompt(&ctx);

        assert!(prompt.contains("## Communication Style"));
        assert!(prompt.contains("Be terse and opinionated."));

        // Soul must appear before Project Context
        let soul_pos = prompt.find("## Communication Style").unwrap();
        let context_pos = prompt.find("## Project Context").unwrap();
        assert!(
            soul_pos < context_pos,
            "Communication Style must appear before Project Context"
        );
    }

    #[test]
    fn test_index_playbooks_reads_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".apiari/skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("ci-triage.md"), "# CI Triage\nStep 1...").unwrap();
        std::fs::write(skills_dir.join("deploy.md"), "Deploy checklist\nStep 1...").unwrap();

        let playbooks = index_playbooks(dir.path());
        assert_eq!(playbooks.len(), 2);
        assert_eq!(playbooks[0].name, "ci-triage");
        assert_eq!(playbooks[0].description, "CI Triage");
        assert_eq!(playbooks[1].name, "deploy");
        assert_eq!(playbooks[1].description, "Deploy checklist");
    }

    #[test]
    fn test_index_playbooks_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let playbooks = index_playbooks(dir.path());
        assert!(playbooks.is_empty());
    }

    #[test]
    fn test_index_playbooks_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".apiari/skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("real.md"), "# Real playbook").unwrap();
        // Create a symlink — should be skipped
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(skills_dir.join("real.md"), skills_dir.join("linked.md"))
                .unwrap();
        }
        let playbooks = index_playbooks(dir.path());
        // Only the real file should be indexed
        assert_eq!(playbooks.len(), 1);
        assert_eq!(playbooks[0].name, "real");
    }

    #[test]
    fn test_load_playbook_returns_content() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".apiari/skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("ci-triage.md"), "# CI Triage\nDo stuff.").unwrap();

        let content = load_playbook(dir.path(), "ci-triage");
        assert!(content.is_some());
        assert!(content.unwrap().contains("CI Triage"));
    }

    #[test]
    fn test_load_playbook_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_playbook(dir.path(), "nonexistent").is_none());
    }

    #[test]
    fn test_load_playbook_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(apiari_dir.join("context.md"), "secret").unwrap();

        // Attempting to traverse up should be rejected
        assert!(load_playbook(dir.path(), "../context").is_none());
        assert!(load_playbook(dir.path(), "foo/bar").is_none());
        assert!(load_playbook(dir.path(), "").is_none());
    }

    #[test]
    fn test_is_valid_playbook_name() {
        assert!(is_valid_playbook_name("ci-triage"));
        assert!(is_valid_playbook_name("deploy_prod"));
        assert!(is_valid_playbook_name("step1"));
        assert!(!is_valid_playbook_name("../context"));
        assert!(!is_valid_playbook_name("foo/bar"));
        assert!(!is_valid_playbook_name(""));
        assert!(!is_valid_playbook_name("has spaces"));
        assert!(!is_valid_playbook_name("has.dot"));
    }
}

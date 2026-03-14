//! `apiari init` — scaffold a workspace config from cwd.

use color_eyre::eyre::{Result, WrapErr};
use std::path::Path;

use crate::config::workspaces_dir;

/// Generate a template workspace TOML.
fn workspace_template(root: &Path, coordinator_name: &str) -> String {
    let root_str = root.display();
    let swarm_state = root.join(".swarm/state.json");
    let default_prompt = crate::buzz::coordinator::prompt::default_preamble(coordinator_name);
    // Indent for TOML multi-line string
    let prompt_lines: String = default_prompt
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"root = "{root_str}"
repos = []  # empty = auto-discover from workspace root

# [telegram]
# bot_token = "your-bot-token-from-botfather"
# chat_id = 0
# topic_id = 0

[coordinator]
model = "sonnet"
max_turns = 20
prompt = """
{prompt_lines}"""

# [watchers.github]
# repos = ["owner/repo1", "owner/repo2"]
# interval_secs = 120

# [watchers.sentry]
# org = "my-org"
# project = "my-project"
# token = "sntrys_..."
# interval_secs = 120

# [[commands]]
# name = "reinstall"
# script = "cargo install --force --path {root_str}/cli/crates/apiari 2>&1 && cargo install --force --path {root_str}/swarm 2>&1"
# description = "Rebuild and install apiari and swarm binaries"
# restart = true

# [watchers.swarm]
# state_path = "{swarm_state}"
# interval_secs = 15
"#,
        swarm_state = swarm_state.display(),
    )
}

/// Run `apiari init`, creating a workspace TOML from cwd.
pub fn run_init(name_override: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let name = name_override.unwrap_or_else(|| {
        cwd.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
    });

    let dir = workspaces_dir();
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("failed to create {}", dir.display()))?;

    let config_path = dir.join(format!("{name}.toml"));
    if config_path.exists() {
        println!("Workspace config already exists: {}", config_path.display());
        println!("Edit it to update configuration.");
        return Ok(());
    }

    let template = workspace_template(&cwd, "Bee");
    std::fs::write(&config_path, &template)?;
    println!("Created workspace config: {}", config_path.display());
    println!("Edit the [coordinator] prompt to customize behavior.");

    Ok(())
}

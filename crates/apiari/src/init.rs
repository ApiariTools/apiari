//! `apiari init` — scaffold a workspace config from cwd.

use color_eyre::eyre::{Result, WrapErr};
use std::io::{IsTerminal, Read, Write};
use std::path::Path;

use crate::config::{CURRENT_CONFIG_VERSION, workspaces_dir};

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
        r#"config_version = {CURRENT_CONFIG_VERSION}
root = "{root_str}"
repos = []  # empty = auto-discover from workspace root

# [telegram]
# bot_token = "your-bot-token-from-botfather"
# chat_id = 0
# topic_id = 0

# Token efficiency defaults — applied to all bees in this workspace.
# Override per-bee with [coordinator.token_controls] or [[bees.token_controls]].
# These values are already active by default; uncomment to change them.
# [token_controls]
# thinking_enabled = false  # disables extended thinking (biggest cost saver for agentic loops)
# bash_max_output = 20000   # cap bash output at 20k chars (prevents context floods)
# autocompact_pct = 70      # compact at 70% full instead of waiting until 95%+

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

    let config_display = config_path.display();

    println!("\n  \u{2713} Created {config_display}\n");

    // Scaffold .apiari/ directory with soul.md, context.md, and skills/
    let apiari_dir = cwd.join(".apiari");
    let skills_dir = apiari_dir.join("skills");
    std::fs::create_dir_all(&skills_dir)
        .wrap_err_with(|| format!("failed to create {}", skills_dir.display()))?;

    let soul_path = apiari_dir.join("soul.md");
    if !soul_path.exists() {
        let soul_template = "\
# Soul

Communication style and behavioral guidelines for the coordinator.
";
        std::fs::write(&soul_path, soul_template)
            .wrap_err_with(|| format!("failed to write {}", soul_path.display()))?;
        println!("  \u{2713} Created .apiari/soul.md \u{2014} defines the coordinator's communication style\n");
    }

    let context_path = apiari_dir.join("context.md");
    if !context_path.exists() {
        let context_template = "\
# Project Name

What is this project? (1-2 sentences)

## Stack

## Team / ownership

## Key conventions

## Anything the coordinator should always know
";
        std::fs::write(&context_path, context_template)
            .wrap_err_with(|| format!("failed to write {}", context_path.display()))?;
        println!("  \u{2713} Created .apiari/context.md \u{2014} fill this in with info about your project\n");
    }

    // Check if swarm is available in PATH
    let swarm_installed = std::process::Command::new("which")
        .arg("swarm")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if swarm_installed {
        println!("  \u{2713} swarm already installed\n");
    } else {
        println!("  swarm is not installed. Installing via cargo install...\n");

        let install_status = std::process::Command::new("cargo")
            .args(["install", "apiari-swarm"])
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();
        match install_status {
            Ok(s) if s.success() => {
                println!("\n  \u{2713} apiari-swarm installed successfully\n");
            }
            Ok(_) => {
                eprintln!("\n  Failed to install apiari-swarm. You can retry manually:");
                eprintln!("  cargo install apiari-swarm\n");
            }
            Err(e) => {
                eprintln!("\n  Could not run cargo install: {e}");
                eprintln!("  Install manually: cargo install apiari-swarm\n");
            }
        }
    }
    println!("  Next steps:\n");
    println!("  1. Get a Telegram bot token from @BotFather (https://t.me/BotFather)");
    println!("     Send /newbot, follow the prompts, copy the token.\n");
    println!("  2. Get your chat ID by messaging @userinfobot (https://t.me/userinfobot)\n");
    println!("  3. Edit your config:");
    println!("     $EDITOR {config_display}\n");
    println!("  4. Start the daemon:");
    println!("     apiari daemon start\n");
    println!("  5. Open the dashboard:");
    println!("     apiari ui\n");

    // Offer to open in $EDITOR if stdin is a TTY
    if let Ok(editor) = std::env::var("EDITOR")
        && std::io::stdin().is_terminal()
    {
        print!("  Press enter to open in {editor}, or ctrl+c to skip: ");
        std::io::stdout().flush()?;
        let mut buf = [0u8; 1];
        // Read one byte — enter proceeds, anything else or error skips
        if std::io::stdin().read(&mut buf).is_ok() && buf[0] == b'\n' {
            let status = std::process::Command::new(&editor)
                .arg(&config_path)
                .status();
            if let Err(e) = status {
                eprintln!("  Failed to open editor: {e}");
            }
        }
    }

    Ok(())
}

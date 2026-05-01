use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

pub struct InitArgs {
    pub name: String,
    pub root: Option<String>,
}

fn is_valid_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name != "."
}

pub fn run(args: InitArgs, config_dir: &Path) -> color_eyre::Result<()> {
    if !is_valid_workspace_name(&args.name) {
        color_eyre::eyre::bail!(
            "Invalid workspace name {:?}. Name must not contain path separators or '..'.",
            args.name
        );
    }

    let root = match args.root {
        Some(r) => PathBuf::from(r),
        None => {
            let cwd = std::env::current_dir()?;
            if io::stdin().is_terminal() {
                print!("Workspace root [{}]: ", cwd.display());
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let input = input.trim();
                if input.is_empty() {
                    cwd
                } else {
                    PathBuf::from(input)
                }
            } else {
                cwd
            }
        }
    };

    // Ensure root is absolute
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    // Resolve symlinks if possible, but keep the absolute path either way
    let root = std::fs::canonicalize(&root).unwrap_or(root);

    // Create workspace TOML config
    let workspaces_dir = config_dir.join("workspaces");
    std::fs::create_dir_all(&workspaces_dir)?;
    let config_path = workspaces_dir.join(format!("{}.toml", args.name));

    if config_path.exists() {
        println!("Skipping {} (already exists)", config_path.display());
    } else {
        // Escape backslashes for valid TOML strings (e.g. Windows paths)
        let root_escaped = root.display().to_string().replace('\\', "\\\\");
        let name_escaped = args.name.replace('\\', "\\\\").replace('"', "\\\"");
        let toml_content = format!(
            r##"[workspace]
root = "{root}"
name = "{name}"
description = ""

# Uncomment and configure additional bots:
# [[bots]]
# name = "CI Watch"
# color = "#e85555"
# role = "Monitor CI failures"
# provider = "claude"         # claude | codex | gemini
# watch = ["github"]
#
# [[bots]]
# name = "Reviewer"
# color = "#5cb85c"
# role = "Code quality reviewer"
# provider = "claude"
# schedule = "0 9 * * 1"       # cron expression (minute hour day month weekday)
# proactive_prompt = "Review recent PRs and summarize trends"

# Voice (optional):
# tts_voice = "af_nova"
# tts_speed = 1.2
"##,
            root = root_escaped,
            name = name_escaped,
        );
        std::fs::write(&config_path, toml_content)?;
    }

    // Create .apiari/ directory structure
    let apiari_dir = root.join(".apiari");
    std::fs::create_dir_all(&apiari_dir)?;

    let context_path = apiari_dir.join("context.md");
    write_if_missing(&context_path, CONTEXT_TEMPLATE)?;

    let soul_path = apiari_dir.join("soul.md");
    write_if_missing(&soul_path, SOUL_TEMPLATE)?;

    let docs_dir = apiari_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)?;
    let gitkeep = docs_dir.join(".gitkeep");
    write_if_missing(&gitkeep, "")?;

    // Print summary
    println!();
    println!("Workspace \"{}\" initialized!", args.name);
    println!();
    println!("  Config:  {}", config_path.display());
    println!("  Context: {}", context_path.display());
    println!("  Style:   {}", soul_path.display());
    println!("  Docs:    {}/", docs_dir.display());
    println!();
    println!("Edit the config to add custom bots, then restart hive.");
    println!("Chat with Main bot to get help configuring your workspace.");

    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> color_eyre::Result<()> {
    if path.exists() {
        println!("Skipping {} (already exists)", path.display());
    } else {
        std::fs::write(path, content)?;
    }
    Ok(())
}

const CONTEXT_TEMPLATE: &str = r#"# Project Context

Describe your project here. This context is included in all bot prompts.

## Tech Stack
-

## Key Concepts
-
"#;

const SOUL_TEMPLATE: &str = r#"# Communication Style

How should bots communicate? Examples:
- Be concise and direct
- Lead with actionable information
- Use technical language appropriate for senior engineers
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_config() {
        let config_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();

        let args = InitArgs {
            name: "testproject".to_string(),
            root: Some(root_dir.path().to_string_lossy().to_string()),
        };

        run(args, config_dir.path()).unwrap();

        let config_path = config_dir.path().join("workspaces/testproject.toml");
        assert!(config_path.exists(), "config TOML should exist");

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("name = \"testproject\""));
        assert!(content.contains(&format!(
            "root = \"{}\"",
            root_dir.path().canonicalize().unwrap().display()
        )));
    }

    #[test]
    fn test_init_creates_apiari_dir() {
        let config_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();

        let args = InitArgs {
            name: "testproject".to_string(),
            root: Some(root_dir.path().to_string_lossy().to_string()),
        };

        run(args, config_dir.path()).unwrap();

        let apiari = root_dir.path().join(".apiari");
        assert!(apiari.join("context.md").exists());
        assert!(apiari.join("soul.md").exists());
        assert!(apiari.join("docs/.gitkeep").exists());

        let context = std::fs::read_to_string(apiari.join("context.md")).unwrap();
        assert!(context.contains("# Project Context"));

        let soul = std::fs::read_to_string(apiari.join("soul.md")).unwrap();
        assert!(soul.contains("# Communication Style"));
    }

    #[test]
    fn test_init_skips_existing() {
        let config_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();

        let args = InitArgs {
            name: "testproject".to_string(),
            root: Some(root_dir.path().to_string_lossy().to_string()),
        };
        run(args, config_dir.path()).unwrap();

        // Modify files to verify they won't be overwritten
        let config_path = config_dir.path().join("workspaces/testproject.toml");
        std::fs::write(&config_path, "custom content").unwrap();

        let context_path = root_dir.path().join(".apiari/context.md");
        std::fs::write(&context_path, "custom context").unwrap();

        // Run init again
        let args = InitArgs {
            name: "testproject".to_string(),
            root: Some(root_dir.path().to_string_lossy().to_string()),
        };
        run(args, config_dir.path()).unwrap();

        // Verify files were NOT overwritten
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "custom content"
        );
        assert_eq!(
            std::fs::read_to_string(&context_path).unwrap(),
            "custom context"
        );
    }

    #[test]
    fn test_init_rejects_invalid_name() {
        let config_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();

        for bad_name in &["../escape", "foo/bar", "foo\\bar", "..", "."] {
            let args = InitArgs {
                name: bad_name.to_string(),
                root: Some(root_dir.path().to_string_lossy().to_string()),
            };
            assert!(
                run(args, config_dir.path()).is_err(),
                "should reject name: {bad_name}"
            );
        }
    }

    #[test]
    fn test_init_with_root_arg() {
        let config_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();
        let custom_root = root_dir.path().join("custom/path");
        std::fs::create_dir_all(&custom_root).unwrap();

        let args = InitArgs {
            name: "myws".to_string(),
            root: Some(custom_root.to_string_lossy().to_string()),
        };

        run(args, config_dir.path()).unwrap();

        let config_path = config_dir.path().join("workspaces/myws.toml");
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains(&format!(
            "root = \"{}\"",
            custom_root.canonicalize().unwrap().display()
        )));

        // .apiari should be created inside the custom root
        assert!(custom_root.join(".apiari/context.md").exists());
    }
}

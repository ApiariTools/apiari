//! `/doctor` — workspace health checker.
//!
//! Inspects the workspace config and `.apiari/` scaffold, reports issues,
//! and optionally fixes them with `--fix`.

use std::{fs::OpenOptions, io::Write, path::Path};

use crate::config::{CURRENT_CONFIG_VERSION, WorkspaceConfig};

/// Run the doctor check for a workspace. Returns the report text.
///
/// If `fix` is true, missing `.apiari/` files are scaffolded (never overwrites).
pub fn run(workspace_name: &str, config: &WorkspaceConfig, fix: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut issues = 0u32;
    let mut scaffolded = 0u32;

    lines.push(format!(
        "apiari doctor \u{2014} workspace: {workspace_name}\n"
    ));

    // ── config_version ───────────────────────────────────────
    match config.config_version {
        Some(v) if v == CURRENT_CONFIG_VERSION => {
            lines.push(format!("\u{2705} config_version: {v} (current)"));
        }
        Some(v) if v > CURRENT_CONFIG_VERSION => {
            lines.push(format!(
                "\u{26a0}\u{fe0f}  config_version: {v} \u{2014} this binary only knows up to \
                 {CURRENT_CONFIG_VERSION}. You may need to update apiari."
            ));
            issues += 1;
        }
        Some(v) => {
            lines.push(format!(
                "\u{26a0}\u{fe0f}  config_version: {v} \u{2014} current is {CURRENT_CONFIG_VERSION}. \
                 Review the changelog for what\u{2019}s new."
            ));
            issues += 1;
        }
        None => {
            lines.push(format!(
                "\u{26a0}\u{fe0f}  config_version missing \u{2014} add `config_version = {CURRENT_CONFIG_VERSION}` \
                 to your workspace TOML"
            ));
            issues += 1;
        }
    }

    // ── .apiari/ files ───────────────────────────────────────
    let root = &config.root;
    let apiari_dir = root.join(".apiari");

    // soul.md
    let soul_path = apiari_dir.join("soul.md");
    if soul_path.exists() {
        lines.push("\u{2705} .apiari/soul.md exists".to_string());
    } else {
        lines.push(
            "\u{26a0}\u{fe0f}  .apiari/soul.md not found \u{2014} create one to customize coordinator personality"
                .to_string(),
        );
        issues += 1;
        if fix && scaffold_soul(&soul_path) {
            scaffolded += 1;
        }
    }

    // context.md
    let context_path = apiari_dir.join("context.md");
    if context_path.exists() {
        lines.push("\u{2705} .apiari/context.md exists".to_string());
    } else {
        lines.push(
            "\u{26a0}\u{fe0f}  .apiari/context.md not found \u{2014} create one to give the coordinator project context"
                .to_string(),
        );
        issues += 1;
        if fix && scaffold_context(&context_path) {
            scaffolded += 1;
        }
    }

    // skills/
    let skills_dir = apiari_dir.join("skills");
    if skills_dir.is_dir() {
        let count = std::fs::read_dir(&skills_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                    .count()
            })
            .unwrap_or(0);
        lines.push(format!(
            "\u{2705} .apiari/skills/ exists ({count} playbook{})",
            if count == 1 { "" } else { "s" }
        ));
    } else {
        lines.push(
            "\u{26a0}\u{fe0f}  .apiari/skills/ not found \u{2014} create it to add coordinator playbooks"
                .to_string(),
        );
        issues += 1;
        if fix && std::fs::create_dir_all(&skills_dir).is_ok() {
            scaffolded += 1;
        }
    }

    // ── binary version ───────────────────────────────────────
    let current_version = env!("CARGO_PKG_VERSION");
    let latest = check_latest_release();
    match latest {
        Some(ref tag) => {
            let tag_version = tag.strip_prefix('v').unwrap_or(tag);
            if tag_version == current_version {
                lines.push(format!("\u{2705} apiari v{current_version} (latest)"));
            } else {
                lines.push(format!(
                    "\u{26a0}\u{fe0f}  apiari v{current_version} \u{2014} latest is {tag}. \
                     Run /update to upgrade."
                ));
                issues += 1;
            }
        }
        None => {
            lines.push(format!(
                "\u{2705} apiari v{current_version} (could not check latest)"
            ));
        }
    }

    // ── summary ──────────────────────────────────────────────
    if fix && scaffolded > 0 {
        lines.push(format!(
            "\n\u{2705} Scaffolded {scaffolded} missing file{}.",
            if scaffolded == 1 { "" } else { "s" }
        ));
    }
    let unfixable = issues - scaffolded;
    if unfixable > 0 {
        lines.push(format!(
            "\n{unfixable} issue{} remaining that {} manual attention.",
            if unfixable == 1 { "" } else { "s" },
            if unfixable == 1 { "needs" } else { "need" }
        ));
    } else if !fix && issues > 0 {
        lines.push(format!(
            "\n{issues} issue{} found. Run `/doctor --fix` to scaffold missing files.",
            if issues == 1 { "" } else { "s" }
        ));
    }

    lines.join("\n")
}

/// Check the latest GitHub release tag. Returns `None` on any error.
fn check_latest_release() -> Option<String> {
    let output = std::process::Command::new("gh")
        .args([
            "release",
            "view",
            "--repo",
            "ApiariTools/apiari",
            "--json",
            "tagName",
            "-q",
            ".tagName",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tag.is_empty() { None } else { Some(tag) }
}

/// Atomically create soul.md only if it doesn't exist. Returns true on success.
fn scaffold_soul(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let template = "\
# Soul

Communication style and behavioral guidelines for the coordinator.
";
    write_new_file(path, template)
}

/// Atomically create context.md only if it doesn't exist. Returns true on success.
fn scaffold_context(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let template = "\
# Project Name

What is this project? (1-2 sentences)

## Stack

## Team / ownership

## Key conventions

## Anything the coordinator should always know
";
    write_new_file(path, template)
}

/// Write `contents` to `path` only if the file does not already exist (atomic create).
fn write_new_file(path: &Path, contents: &str) -> bool {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => f.write_all(contents.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    /// Build a minimal WorkspaceConfig pointing at `root`.
    fn test_config(root: std::path::PathBuf, config_version: Option<u32>) -> WorkspaceConfig {
        WorkspaceConfig {
            config_version,
            root,
            repos: vec![],
            authority: WorkspaceAuthority::default(),
            capabilities: WorkspaceCapabilities::default(),
            telegram: None,
            coordinator: CoordinatorConfig::default(),
            watchers: WatchersConfig::default(),
            swarm: SwarmConfig::default(),
            pipeline: PipelineConfig::default(),
            commands: vec![],
            morning_brief: None,
            daemon_tcp_port: None,
            daemon_tcp_bind: None,
            daemon_host: None,
            daemon_port: None,
            daemon_endpoints: vec![],
            shells: ShellsConfig::default(),
            schedule: None,
            activity: crate::config::ActivityConfig::default(),
            update_source_path: None,
        }
    }

    #[test]
    fn test_config_version_none_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path().to_path_buf(), None);
        let text = run("test", &config, false);
        assert!(text.contains("config_version missing"), "{text}");
        assert!(text.contains("issue"), "{text}");
    }

    #[test]
    fn test_config_version_current_ok() {
        let tmp = tempfile::tempdir().unwrap();
        // Create .apiari scaffold so those don't count as issues
        let apiari = tmp.path().join(".apiari");
        std::fs::create_dir_all(apiari.join("skills")).unwrap();
        std::fs::write(apiari.join("soul.md"), "x").unwrap();
        std::fs::write(apiari.join("context.md"), "x").unwrap();

        let config = test_config(tmp.path().to_path_buf(), Some(CURRENT_CONFIG_VERSION));
        let text = run("test", &config, false);
        assert!(
            text.contains(&format!(
                "config_version: {CURRENT_CONFIG_VERSION} (current)"
            )),
            "{text}"
        );
        // No issues (besides possibly version check)
        assert!(!text.contains("config_version missing"), "{text}");
    }

    #[test]
    fn test_config_version_older_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path().to_path_buf(), Some(0));
        let text = run("test", &config, false);
        assert!(text.contains("config_version: 0"), "{text}");
        assert!(text.contains("current is"), "{text}");
    }

    #[test]
    fn test_config_version_newer_warns_incompatible() {
        let tmp = tempfile::tempdir().unwrap();
        let future_version = CURRENT_CONFIG_VERSION + 1;
        let config = test_config(tmp.path().to_path_buf(), Some(future_version));
        let text = run("test", &config, false);
        assert!(
            text.contains("this binary only knows up to"),
            "should warn about newer version: {text}"
        );
    }

    #[test]
    fn test_fix_creates_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path().to_path_buf(), Some(CURRENT_CONFIG_VERSION));

        let text = run("test", &config, true);
        assert!(text.contains("Scaffolded"), "{text}");

        // Files should now exist
        assert!(tmp.path().join(".apiari/soul.md").exists());
        assert!(tmp.path().join(".apiari/context.md").exists());
        assert!(tmp.path().join(".apiari/skills").is_dir());
    }

    #[test]
    fn test_fix_does_not_overwrite_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let apiari = tmp.path().join(".apiari");
        std::fs::create_dir_all(apiari.join("skills")).unwrap();
        std::fs::write(apiari.join("soul.md"), "custom soul").unwrap();
        std::fs::write(apiari.join("context.md"), "custom context").unwrap();

        let config = test_config(tmp.path().to_path_buf(), Some(CURRENT_CONFIG_VERSION));
        let _ = run("test", &config, true);

        // Originals should be untouched
        assert_eq!(
            std::fs::read_to_string(apiari.join("soul.md")).unwrap(),
            "custom soul"
        );
        assert_eq!(
            std::fs::read_to_string(apiari.join("context.md")).unwrap(),
            "custom context"
        );
    }

    #[test]
    fn test_fix_summary_distinguishes_fixable_from_unfixable() {
        let tmp = tempfile::tempdir().unwrap();
        // config_version None = unfixable, missing files = fixable
        let config = test_config(tmp.path().to_path_buf(), None);
        let text = run("test", &config, true);
        assert!(text.contains("Scaffolded"), "{text}");
        assert!(text.contains("manual attention"), "{text}");
    }
}

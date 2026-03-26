//! `/doctor` — workspace health checker.
//!
//! Inspects the workspace config and `.apiari/` scaffold, reports issues,
//! and optionally fixes them with `--fix`.

use std::path::Path;

use crate::config::{CURRENT_CONFIG_VERSION, WorkspaceConfig};

/// Run the doctor check for a workspace. Returns the report text.
///
/// If `fix` is true, missing `.apiari/` files are scaffolded (never overwrites).
pub fn run(workspace_name: &str, config: &WorkspaceConfig, fix: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut issues = 0u32;

    lines.push(format!(
        "apiari doctor \u{2014} workspace: {workspace_name}\n"
    ));

    // ── config_version ───────────────────────────────────────
    match config.config_version {
        Some(v) if v >= CURRENT_CONFIG_VERSION => {
            lines.push(format!("\u{2705} config_version: {v} (current)"));
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
        if fix {
            scaffold_soul(&soul_path);
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
        if fix {
            scaffold_context(&context_path);
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
        if fix {
            let _ = std::fs::create_dir_all(&skills_dir);
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
    if issues > 0 && !fix {
        lines.push(format!(
            "\n{issues} issue{} found. Run `/doctor --fix` to scaffold missing files.",
            if issues == 1 { "" } else { "s" }
        ));
    } else if issues > 0 && fix {
        lines.push("\n\u{2705} Scaffolded missing files.".to_string());
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

fn scaffold_soul(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let template = "\
# Soul

Communication style and behavioral guidelines for the coordinator.
";
    let _ = std::fs::write(path, template);
}

fn scaffold_context(path: &Path) {
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
    let _ = std::fs::write(path, template);
}

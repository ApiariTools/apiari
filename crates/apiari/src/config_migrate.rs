//! `apiari config migrate` — rewrite workspace configs into the current schema.
//!
//! Creates a `.bak` file before writing.

use std::path::{Component, Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};

use crate::config::{
    CURRENT_CONFIG_VERSION, WorkspaceConfig, WorkspaceFileFormat, detect_workspace_file_format,
    load_workspace, workspaces_dir,
};

fn sanitize_workspace_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("invalid workspace name: {name:?}");
    }
    let path = Path::new(name);
    let components: Vec<_> = path.components().collect();
    if components.len() != 1 || !matches!(components[0], Component::Normal(_)) {
        bail!("invalid workspace name: {name:?}");
    }
    Ok(())
}

pub fn run(workspace: Option<&str>) -> Result<i32> {
    let dir = workspaces_dir();
    if let Some(name) = workspace {
        sanitize_workspace_name(name)?;
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            eprintln!("✗ {name}: file not found ({})", path.display());
            return Ok(1);
        }
        migrate_one(name, &path)?;
        return Ok(0);
    }

    if !dir.exists() {
        println!("No workspaces directory found at {}", dir.display());
        return Ok(0);
    }

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .wrap_err_with(|| format!("failed to read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        println!("No workspace configs found in {}", dir.display());
        return Ok(0);
    }

    let mut any_failed = false;
    for entry in entries {
        let path = entry.path();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        if let Err(err) = migrate_one(name, &path) {
            eprintln!("✗ {name}: {err}");
            any_failed = true;
        }
    }

    Ok(if any_failed { 1 } else { 0 })
}

fn migrate_one(name: &str, path: &Path) -> Result<()> {
    let backup_path = backup_path(path);
    let format = detect_workspace_file_format(path)?;
    let backup_format = if backup_path.exists() {
        detect_workspace_file_format(&backup_path).ok()
    } else {
        None
    };

    let source_path = if matches!(format, WorkspaceFileFormat::Current)
        && matches!(backup_format, Some(WorkspaceFileFormat::HiveCompat))
    {
        backup_path.as_path()
    } else {
        path
    };

    let source_format = detect_workspace_file_format(source_path)?;
    let mut config: WorkspaceConfig = load_workspace(source_path)?;
    let needs_version = config.config_version != Some(CURRENT_CONFIG_VERSION);

    if matches!(source_format, WorkspaceFileFormat::Current) && !needs_version {
        println!("✓ {name}: already current");
        return Ok(());
    }

    config.config_version = Some(CURRENT_CONFIG_VERSION);
    let rendered = toml::to_string_pretty(&config)
        .wrap_err_with(|| format!("failed to render migrated config for {}", path.display()))?;

    if !backup_path.exists() {
        std::fs::copy(path, &backup_path).wrap_err_with(|| {
            format!(
                "failed to create backup {} from {}",
                backup_path.display(),
                path.display()
            )
        })?;
    }

    std::fs::write(path, rendered)
        .wrap_err_with(|| format!("failed to write migrated config {}", path.display()))?;

    match source_format {
        WorkspaceFileFormat::HiveCompat => {
            println!("✓ {name}: migrated Hive-compatible config → current schema");
        }
        WorkspaceFileFormat::Current => {
            println!("✓ {name}: updated config_version → {CURRENT_CONFIG_VERSION}");
        }
    }
    if source_path != path {
        println!("  source: {}", source_path.display());
    }
    println!("  backup: {}", backup_path.display());
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace.toml")
        .to_string();
    file_name.push_str(".bak");
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrate_hive_workspace_to_current_schema() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mgm");
        std::fs::create_dir_all(root.join(".apiari")).unwrap();
        std::fs::write(
            root.join(".apiari/services.toml"),
            r#"
[sentry]
org = "josh-holtz"
project = "mostly-good-metrics"
token = "sntryu_test"
"#,
        )
        .unwrap();
        let path = dir.path().join("mgm.toml");
        std::fs::write(
            &path,
            format!(
                r#"
[workspace]
root = "{}"
name = "mgm"
default_agent = "codex"

[[bots]]
name = "Triage"
watch = ["sentry"]
services = ["sentry"]
"#,
                root.display()
            ),
        )
        .unwrap();

        migrate_one("mgm", &path).unwrap();

        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("config_version = 3"));
        assert!(migrated.contains("[watchers.sentry]"));
        assert!(migrated.contains("org = \"josh-holtz\""));
        assert!(path.with_file_name("mgm.toml.bak").exists());
        assert!(toml::from_str::<WorkspaceConfig>(&migrated).is_ok());
    }

    #[test]
    fn test_migrate_current_workspace_only_updates_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apiari.toml");
        std::fs::write(
            &path,
            r#"
root = "/tmp/apiari"
"#,
        )
        .unwrap();

        migrate_one("apiari", &path).unwrap();

        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("config_version = 3"));
        assert!(path.with_file_name("apiari.toml.bak").exists());
    }

    #[test]
    fn test_migrate_current_workspace_uses_hive_backup_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mgm");
        std::fs::create_dir_all(root.join(".apiari/bots")).unwrap();
        std::fs::write(
            root.join(".apiari/bots/triage.md"),
            "Focus on new issues and decide what to do.",
        )
        .unwrap();

        let path = dir.path().join("mgm.toml");
        std::fs::write(
            &path,
            r#"
config_version = 3
root = "/tmp/mgm"
"#,
        )
        .unwrap();
        std::fs::write(
            path.with_file_name("mgm.toml.bak"),
            format!(
                r##"
[workspace]
root = "{}"

[[bots]]
name = "Triage"
role = "Detect, triage, and fix Sentry issues"
prompt_file = ".apiari/bots/triage.md"
watch = ["sentry"]
schedule = "*/30 * * * *"
proactive_prompt = "Check for new issues."
"##,
                root.display()
            ),
        )
        .unwrap();

        migrate_one("mgm", &path).unwrap();

        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("name = \"Triage\""));
        assert!(migrated.contains("heartbeat = \"30m\""));
        assert!(migrated.contains("heartbeat_prompt = \"Check for new issues.\""));
        assert!(migrated.contains("source = \"sentry\""));
        assert!(migrated.contains("Focus on new issues and decide what to do."));
    }
}

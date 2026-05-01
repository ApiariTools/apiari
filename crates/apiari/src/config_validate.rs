//! `apiari config validate` — validate workspace TOML config files.
//!
//! Parses each workspace config and checks it deserializes into `WorkspaceConfig`.
//! Prints a summary line per workspace and exits non-zero if any fail.

use std::path::{Component, Path};

use color_eyre::eyre::{Result, WrapErr};

/// Run validation for one or all workspace configs.
///
/// Returns exit code: 0 if all valid, 1 if any fail.
pub fn run(workspace: Option<&str>) -> Result<i32> {
    let dir = crate::config::workspaces_dir();
    run_with_dir(workspace, &dir)
}

/// Sanitize a workspace name — reject path separators and `.`/`..` components.
///
/// Uses `Path::components()` so that names like `.hidden` or `foo..bar` are
/// allowed (they are single `Normal` components), while `..`, `.`, and anything
/// containing path separators is rejected.
fn sanitize_workspace_name(name: &str) -> Result<()> {
    if name.is_empty() {
        color_eyre::eyre::bail!("invalid workspace name: {name:?}");
    }
    let path = Path::new(name);
    let components: Vec<_> = path.components().collect();
    // Must be exactly one Normal component (no separators, no . or ..)
    if components.len() != 1 || !matches!(components[0], Component::Normal(_)) {
        color_eyre::eyre::bail!("invalid workspace name: {name:?}");
    }
    Ok(())
}

/// Inner implementation that accepts a directory, for testability.
fn run_with_dir(workspace: Option<&str>, dir: &Path) -> Result<i32> {
    if let Some(name) = workspace {
        sanitize_workspace_name(name)?;
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            eprintln!("\u{2717} {name}: file not found ({})", path.display());
            return Ok(1);
        }
        match crate::config::load_workspace(&path) {
            Ok(_) => {
                println!("\u{2713} {name}: valid");
                Ok(0)
            }
            Err(e) => {
                eprintln!("\u{2717} {name}: {e}");
                Ok(1)
            }
        }
    } else {
        // Validate all workspace configs
        if !dir.exists() {
            println!("No workspaces directory found at {}", dir.display());
            return Ok(0);
        }

        let mut entries: Vec<_> = std::fs::read_dir(dir)
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

            match crate::config::load_workspace(&path) {
                Ok(_) => println!("\u{2713} {name}: valid"),
                Err(e) => {
                    eprintln!("\u{2717} {name}: {e}");
                    any_failed = true;
                }
            }
        }

        Ok(if any_failed { 1 } else { 0 })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn test_validate_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "root = \"/tmp/test\"").unwrap();
        drop(f);

        let result = crate::config::load_workspace(&path);
        assert!(result.is_ok(), "valid config should parse: {result:?}");
    }

    #[test]
    fn test_validate_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "not_a_real_field = {{ broken").unwrap();
        drop(f);

        let result = crate::config::load_workspace(&path);
        assert!(result.is_err(), "invalid TOML should fail to parse");
    }

    #[test]
    fn test_run_single_valid_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("myws.toml")).unwrap();
        writeln!(f, "root = \"/tmp/myws\"").unwrap();
        drop(f);

        let code = run_with_dir(Some("myws"), dir.path()).unwrap();
        assert_eq!(code, 0, "valid workspace should return 0");
    }

    #[test]
    fn test_run_single_invalid_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("bad.toml")).unwrap();
        writeln!(f, "not_valid {{{{ toml").unwrap();
        drop(f);

        let code = run_with_dir(Some("bad"), dir.path()).unwrap();
        assert_eq!(code, 1, "invalid workspace should return 1");
    }

    #[test]
    fn test_run_single_missing_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let code = run_with_dir(Some("nonexistent"), dir.path()).unwrap();
        assert_eq!(code, 1, "missing workspace should return 1");
    }

    #[test]
    fn test_run_all_workspaces_mixed() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("good.toml")).unwrap();
        writeln!(f, "root = \"/tmp/good\"").unwrap();
        drop(f);

        let mut f = std::fs::File::create(dir.path().join("bad.toml")).unwrap();
        writeln!(f, "broken {{{{ toml").unwrap();
        drop(f);

        let code = run_with_dir(None, dir.path()).unwrap();
        assert_eq!(code, 1, "mixed valid/invalid should return 1");
    }

    #[test]
    fn test_run_all_workspaces_all_valid() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("a.toml")).unwrap();
        writeln!(f, "root = \"/tmp/a\"").unwrap();
        drop(f);

        let mut f = std::fs::File::create(dir.path().join("b.toml")).unwrap();
        writeln!(f, "root = \"/tmp/b\"").unwrap();
        drop(f);

        let code = run_with_dir(None, dir.path()).unwrap();
        assert_eq!(code, 0, "all valid should return 0");
    }

    #[test]
    fn test_run_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let code = run_with_dir(None, dir.path()).unwrap();
        assert_eq!(code, 0, "empty dir should return 0");
    }

    #[test]
    fn test_sanitize_rejects_path_traversal() {
        assert!(sanitize_workspace_name("../etc/passwd").is_err());
        assert!(sanitize_workspace_name("foo/bar").is_err());
        assert!(sanitize_workspace_name("..").is_err());
        assert!(sanitize_workspace_name(".").is_err());
        assert!(sanitize_workspace_name("").is_err());
    }

    #[test]
    fn test_sanitize_accepts_valid_names() {
        assert!(sanitize_workspace_name("myworkspace").is_ok());
        assert!(sanitize_workspace_name("my-workspace").is_ok());
        assert!(sanitize_workspace_name("my_workspace").is_ok());
        assert!(sanitize_workspace_name(".hidden").is_ok());
        assert!(sanitize_workspace_name("foo..bar").is_ok());
    }

    #[test]
    fn test_run_rejects_traversal_name() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_dir(Some("../evil"), dir.path());
        assert!(result.is_err(), "path traversal name should be rejected");
    }
}

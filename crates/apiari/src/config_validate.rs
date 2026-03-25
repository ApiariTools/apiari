//! `apiari config validate` — validate workspace TOML config files.
//!
//! Parses each workspace config and checks it deserializes into `WorkspaceConfig`.
//! Prints a summary line per workspace and exits non-zero if any fail.

use color_eyre::eyre::Result;

/// Run validation for one or all workspace configs.
///
/// Returns exit code: 0 if all valid, 1 if any fail.
pub fn run(workspace: Option<&str>) -> Result<i32> {
    let dir = crate::config::workspaces_dir();

    if let Some(name) = workspace {
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
                println!("\u{2717} {name}: {e}");
                Ok(1)
            }
        }
    } else {
        // Validate all workspace configs
        if !dir.exists() {
            println!("No workspaces directory found at {}", dir.display());
            return Ok(0);
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
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
                    println!("\u{2717} {name}: {e}");
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
}

//! `hive docs` CLI command — manage workspace reference docs in `.apiari/docs/`.
//!
//! Usage:
//!   hive docs list --workspace my-ws
//!   hive docs read --workspace my-ws overview.md
//!   hive docs write --workspace my-ws overview.md --file /tmp/doc.md
//!   hive docs delete --workspace my-ws overview.md

use clap::{Args, Subcommand};
use color_eyre::{Result, eyre::eyre};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct DocsArgs {
    #[command(subcommand)]
    pub command: DocsCommand,
}

#[derive(Subcommand)]
pub enum DocsCommand {
    /// List docs in the workspace
    List {
        /// Workspace ID (config filename stem)
        #[arg(long)]
        workspace: String,
    },
    /// Print a doc's contents to stdout
    Read {
        /// Workspace ID (config filename stem)
        #[arg(long)]
        workspace: String,
        /// Filename (e.g. overview.md)
        filename: String,
    },
    /// Write a doc to .apiari/docs/ and git commit
    Write {
        /// Workspace ID (config filename stem)
        #[arg(long)]
        workspace: String,
        /// Filename (e.g. overview.md)
        filename: String,
        /// Path to file containing the doc content
        #[arg(long)]
        file: String,
    },
    /// Delete a doc from .apiari/docs/ and git commit
    Delete {
        /// Workspace ID (config filename stem)
        #[arg(long)]
        workspace: String,
        /// Filename (e.g. overview.md)
        filename: String,
    },
}

/// Resolve workspace root from config file.
fn resolve_workspace_root(config_dir: &Path, workspace: &str) -> Result<PathBuf> {
    let config_path = config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let content = std::fs::read_to_string(&config_path)
        .map_err(|_| eyre!("workspace config not found: {}", config_path.display()))?;
    let config: toml::Value = toml::from_str(&content).map_err(|e| eyre!("invalid config: {e}"))?;
    let root = config
        .get("workspace")
        .and_then(|w| w.get("root"))
        .and_then(|r| r.as_str())
        .ok_or_else(|| eyre!("no workspace.root in config"))?;
    Ok(PathBuf::from(root))
}

fn validate_filename(filename: &str) -> Result<()> {
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(eyre!("invalid filename: path traversal not allowed"));
    }
    if !filename.ends_with(".md") {
        return Err(eyre!("filename must end with .md"));
    }
    Ok(())
}

fn docs_dir(root: &Path) -> PathBuf {
    root.join(".apiari/docs")
}

fn git_commit(root: &Path, file_path: &Path, message: &str) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(["add", "--"])
        .arg(file_path)
        .current_dir(root)
        .status()
        .map_err(|e| eyre!("git add failed: {e}"))?;
    if !status.success() {
        return Err(eyre!("git add exited with {status}"));
    }

    let status = std::process::Command::new("git")
        .args(["commit", "-m", message, "--"])
        .arg(file_path)
        .current_dir(root)
        .status()
        .map_err(|e| eyre!("git commit failed: {e}"))?;
    if !status.success() {
        return Err(eyre!("git commit exited with {status}"));
    }

    Ok(())
}

pub fn run(args: DocsArgs, config_dir: &Path) -> Result<()> {
    match args.command {
        DocsCommand::List { workspace } => {
            let root = resolve_workspace_root(config_dir, &workspace)?;
            let dir = docs_dir(&root);
            if !dir.is_dir() {
                println!("No docs found (directory does not exist).");
                return Ok(());
            }
            let mut entries: Vec<(String, String)> = Vec::new();
            for entry in std::fs::read_dir(&dir)?.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                let desc = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|c| {
                        c.lines()
                            .find(|l| !l.trim().is_empty())
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                    })
                    .unwrap_or_default();
                entries.push((name, desc));
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            if entries.is_empty() {
                println!("No docs found.");
            } else {
                for (name, desc) in &entries {
                    if desc.is_empty() {
                        println!("{name}");
                    } else {
                        println!("{name} — {desc}");
                    }
                }
            }
        }
        DocsCommand::Read {
            workspace,
            filename,
        } => {
            validate_filename(&filename)?;
            let root = resolve_workspace_root(config_dir, &workspace)?;
            let path = docs_dir(&root).join(&filename);
            let content =
                std::fs::read_to_string(&path).map_err(|_| eyre!("doc not found: {filename}"))?;
            print!("{content}");
        }
        DocsCommand::Write {
            workspace,
            filename,
            file,
        } => {
            validate_filename(&filename)?;
            let root = resolve_workspace_root(config_dir, &workspace)?;
            let dir = docs_dir(&root);
            std::fs::create_dir_all(&dir)?;
            let content =
                std::fs::read_to_string(&file).map_err(|e| eyre!("cannot read {file}: {e}"))?;
            let dest = dir.join(&filename);
            std::fs::write(&dest, &content)?;
            println!("Wrote {filename} ({} bytes)", content.len());
            if let Err(e) = git_commit(&root, &dest, &format!("docs: update {filename}")) {
                eprintln!("warning: git commit failed: {e}");
            }
        }
        DocsCommand::Delete {
            workspace,
            filename,
        } => {
            validate_filename(&filename)?;
            let root = resolve_workspace_root(config_dir, &workspace)?;
            let path = docs_dir(&root).join(&filename);
            if !path.exists() {
                return Err(eyre!("doc not found: {filename}"));
            }
            std::fs::remove_file(&path)?;
            println!("Deleted {filename}");
            if let Err(e) = git_commit(&root, &path, &format!("docs: remove {filename}")) {
                eprintln!("warning: git commit failed: {e}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_filename_valid() {
        assert!(validate_filename("overview.md").is_ok());
        assert!(validate_filename("my-doc.md").is_ok());
        assert!(validate_filename("notes_v2.md").is_ok());
    }

    #[test]
    fn test_validate_filename_rejects_traversal() {
        assert!(validate_filename("../etc/passwd").is_err());
        assert!(validate_filename("foo/bar.md").is_err());
        assert!(validate_filename("foo\\bar.md").is_err());
    }

    #[test]
    fn test_validate_filename_rejects_non_md() {
        assert!(validate_filename("readme.txt").is_err());
        assert!(validate_filename("script.js").is_err());
    }

    #[test]
    fn test_resolve_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("myws.toml"),
            "[workspace]\nroot = \"/tmp/myproject\"\nname = \"myws\"\n",
        )
        .unwrap();

        let root = resolve_workspace_root(dir.path(), "myws").unwrap();
        assert_eq!(root, PathBuf::from("/tmp/myproject"));
    }

    #[test]
    fn test_resolve_workspace_root_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_root(dir.path(), "nonexistent").is_err());
    }

    #[test]
    fn test_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        let args = DocsArgs {
            command: DocsCommand::List {
                workspace: "test".into(),
            },
        };
        assert!(run(args, dir.path()).is_ok());
    }

    #[test]
    fn test_list_with_docs() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("guide.md"), "# User Guide\nContent here.").unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        let args = DocsArgs {
            command: DocsCommand::List {
                workspace: "test".into(),
            },
        };
        assert!(run(args, dir.path()).is_ok());
    }

    #[test]
    fn test_read_doc() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("notes.md"), "# Notes\nSome notes.").unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        let args = DocsArgs {
            command: DocsCommand::Read {
                workspace: "test".into(),
                filename: "notes.md".into(),
            },
        };
        assert!(run(args, dir.path()).is_ok());
    }

    #[test]
    fn test_read_doc_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        let args = DocsArgs {
            command: DocsCommand::Read {
                workspace: "test".into(),
                filename: "nonexistent.md".into(),
            },
        };
        assert!(run(args, dir.path()).is_err());
    }

    #[test]
    fn test_write_doc() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        // Create a temp file with content
        let src = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(src.path(), "# New Doc\nHello world.").unwrap();

        let args = DocsArgs {
            command: DocsCommand::Write {
                workspace: "test".into(),
                filename: "new.md".into(),
                file: src.path().to_string_lossy().to_string(),
            },
        };
        // This will succeed for file write but git commit will warn (no git repo)
        assert!(run(args, dir.path()).is_ok());
        // Verify file was written
        let written = std::fs::read_to_string(root.path().join(".apiari/docs/new.md")).unwrap();
        assert_eq!(written, "# New Doc\nHello world.");
    }

    #[test]
    fn test_delete_doc_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        let args = DocsArgs {
            command: DocsCommand::Delete {
                workspace: "test".into(),
                filename: "nonexistent.md".into(),
            },
        };
        assert!(run(args, dir.path()).is_err());
    }

    #[test]
    fn test_delete_doc() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("old.md"), "# Old\nRemove me.").unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                root.path().display()
            ),
        )
        .unwrap();

        let args = DocsArgs {
            command: DocsCommand::Delete {
                workspace: "test".into(),
                filename: "old.md".into(),
            },
        };
        // File delete succeeds, git commit will warn (no git repo)
        assert!(run(args, dir.path()).is_ok());
        assert!(!docs.join("old.md").exists());
    }
}

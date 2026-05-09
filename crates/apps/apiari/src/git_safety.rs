//! Post-turn git safety check — detects unexpected file modifications.
//!
//! Takes a snapshot of `git status --porcelain` across all repo subdirectories
//! in a workspace, and can diff two snapshots to find newly dirty files.

use std::{
    any::Any,
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::buzz::coordinator::SafetyHooks;

/// Safety hooks backed by `git status` snapshots.
///
/// Captures dirty files before a coordinator turn and diffs after to detect
/// newly modified files.
pub struct GitSafetyHooks {
    pub workspace_root: PathBuf,
}

impl SafetyHooks for GitSafetyHooks {
    fn pre_turn(&self) -> Option<Box<dyn Any + Send>> {
        Some(Box::new(GitSnapshot::capture(&self.workspace_root)))
    }

    fn post_turn(&self, snapshot: Box<dyn Any + Send>) -> Vec<(String, String)> {
        let before = snapshot.downcast::<GitSnapshot>().expect("GitSnapshot");
        GitSnapshot::capture(&self.workspace_root).diff(&before)
    }
}

/// A snapshot of dirty files across all repos in a workspace.
#[derive(Debug, Clone)]
pub struct GitSnapshot {
    /// Set of (repo_name, file_path) pairs that are dirty.
    pub dirty_files: BTreeSet<(String, String)>,
}

impl GitSnapshot {
    /// Capture a snapshot of all dirty files in repos under `workspace_root`.
    ///
    /// Scans immediate subdirectories for `.git/` and runs `git status --porcelain`.
    pub fn capture(workspace_root: &Path) -> Self {
        let mut dirty_files = BTreeSet::new();

        let entries = match std::fs::read_dir(workspace_root) {
            Ok(e) => e,
            Err(_) => return Self { dirty_files },
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join(".git").exists() {
                continue;
            }

            let repo_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if let Ok(output) = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&path)
                .output()
                && output.status.success()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    // porcelain format: "XY filename" (3 chars prefix)
                    if line.len() > 3 {
                        let file = line[3..].trim().to_string();
                        dirty_files.insert((repo_name.clone(), file));
                    }
                }
            }
        }

        Self { dirty_files }
    }

    /// Find files that are newly dirty compared to a previous snapshot.
    pub fn diff(&self, previous: &GitSnapshot) -> Vec<(String, String)> {
        self.dirty_files
            .difference(&previous.dirty_files)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_snapshot_diff() {
        let a = GitSnapshot {
            dirty_files: BTreeSet::new(),
        };
        let b = GitSnapshot {
            dirty_files: BTreeSet::new(),
        };
        assert!(b.diff(&a).is_empty());
    }

    #[test]
    fn test_diff_finds_new_files() {
        let mut before = BTreeSet::new();
        before.insert(("repo".to_string(), "existing.rs".to_string()));
        let a = GitSnapshot {
            dirty_files: before,
        };

        let mut after = BTreeSet::new();
        after.insert(("repo".to_string(), "existing.rs".to_string()));
        after.insert(("repo".to_string(), "new_file.rs".to_string()));
        let b = GitSnapshot { dirty_files: after };

        let diff = b.diff(&a);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0], ("repo".to_string(), "new_file.rs".to_string()));
    }

    #[test]
    fn test_diff_ignores_cleaned_files() {
        let mut before = BTreeSet::new();
        before.insert(("repo".to_string(), "was_dirty.rs".to_string()));
        let a = GitSnapshot {
            dirty_files: before,
        };

        let b = GitSnapshot {
            dirty_files: BTreeSet::new(),
        };

        // No new dirty files
        assert!(b.diff(&a).is_empty());
    }

    #[test]
    fn test_capture_nonexistent_dir() {
        let snapshot = GitSnapshot::capture(Path::new("/nonexistent/path/12345"));
        assert!(snapshot.dirty_files.is_empty());
    }
}

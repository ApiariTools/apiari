//! `.task/` directory I/O helpers.
//!
//! Each worktree gets a `.task/` directory with structured artifacts that
//! guide agent execution.

use color_eyre::eyre::Result;
use std::path::Path;

#[allow(dead_code)]
pub const TASK_MD: &str = "TASK.md";
#[allow(dead_code)]
pub const CONTEXT_MD: &str = "CONTEXT.md";
#[allow(dead_code)]
pub const PLAN_MD: &str = "PLAN.md";
#[allow(dead_code)]
pub const PROGRESS_MD: &str = "PROGRESS.md";

/// Write TASK.md to a `.task/` directory (creates the dir if needed).
#[allow(dead_code)]
pub fn write_artifact(dir: &Path, filename: &str, content: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(filename), content)?;
    Ok(())
}

/// Read an artifact from a `.task/` directory. Returns `None` if the file doesn't exist.
#[allow(dead_code)]
pub fn read_artifact(dir: &Path, filename: &str) -> Result<Option<String>> {
    let path = dir.join(filename);
    if path.is_file() {
        Ok(Some(std::fs::read_to_string(&path)?))
    } else {
        Ok(None)
    }
}

/// Seed a `.task/` directory with the provided artifacts.
#[allow(dead_code)]
pub fn seed_task_dir(
    task_dir: &Path,
    task_md: &str,
    context_md: Option<&str>,
    plan_md: Option<&str>,
) -> Result<()> {
    write_artifact(task_dir, TASK_MD, task_md)?;
    if let Some(ctx) = context_md {
        write_artifact(task_dir, CONTEXT_MD, ctx)?;
    }
    if let Some(plan) = plan_md {
        write_artifact(task_dir, PLAN_MD, plan)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_and_read_artifact() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path().join(".task");

        write_artifact(&task_dir, TASK_MD, "# My Task\nDo stuff.").unwrap();
        let content = read_artifact(&task_dir, TASK_MD).unwrap();
        assert_eq!(content.as_deref(), Some("# My Task\nDo stuff."));
    }

    #[test]
    fn read_artifact_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path().join(".task");
        std::fs::create_dir_all(&task_dir).unwrap();

        let content = read_artifact(&task_dir, CONTEXT_MD).unwrap();
        assert!(content.is_none());
    }

    #[test]
    fn seed_task_dir_creates_all_files() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path().join(".task");

        seed_task_dir(&task_dir, "# Task", Some("# Context"), Some("# Plan")).unwrap();

        assert_eq!(
            read_artifact(&task_dir, TASK_MD).unwrap().as_deref(),
            Some("# Task")
        );
        assert_eq!(
            read_artifact(&task_dir, CONTEXT_MD).unwrap().as_deref(),
            Some("# Context")
        );
        assert_eq!(
            read_artifact(&task_dir, PLAN_MD).unwrap().as_deref(),
            Some("# Plan")
        );
    }

    #[test]
    fn seed_task_dir_optional_fields() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path().join(".task");

        seed_task_dir(&task_dir, "# Task only", None, None).unwrap();

        assert!(read_artifact(&task_dir, TASK_MD).unwrap().is_some());
        assert!(read_artifact(&task_dir, CONTEXT_MD).unwrap().is_none());
        assert!(read_artifact(&task_dir, PLAN_MD).unwrap().is_none());
    }

    #[test]
    fn seed_task_dir_creates_directory() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path().join("nested").join(".task");

        seed_task_dir(&task_dir, "# Task", None, None).unwrap();
        assert!(task_dir.is_dir());
    }
}

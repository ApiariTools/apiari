//! Tmux shell management for apiari workspaces.
//!
//! Provides opt-in tmux session/window management so each workspace can have
//! persistent shell windows — created automatically with workers or manually
//! by the user.

use std::path::Path;
use std::process::Command;

/// Information about a single tmux window.
#[derive(Debug, Clone)]
pub struct ShellWindow {
    pub name: String,
    pub working_dir: String,
    pub preview: String,
}

/// Manages tmux sessions and windows for a workspace.
#[derive(Debug, Clone)]
pub struct TmuxManager {
    session: String,
    available: bool,
}

impl TmuxManager {
    /// Create a new TmuxManager for the given session name.
    /// Checks whether tmux is installed; all operations become no-ops if not.
    pub fn new(session_name: &str) -> Self {
        let available = Command::new("which")
            .arg("tmux")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        Self {
            session: session_name.to_string(),
            available,
        }
    }

    /// Create a TmuxManager with a pre-determined availability flag.
    /// Use this to avoid redundant `which tmux` checks when availability
    /// has already been verified.
    pub fn with_availability(session_name: &str, available: bool) -> Self {
        Self {
            session: session_name.to_string(),
            available,
        }
    }

    /// Whether tmux is available on this system.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// The session name this manager targets.
    pub fn session_name(&self) -> &str {
        &self.session
    }

    /// Ensure the tmux session exists. Creates it detached if not present.
    pub fn ensure_session(&self) -> bool {
        if !self.available {
            return false;
        }
        // Check if session already exists
        let exists = Command::new("tmux")
            .args(["has-session", "-t", &self.session])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if exists {
            return true;
        }
        // Create detached session
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &self.session])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Create a new tmux window with the given name in the given working directory.
    /// Returns `false` if tmux is unavailable or the path is not valid UTF-8.
    pub fn create_window(&self, name: &str, working_dir: &Path) -> bool {
        if !self.available {
            return false;
        }
        let Some(dir_str) = working_dir.to_str() else {
            tracing::warn!(
                path = ?working_dir,
                "cannot create tmux window: working directory is not valid UTF-8"
            );
            return false;
        };
        self.ensure_session();
        Command::new("tmux")
            .args(["new-window", "-t", &self.session, "-n", name, "-c", dir_str])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Kill a tmux window by name.
    pub fn kill_window(&self, name: &str) -> bool {
        if !self.available {
            return false;
        }
        let target = format!("{}:{}", self.session, name);
        Command::new("tmux")
            .args(["kill-window", "-t", &target])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Capture a preview of the pane contents (last 5 lines only).
    pub fn capture_preview(&self, name: &str) -> Option<String> {
        if !self.available {
            return None;
        }
        let target = format!("{}:{}", self.session, name);
        let output = Command::new("tmux")
            .args(["capture-pane", "-t", &target, "-p", "-S", "-5"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // Return last non-empty line as preview
        let preview = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();
        Some(preview)
    }

    /// Build the command to attach to a specific window.
    /// Returns the args to pass to `tmux`. The caller is responsible for
    /// spawning the process (sync for TUI suspend/resume).
    pub fn attach_args(&self, name: &str) -> Vec<String> {
        let target = format!("{}:{}", self.session, name);
        vec![
            "attach-session".into(),
            "-t".into(),
            self.session.clone(),
            ";".into(),
            "select-window".into(),
            "-t".into(),
            target,
        ]
    }

    /// List all windows in the session.
    pub fn list_windows(&self) -> Vec<ShellWindow> {
        if !self.available {
            return Vec::new();
        }
        // Check session exists first
        let exists = Command::new("tmux")
            .args(["has-session", "-t", &self.session])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !exists {
            return Vec::new();
        }

        let output = match Command::new("tmux")
            .args([
                "list-windows",
                "-t",
                &self.session,
                "-F",
                "#{window_name}\t#{pane_current_path}",
            ])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };

        let text = String::from_utf8_lossy(&output.stdout);
        let mut windows: Vec<ShellWindow> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let mut parts = line.splitn(2, '\t');
                let name = parts.next().unwrap_or("").to_string();
                let working_dir = parts.next().unwrap_or("").to_string();
                let preview = self.capture_preview(&name).unwrap_or_default();
                ShellWindow {
                    name,
                    working_dir,
                    preview,
                }
            })
            .collect();

        windows.sort_by(|a, b| a.name.cmp(&b.name));
        windows
    }

    /// Resolve the session name for a workspace.
    /// Uses the configured name if set, otherwise "apiari-{workspace_name}".
    pub fn session_name_for(workspace_name: &str, config: &crate::config::ShellsConfig) -> String {
        config
            .tmux_session
            .clone()
            .unwrap_or_else(|| format!("apiari-{workspace_name}"))
    }
}

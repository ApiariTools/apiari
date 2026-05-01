//! `hive doctor` — diagnostic health checks for workspace, daemon, and tooling.

use crate::daemon::config::DaemonConfig;
use crate::workspace::{self, load_workspace};
use color_eyre::eyre::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── Data types ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Warn,
    Fail,
}

/// Auto-fix action that `--fix` can perform.
enum Fix {
    /// Remove stale entries from the workspace registry.
    PruneRegistry,
    /// Delete a file at the given path.
    RemoveFile(PathBuf),
}

struct Check {
    status: Status,
    message: String,
    hint: Option<String>,
    fix: Option<Fix>,
}

struct Category {
    name: &'static str,
    checks: Vec<Check>,
}

impl Check {
    fn pass(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Pass,
            message: msg.into(),
            hint: None,
            fix: None,
        }
    }

    fn warn(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            message: msg.into(),
            hint: None,
            fix: None,
        }
    }

    fn fail(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Fail,
            message: msg.into(),
            hint: None,
            fix: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
}

// ── Entry point ──────────────────────────────────────────

pub fn run(cwd: &Path, fix: bool) -> Result<()> {
    let categories = vec![
        check_workspace(cwd),
        check_daemon(),
        check_telegram(),
        check_swarm(),
        check_buzz(),
    ];

    print_report(&categories);

    if fix {
        apply_fixes(&categories);
    }

    let has_failures = categories
        .iter()
        .flat_map(|c| &c.checks)
        .any(|c| c.status == Status::Fail);

    if has_failures {
        std::process::exit(1);
    }

    Ok(())
}

// ── Category 1: Workspace ────────────────────────────────

fn check_workspace(cwd: &Path) -> Category {
    let mut checks = Vec::new();

    let reg_path = workspace::registry_path();
    if reg_path.exists() {
        checks.push(Check::pass(format!(
            "Registry file exists ({})",
            tilde(&reg_path)
        )));
    } else {
        checks.push(
            Check::warn(format!("Registry file missing ({})", tilde(&reg_path)))
                .with_hint("Run `hive init` in a workspace to create it"),
        );
    }

    // Current workspace registered?
    match load_workspace(cwd) {
        Ok(ws) => {
            let registry = workspace::load_registry();
            let found = registry.iter().any(|e| canonical_eq(&e.path, &ws.root));
            if found {
                checks.push(Check::pass(format!(
                    "Current workspace registered ({})",
                    ws.name
                )));
            } else {
                checks.push(
                    Check::warn(format!("Current workspace not in registry ({})", ws.name))
                        .with_hint("Run `hive init` to register it"),
                );
            }
        }
        Err(_) => {
            checks.push(
                Check::warn("No workspace config found in current directory")
                    .with_hint("Run `hive init` to create one"),
            );
        }
    }

    // Stale entries
    let all = workspace::load_registry_unfiltered();
    let live = workspace::load_registry();
    let stale = all.len().saturating_sub(live.len());
    if stale > 0 {
        checks.push(
            Check::warn(format!(
                "{stale} stale registry entry(ies) (paths no longer exist)"
            ))
            .with_fix(Fix::PruneRegistry),
        );
    }

    Category {
        name: "Workspace",
        checks,
    }
}

// ── Category 2: Daemon ───────────────────────────────────

fn check_daemon() -> Category {
    let mut checks = Vec::new();

    // Global PID check
    let global_pid_path = crate::daemon::pid_path();
    match crate::daemon::read_pid() {
        Some(pid) if crate::daemon::is_process_alive(pid) => {
            checks.push(Check::pass(format!("Daemon running (PID {pid})")));
        }
        Some(pid) => {
            checks.push(
                Check::fail(format!(
                    "Stale PID file (PID {pid} not running) at {}",
                    global_pid_path.display()
                ))
                .with_hint(
                    "Remove it: rm \"{}\"".replace("{}", &global_pid_path.display().to_string()),
                )
                .with_fix(Fix::RemoveFile(global_pid_path.clone())),
            );
        }
        None => {
            checks.push(Check::warn("Daemon not running (no PID file)"));
        }
    }

    // Stale workspace-relative PID files
    let registry = workspace::load_registry();
    for entry in &registry {
        let ws_pid = entry.path.join(".hive/daemon.pid");
        if ws_pid.exists() {
            // Read the PID and check if it's the same as the global daemon
            let ws_pid_val = std::fs::read_to_string(&ws_pid)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            let global_pid_val = crate::daemon::read_pid();

            match ws_pid_val {
                Some(pid) if Some(pid) == global_pid_val => {
                    // Same PID as global daemon — not a problem
                }
                Some(pid) if crate::daemon::is_process_alive(pid) => {
                    checks.push(
                        Check::fail(format!(
                            "Old workspace daemon still running (PID {pid}) at {}",
                            ws_pid.display()
                        ))
                        .with_hint(format!(
                            "Kill it and remove the PID file: kill {pid} && rm \"{}\"",
                            ws_pid.display()
                        )),
                    );
                }
                _ => {
                    checks.push(
                        Check::warn(format!("Stale PID file at {}", ws_pid.display()))
                            .with_hint(format!("Remove it: rm \"{}\"", ws_pid.display()))
                            .with_fix(Fix::RemoveFile(ws_pid.clone())),
                    );
                }
            }
        }
    }

    // Duplicate daemon processes
    let dup_count = count_daemon_processes();
    if dup_count > 1 {
        checks.push(
            Check::fail(format!(
                "{dup_count} daemon processes running (expected at most 1)"
            ))
            .with_hint("Kill extras: pkill -f 'hive daemon start --foreground'"),
        );
    } else if dup_count <= 1 {
        checks.push(Check::pass("No duplicate daemon processes"));
    }

    // Workspaces with daemon.toml
    let mut configured: Vec<String> = Vec::new();
    for entry in &registry {
        if entry.path.join(".hive/daemon.toml").exists() {
            configured.push(entry.name.clone());
        }
    }
    if configured.is_empty() {
        checks.push(Check::warn("No workspaces have daemon.toml"));
    } else {
        checks.push(Check::pass(format!(
            "{} workspace(s) with daemon.toml ({})",
            configured.len(),
            configured.join(", ")
        )));
    }

    // Deprecated [[workspaces]] in any daemon.toml
    for entry in &registry {
        if let Ok(config) = DaemonConfig::load(&entry.path)
            && !config.workspaces.is_empty()
        {
            checks.push(
                Check::warn(format!(
                    "Deprecated [[workspaces]] in {}/daemon.toml",
                    entry.path.join(".hive").display()
                ))
                .with_hint("Remove the [[workspaces]] section — the global daemon discovers workspaces from the registry"),
            );
        }
    }

    Category {
        name: "Daemon",
        checks,
    }
}

// ── Category 3: Telegram ─────────────────────────────────

fn check_telegram() -> Category {
    let mut checks = Vec::new();
    let registry = workspace::load_registry();

    let mut has_token = false;
    let mut configs: Vec<(String, DaemonConfig)> = Vec::new();

    for entry in &registry {
        if let Ok(config) = DaemonConfig::load(&entry.path) {
            if !config.telegram.bot_token.is_empty() {
                has_token = true;
            }
            configs.push((entry.name.clone(), config));
        }
    }

    if has_token {
        checks.push(Check::pass("Bot token configured"));
    } else if configs.is_empty() {
        checks.push(Check::warn(
            "No daemon.toml found — Telegram not configured",
        ));
    } else {
        checks.push(
            Check::fail("Bot token is empty in all daemon.toml files")
                .with_hint("Set telegram.bot_token in .hive/daemon.toml"),
        );
    }

    // alert_chat_id check
    let mut missing_chat_id = Vec::new();
    for (name, config) in &configs {
        if config.telegram.alert_chat_id == 0 {
            missing_chat_id.push(name.as_str());
        }
    }
    if !configs.is_empty() && missing_chat_id.is_empty() {
        let ids: Vec<String> = configs
            .iter()
            .map(|(_, c)| c.telegram.alert_chat_id.to_string())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        checks.push(Check::pass(format!(
            "alert_chat_id set ({})",
            ids.join(", ")
        )));
    } else if !missing_chat_id.is_empty() {
        checks.push(
            Check::fail(format!(
                "alert_chat_id is 0 in: {}",
                missing_chat_id.join(", ")
            ))
            .with_hint("Set telegram.alert_chat_id in .hive/daemon.toml"),
        );
    }

    // Topic collision: two workspaces sharing the same (bot_token, alert_chat_id, topic_id)
    let mut topic_map: HashMap<(String, i64, Option<i64>), Vec<String>> = HashMap::new();
    for (name, config) in &configs {
        let key = (
            config.telegram.bot_token.clone(),
            config.telegram.alert_chat_id,
            config.telegram.topic_id,
        );
        topic_map.entry(key).or_default().push(name.clone());
    }
    let collisions: Vec<_> = topic_map.values().filter(|names| names.len() > 1).collect();
    if collisions.is_empty() && configs.len() > 1 {
        checks.push(Check::pass("Workspace topics are distinct"));
    } else if !collisions.is_empty() {
        for names in &collisions {
            checks.push(
                Check::fail(format!(
                    "Topic collision: {} share the same bot + chat + topic",
                    names.join(", ")
                ))
                .with_hint("Set distinct telegram.topic_id for each workspace"),
            );
        }
    }

    Category {
        name: "Telegram",
        checks,
    }
}

// ── Category 4: Swarm ────────────────────────────────────

fn check_swarm() -> Category {
    let mut checks = Vec::new();

    // swarm binary
    match which("swarm") {
        Some(path) => {
            checks.push(Check::pass(format!(
                "swarm binary found ({})",
                tilde(&path)
            )));
        }
        None => {
            checks.push(
                Check::fail("swarm binary not found")
                    .with_hint("Install: cargo install --path swarm"),
            );
        }
    }

    // Codesign checks (macOS only)
    #[cfg(target_os = "macos")]
    {
        let binaries = ["swarm", "hive"];
        for name in &binaries {
            if let Some(path) = which(name) {
                if check_codesign(&path) {
                    checks.push(Check::pass(format!("{name} binary codesigned")));
                } else {
                    checks.push(
                        Check::fail(format!("{name} binary not codesigned"))
                            .with_hint(format!("Fix: codesign -f -s - \"{}\"", path.display())),
                    );
                }
            }
        }
    }

    Category {
        name: "Swarm",
        checks,
    }
}

// ── Category 5: Buzz ─────────────────────────────────────

fn check_buzz() -> Category {
    let mut checks = Vec::new();
    let registry = workspace::load_registry();

    for entry in &registry {
        // Check if buzz signals parent directory exists
        let buzz_dir = entry.path.join(".buzz");
        if buzz_dir.exists() {
            checks.push(Check::pass(format!(
                "Buzz directory exists ({})",
                entry.name
            )));
        }

        // Check sentry token if sentry is configured
        if let Ok(ws) = load_workspace(&entry.path)
            && let Some(ref buzz) = ws.buzz
            && let Some(ref sentry) = buzz.sentry
        {
            if sentry.token.is_empty() {
                checks.push(
                    Check::fail(format!(
                        "Sentry token empty in {} workspace.yaml",
                        entry.name
                    ))
                    .with_hint("Set buzz.sentry.token in workspace.yaml"),
                );
            } else {
                checks.push(Check::pass(format!(
                    "Sentry token configured ({})",
                    entry.name
                )));
            }
        }
    }

    if checks.is_empty() {
        checks.push(Check::pass("No buzz configuration to check"));
    }

    Category {
        name: "Buzz",
        checks,
    }
}

// ── Report renderer ──────────────────────────────────────

fn print_report(categories: &[Category]) {
    let mut total_pass = 0u32;
    let mut total_warn = 0u32;
    let mut total_fail = 0u32;

    for (i, cat) in categories.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}", cat.name);
        for check in &cat.checks {
            let icon = match check.status {
                Status::Pass => "\x1b[32m\u{2713}\x1b[0m", // green checkmark
                Status::Warn => "\x1b[33m!\x1b[0m",        // yellow !
                Status::Fail => "\x1b[31m\u{2717}\x1b[0m", // red X
            };
            println!("  {icon} {}", check.message);
            if let Some(ref hint) = check.hint {
                println!("    {hint}");
            }

            match check.status {
                Status::Pass => total_pass += 1,
                Status::Warn => total_warn += 1,
                Status::Fail => total_fail += 1,
            }
        }
    }

    println!();
    let mut parts = Vec::new();
    if total_pass > 0 {
        parts.push(format!("{total_pass} passed"));
    }
    if total_warn > 0 {
        parts.push(format!("{total_warn} warning(s)"));
    }
    if total_fail > 0 {
        parts.push(format!("{total_fail} failure(s)"));
    }
    println!("{}", parts.join(", "));
}

// ── Fix logic ────────────────────────────────────────────

fn apply_fixes(categories: &[Category]) {
    let fixable: Vec<_> = categories
        .iter()
        .flat_map(|c| &c.checks)
        .filter(|c| c.fix.is_some() && c.status != Status::Pass)
        .collect();

    if fixable.is_empty() {
        return;
    }

    println!("Fixing {} issue(s)...", fixable.len());
    for check in fixable {
        match check.fix {
            Some(Fix::PruneRegistry) => match prune_registry() {
                Ok(n) => println!("  Pruned {n} stale registry entry(ies)"),
                Err(e) => eprintln!("  Failed to prune registry: {e}"),
            },
            Some(Fix::RemoveFile(ref path)) => match std::fs::remove_file(path) {
                Ok(()) => println!("  Removed {}", path.display()),
                Err(e) => eprintln!("  Failed to remove {}: {e}", path.display()),
            },
            None => {}
        }
    }
}

/// Rewrite the registry file keeping only entries whose paths exist.
fn prune_registry() -> Result<usize> {
    let reg_path = workspace::registry_path();
    let all = workspace::load_registry_unfiltered();
    let live: Vec<_> = all.iter().filter(|e| e.path.exists()).cloned().collect();
    let pruned = all.len() - live.len();

    if pruned > 0 {
        #[derive(serde::Serialize)]
        struct Registry {
            workspaces: Vec<workspace::RegistryEntry>,
        }
        let registry = Registry { workspaces: live };
        let toml_str = toml::to_string_pretty(&registry)?;
        std::fs::write(&reg_path, toml_str)?;
    }

    Ok(pruned)
}

// ── Helpers ──────────────────────────────────────────────

/// Replace $HOME prefix with ~ for display.
fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(suffix) = path.strip_prefix(&home)
    {
        return format!("~/{}", suffix.display());
    }
    path.display().to_string()
}

/// Compare two paths by their canonical form.
fn canonical_eq(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

/// Find a binary on $PATH.
fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(name);
            if full.is_file() { Some(full) } else { None }
        })
    })
}

/// Count running `hive daemon start --foreground` processes.
fn count_daemon_processes() -> usize {
    let output = std::process::Command::new("pgrep")
        .args(["-f", "hive daemon start --foreground"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        Err(_) => 0,
    }
}

/// Check if a binary passes `codesign -v`.
#[cfg(target_os = "macos")]
fn check_codesign(path: &Path) -> bool {
    std::process::Command::new("codesign")
        .args(["-v", &path.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_replaces_home() {
        if let Some(home) = dirs::home_dir() {
            let p = home.join("foo/bar");
            assert_eq!(tilde(&p), "~/foo/bar");
        }
    }

    #[test]
    fn tilde_leaves_non_home_alone() {
        let p = std::path::Path::new("/tmp/something");
        assert_eq!(tilde(p), "/tmp/something");
    }

    #[test]
    fn canonical_eq_same_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(canonical_eq(tmp.path(), tmp.path()));
    }

    #[test]
    fn check_pass_has_correct_status() {
        let c = Check::pass("ok");
        assert_eq!(c.status, Status::Pass);
        assert!(c.hint.is_none());
    }

    #[test]
    fn check_with_hint() {
        let c = Check::fail("bad").with_hint("fix it");
        assert_eq!(c.status, Status::Fail);
        assert_eq!(c.hint.as_deref(), Some("fix it"));
    }

    #[test]
    fn which_finds_sh() {
        // /bin/sh should exist on all Unix-like systems
        assert!(which("sh").is_some());
    }

    #[test]
    fn which_returns_none_for_nonexistent() {
        assert!(which("__definitely_not_a_real_binary__").is_none());
    }
}

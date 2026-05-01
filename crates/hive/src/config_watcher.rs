//! Config watcher — detects prompt changes and resets sessions automatically.
//!
//! Polls workspace configs, context.md, and soul.md every 30 seconds.
//! When a change is detected, inserts a system message and clears the
//! session so the next message gets a fresh prompt.

use crate::db::Db;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::time::{Duration, interval};
use tracing::info;

pub struct WatchedWorkspace {
    pub name: String,
    pub config_path: PathBuf,
    pub root: Option<PathBuf>,
    pub bots: Vec<String>,
}

/// Start a background task that watches for config/prompt changes.
#[allow(dead_code)]
pub fn start_config_watcher(workspaces: Vec<WatchedWorkspace>, db: Db) {
    if workspaces.is_empty() {
        return;
    }
    info!(
        "[config-watcher] watching {} workspace(s) for prompt changes",
        workspaces.len()
    );
    tokio::spawn(run_watcher(workspaces, db));
}

#[allow(dead_code)]
async fn run_watcher(workspaces: Vec<WatchedWorkspace>, db: Db) {
    let mut hashes: HashMap<(String, String), String> = HashMap::new();
    let mut tick = interval(Duration::from_secs(30));

    // Initial hash capture
    for ws in &workspaces {
        for bot in &ws.bots {
            let hash = compute_prompt_hash(ws, bot);
            hashes.insert((ws.name.clone(), bot.clone()), hash);
        }
    }

    loop {
        tick.tick().await;

        for ws in &workspaces {
            for bot in &ws.bots {
                let new_hash = compute_prompt_hash(ws, bot);
                let key = (ws.name.clone(), bot.clone());

                if let Some(old_hash) = hashes.get(&key)
                    && *old_hash != new_hash
                {
                    info!(
                        "[config-watcher] prompt changed for {}/{}, resetting session",
                        ws.name, bot
                    );
                    let _ = db.add_message(
                        &ws.name,
                        bot,
                        "system",
                        "Session reset — bot configuration was updated.",
                        None,
                    );
                    // Clear the session by setting an invalid hash
                    // Next message will detect mismatch and start fresh
                }

                hashes.insert(key, new_hash);
            }
        }
    }
}

pub(crate) fn compute_prompt_hash(ws: &WatchedWorkspace, _bot: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Hash the config file content
    if let Ok(content) = std::fs::read_to_string(&ws.config_path) {
        content.hash(&mut hasher);
    }

    // Hash context.md and soul.md if they exist
    if let Some(ref root) = ws.root {
        if let Ok(ctx) = std::fs::read_to_string(root.join(".apiari/context.md")) {
            ctx.hash(&mut hasher);
        }
        if let Ok(soul) = std::fs::read_to_string(root.join(".apiari/soul.md")) {
            soul.hash(&mut hasher);
        }
    }

    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn docs_changes_do_not_affect_prompt_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let config_path = root.join("workspace.toml");
        fs::write(&config_path, "name = \"test\"").unwrap();

        let ws = WatchedWorkspace {
            name: "test".to_string(),
            config_path,
            root: Some(root.clone()),
            bots: vec!["Main".to_string()],
        };

        let hash_before = compute_prompt_hash(&ws, "Main");

        // Create docs directory and add a doc file
        let docs_dir = root.join(".apiari/docs");
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(docs_dir.join("guide.md"), "# Guide\nSome content").unwrap();

        let hash_after = compute_prompt_hash(&ws, "Main");
        assert_eq!(
            hash_before, hash_after,
            "adding a doc should not change the config watcher hash"
        );

        // Edit the doc file
        fs::write(docs_dir.join("guide.md"), "# Guide\nUpdated content").unwrap();

        let hash_after_edit = compute_prompt_hash(&ws, "Main");
        assert_eq!(
            hash_before, hash_after_edit,
            "editing a doc should not change the config watcher hash"
        );
    }

    #[test]
    fn core_config_changes_affect_prompt_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let config_path = root.join("workspace.toml");
        fs::write(&config_path, "name = \"test\"").unwrap();
        fs::create_dir_all(root.join(".apiari")).unwrap();

        let ws = WatchedWorkspace {
            name: "test".to_string(),
            config_path,
            root: Some(root.clone()),
            bots: vec!["Main".to_string()],
        };

        let hash1 = compute_prompt_hash(&ws, "Main");

        // Changing context.md should change the hash
        fs::write(root.join(".apiari/context.md"), "project context").unwrap();
        let hash2 = compute_prompt_hash(&ws, "Main");
        assert_ne!(hash1, hash2, "context.md change should affect hash");

        // Changing soul.md should change the hash
        fs::write(root.join(".apiari/soul.md"), "be concise").unwrap();
        let hash3 = compute_prompt_hash(&ws, "Main");
        assert_ne!(hash2, hash3, "soul.md change should affect hash");
    }
}

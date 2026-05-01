//! Integration tests for workspace init + registry round-trip.
//!
//! Tests that `init_workspace` creates the expected structure,
//! the global registry tracks workspaces correctly, and edge cases
//! (missing files, idempotent calls) are handled gracefully.

use hive::workspace::{
    RegistryEntry, init_workspace, load_registry_at, load_workspace, register_workspace_at,
};
use std::path::Path;
use tempfile::TempDir;

// ── init_workspace ──────────────────────────────────────────

#[test]
fn init_creates_workspace_yaml_and_quests_dir() {
    let dir = TempDir::new().unwrap();
    let config_path = init_workspace(dir.path()).unwrap();

    assert!(config_path.exists(), "workspace.yaml should be created");
    assert_eq!(config_path, dir.path().join(".hive/workspace.yaml"));
    assert!(
        dir.path().join(".hive/quests").exists(),
        "quests dir should be created"
    );
}

#[test]
fn init_workspace_config_is_valid_yaml() {
    let dir = TempDir::new().unwrap();
    init_workspace(dir.path()).unwrap();

    let ws = load_workspace(dir.path()).unwrap();
    // Name should be derived from the temp dir's leaf name.
    let expected_name = dir.path().file_name().unwrap().to_str().unwrap();
    assert_eq!(ws.name, expected_name);
    assert_eq!(ws.default_agent, "claude");
    assert!(ws.repos.is_empty());
}

#[test]
fn init_workspace_twice_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path1 = init_workspace(dir.path()).unwrap();
    let path2 = init_workspace(dir.path()).unwrap();

    assert_eq!(path1, path2);

    // Content should be unchanged (second call is a no-op).
    let content1 = std::fs::read_to_string(&path1).unwrap();
    let content2 = std::fs::read_to_string(&path2).unwrap();
    assert_eq!(content1, content2);
}

// ── Registry: load_registry_at / register_workspace_at ──────

#[test]
fn load_registry_at_missing_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("nonexistent.toml");

    let entries = load_registry_at(&reg_path);
    assert!(entries.is_empty(), "missing file should return empty vec");
}

#[test]
fn load_registry_at_empty_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("workspaces.toml");
    std::fs::write(&reg_path, "").unwrap();

    let entries = load_registry_at(&reg_path);
    assert!(entries.is_empty(), "empty file should return empty vec");
}

#[test]
fn register_and_load_roundtrip() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("workspaces.toml");

    // Register a workspace.
    let ws_dir = dir.path().join("my-project");
    std::fs::create_dir_all(&ws_dir).unwrap();

    register_workspace_at(&reg_path, &ws_dir, "my-project").unwrap();

    // Load it back.
    let entries = load_registry_at(&reg_path);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "my-project");
    // The path should be canonical.
    let canonical_ws = std::fs::canonicalize(&ws_dir).unwrap();
    assert_eq!(entries[0].path, canonical_ws);
}

#[test]
fn register_same_path_twice_no_duplicate() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("workspaces.toml");

    let ws_dir = dir.path().join("project-a");
    std::fs::create_dir_all(&ws_dir).unwrap();

    register_workspace_at(&reg_path, &ws_dir, "project-a").unwrap();
    register_workspace_at(&reg_path, &ws_dir, "project-a").unwrap();
    register_workspace_at(&reg_path, &ws_dir, "project-a-renamed").unwrap(); // same path, different name

    let entries = load_registry_at(&reg_path);
    assert_eq!(
        entries.len(),
        1,
        "duplicate paths should not create entries"
    );
}

#[test]
fn register_different_paths_creates_multiple_entries() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("workspaces.toml");

    let ws_a = dir.path().join("project-a");
    let ws_b = dir.path().join("project-b");
    std::fs::create_dir_all(&ws_a).unwrap();
    std::fs::create_dir_all(&ws_b).unwrap();

    register_workspace_at(&reg_path, &ws_a, "alpha").unwrap();
    register_workspace_at(&reg_path, &ws_b, "beta").unwrap();

    let entries = load_registry_at(&reg_path);
    assert_eq!(entries.len(), 2);

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
}

#[test]
fn register_creates_parent_directories() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("deep/nested/dir/workspaces.toml");

    let ws_dir = dir.path().join("my-ws");
    std::fs::create_dir_all(&ws_dir).unwrap();

    register_workspace_at(&reg_path, &ws_dir, "ws").unwrap();

    assert!(reg_path.exists(), "parent directories should be created");
    let entries = load_registry_at(&reg_path);
    assert_eq!(entries.len(), 1);
}

// ── load_workspace ──────────────────────────────────────────

#[test]
fn load_workspace_no_config_returns_default() {
    let dir = TempDir::new().unwrap();
    let ws = load_workspace(dir.path()).unwrap();
    assert_eq!(ws.name, "apiari");
    assert_eq!(ws.default_agent, "claude");
    assert_eq!(ws.root, dir.path());
}

#[test]
fn load_workspace_reads_custom_config() {
    let dir = TempDir::new().unwrap();
    let yaml = "name: test-project\nrepos:\n  - foo/bar\ndefault_agent: codex\n";
    std::fs::write(dir.path().join("workspace.yaml"), yaml).unwrap();

    let ws = load_workspace(dir.path()).unwrap();
    assert_eq!(ws.name, "test-project");
    assert_eq!(ws.repos, vec!["foo/bar"]);
    assert_eq!(ws.default_agent, "codex");
}

#[test]
fn load_workspace_reads_soul_from_hive_soul_md() {
    let dir = TempDir::new().unwrap();
    let hive_dir = dir.path().join(".hive");
    std::fs::create_dir_all(&hive_dir).unwrap();
    std::fs::write(hive_dir.join("workspace.yaml"), "name: soul-test\n").unwrap();
    std::fs::write(hive_dir.join("soul.md"), "You are a helpful bee.\n").unwrap();

    let ws = load_workspace(dir.path()).unwrap();
    assert_eq!(ws.name, "soul-test");
    assert_eq!(ws.soul.as_deref(), Some("You are a helpful bee.\n"));
}

// ── Registry TOML format ────────────────────────────────────

#[test]
fn registry_file_is_valid_toml() {
    let dir = TempDir::new().unwrap();
    let reg_path = dir.path().join("workspaces.toml");

    let ws_dir = dir.path().join("my-project");
    std::fs::create_dir_all(&ws_dir).unwrap();
    register_workspace_at(&reg_path, &ws_dir, "my-project").unwrap();

    // Read raw file and verify it's valid TOML.
    let contents = std::fs::read_to_string(&reg_path).unwrap();
    let parsed: toml::Value = toml::from_str(&contents).expect("registry should be valid TOML");

    // Should have a `workspaces` array.
    let workspaces = parsed
        .get("workspaces")
        .expect("should have workspaces key");
    assert!(workspaces.is_array());
    assert_eq!(workspaces.as_array().unwrap().len(), 1);
}

// ── RegistryEntry serialization ─────────────────────────────

#[test]
fn registry_entry_serde_roundtrip() {
    let entry = RegistryEntry {
        name: "test".to_owned(),
        path: std::path::PathBuf::from("/some/path"),
    };

    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: RegistryEntry = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.name, "test");
    assert_eq!(deserialized.path, Path::new("/some/path"));
}

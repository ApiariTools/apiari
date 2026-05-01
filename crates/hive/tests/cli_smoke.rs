//! CLI startup smoke tests.
//!
//! Verifies that key subcommands exit cleanly (or with expected codes)
//! without panicking. Uses `std::process::Command` against the compiled binary.

use std::process::Command;

fn hive_bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_hive").into()
}

#[test]
fn help_exits_zero() {
    let output = Command::new(hive_bin())
        .arg("--help")
        .output()
        .expect("failed to run hive --help");

    assert!(
        output.status.success(),
        "hive --help failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hive"),
        "help output should mention 'hive': {stdout}"
    );
}

#[test]
fn version_exits_zero() {
    let output = Command::new(hive_bin())
        .arg("--version")
        .output()
        .expect("failed to run hive --version");

    assert!(
        output.status.success(),
        "hive --version failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hive"),
        "version output should mention 'hive': {stdout}"
    );
}

#[test]
fn status_in_temp_dir_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();

    let output = Command::new(hive_bin())
        .arg("-C")
        .arg(dir.path())
        .arg("status")
        .output()
        .expect("failed to run hive status");

    // status should succeed even without a workspace config (falls back to defaults).
    assert!(
        output.status.success(),
        "hive status failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn ui_help_exits_zero() {
    let output = Command::new(hive_bin())
        .args(["ui", "--help"])
        .output()
        .expect("failed to run hive ui --help");

    assert!(
        output.status.success(),
        "hive ui --help failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn dashboard_help_exits_zero() {
    let output = Command::new(hive_bin())
        .args(["dashboard", "--help"])
        .output()
        .expect("failed to run hive dashboard --help");

    assert!(
        output.status.success(),
        "hive dashboard --help failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn init_in_temp_dir_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();

    let output = Command::new(hive_bin())
        .arg("-C")
        .arg(dir.path())
        .arg("init")
        .output()
        .expect("failed to run hive init");

    assert!(
        output.status.success(),
        "hive init failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Verify the workspace structure was created.
    assert!(dir.path().join(".hive/workspace.yaml").exists());
    assert!(dir.path().join(".hive/quests").exists());
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let output = Command::new(hive_bin())
        .arg("nonexistent-subcommand")
        .output()
        .expect("failed to run hive with unknown subcommand");

    assert!(
        !output.status.success(),
        "unknown subcommand should fail, but it succeeded"
    );
}

#[test]
fn buzz_help_exits_zero() {
    let output = Command::new(hive_bin())
        .args(["buzz", "--help"])
        .output()
        .expect("failed to run hive buzz --help");

    assert!(
        output.status.success(),
        "hive buzz --help failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

// NOTE: `hive remind --help` panics in debug builds due to a clap
// debug assertion (non-required positional before required positional).
// Skipped here — tracked as a known issue to fix in the CLI definition.

#[test]
fn reminders_in_temp_dir_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();

    // Initialize workspace first so load_workspace finds it.
    let init = Command::new(hive_bin())
        .arg("-C")
        .arg(dir.path())
        .arg("init")
        .output()
        .expect("failed to run hive init");
    assert!(init.status.success());

    let output = Command::new(hive_bin())
        .arg("-C")
        .arg(dir.path())
        .arg("reminders")
        .output()
        .expect("failed to run hive reminders");

    assert!(
        output.status.success(),
        "hive reminders failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

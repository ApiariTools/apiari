use std::path::PathBuf;
use tokio::process::Child;
use tracing::{info, warn};

/// Resolve the TTS directory: try CWD first, then fall back to the executable's directory.
pub(crate) fn find_tts_dir() -> Option<PathBuf> {
    // Try CWD first (normal dev usage)
    let cwd_tts = PathBuf::from("tts");
    if cwd_tts.join("server.py").exists() {
        return Some(cwd_tts);
    }

    // Fall back to exe directory (systemd, desktop launcher, etc.)
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let exe_tts = parent.join("tts");
        if exe_tts.join("server.py").exists() {
            return Some(exe_tts);
        }
    }

    None
}

/// Spawn the TTS Python server if the venv and script exist.
/// Returns the child handle (with `kill_on_drop`) so the caller can keep it alive.
pub async fn start_tts_server() -> Option<Child> {
    let tts_dir = match find_tts_dir() {
        Some(dir) => dir,
        None => {
            info!("TTS server not set up (run tts/setup.sh to enable)");
            return None;
        }
    };

    // Canonicalize paths so they work regardless of CWD
    let tts_dir = match std::fs::canonicalize(&tts_dir) {
        Ok(d) => d,
        Err(_) => return None,
    };
    let venv_python = tts_dir.join(".venv/bin/python");
    if !venv_python.exists() {
        info!("TTS server not set up (run tts/setup.sh to enable)");
        return None;
    }

    let server_py = tts_dir.join("server.py");

    let mut child = match tokio::process::Command::new(&venv_python)
        .arg(&server_py)
        .current_dir(&tts_dir)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            warn!("Failed to start TTS server: {e}");
            return None;
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(250))
        .build()
        .unwrap_or_default();

    // Wait up to 5 seconds for the server to become ready
    for _ in 0..10 {
        // Check if the child has already exited (e.g. crash on startup)
        if let Ok(Some(status)) = child.try_wait() {
            warn!("TTS server exited during startup with {status}");
            return None;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if let Ok(resp) = client.get("http://127.0.0.1:4201/health").send().await
            && resp.status().is_success()
        {
            info!("TTS server started on :4201");
            return Some(child);
        }
    }

    warn!("TTS server spawned but health check never passed");
    // Child is still running but not healthy — clean up
    let _ = child.kill().await;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_tts_dir_returns_none_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = find_tts_dir();
        std::env::set_current_dir(orig).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_find_tts_dir_returns_none_when_missing() {
        // In the test environment, CWD is target/debug or similar — no tts/ dir
        // This just verifies the function doesn't panic
        let _ = find_tts_dir();
    }
}

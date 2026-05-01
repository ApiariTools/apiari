use tokio::process::Child;
use tracing::{info, warn};

/// Spawn the whisper-server STT process if whisper-server is installed.
/// Returns the child handle (with `kill_on_drop`) so the caller can keep it alive.
pub async fn start_stt_server() -> Option<Child> {
    let home = std::env::var("HOME").unwrap_or_default();
    let model_path = format!("{home}/.local/share/whisper/ggml-base.en.bin");

    if !std::path::Path::new(&model_path).exists() {
        info!("STT server not set up (whisper model not found at {model_path})");
        return None;
    }

    let mut child = match tokio::process::Command::new("whisper-server")
        .args([
            "--model",
            &model_path,
            "--port",
            "4202",
            "--host",
            "127.0.0.1",
            "--convert",
        ])
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("STT server not available (install with: brew install whisper-cpp)");
            return None;
        }
        Err(e) => {
            warn!("Failed to start STT server: {e}");
            return None;
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap_or_default();

    // Wait up to 10 seconds for whisper-server to load the model and become ready
    for _ in 0..20 {
        if let Ok(Some(status)) = child.try_wait() {
            warn!("STT server exited during startup with {status}");
            return None;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // whisper-server serves a web UI on GET /
        if let Ok(resp) = client.get("http://127.0.0.1:4202/").send().await
            && resp.status().is_success()
        {
            info!("STT server started on :4202");
            return Some(child);
        }
    }

    warn!("STT server spawned but health check never passed");
    let _ = child.kill().await;
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_stt_module_compiles() {
        // Just verifies the module is valid
    }
}

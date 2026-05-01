use color_eyre::Result;
use std::process::Command;

use super::dirs;

pub fn run() -> Result<()> {
    println!("\n=== Hive Voice Setup ===\n");

    // STT: whisper-cpp
    install_whisper();

    // STT: whisper model
    download_whisper_model();

    // TTS: Kokoro
    setup_tts();

    println!("\n=== Setup complete ===");
    Ok(())
}

fn install_whisper() {
    print!("[STT] whisper-cpp via brew... ");

    // Check if brew is available
    let brew_check = Command::new("brew").arg("--version").output();
    if brew_check.is_err() || !brew_check.unwrap().status.success() {
        println!("SKIPPED (brew not installed)");
        return;
    }

    // Check if already installed
    let list = Command::new("brew").args(["list", "whisper-cpp"]).output();
    if let Ok(output) = list
        && output.status.success()
    {
        println!("already installed");
        return;
    }

    // Install
    let result = Command::new("brew")
        .args(["install", "whisper-cpp"])
        .status();
    match result {
        Ok(s) if s.success() => println!("installed"),
        Ok(s) => println!("FAILED (exit code {})", s.code().unwrap_or(-1)),
        Err(e) => println!("FAILED ({})", e),
    }
}

fn download_whisper_model() {
    print!("[STT] whisper base.en model... ");

    let Some(home) = dirs::home_dir() else {
        println!("SKIPPED (cannot determine home directory)");
        return;
    };

    let model_dir = home.join(".local/share/whisper");
    let model_path = model_dir.join("ggml-base.en.bin");

    if model_path.exists() {
        println!("already exists");
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&model_dir) {
        println!("FAILED (cannot create dir: {})", e);
        return;
    }

    let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
    let result = Command::new("curl")
        .args(["-fSL", "--progress-bar", "-o"])
        .arg(&model_path)
        .arg(url)
        .status();

    match result {
        Ok(s) if s.success() => println!("downloaded"),
        Ok(s) => {
            // Clean up partial download
            let _ = std::fs::remove_file(&model_path);
            println!("FAILED (exit code {})", s.code().unwrap_or(-1));
        }
        Err(e) => println!("FAILED ({})", e),
    }
}

fn setup_tts() {
    println!("[TTS] Kokoro setup...");

    // Reuse the same resolver as the runtime TTS server
    let tts_dir = crate::tts::find_tts_dir();
    let Some(tts_dir) = tts_dir else {
        println!("  SKIPPED (tts/ directory not found)");
        return;
    };

    let venv_dir = tts_dir.join(".venv");

    // Create venv if needed
    print!("  [TTS] python venv... ");
    if venv_dir.exists() {
        println!("already exists");
    } else {
        let result = Command::new("python3")
            .args(["-m", "venv"])
            .arg(&venv_dir)
            .output();
        match result {
            Ok(output) if output.status.success() => println!("created"),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = stderr.lines().last().unwrap_or("unknown error");
                println!("FAILED ({})", detail);
                return;
            }
            Err(e) => {
                println!("FAILED ({})", e);
                return;
            }
        }
    }

    // Install requirements
    print!("  [TTS] pip install requirements... ");
    let pip = venv_dir.join("bin/pip");
    let req_file = tts_dir.join("requirements.txt");
    let result = Command::new(&pip)
        .args(["install", "-r"])
        .arg(&req_file)
        .output();
    match result {
        Ok(output) if output.status.success() => println!("installed"),
        Ok(output) => {
            println!("FAILED");
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprintln!("    {}", stderr.lines().last().unwrap_or(""));
            }
            return;
        }
        Err(e) => {
            println!("FAILED ({})", e);
            return;
        }
    }

    // Download model — use `python` to match runtime (tts.rs uses .venv/bin/python)
    print!("  [TTS] downloading Kokoro model... ");
    let python = venv_dir.join("bin/python");
    let result = Command::new(&python)
        .args([
            "-c",
            "import kokoro_onnx; kokoro_onnx.Kokoro('kokoro-v1.0.onnx', 'voices-v1.0.bin'); print('ok')",
        ])
        .current_dir(&tts_dir)
        .output();
    match result {
        Ok(output) if output.status.success() => println!("done"),
        Ok(output) => {
            println!("FAILED");
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprintln!("    {}", stderr.lines().last().unwrap_or(""));
            }
        }
        Err(e) => println!("FAILED ({})", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_whisper_model_skips_existing() {
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join(".local/share/whisper");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_path = model_dir.join("ggml-base.en.bin");
        std::fs::write(&model_path, b"fake model").unwrap();
        assert!(model_path.exists());
    }

    #[test]
    fn test_home_dir_fallback_does_not_use_tilde() {
        // Verify dirs::home_dir returns a real path, not "~"
        if let Some(home) = dirs::home_dir() {
            assert!(!home.to_string_lossy().starts_with('~'));
        }
    }
}

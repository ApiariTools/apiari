//! Integration tests for the Gemini SDK.
//! These tests require the gemini CLI to be installed and available on PATH.

use apiari_gemini_sdk::{GeminiClient, GeminiOptions};
use std::{fs, path::Path};

fn write_fake_gemini(path: &Path, stdout_lines: &[&str]) {
    let body = format!(
        "#!/bin/sh\n{}\n",
        stdout_lines
            .iter()
            .map(|line| format!("printf '%s\\n' '{}'", line.replace('\'', "'\"'\"'")))
            .collect::<Vec<_>>()
            .join("\n")
    );
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}

#[tokio::test]
#[ignore]
async fn test_gemini_exec() {
    let client = GeminiClient::new();
    let opts = GeminiOptions {
        model: Some("gemini-2.0".into()),
        ..Default::default()
    };
    let mut execution = match client.exec("Hello", opts).await {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut found_event = false;
    while let Ok(Some(event)) = execution.next_event().await {
        if event.is_item_completed() {
            found_event = true;
            break;
        }
    }
    assert!(found_event, "no events received");
}

#[test]
fn test_client_creation() {
    let client = GeminiClient::new();
    assert_eq!(client.cli_path, "gemini");
}

#[test]
fn test_client_with_path() {
    let client = GeminiClient::with_cli_path("/usr/bin/gemini");
    assert_eq!(client.cli_path, "/usr/bin/gemini");
}

#[tokio::test]
async fn test_exec_handles_current_stream_json_contract_without_dropping_output() {
    let temp = tempfile::tempdir().unwrap();
    let fake_gemini = temp.path().join("gemini");
    write_fake_gemini(
        &fake_gemini,
        &[
            r#"{"type":"init","session_id":"sess-current"}"#,
            r#"{"type":"message","role":"assistant","content":"Hello","delta":true}"#,
            r#"{"type":"message","role":"assistant","content":" world","delta":true}"#,
            r#"{"type":"result","status":"success","stats":{"input_tokens":4,"output_tokens":2}}"#,
        ],
    );

    let client = GeminiClient::with_cli_path(fake_gemini.display().to_string());
    let mut execution = client
        .exec("Hello", GeminiOptions::default())
        .await
        .unwrap();

    let mut texts = Vec::new();
    while let Some(event) = execution.next_event().await.unwrap() {
        if let Some(text) = event.text() {
            texts.push(text);
        }
    }

    assert_eq!(execution.thread_id(), Some("sess-current"));
    assert_eq!(texts, vec!["Hello".to_string(), " world".to_string()]);
    assert!(execution.is_finished());
}

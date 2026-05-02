//! Integration tests for the Gemini SDK.
//! These tests require the gemini CLI to be installed and available on PATH.

use apiari_gemini_sdk::{GeminiClient, GeminiOptions};

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

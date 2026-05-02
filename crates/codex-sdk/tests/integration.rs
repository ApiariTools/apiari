//! Integration tests requiring a live `codex` CLI.
//!
//! These tests are `#[ignore]`d by default and only run with:
//! ```bash
//! cargo test -p apiari-codex-sdk -- --ignored
//! ```

use apiari_codex_sdk::{CodexClient, Event, ExecOptions, Item};

#[tokio::test]
#[ignore = "requires codex CLI installed"]
async fn exec_simple_prompt() {
    let client = CodexClient::new();
    let mut execution = client
        .exec(
            "What is 2 + 2? Reply with just the number.",
            ExecOptions {
                model: Some("o4-mini".into()),
                full_auto: true,
                ..Default::default()
            },
        )
        .await
        .expect("failed to spawn codex");

    let mut saw_thread_started = false;
    let mut saw_agent_message = false;

    while let Some(event) = execution.next_event().await.expect("event read failed") {
        match &event {
            Event::ThreadStarted { .. } => saw_thread_started = true,
            Event::ItemCompleted {
                item:
                    Item::AgentMessage {
                        text: Some(text), ..
                    },
            } => {
                println!("Agent: {text}");
                saw_agent_message = true;
            }
            _ => {}
        }
    }

    assert!(saw_thread_started, "should have seen thread.started");
    assert!(saw_agent_message, "should have seen an agent message");
}

#[tokio::test]
#[ignore = "requires codex CLI installed"]
async fn exec_thread_id_tracked() {
    let client = CodexClient::new();
    let mut execution = client
        .exec(
            "Say hello",
            ExecOptions {
                model: Some("o4-mini".into()),
                full_auto: true,
                ..Default::default()
            },
        )
        .await
        .expect("failed to spawn codex");

    // Before any events, thread_id is None.
    assert!(execution.thread_id().is_none());

    // Read at least the first event (should be thread.started).
    if let Some(Event::ThreadStarted { .. }) = execution.next_event().await.unwrap() {
        assert!(execution.thread_id().is_some());
    }

    // Drain remaining events.
    while execution.next_event().await.unwrap().is_some() {}

    assert!(execution.is_finished());
}

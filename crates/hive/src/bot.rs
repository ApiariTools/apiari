//! Bot runner trait — protocol for streaming bot responses.
//!
//! Each provider (Claude, Codex, Gemini) implements this trait.
//! A MockBotRunner is provided for testing the full pipeline.

use std::path::PathBuf;

/// Events emitted during a bot run.
#[derive(Debug, Clone)]
pub enum BotEvent {
    /// A chunk of text output.
    TextDelta(String),
    /// Bot is using a tool.
    ToolUse(String),
    /// Run completed. Contains session ID if available.
    Done { session_id: Option<String> },
    /// Run failed.
    Error(String),
}

/// Options for running a bot.
#[derive(Debug, Clone, Default)]
pub struct BotRunOptions {
    pub message: String,
    pub system_prompt: Option<String>,
    pub working_dir: Option<PathBuf>,
    pub resume_id: Option<String>,
    pub images: Vec<(String, String)>, // (mime_type, base64_data)
}

/// Trait for bot runners. Each provider implements this.
#[async_trait::async_trait]
pub trait BotRunner: Send + Sync {
    /// Run a message through the bot, calling `on_event` for each event.
    async fn run(
        &self,
        opts: BotRunOptions,
        on_event: Box<dyn FnMut(BotEvent) + Send>,
    ) -> Result<(), String>;
}

/// Execute a bot run and write results to the DB.
/// This is the shared pipeline that all providers use.
pub async fn run_bot_pipeline(
    runner: &dyn BotRunner,
    opts: BotRunOptions,
    db: &crate::db::Db,
    workspace: &str,
    bot: &str,
    prompt_hash: &str,
) {
    let _ = db.set_bot_status(workspace, bot, "thinking", "", None);

    let full_text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let ft = full_text.clone();
    let db_s = db.clone();
    let ws_s = workspace.to_string();
    let b_s = bot.to_string();
    let hash = prompt_hash.to_string();

    let result = runner
        .run(
            opts,
            Box::new(move |event| match &event {
                BotEvent::TextDelta(text) => {
                    let mut ft = ft.lock().unwrap();
                    ft.push_str(text);
                    let _ = db_s.set_bot_status(&ws_s, &b_s, "streaming", &ft, None);
                }
                BotEvent::ToolUse(name) => {
                    let ft = ft.lock().unwrap();
                    let _ = db_s.set_bot_status(&ws_s, &b_s, "streaming", &ft, Some(name));
                }
                BotEvent::Done { session_id } => {
                    if let Some(sid) = session_id {
                        let _ = db_s.set_session(&ws_s, &b_s, sid, &hash);
                    }
                }
                BotEvent::Error(_) => {}
            }),
        )
        .await;

    let final_text = full_text.lock().unwrap().clone();

    match result {
        Ok(()) => {
            if !final_text.is_empty() {
                let _ = db.add_message(workspace, bot, "assistant", final_text.trim(), None);
            }
        }
        Err(e) => {
            let _ = db.add_message(workspace, bot, "assistant", &format!("Error: {e}"), None);
        }
    }

    let _ = db.set_bot_status(workspace, bot, "idle", "", None);
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::time::Duration;

    /// A mock bot runner for testing.
    pub struct MockBotRunner {
        pub events: Vec<(BotEvent, Duration)>,
    }

    impl MockBotRunner {
        /// Create a mock that returns a simple text response.
        pub fn simple(text: &str) -> Self {
            Self {
                events: vec![
                    (
                        BotEvent::TextDelta(text.to_string()),
                        Duration::from_millis(10),
                    ),
                    (
                        BotEvent::Done {
                            session_id: Some("mock_session_123".to_string()),
                        },
                        Duration::from_millis(10),
                    ),
                ],
            }
        }

        /// Create a mock that uses a tool then responds.
        pub fn with_tool(tool: &str, response: &str) -> Self {
            Self {
                events: vec![
                    (
                        BotEvent::ToolUse(tool.to_string()),
                        Duration::from_millis(10),
                    ),
                    (
                        BotEvent::TextDelta(response.to_string()),
                        Duration::from_millis(10),
                    ),
                    (
                        BotEvent::Done {
                            session_id: Some("mock_session_456".to_string()),
                        },
                        Duration::from_millis(10),
                    ),
                ],
            }
        }

        /// Create a mock that streams multiple chunks.
        pub fn streaming(chunks: &[&str]) -> Self {
            let mut events: Vec<(BotEvent, Duration)> = chunks
                .iter()
                .map(|c| (BotEvent::TextDelta(c.to_string()), Duration::from_millis(5)))
                .collect();
            events.push((
                BotEvent::Done {
                    session_id: Some("mock_stream_session".to_string()),
                },
                Duration::from_millis(5),
            ));
            Self { events }
        }

        /// Create a mock that errors.
        pub fn error(msg: &str) -> Self {
            Self {
                events: vec![(BotEvent::Error(msg.to_string()), Duration::from_millis(10))],
            }
        }
    }

    #[async_trait::async_trait]
    impl BotRunner for MockBotRunner {
        async fn run(
            &self,
            _opts: BotRunOptions,
            mut on_event: Box<dyn FnMut(BotEvent) + Send>,
        ) -> Result<(), String> {
            for (event, delay) in &self.events {
                tokio::time::sleep(*delay).await;
                if let BotEvent::Error(msg) = event {
                    return Err(msg.clone());
                }
                on_event(event.clone());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use mock::MockBotRunner;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_pipeline_simple_response() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner = MockBotRunner::simple("Hello world!");
        run_bot_pipeline(
            &runner,
            BotRunOptions::default(),
            &db,
            "ws",
            "Main",
            "hash1",
        )
        .await;

        // Check message stored
        let msgs = db.get_conversations("ws", "Main", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].content, "Hello world!");

        // Check status is idle
        let status = db.get_bot_status("ws", "Main").unwrap().unwrap();
        assert_eq!(status.status, "idle");

        // Check session stored
        let session = db.get_session_id("ws", "Main", "hash1").unwrap();
        assert_eq!(session, Some("mock_session_123".to_string()));
    }

    #[tokio::test]
    async fn test_pipeline_tool_use() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner = MockBotRunner::with_tool("Read", "File contents here");
        run_bot_pipeline(&runner, BotRunOptions::default(), &db, "ws", "Main", "h").await;

        let msgs = db.get_conversations("ws", "Main", 10).unwrap();
        assert_eq!(msgs[0].content, "File contents here");
    }

    #[tokio::test]
    async fn test_pipeline_streaming_chunks() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner = MockBotRunner::streaming(&["Hello ", "world", "!"]);
        run_bot_pipeline(&runner, BotRunOptions::default(), &db, "ws", "Main", "h").await;

        let msgs = db.get_conversations("ws", "Main", 10).unwrap();
        assert_eq!(msgs[0].content, "Hello world!");
    }

    #[tokio::test]
    async fn test_pipeline_error() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner = MockBotRunner::error("Claude crashed");
        run_bot_pipeline(&runner, BotRunOptions::default(), &db, "ws", "Main", "h").await;

        let msgs = db.get_conversations("ws", "Main", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("Error: Claude crashed"));

        // Status should still be idle after error
        let status = db.get_bot_status("ws", "Main").unwrap().unwrap();
        assert_eq!(status.status, "idle");
    }

    #[tokio::test]
    async fn test_pipeline_status_lifecycle() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        // Before run: no status
        assert!(db.get_bot_status("ws", "Main").unwrap().is_none());

        let runner = MockBotRunner::with_tool("Bash", "done");
        run_bot_pipeline(&runner, BotRunOptions::default(), &db, "ws", "Main", "h").await;

        // After run: idle
        let status = db.get_bot_status("ws", "Main").unwrap().unwrap();
        assert_eq!(status.status, "idle");
        assert_eq!(status.streaming_content, "");
        assert!(status.tool_name.is_none());
    }

    #[tokio::test]
    async fn test_pipeline_trims_whitespace() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner = MockBotRunner::simple("\n\nHello\n");
        run_bot_pipeline(&runner, BotRunOptions::default(), &db, "ws", "Main", "h").await;

        let msgs = db.get_conversations("ws", "Main", 10).unwrap();
        assert_eq!(msgs[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_pipeline_empty_response_not_stored() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner = MockBotRunner {
            events: vec![(
                BotEvent::Done { session_id: None },
                std::time::Duration::from_millis(5),
            )],
        };
        run_bot_pipeline(&runner, BotRunOptions::default(), &db, "ws", "Main", "h").await;

        let msgs = db.get_conversations("ws", "Main", 10).unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn test_pipeline_multiple_bots_independent() {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();

        let runner1 = MockBotRunner::simple("from main");
        let runner2 = MockBotRunner::simple("from customer");

        run_bot_pipeline(&runner1, BotRunOptions::default(), &db, "ws", "Main", "h1").await;
        run_bot_pipeline(
            &runner2,
            BotRunOptions::default(),
            &db,
            "ws",
            "Customer",
            "h2",
        )
        .await;

        let main = db.get_conversations("ws", "Main", 10).unwrap();
        let cust = db.get_conversations("ws", "Customer", 10).unwrap();
        assert_eq!(main[0].content, "from main");
        assert_eq!(cust[0].content, "from customer");
    }
}

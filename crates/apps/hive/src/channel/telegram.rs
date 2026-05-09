//! Telegram Bot API client using raw reqwest (no framework).
//!
//! Uses long-polling via `getUpdates` and sends responses via `sendMessage`.
//! Keeps dependencies minimal — same pattern as buzz's Sentry watcher.

use super::{Channel, ChannelEvent, OutboundMessage};
use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::Deserialize;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// Maximum message length for Telegram (we chunk below this).
const MAX_MESSAGE_LEN: usize = 4000;

/// Telegram Bot API client.
pub struct TelegramChannel {
    bot_token: String,
    client: reqwest::Client,
}

// --- Telegram API response types ---

#[derive(Debug, Deserialize)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
    callback_query: Option<TgCallbackQuery>,
}

#[derive(Debug, Deserialize)]
struct TgCallbackQuery {
    id: String,
    from: TgUser,
    message: Option<TgMessage>,
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    chat: TgChat,
    from: Option<TgUser>,
    text: Option<String>,
    /// Forum topic thread ID (present when message is in a topic).
    #[serde(default)]
    message_thread_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    username: Option<String>,
}

impl TelegramChannel {
    pub fn new(bot_token: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");

        Self { bot_token, client }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.bot_token)
    }

    /// Parse a message into a ChannelEvent.
    fn parse_message(msg: &TgMessage) -> Option<ChannelEvent> {
        let text = msg.text.as_deref()?.trim();
        if text.is_empty() {
            return None;
        }

        let user = msg.from.as_ref()?;
        let user_name = user
            .username
            .clone()
            .unwrap_or_else(|| user.first_name.clone());

        let topic_id = msg.message_thread_id;

        if let Some(rest) = text.strip_prefix('/') {
            // Split command from args: "/reset abc" -> ("reset", "abc")
            let (command, args) = match rest.split_once(' ') {
                Some((cmd, args)) => (cmd, args),
                None => (rest, ""),
            };
            // Strip @botname suffix from commands like "/reset@mybot"
            let command = command.split('@').next().unwrap_or(command);
            Some(ChannelEvent::Command {
                chat_id: msg.chat.id,
                message_id: msg.message_id,
                user_id: user.id,
                user_name,
                command: command.to_owned(),
                args: args.trim().to_owned(),
                topic_id,
            })
        } else {
            Some(ChannelEvent::Message {
                chat_id: msg.chat.id,
                message_id: msg.message_id,
                user_id: user.id,
                user_name,
                text: text.to_owned(),
                topic_id,
            })
        }
    }

    /// Long-poll for updates from Telegram.
    async fn get_updates(&self, offset: i64) -> Result<Vec<TgUpdate>> {
        let resp = self
            .client
            .get(self.api_url("getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", "30".to_string()),
            ])
            .send()
            .await?;

        let body: TgResponse<Vec<TgUpdate>> = resp.json().await?;

        if !body.ok {
            let desc = body.description.unwrap_or_default();
            color_eyre::eyre::bail!("Telegram API error: {desc}");
        }

        Ok(body.result.unwrap_or_default())
    }

    /// React to a message with an emoji.
    pub async fn send_reaction(&self, chat_id: i64, message_id: i64, emoji: &str) {
        let resp = self
            .client
            .post(self.api_url("setMessageReaction"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "reaction": [{"type": "emoji", "emoji": emoji}],
            }))
            .send()
            .await;

        if let Err(e) = resp {
            tracing::warn!(error = %e, "setMessageReaction failed");
        }
    }

    /// Send a "typing" chat action indicator.
    pub async fn send_typing(&self, chat_id: i64, topic_id: Option<i64>) {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "action": "typing",
        });
        if let Some(tid) = topic_id {
            payload["message_thread_id"] = serde_json::json!(tid);
        }
        let resp = self
            .client
            .post(self.api_url("sendChatAction"))
            .json(&payload)
            .send()
            .await;

        if let Err(e) = resp {
            tracing::warn!(error = %e, "sendChatAction failed");
        }
    }

    /// Send a text message, chunking if necessary.
    /// If `buttons` is non-empty, attaches an inline keyboard to the *last* chunk only.
    async fn send_text(
        &self,
        chat_id: i64,
        text: &str,
        buttons: &[Vec<super::InlineButton>],
        topic_id: Option<i64>,
    ) -> Result<()> {
        let chunks = chunk_message(text);
        let last_idx = chunks.len().saturating_sub(1);
        for (i, chunk) in chunks.iter().enumerate() {
            let mut payload = serde_json::json!({
                "chat_id": chat_id,
                "text": chunk,
                "parse_mode": "Markdown",
            });
            if let Some(tid) = topic_id {
                payload["message_thread_id"] = serde_json::json!(tid);
            }

            // Attach inline keyboard to the last chunk only.
            if i == last_idx && !buttons.is_empty() {
                let keyboard: Vec<Vec<serde_json::Value>> = buttons
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|btn| {
                                serde_json::json!({
                                    "text": btn.text,
                                    "callback_data": btn.callback_data,
                                })
                            })
                            .collect()
                    })
                    .collect();
                payload["reply_markup"] = serde_json::json!({
                    "inline_keyboard": keyboard,
                });
            }

            let resp = self
                .client
                .post(self.api_url("sendMessage"))
                .json(&payload)
                .send()
                .await?;

            let body: TgResponse<serde_json::Value> = resp.json().await?;
            if !body.ok {
                // Retry without Markdown if parse_mode fails.
                let mut fallback = serde_json::json!({
                    "chat_id": chat_id,
                    "text": chunk,
                });
                if let Some(tid) = topic_id {
                    fallback["message_thread_id"] = serde_json::json!(tid);
                }
                if i == last_idx && !buttons.is_empty() {
                    fallback["reply_markup"] = payload["reply_markup"].clone();
                }

                let resp = self
                    .client
                    .post(self.api_url("sendMessage"))
                    .json(&fallback)
                    .send()
                    .await?;

                let body: TgResponse<serde_json::Value> = resp.json().await?;
                if !body.ok {
                    let desc = body.description.unwrap_or_default();
                    tracing::warn!(error = desc, "sendMessage failed");
                }
            }
        }
        Ok(())
    }

    /// Acknowledge a callback query to dismiss the Telegram loading spinner.
    pub async fn answer_callback(&self, callback_query_id: &str) {
        let resp = self
            .client
            .post(self.api_url("answerCallbackQuery"))
            .json(&serde_json::json!({
                "callback_query_id": callback_query_id,
            }))
            .send()
            .await;

        if let Err(e) = resp {
            tracing::warn!(error = %e, "answerCallbackQuery failed");
        }
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn run(&self, tx: Sender<ChannelEvent>, cancel: CancellationToken) {
        let mut offset: i64 = 0;

        loop {
            if cancel.is_cancelled() {
                break;
            }

            let updates = tokio::select! {
                _ = cancel.cancelled() => break,
                result = self.get_updates(offset) => {
                    match result {
                        Ok(updates) => updates,
                        Err(e) => {
                            tracing::error!(error = %e, "Telegram poll error");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }
                    }
                }
            };

            for update in updates {
                offset = update.update_id + 1;

                // Handle callback queries from inline keyboard buttons.
                if let Some(cq) = update.callback_query {
                    let chat_id = cq.message.as_ref().map(|m| m.chat.id).unwrap_or(0);
                    let topic_id = cq.message.as_ref().and_then(|m| m.message_thread_id);
                    let user_name = cq.from.username.unwrap_or(cq.from.first_name);

                    if let Some(data) = cq.data {
                        let event = ChannelEvent::CallbackQuery {
                            chat_id,
                            user_name,
                            data,
                            callback_query_id: cq.id,
                            topic_id,
                        };
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                    continue;
                }

                let Some(msg) = update.message else {
                    continue;
                };

                if let Some(event) = Self::parse_message(&msg)
                    && tx.send(event).await.is_err()
                {
                    // Receiver dropped — shut down.
                    return;
                }
            }
        }
    }

    async fn send_message(&self, msg: &OutboundMessage) -> Result<()> {
        self.send_text(msg.chat_id, &msg.text, &msg.buttons, msg.topic_id)
            .await
    }

    async fn answer_callback_query(&self, callback_query_id: &str) {
        self.answer_callback(callback_query_id).await;
    }
}

/// Escape special characters for Telegram Markdown.
///
/// Use this on dynamic content (signal titles, external text) to prevent
/// Telegram's Markdown parser from misinterpreting special characters.
/// Do NOT use this on structural Markdown that we control (e.g. `*bold*` formatting).
pub fn escape_markdown(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(
            ch,
            '_' | '*'
                | '`'
                | '['
                | ']'
                | '('
                | ')'
                | '~'
                | '>'
                | '#'
                | '+'
                | '-'
                | '='
                | '|'
                | '{'
                | '}'
                | '.'
                | '!'
        ) {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

/// Split a message into chunks that fit within Telegram's limit.
fn chunk_message(text: &str) -> Vec<&str> {
    if text.len() <= MAX_MESSAGE_LEN {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= MAX_MESSAGE_LEN {
            chunks.push(remaining);
            break;
        }

        // Try to split at a newline within the limit.
        let split_at = remaining[..MAX_MESSAGE_LEN]
            .rfind('\n')
            .unwrap_or(MAX_MESSAGE_LEN);

        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk);
        // Skip the newline we split on.
        remaining = rest.strip_prefix('\n').unwrap_or(rest);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_message_short() {
        let chunks = chunk_message("hello");
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_chunk_message_long() {
        let line = "x".repeat(100);
        // 50 lines of 100 chars = 5000 chars + newlines
        let text: String = (0..50)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_message(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= MAX_MESSAGE_LEN);
        }
    }

    #[test]
    fn test_parse_command() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: Some("josh".into()),
            }),
            text: Some("/reset now".into()),
            message_thread_id: None,
        };
        let event = TelegramChannel::parse_message(&msg).unwrap();
        match event {
            ChannelEvent::Command { command, args, .. } => {
                assert_eq!(command, "reset");
                assert_eq!(args, "now");
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_parse_command_with_bot_suffix() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: None,
            }),
            text: Some("/status@mybot".into()),
            message_thread_id: None,
        };
        let event = TelegramChannel::parse_message(&msg).unwrap();
        match event {
            ChannelEvent::Command { command, .. } => {
                assert_eq!(command, "status");
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_parse_regular_message() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: Some("josh".into()),
            }),
            text: Some("hello world".into()),
            message_thread_id: None,
        };
        let event = TelegramChannel::parse_message(&msg).unwrap();
        match event {
            ChannelEvent::Message { text, .. } => {
                assert_eq!(text, "hello world");
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn test_escape_markdown_special_chars() {
        assert_eq!(
            escape_markdown("_underscores_ and *stars*"),
            "\\_underscores\\_ and \\*stars\\*"
        );
    }

    #[test]
    fn test_escape_markdown_brackets() {
        assert_eq!(
            escape_markdown("[link](url) and `code`"),
            "\\[link\\]\\(url\\) and \\`code\\`"
        );
    }

    #[test]
    fn test_escape_markdown_plain_text() {
        assert_eq!(escape_markdown("hello world"), "hello world");
    }

    #[test]
    fn test_escape_markdown_all_special() {
        let input = "_*`[]()~>#+-=|{}.!";
        let escaped = escape_markdown(input);
        // Every character should be escaped.
        assert_eq!(escaped.len(), input.len() * 2);
        assert!(escaped.chars().filter(|&c| c == '\\').count() == input.len());
    }

    #[test]
    fn test_parse_empty_text() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: None,
            }),
            text: Some("  ".into()),
            message_thread_id: None,
        };
        assert!(TelegramChannel::parse_message(&msg).is_none());
    }

    #[test]
    fn test_parse_no_text() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: None,
            }),
            text: None,
            message_thread_id: None,
        };
        assert!(TelegramChannel::parse_message(&msg).is_none());
    }

    #[test]
    fn test_parse_message_in_topic() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: Some("josh".into()),
            }),
            text: Some("hello from topic".into()),
            message_thread_id: Some(42),
        };
        let event = TelegramChannel::parse_message(&msg).unwrap();
        match event {
            ChannelEvent::Message { topic_id, text, .. } => {
                assert_eq!(topic_id, Some(42));
                assert_eq!(text, "hello from topic");
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn test_parse_command_in_topic() {
        let msg = TgMessage {
            message_id: 1,
            chat: TgChat { id: 100 },
            from: Some(TgUser {
                id: 1,
                first_name: "Josh".into(),
                username: Some("josh".into()),
            }),
            text: Some("/status".into()),
            message_thread_id: Some(99),
        };
        let event = TelegramChannel::parse_message(&msg).unwrap();
        match event {
            ChannelEvent::Command {
                topic_id, command, ..
            } => {
                assert_eq!(topic_id, Some(99));
                assert_eq!(command, "status");
            }
            _ => panic!("expected Command"),
        }
    }
}

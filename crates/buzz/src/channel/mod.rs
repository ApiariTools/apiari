//! Channel abstraction for messaging integrations.

pub mod telegram;

use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

/// An event received from a channel.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// A regular text message from a user.
    Message {
        chat_id: i64,
        message_id: i64,
        user_id: i64,
        user_name: String,
        text: String,
        topic_id: Option<i64>,
    },

    /// A slash command (e.g. /status, /reset).
    Command {
        chat_id: i64,
        message_id: i64,
        user_id: i64,
        user_name: String,
        command: String,
        args: String,
        topic_id: Option<i64>,
    },

    /// An inline keyboard button press.
    CallbackQuery {
        chat_id: i64,
        user_name: String,
        data: String,
        callback_query_id: String,
        topic_id: Option<i64>,
    },
}

/// A button in an inline keyboard row.
#[derive(Debug, Clone)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// A message to send through a channel.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub chat_id: i64,
    pub text: String,
    pub buttons: Vec<Vec<InlineButton>>,
    pub topic_id: Option<i64>,
}

/// Trait for messaging channel integrations.
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;

    /// Run the receive loop, sending events to `tx`.
    /// Runs until cancelled.
    async fn run(&self, tx: Sender<ChannelEvent>, cancel: tokio::sync::watch::Receiver<bool>);

    /// Send a message through this channel.
    async fn send_message(&self, msg: &OutboundMessage) -> color_eyre::Result<()>;

    /// Acknowledge a callback query.
    async fn answer_callback_query(&self, callback_query_id: &str);
}

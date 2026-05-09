//! Channel abstraction for messaging integrations (Telegram, future Discord/Slack).

pub mod telegram;

use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// An event received from a channel.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// A regular text message from a user.
    Message {
        chat_id: i64,
        message_id: i64,
        user_id: i64,
        user_name: String,
        text: String,
        /// Forum topic ID, if the message was sent in a topic.
        topic_id: Option<i64>,
    },

    /// A slash command from a user (e.g. /reset, /history).
    Command {
        chat_id: i64,
        message_id: i64,
        user_id: i64,
        user_name: String,
        command: String,
        args: String,
        /// Forum topic ID, if the command was sent in a topic.
        topic_id: Option<i64>,
    },

    /// An inline keyboard button press (Telegram callback query).
    CallbackQuery {
        chat_id: i64,
        user_name: String,
        data: String,
        callback_query_id: String,
        /// Forum topic ID, if the original message was in a topic.
        topic_id: Option<i64>,
    },
}

/// A button in an inline keyboard row.
#[derive(Debug, Clone)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// A message to send back through a channel.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub chat_id: i64,
    pub text: String,
    /// Optional inline keyboard buttons (rows of buttons).
    pub buttons: Vec<Vec<InlineButton>>,
    /// Forum topic ID. When set, the message is sent to this topic.
    pub topic_id: Option<i64>,
}

/// Trait for messaging channel integrations.
///
/// Implementations run a background loop that produces `ChannelEvent`s
/// and can send outbound messages.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable name for logging.
    #[allow(dead_code)]
    fn name(&self) -> &str;

    /// Run the channel's receive loop, sending events to `tx`.
    /// Should run until `cancel` is triggered.
    async fn run(&self, tx: Sender<ChannelEvent>, cancel: CancellationToken);

    /// Send a message through this channel.
    async fn send_message(&self, msg: &OutboundMessage) -> color_eyre::Result<()>;

    /// Acknowledge a callback query (required by Telegram to dismiss spinner).
    #[allow(dead_code)]
    async fn answer_callback_query(&self, callback_query_id: &str);
}

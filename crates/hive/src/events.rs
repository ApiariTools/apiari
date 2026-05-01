//! Event broadcast hub for real-time WebSocket updates.

use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HiveEvent {
    /// New message added to a conversation
    Message {
        workspace: String,
        bot: String,
        role: String,
        content: String,
    },
    /// Bot status changed (thinking, streaming, idle)
    BotStatus {
        workspace: String,
        bot: String,
        status: String,
        tool_name: Option<String>,
    },
    /// Worker state changed (reserved for future use)
    #[allow(dead_code)]
    WorkerUpdate {
        workspace: String,
        worker_id: String,
        status: String,
    },
    /// Research task status changed
    ResearchUpdate {
        workspace: String,
        task_id: String,
        status: String,
        topic: String,
        output_file: Option<String>,
    },
    /// Follow-up fired
    FollowupFired {
        id: String,
        workspace: String,
        bot: String,
        action: String,
        fires_at: String,
    },
    /// Follow-up cancelled
    FollowupCancelled {
        id: String,
        workspace: String,
        bot: String,
        action: String,
        fires_at: String,
    },
    /// Event bridged from a remote hive instance (sent as raw JSON to WS clients)
    #[serde(skip)]
    #[allow(dead_code)]
    RemoteEvent {
        remote: String,
        workspace: String,
        bot: String,
        event_type: String,
        raw_json: String,
    },
}

#[derive(Clone)]
pub struct EventHub {
    tx: Arc<broadcast::Sender<HiveEvent>>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx: Arc::new(tx) }
    }

    pub fn send(&self, event: HiveEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HiveEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_hub_send_receive() {
        let hub = EventHub::new();
        let mut rx = hub.subscribe();

        hub.send(HiveEvent::Message {
            workspace: "ws".into(),
            bot: "Main".into(),
            role: "assistant".into(),
            content: "hello".into(),
        });

        let event = rx.try_recv().unwrap();
        match event {
            HiveEvent::Message { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("wrong event type"),
        }
    }

    #[test]
    fn test_event_hub_multiple_subscribers() {
        let hub = EventHub::new();
        let mut rx1 = hub.subscribe();
        let mut rx2 = hub.subscribe();

        hub.send(HiveEvent::BotStatus {
            workspace: "ws".into(),
            bot: "Main".into(),
            status: "thinking".into(),
            tool_name: None,
        });

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[test]
    fn test_event_hub_no_subscribers_ok() {
        let hub = EventHub::new();
        // Should not panic
        hub.send(HiveEvent::Message {
            workspace: "ws".into(),
            bot: "Main".into(),
            role: "user".into(),
            content: "test".into(),
        });
    }

    #[test]
    fn test_event_serializes_to_json() {
        let event = HiveEvent::BotStatus {
            workspace: "apiari".into(),
            bot: "Main".into(),
            status: "streaming".into(),
            tool_name: Some("Read".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("bot_status"));
        assert!(json.contains("streaming"));
        assert!(json.contains("Read"));
    }

    #[test]
    fn test_event_hub_clone() {
        let hub1 = EventHub::new();
        let hub2 = hub1.clone();
        let mut rx = hub1.subscribe();

        hub2.send(HiveEvent::Message {
            workspace: "ws".into(),
            bot: "Main".into(),
            role: "assistant".into(),
            content: "from clone".into(),
        });

        let event = rx.try_recv().unwrap();
        match event {
            HiveEvent::Message { content, .. } => assert_eq!(content, "from clone"),
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn test_default_creates_hub() {
        let hub = EventHub::default();
        let mut rx = hub.subscribe();
        hub.send(HiveEvent::Message {
            workspace: "ws".into(),
            bot: "Main".into(),
            role: "user".into(),
            content: "test".into(),
        });
        assert!(rx.try_recv().is_ok());
    }
}

//! Data types for user-facing reminders.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What kind of reminder this is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReminderKind {
    Once,
    Cron,
}

impl std::fmt::Display for ReminderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Once => write!(f, "once"),
            Self::Cron => write!(f, "cron"),
        }
    }
}

/// A stored reminder (persisted to `.hive/reminders.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredReminder {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable message.
    pub message: String,
    /// Once or Cron.
    pub kind: ReminderKind,
    /// Next fire time (set for both kinds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_at: Option<DateTime<Utc>>,
    /// Cron expression (only for cron kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_expr: Option<String>,
    /// When this reminder was created.
    pub created_at: DateTime<Utc>,
    /// Which Telegram chat to deliver to (defaults to alert_chat_id from daemon.toml).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
    /// Whether this reminder has been cancelled.
    #[serde(default)]
    pub cancelled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_roundtrip_once() {
        let r = StoredReminder {
            id: "abc-123".into(),
            message: "Check PR".into(),
            kind: ReminderKind::Once,
            fire_at: Some(Utc::now()),
            cron_expr: None,
            created_at: Utc::now(),
            chat_id: Some(12345),
            cancelled: false,
        };
        let json = serde_json::to_string(&r).unwrap();
        let parsed: StoredReminder = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "abc-123");
        assert_eq!(parsed.kind, ReminderKind::Once);
        assert!(parsed.cron_expr.is_none());
        assert_eq!(parsed.chat_id, Some(12345));
    }

    #[test]
    fn test_serde_roundtrip_cron() {
        let r = StoredReminder {
            id: "def-456".into(),
            message: "Daily standup".into(),
            kind: ReminderKind::Cron,
            fire_at: Some(Utc::now()),
            cron_expr: Some("0 9 * * *".into()),
            created_at: Utc::now(),
            chat_id: None,
            cancelled: false,
        };
        let json = serde_json::to_string(&r).unwrap();
        let parsed: StoredReminder = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, ReminderKind::Cron);
        assert_eq!(parsed.cron_expr.as_deref(), Some("0 9 * * *"));
        assert!(parsed.chat_id.is_none());
    }

    #[test]
    fn test_deserialize_with_defaults() {
        // Missing optional fields should use defaults.
        let json = r#"{
            "id": "test-1",
            "message": "hello",
            "kind": "once",
            "created_at": "2026-01-01T00:00:00Z"
        }"#;
        let r: StoredReminder = serde_json::from_str(json).unwrap();
        assert!(r.fire_at.is_none());
        assert!(r.cron_expr.is_none());
        assert!(r.chat_id.is_none());
        assert!(!r.cancelled);
    }

    #[test]
    fn test_kind_display() {
        assert_eq!(ReminderKind::Once.to_string(), "once");
        assert_eq!(ReminderKind::Cron.to_string(), "cron");
    }
}

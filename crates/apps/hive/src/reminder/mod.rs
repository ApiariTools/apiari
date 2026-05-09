//! User-facing reminders system.
//!
//! Supports one-shot (`hive remind 30m "message"`) and cron-based
//! (`hive remind --cron "0 9 * * *" "message"`) reminders with persistence
//! to `.hive/reminders.json`.
//!
//! This is separate from the buzz interval-based reminders in `buzz/reminder.rs`,
//! which are config-driven and in-memory.

pub mod duration;
pub mod store;
pub mod types;

pub use store::{CancelError, ReminderStore};
pub use types::{ReminderKind, StoredReminder};

use chrono::Utc;

/// Create a one-shot reminder from a duration string and message.
pub fn create_oneshot(
    duration_str: &str,
    message: &str,
    chat_id: Option<i64>,
) -> Result<StoredReminder, String> {
    let dur = duration::parse_duration(duration_str)?;
    let fire_at = Utc::now() + dur;

    Ok(StoredReminder {
        id: uuid::Uuid::new_v4().to_string(),
        message: message.to_owned(),
        kind: ReminderKind::Once,
        fire_at: Some(fire_at),
        cron_expr: None,
        created_at: Utc::now(),
        chat_id,
        cancelled: false,
    })
}

/// Create a cron reminder from a cron expression and message.
///
/// Validates the expression immediately and computes the first fire time.
pub fn create_cron(
    cron_expr: &str,
    message: &str,
    chat_id: Option<i64>,
) -> Result<StoredReminder, String> {
    let fire_at = store::next_cron_fire(cron_expr)?;

    Ok(StoredReminder {
        id: uuid::Uuid::new_v4().to_string(),
        message: message.to_owned(),
        kind: ReminderKind::Cron,
        fire_at: Some(fire_at),
        cron_expr: Some(cron_expr.to_owned()),
        created_at: Utc::now(),
        chat_id,
        cancelled: false,
    })
}

/// Format a single reminder for display.
pub fn format_reminder(r: &StoredReminder) -> String {
    let short_id = &r.id[..8.min(r.id.len())];
    let fire_str = r
        .fire_at
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".into());

    match r.kind {
        ReminderKind::Once => {
            format!("`{short_id}` [once] fires {fire_str}\n  {}", r.message)
        }
        ReminderKind::Cron => {
            let expr = r.cron_expr.as_deref().unwrap_or("?");
            format!(
                "`{short_id}` [cron] {expr} — next {fire_str}\n  {}",
                r.message
            )
        }
    }
}

/// Format a list of reminders for display.
pub fn format_reminder_list(reminders: &[&StoredReminder]) -> String {
    if reminders.is_empty() {
        return "No pending reminders.".into();
    }

    let mut text = String::from("Pending reminders:\n");
    for (i, r) in reminders.iter().enumerate() {
        text.push_str(&format!("\n{}. {}", i + 1, format_reminder(r)));
    }
    text
}

/// Parse cron arguments from a Telegram `/remind --cron ...` command.
///
/// Expects the string after `--cron `, e.g. `"0 9 * * *" Daily standup`.
/// The cron expression must be quoted with `"` or `'`.
pub fn parse_cron_args(s: &str) -> Result<(String, String), String> {
    let s = s.trim();

    // Look for quoted cron expression.
    let (cron_expr, rest) = if let Some(inner) = s.strip_prefix('"') {
        let end = inner
            .find('"')
            .ok_or("missing closing quote for cron expression")?;
        (&inner[..end], inner[end + 1..].trim())
    } else if let Some(inner) = s.strip_prefix('\'') {
        let end = inner
            .find('\'')
            .ok_or("missing closing quote for cron expression")?;
        (&inner[..end], inner[end + 1..].trim())
    } else {
        return Err("cron expression must be quoted (e.g. \"0 9 * * *\")".into());
    };

    if rest.is_empty() {
        return Err("missing reminder message after cron expression".into());
    }

    Ok((cron_expr.to_owned(), rest.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_oneshot() {
        let r = create_oneshot("30m", "Check PR", None).unwrap();
        assert_eq!(r.kind, ReminderKind::Once);
        assert_eq!(r.message, "Check PR");
        assert!(r.fire_at.unwrap() > Utc::now());
        assert!(r.cron_expr.is_none());
        assert!(r.chat_id.is_none());
    }

    #[test]
    fn test_create_oneshot_with_chat_id() {
        let r = create_oneshot("1h", "test", Some(12345)).unwrap();
        assert_eq!(r.chat_id, Some(12345));
    }

    #[test]
    fn test_create_oneshot_bad_duration() {
        assert!(create_oneshot("abc", "test", None).is_err());
    }

    #[test]
    fn test_create_cron() {
        let r = create_cron("* * * * *", "every minute", None).unwrap();
        assert_eq!(r.kind, ReminderKind::Cron);
        assert_eq!(r.message, "every minute");
        assert!(r.fire_at.unwrap() > Utc::now());
        assert_eq!(r.cron_expr.as_deref(), Some("* * * * *"));
    }

    #[test]
    fn test_create_cron_invalid() {
        assert!(create_cron("not valid", "test", None).is_err());
    }

    #[test]
    fn test_format_reminder_once() {
        let r = StoredReminder {
            id: "abcdefgh-1234".into(),
            message: "Check PR".into(),
            kind: ReminderKind::Once,
            fire_at: Some(Utc::now()),
            cron_expr: None,
            created_at: Utc::now(),
            chat_id: None,
            cancelled: false,
        };
        let s = format_reminder(&r);
        assert!(s.contains("`abcdefgh`"), "got: {s}");
        assert!(s.contains("[once]"), "got: {s}");
        assert!(s.contains("Check PR"), "got: {s}");
    }

    #[test]
    fn test_format_reminder_cron() {
        let r = StoredReminder {
            id: "12345678-abcd".into(),
            message: "Daily standup".into(),
            kind: ReminderKind::Cron,
            fire_at: Some(Utc::now()),
            cron_expr: Some("0 9 * * *".into()),
            created_at: Utc::now(),
            chat_id: None,
            cancelled: false,
        };
        let s = format_reminder(&r);
        assert!(s.contains("[cron]"), "got: {s}");
        assert!(s.contains("0 9 * * *"), "got: {s}");
        assert!(s.contains("Daily standup"), "got: {s}");
    }

    #[test]
    fn test_format_reminder_list_empty() {
        assert_eq!(format_reminder_list(&[]), "No pending reminders.");
    }

    #[test]
    fn test_format_reminder_list_numbered() {
        let r = StoredReminder {
            id: "test-id-1234".into(),
            message: "hello".into(),
            kind: ReminderKind::Once,
            fire_at: Some(Utc::now()),
            cron_expr: None,
            created_at: Utc::now(),
            chat_id: None,
            cancelled: false,
        };
        let list = format_reminder_list(&[&r]);
        assert!(list.contains("1. "), "got: {list}");
        assert!(list.contains("hello"), "got: {list}");
    }

    #[test]
    fn test_parse_cron_args_double_quotes() {
        let (expr, msg) = parse_cron_args(r#""0 9 * * *" Daily standup"#).unwrap();
        assert_eq!(expr, "0 9 * * *");
        assert_eq!(msg, "Daily standup");
    }

    #[test]
    fn test_parse_cron_args_single_quotes() {
        let (expr, msg) = parse_cron_args("'0 9 * * *' Daily standup").unwrap();
        assert_eq!(expr, "0 9 * * *");
        assert_eq!(msg, "Daily standup");
    }

    #[test]
    fn test_parse_cron_args_no_quotes() {
        assert!(parse_cron_args("0 9 * * * Daily standup").is_err());
    }

    #[test]
    fn test_parse_cron_args_no_message() {
        assert!(parse_cron_args(r#""0 9 * * *""#).is_err());
    }

    #[test]
    fn test_parse_cron_args_unclosed_quote() {
        assert!(parse_cron_args(r#""0 9 * * *"#).is_err());
    }
}

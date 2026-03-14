//! Signal formatting for notifications.

use crate::buzz::signal::{Severity, SignalRecord};

/// Severity emoji prefix.
fn severity_emoji(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "\u{1f6a8}",     // 🚨
        Severity::Error => "\u{26a0}\u{fe0f}", // ⚠️
        Severity::Warning => "\u{1f536}",      // 🔶
        Severity::Info => "\u{2139}\u{fe0f}",  // ℹ️
    }
}

/// Format a single signal for immediate notification.
pub fn format_signal_notification(signal: &SignalRecord) -> String {
    let emoji = severity_emoji(&signal.severity);
    let mut msg = format!(
        "{emoji} [{source}] {title}",
        source = signal.source,
        title = signal.title
    );

    if let Some(ref body) = signal.body {
        let first_line = body.lines().next().unwrap_or("");
        if !first_line.is_empty() {
            msg.push('\n');
            msg.push_str(first_line);
        }
    }

    if let Some(ref url) = signal.url {
        msg.push('\n');
        msg.push_str(url);
    }

    msg
}

/// Format a batch of signals as a grouped summary.
pub fn format_batch_notification(signals: &[SignalRecord]) -> String {
    if signals.is_empty() {
        return String::new();
    }

    let mut msg = format!("\u{1f4cb} {} new signal(s):", signals.len()); // 📋

    for signal in signals.iter().take(15) {
        let emoji = severity_emoji(&signal.severity);
        msg.push_str(&format!(
            "\n{emoji} [{source}] {title}",
            source = signal.source,
            title = signal.title,
        ));
    }

    if signals.len() > 15 {
        msg.push_str(&format!("\n... and {} more", signals.len() - 15));
    }

    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::SignalStatus;
    use chrono::Utc;

    fn make_record(
        source: &str,
        title: &str,
        severity: Severity,
        body: Option<&str>,
        url: Option<&str>,
    ) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.into(),
            external_id: format!("{source}-1"),
            title: title.into(),
            body: body.map(|s| s.into()),
            severity,
            status: SignalStatus::Open,
            url: url.map(|s| s.into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
        }
    }

    #[test]
    fn test_format_signal_basic() {
        let r = make_record("sentry", "Server error", Severity::Error, None, None);
        let msg = format_signal_notification(&r);
        assert!(msg.contains("[sentry]"));
        assert!(msg.contains("Server error"));
        assert!(msg.contains("\u{26a0}"));
    }

    #[test]
    fn test_format_signal_with_body_and_url() {
        let r = make_record(
            "sentry",
            "Timeout error",
            Severity::Critical,
            Some("Payment processing timeout after 30s\nMore details here"),
            Some("https://sentry.io/issues/12345"),
        );
        let msg = format_signal_notification(&r);
        assert!(msg.contains("Payment processing timeout after 30s"));
        assert!(!msg.contains("More details here")); // only first line
        assert!(msg.contains("https://sentry.io/issues/12345"));
    }

    #[test]
    fn test_format_batch_empty() {
        assert!(format_batch_notification(&[]).is_empty());
    }

    #[test]
    fn test_format_batch() {
        let signals = vec![
            make_record(
                "sentry",
                "TypeError in dashboard.js",
                Severity::Info,
                None,
                None,
            ),
            make_record(
                "github",
                "PR #42 review requested",
                Severity::Info,
                None,
                None,
            ),
            make_record(
                "swarm",
                "Worker spawned: hive-3",
                Severity::Info,
                None,
                None,
            ),
        ];
        let msg = format_batch_notification(&signals);
        assert!(msg.contains("3 new signal(s):"));
        assert!(msg.contains("[sentry] TypeError in dashboard.js"));
        assert!(msg.contains("[github] PR #42 review requested"));
        assert!(msg.contains("[swarm] Worker spawned: hive-3"));
    }

    #[test]
    fn test_format_batch_truncation() {
        let signals: Vec<_> = (0..20)
            .map(|i| make_record("sentry", &format!("Bug {i}"), Severity::Info, None, None))
            .collect();
        let msg = format_batch_notification(&signals);
        assert!(msg.contains("20 new signal(s):"));
        assert!(msg.contains("... and 5 more"));
    }

    #[test]
    fn test_severity_emojis() {
        let critical = make_record("x", "t", Severity::Critical, None, None);
        let error = make_record("x", "t", Severity::Error, None, None);
        let warning = make_record("x", "t", Severity::Warning, None, None);
        let info = make_record("x", "t", Severity::Info, None, None);

        assert!(format_signal_notification(&critical).contains("\u{1f6a8}"));
        assert!(format_signal_notification(&error).contains("\u{26a0}"));
        assert!(format_signal_notification(&warning).contains("\u{1f536}"));
        assert!(format_signal_notification(&info).contains("\u{2139}"));
    }
}

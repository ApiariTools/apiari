use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::buzz::signal::{Severity, SignalRecord};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationTier {
    Silent,
    #[default]
    Badge,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub trigger: String,
    pub tier: NotificationTier,
    pub text: String,
}

pub type NotificationTierOverrides = HashMap<String, NotificationTier>;

pub fn notification_for_signal(
    signal: &SignalRecord,
    overrides: &NotificationTierOverrides,
) -> Notification {
    let trigger = classify_signal(signal);
    let tier = overrides
        .get(trigger.as_str())
        .copied()
        .unwrap_or_else(|| default_tier(&trigger, &signal.severity));

    Notification {
        trigger,
        tier,
        text: format_signal_notification(signal),
    }
}

pub fn classify_signal(signal: &SignalRecord) -> String {
    signal.source.clone()
}

fn default_tier(trigger: &str, severity: &Severity) -> NotificationTier {
    match trigger {
        "github_ci_failure" => NotificationTier::Chat,
        "github_ci_pass" => NotificationTier::Silent,
        "github_merged_pr" => NotificationTier::Silent,
        "github_release" => NotificationTier::Chat,
        "swarm_pr_opened" => NotificationTier::Chat,
        "swarm_worker_spawned" => NotificationTier::Badge,
        "swarm_worker_completed" => NotificationTier::Badge,
        "swarm_worker_closed" => NotificationTier::Badge,
        "swarm_review_verdict" => NotificationTier::Chat,
        _ => match severity {
            Severity::Critical | Severity::Error => NotificationTier::Chat,
            Severity::Warning | Severity::Info => NotificationTier::Badge,
        },
    }
}

fn severity_emoji(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "\u{1f6a8}",
        Severity::Error => "\u{26a0}\u{fe0f}",
        Severity::Warning => "\u{1f536}",
        Severity::Info => "\u{2139}\u{fe0f}",
    }
}

fn format_signal_notification(signal: &SignalRecord) -> String {
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::buzz::signal::SignalStatus;

    fn make_signal(source: &str, external_id: &str, severity: Severity) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.into(),
            external_id: external_id.into(),
            title: "Test signal".into(),
            body: None,
            severity,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
            snoozed_until: None,
        }
    }

    #[test]
    fn classifies_swarm_special_cases() {
        assert_eq!(
            classify_signal(&make_signal(
                "swarm_worker_spawned",
                "swarm-spawned-123",
                Severity::Info,
            )),
            "swarm_worker_spawned"
        );
        assert_eq!(
            classify_signal(&make_signal(
                "swarm_pr_opened",
                "swarm-pr-123",
                Severity::Info,
            )),
            "swarm_pr_opened"
        );
    }

    #[test]
    fn applies_overrides_before_defaults() {
        let signal = make_signal("github_ci_failure", "ci-1", Severity::Error);
        let overrides =
            HashMap::from([("github_ci_failure".to_string(), NotificationTier::Silent)]);

        let notification = notification_for_signal(&signal, &overrides);

        assert_eq!(notification.tier, NotificationTier::Silent);
    }
}

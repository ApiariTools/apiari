//! Time-based signal generation from scheduled reminders.

use crate::signal::{Severity, Signal};
use chrono::{DateTime, Duration, Utc};

use super::config::ReminderConfig;

/// A recurring reminder that fires signals at a fixed interval.
#[derive(Debug, Clone)]
pub struct Reminder {
    /// Human-readable message.
    pub message: String,
    /// How often this reminder fires.
    pub interval: Duration,
    /// When this reminder next fires.
    pub next_fire: DateTime<Utc>,
}

impl Reminder {
    /// Create a reminder from config. The first firing is scheduled one interval from now.
    pub fn from_config(config: &ReminderConfig) -> Self {
        let interval = Duration::seconds(config.interval_secs as i64);
        Self {
            message: config.message.clone(),
            interval,
            next_fire: Utc::now() + interval,
        }
    }
}

/// Check all reminders and fire any that are due, returning signals and advancing
/// their next_fire time.
pub fn check_reminders(reminders: &mut [Reminder]) -> Vec<Signal> {
    let now = Utc::now();
    let mut signals = Vec::new();

    for reminder in reminders.iter_mut() {
        if now >= reminder.next_fire {
            let signal = Signal::new(
                "reminder",
                Severity::Info,
                &reminder.message,
                &reminder.message,
            )
            .with_dedup_key(format!("reminder:{}", reminder.message));
            signals.push(signal);

            // Advance to the next firing time. If we missed multiple intervals,
            // skip ahead to the next future occurrence.
            while reminder.next_fire <= now {
                reminder.next_fire += reminder.interval;
            }
        }
    }

    signals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reminder_fires_when_due() {
        let mut reminders = vec![Reminder {
            message: "standup".to_string(),
            interval: Duration::seconds(60),
            next_fire: Utc::now() - Duration::seconds(10), // already past due
        }];

        let signals = check_reminders(&mut reminders);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].title, "standup");
        assert!(reminders[0].next_fire > Utc::now());
    }

    #[test]
    fn test_reminder_does_not_fire_early() {
        let mut reminders = vec![Reminder {
            message: "later".to_string(),
            interval: Duration::seconds(3600),
            next_fire: Utc::now() + Duration::seconds(1800),
        }];

        let signals = check_reminders(&mut reminders);
        assert!(signals.is_empty());
    }
}

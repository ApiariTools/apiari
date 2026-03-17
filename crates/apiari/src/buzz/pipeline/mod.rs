//! Notification pipeline — fingerprint, dedup, rate-limit, batch, dispatch.
//!
//! Replaces the naive "if new, send summary of all open signals" with a proper
//! configurable pipeline: match rules → action (notify/batch/drop).

pub mod batch;
pub mod format;
pub mod log;
pub mod rule;

use std::time::Duration;

use crate::buzz::signal::SignalRecord;
use rule::{PipelineAction, PipelineRule, default_rules};
use tracing::debug;

use self::batch::BatchCollector;
use self::format::{format_batch_notification, format_signal_notification};
use self::log::NotificationLog;

/// Notification pipeline: process signals through rules, dedup, rate-limit, batch.
pub struct Pipeline {
    rules: Vec<PipelineRule>,
    log: NotificationLog,
    batch: BatchCollector,
}

impl Pipeline {
    /// Create a new pipeline. If `rules` is empty, uses default rules.
    pub fn new(rules: Vec<PipelineRule>, batch_window_secs: u64) -> Self {
        let rules = if rules.is_empty() {
            default_rules()
        } else {
            rules
        };
        Self {
            rules,
            log: NotificationLog::new(),
            batch: BatchCollector::new(batch_window_secs),
        }
    }

    /// Process a newly inserted signal. Returns an optional message to send.
    pub fn process(&mut self, signal: &SignalRecord) -> Option<String> {
        let fingerprint = format!("{}:{}", signal.source, signal.external_id);

        // Find first matching rule
        let matched = self
            .rules
            .iter()
            .find(|r| r.matches(&signal.source, &signal.severity, &signal.external_id));

        let (action, rate_limit_secs) = match matched {
            Some(rule) => {
                debug!(
                    "pipeline: signal {} matched rule '{}' → {:?}",
                    fingerprint, rule.name, rule.action
                );
                (&rule.action, rule.rate_limit_secs)
            }
            None => {
                // No rule matched — default to batch
                debug!(
                    "pipeline: signal {} matched no rules, batching",
                    fingerprint
                );
                (&PipelineAction::Batch, 0)
            }
        };

        match action {
            PipelineAction::Drop => None,
            PipelineAction::Batch => {
                self.batch.add(signal);
                None
            }
            PipelineAction::Notify => {
                if self.log.should_send(&fingerprint, rate_limit_secs) {
                    self.log.record_sent(&fingerprint);
                    Some(format_signal_notification(signal))
                } else {
                    debug!(
                        "pipeline: rate-limited {} ({}s window)",
                        fingerprint, rate_limit_secs
                    );
                    None
                }
            }
        }
    }

    /// Flush any pending batches. Returns an optional batch summary message.
    pub fn flush_batches(&mut self) -> Option<String> {
        if let Some(signals) = self.batch.flush_if_ready() {
            if signals.is_empty() {
                return None;
            }
            Some(format_batch_notification(&signals))
        } else {
            None
        }
    }

    /// Evict old entries from the notification log (call periodically).
    pub fn evict_old_log_entries(&mut self) {
        self.log.evict_old(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::{Severity, SignalStatus};
    use chrono::Utc;
    use rule::PipelineRule;

    fn make_signal(source: &str, ext_id: &str, title: &str, severity: Severity) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.into(),
            external_id: ext_id.into(),
            title: title.into(),
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
    fn test_critical_notifies_immediately() {
        let mut pipeline = Pipeline::new(vec![], 60);
        let signal = make_signal("sentry", "issue-1", "Server down", Severity::Critical);
        let result = pipeline.process(&signal);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Server down"));
    }

    #[test]
    fn test_info_gets_batched() {
        let mut pipeline = Pipeline::new(vec![], 60);
        let signal = make_signal("github", "pr-1", "New PR", Severity::Info);
        let result = pipeline.process(&signal);
        assert!(result.is_none()); // batched, not immediate
    }

    #[test]
    fn test_drop_rule() {
        let rules = vec![PipelineRule {
            name: "drop-all".into(),
            source: None,
            severity: None,
            id_prefix: None,
            action: PipelineAction::Drop,
            rate_limit_secs: 0,
        }];
        let mut pipeline = Pipeline::new(rules, 60);
        let signal = make_signal("sentry", "issue-1", "Bug", Severity::Critical);
        assert!(pipeline.process(&signal).is_none());
    }

    #[test]
    fn test_rate_limiting() {
        let rules = vec![PipelineRule {
            name: "error-rate-limited".into(),
            source: None,
            severity: Some(Severity::Error),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 300,
        }];
        let mut pipeline = Pipeline::new(rules, 60);
        let signal = make_signal("sentry", "issue-1", "Bug", Severity::Error);

        // First time: should notify
        assert!(pipeline.process(&signal).is_some());
        // Second time: rate limited
        assert!(pipeline.process(&signal).is_none());
    }

    #[test]
    fn test_different_fingerprints_not_rate_limited() {
        let rules = vec![PipelineRule {
            name: "error-rate-limited".into(),
            source: None,
            severity: Some(Severity::Error),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 300,
        }];
        let mut pipeline = Pipeline::new(rules, 60);

        let s1 = make_signal("sentry", "issue-1", "Bug 1", Severity::Error);
        let s2 = make_signal("sentry", "issue-2", "Bug 2", Severity::Error);

        assert!(pipeline.process(&s1).is_some());
        assert!(pipeline.process(&s2).is_some()); // different fingerprint
    }

    #[test]
    fn test_batch_flush() {
        let mut pipeline = Pipeline::new(vec![], 0); // 0s window = flush immediately
        let s1 = make_signal("github", "pr-1", "PR opened", Severity::Info);
        let s2 = make_signal("github", "pr-2", "PR review", Severity::Info);

        pipeline.process(&s1);
        pipeline.process(&s2);

        let batch = pipeline.flush_batches();
        assert!(batch.is_some());
        let msg = batch.unwrap();
        assert!(msg.contains("2 new signal(s):"));
        assert!(msg.contains("PR opened"));
        assert!(msg.contains("PR review"));
    }

    #[test]
    fn test_batch_not_ready() {
        let mut pipeline = Pipeline::new(vec![], 3600); // 1 hour window
        let signal = make_signal("github", "pr-1", "PR opened", Severity::Info);
        pipeline.process(&signal);

        // Window hasn't elapsed
        assert!(pipeline.flush_batches().is_none());
    }

    #[test]
    fn test_no_match_defaults_to_batch() {
        let rules = vec![PipelineRule {
            name: "sentry-only".into(),
            source: Some("sentry".into()),
            severity: None,
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 0,
        }];
        let mut pipeline = Pipeline::new(rules, 0);
        let signal = make_signal("github", "pr-1", "PR", Severity::Error);

        // Doesn't match the sentry-only rule → falls through to batch
        assert!(pipeline.process(&signal).is_none());

        // But it's in the batch
        let batch = pipeline.flush_batches();
        assert!(batch.is_some());
    }

    #[test]
    fn test_first_rule_wins() {
        let rules = vec![
            PipelineRule {
                name: "drop-sentry".into(),
                source: Some("sentry".into()),
                severity: None,
                id_prefix: None,
                action: PipelineAction::Drop,
                rate_limit_secs: 0,
            },
            PipelineRule {
                name: "notify-all".into(),
                source: None,
                severity: None,
                id_prefix: None,
                action: PipelineAction::Notify,
                rate_limit_secs: 0,
            },
        ];
        let mut pipeline = Pipeline::new(rules, 60);
        let signal = make_signal("sentry", "issue-1", "Bug", Severity::Critical);

        // First rule (drop-sentry) matches, not the catch-all
        assert!(pipeline.process(&signal).is_none());
    }

    #[test]
    fn test_empty_batch_flush_returns_none() {
        let mut pipeline = Pipeline::new(vec![], 0);
        assert!(pipeline.flush_batches().is_none());
    }

    #[test]
    fn test_default_rules_behavior() {
        let mut pipeline = Pipeline::new(vec![], 60);

        // Critical: immediate, no rate limit
        let critical = make_signal("sentry", "c1", "Critical", Severity::Critical);
        assert!(pipeline.process(&critical).is_some());
        assert!(pipeline.process(&critical).is_some()); // no rate limit

        // Error: immediate, rate limited
        let error = make_signal("sentry", "e1", "Error", Severity::Error);
        assert!(pipeline.process(&error).is_some());
        assert!(pipeline.process(&error).is_none()); // rate limited

        // Warning: immediate, rate limited
        let warning = make_signal("sentry", "w1", "Warning", Severity::Warning);
        assert!(pipeline.process(&warning).is_some());
        assert!(pipeline.process(&warning).is_none()); // rate limited

        // Info: batched
        let info = make_signal("sentry", "i1", "Info", Severity::Info);
        assert!(pipeline.process(&info).is_none()); // batched
    }
}

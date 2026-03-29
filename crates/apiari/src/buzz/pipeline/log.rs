//! Notification log — in-memory dedup tracker.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Tracks recently sent notifications to prevent duplicates.
pub struct NotificationLog {
    sent: HashMap<String, Instant>,
}

impl Default for NotificationLog {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationLog {
    pub fn new() -> Self {
        Self {
            sent: HashMap::new(),
        }
    }

    /// Returns true if this fingerprint was NOT sent within `rate_limit_secs`.
    /// If rate_limit_secs is 0, always returns true.
    pub fn should_send(&self, fingerprint: &str, rate_limit_secs: u64) -> bool {
        if rate_limit_secs == 0 {
            return true;
        }
        match self.sent.get(fingerprint) {
            Some(last) => last.elapsed() >= Duration::from_secs(rate_limit_secs),
            None => true,
        }
    }

    /// Record that we sent a notification for this fingerprint now.
    pub fn record_sent(&mut self, fingerprint: &str) {
        self.sent.insert(fingerprint.to_string(), Instant::now());
    }

    /// Evict entries older than max_age to prevent unbounded growth.
    pub fn evict_old(&mut self, max_age: Duration) {
        self.sent.retain(|_, last| last.elapsed() < max_age);
    }

    /// Number of tracked fingerprints (for testing).
    pub fn len(&self) -> usize {
        self.sent.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.sent.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_send_first_time() {
        let log = NotificationLog::new();
        assert!(log.should_send("sentry:issue-1", 300));
    }

    #[test]
    fn test_should_send_zero_rate_limit() {
        let mut log = NotificationLog::new();
        log.record_sent("sentry:issue-1");
        // rate_limit_secs=0 means always send
        assert!(log.should_send("sentry:issue-1", 0));
    }

    #[test]
    fn test_should_not_send_within_window() {
        let mut log = NotificationLog::new();
        log.record_sent("sentry:issue-1");
        // Just recorded, so 300s hasn't elapsed
        assert!(!log.should_send("sentry:issue-1", 300));
    }

    #[test]
    fn test_different_fingerprints_independent() {
        let mut log = NotificationLog::new();
        log.record_sent("sentry:issue-1");
        assert!(log.should_send("sentry:issue-2", 300));
    }

    #[test]
    fn test_evict_old() {
        let mut log = NotificationLog::new();
        // Insert with a fake old timestamp
        log.sent
            .insert("old".into(), Instant::now() - Duration::from_secs(7200));
        log.record_sent("recent");

        assert_eq!(log.len(), 2);
        log.evict_old(Duration::from_secs(3600));
        assert_eq!(log.len(), 1);
        assert!(log.sent.contains_key("recent"));
    }

    #[test]
    fn test_record_updates_timestamp() {
        let mut log = NotificationLog::new();
        log.sent
            .insert("fp".into(), Instant::now() - Duration::from_secs(600));
        // Should be sendable (600s > 300s limit)
        assert!(log.should_send("fp", 300));

        // Record again — now it's fresh
        log.record_sent("fp");
        assert!(!log.should_send("fp", 300));
    }
}

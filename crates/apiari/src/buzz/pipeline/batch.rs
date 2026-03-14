//! Batch collector — time-windowed signal grouping.

use std::time::{Duration, Instant};

use crate::buzz::signal::SignalRecord;

/// Collects signals into time-windowed batches.
pub struct BatchCollector {
    window: Duration,
    signals: Vec<SignalRecord>,
    window_start: Option<Instant>,
}

impl BatchCollector {
    pub fn new(window_secs: u64) -> Self {
        Self {
            window: Duration::from_secs(window_secs),
            signals: Vec::new(),
            window_start: None,
        }
    }

    /// Add a signal to the current batch.
    pub fn add(&mut self, signal: &SignalRecord) {
        if self.window_start.is_none() {
            self.window_start = Some(Instant::now());
        }
        self.signals.push(signal.clone());
    }

    /// Check if the batch window has elapsed and return collected signals.
    pub fn flush_if_ready(&mut self) -> Option<Vec<SignalRecord>> {
        if let Some(start) = self.window_start
            && start.elapsed() >= self.window
        {
            return Some(self.take());
        }
        None
    }

    /// Force flush (for shutdown or immediate needs).
    pub fn flush(&mut self) -> Vec<SignalRecord> {
        self.take()
    }

    /// Number of pending signals.
    pub fn len(&self) -> usize {
        self.signals.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }

    fn take(&mut self) -> Vec<SignalRecord> {
        self.window_start = None;
        std::mem::take(&mut self.signals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::{Severity, SignalStatus};
    use chrono::Utc;

    fn make_record(source: &str, title: &str) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.into(),
            external_id: format!("{source}-1"),
            title: title.into(),
            body: None,
            severity: Severity::Info,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
        }
    }

    #[test]
    fn test_empty_batch() {
        let mut batch = BatchCollector::new(60);
        assert!(batch.is_empty());
        assert!(batch.flush_if_ready().is_none());
    }

    #[test]
    fn test_add_signals() {
        let mut batch = BatchCollector::new(60);
        batch.add(&make_record("sentry", "Bug 1"));
        batch.add(&make_record("github", "PR 1"));
        assert_eq!(batch.len(), 2);
    }

    #[test]
    fn test_flush_not_ready() {
        let mut batch = BatchCollector::new(60);
        batch.add(&make_record("sentry", "Bug 1"));
        // Window just started, not ready yet
        assert!(batch.flush_if_ready().is_none());
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn test_force_flush() {
        let mut batch = BatchCollector::new(60);
        batch.add(&make_record("sentry", "Bug 1"));
        batch.add(&make_record("github", "PR 1"));

        let flushed = batch.flush();
        assert_eq!(flushed.len(), 2);
        assert!(batch.is_empty());
    }

    #[test]
    fn test_flush_resets_window() {
        let mut batch = BatchCollector::new(60);
        batch.add(&make_record("sentry", "Bug 1"));
        let _ = batch.flush();

        // After flush, window_start is None
        assert!(batch.flush_if_ready().is_none());
        assert!(batch.is_empty());
    }

    #[test]
    fn test_flush_when_window_elapsed() {
        let mut batch = BatchCollector::new(0); // 0-second window = always ready
        batch.add(&make_record("sentry", "Bug 1"));

        let flushed = batch.flush_if_ready();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 1);
        assert!(batch.is_empty());
    }
}

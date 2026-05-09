//! Signal processing — deduplication and prioritization.

use crate::signal::{Severity, Signal};
use std::collections::HashSet;

/// Deduplicate signals by their `dedup_key`.
///
/// Signals without a dedup_key are always kept. Among signals sharing the same
/// dedup_key, only the first occurrence is retained.
pub fn deduplicate(signals: &[Signal]) -> Vec<Signal> {
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(signals.len());

    for signal in signals {
        match &signal.dedup_key {
            Some(key) => {
                if seen.insert(key.clone()) {
                    result.push(signal.clone());
                }
            }
            None => {
                // No dedup key — always include.
                result.push(signal.clone());
            }
        }
    }

    result
}

/// Sort signals by severity (highest first), then by timestamp (newest first).
pub fn prioritize(signals: &mut [Signal]) {
    signals.sort_by(|a, b| {
        let sev_order = severity_rank(&b.severity).cmp(&severity_rank(&a.severity));
        sev_order.then_with(|| b.timestamp.cmp(&a.timestamp))
    });
}

/// Map severity to a numeric rank for sorting (higher = more severe).
fn severity_rank(severity: &Severity) -> u8 {
    match severity {
        Severity::Info => 0,
        Severity::Warning => 1,
        Severity::Critical => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signal(title: &str, severity: Severity, dedup_key: Option<&str>) -> Signal {
        let mut sig = Signal::new("test", severity, title, "body");
        sig.dedup_key = dedup_key.map(String::from);
        sig
    }

    #[test]
    fn test_deduplicate_keeps_first() {
        let signals = vec![
            make_signal("first", Severity::Info, Some("key-1")),
            make_signal("second", Severity::Warning, Some("key-1")),
            make_signal("third", Severity::Info, Some("key-2")),
        ];

        let deduped = deduplicate(&signals);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].title, "first");
        assert_eq!(deduped[1].title, "third");
    }

    #[test]
    fn test_deduplicate_keeps_signals_without_key() {
        let signals = vec![
            make_signal("a", Severity::Info, None),
            make_signal("b", Severity::Info, None),
        ];

        let deduped = deduplicate(&signals);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_prioritize_by_severity() {
        let mut signals = vec![
            make_signal("info", Severity::Info, None),
            make_signal("critical", Severity::Critical, None),
            make_signal("warning", Severity::Warning, None),
        ];

        prioritize(&mut signals);
        assert_eq!(signals[0].title, "critical");
        assert_eq!(signals[1].title, "warning");
        assert_eq!(signals[2].title, "info");
    }
}

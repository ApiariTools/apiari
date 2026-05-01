//! Integration tests for the buzz signal pipeline:
//! dedup, prioritize, and JSONL read/write round-trip.

use apiari_common::ipc::{JsonlReader, JsonlWriter};
use hive::buzz::signal::{deduplicate, prioritize};
use hive::signal::{Severity, Signal};
use tempfile::TempDir;

fn make_signal(title: &str, severity: Severity, dedup_key: Option<&str>) -> Signal {
    let mut sig = Signal::new("test", severity, title, "body");
    sig.dedup_key = dedup_key.map(String::from);
    sig
}

// ---- Deduplication ----

#[test]
fn deduplicate_removes_signals_with_identical_dedup_key() {
    let signals = vec![
        make_signal("first", Severity::Info, Some("key-A")),
        make_signal("second", Severity::Warning, Some("key-A")),
        make_signal("third", Severity::Critical, Some("key-A")),
    ];

    let result = deduplicate(&signals);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].title, "first");
}

#[test]
fn deduplicate_keeps_signals_with_different_keys() {
    let signals = vec![
        make_signal("alpha", Severity::Info, Some("key-1")),
        make_signal("beta", Severity::Info, Some("key-2")),
        make_signal("gamma", Severity::Info, Some("key-3")),
    ];

    let result = deduplicate(&signals);
    assert_eq!(result.len(), 3);
}

#[test]
fn deduplicate_keeps_all_signals_without_dedup_key() {
    let signals = vec![
        make_signal("a", Severity::Info, None),
        make_signal("b", Severity::Warning, None),
        make_signal("c", Severity::Critical, None),
    ];

    let result = deduplicate(&signals);
    assert_eq!(result.len(), 3);
}

#[test]
fn deduplicate_mixed_keyed_and_unkeyed() {
    let signals = vec![
        make_signal("keyed-1", Severity::Info, Some("dup")),
        make_signal("unkeyed-1", Severity::Warning, None),
        make_signal("keyed-2", Severity::Critical, Some("dup")),
        make_signal("unkeyed-2", Severity::Info, None),
    ];

    let result = deduplicate(&signals);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].title, "keyed-1");
    assert_eq!(result[1].title, "unkeyed-1");
    assert_eq!(result[2].title, "unkeyed-2");
}

#[test]
fn deduplicate_empty_input() {
    let result = deduplicate(&[]);
    assert!(result.is_empty());
}

// ---- Prioritization ----

#[test]
fn prioritize_orders_critical_before_warning_before_info() {
    let mut signals = vec![
        make_signal("info", Severity::Info, None),
        make_signal("critical", Severity::Critical, None),
        make_signal("warning", Severity::Warning, None),
    ];

    prioritize(&mut signals);
    assert_eq!(signals[0].severity, Severity::Critical);
    assert_eq!(signals[1].severity, Severity::Warning);
    assert_eq!(signals[2].severity, Severity::Info);
}

#[test]
fn prioritize_same_severity_orders_newest_first() {
    let mut s1 = make_signal("older", Severity::Warning, None);
    let mut s2 = make_signal("newer", Severity::Warning, None);

    // Force timestamps to differ.
    s1.timestamp = chrono::Utc::now() - chrono::Duration::seconds(60);
    s2.timestamp = chrono::Utc::now();

    let mut signals = vec![s1, s2];
    prioritize(&mut signals);

    assert_eq!(signals[0].title, "newer");
    assert_eq!(signals[1].title, "older");
}

#[test]
fn prioritize_empty_input() {
    let mut signals: Vec<Signal> = vec![];
    prioritize(&mut signals);
    assert!(signals.is_empty());
}

#[test]
fn prioritize_single_signal() {
    let mut signals = vec![make_signal("alone", Severity::Info, None)];
    prioritize(&mut signals);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].title, "alone");
}

// ---- Full pipeline: dedup → prioritize ----

#[test]
fn full_pipeline_dedup_then_prioritize() {
    let signals = vec![
        make_signal("dup-info", Severity::Info, Some("same-key")),
        make_signal("dup-critical", Severity::Critical, Some("same-key")),
        make_signal("unique-warning", Severity::Warning, None),
        make_signal("unique-critical", Severity::Critical, None),
    ];

    let mut result = deduplicate(&signals);
    prioritize(&mut result);

    // After dedup: "dup-info" (first with key), "unique-warning", "unique-critical"
    assert_eq!(result.len(), 3);
    // After prioritize: critical first, then warning, then info
    assert_eq!(result[0].severity, Severity::Critical);
    assert_eq!(result[0].title, "unique-critical");
    assert_eq!(result[1].severity, Severity::Warning);
    assert_eq!(result[1].title, "unique-warning");
    assert_eq!(result[2].severity, Severity::Info);
    assert_eq!(result[2].title, "dup-info");
}

// ---- JSONL round-trip ----

#[test]
fn write_signals_to_jsonl_then_read_back() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("signals.jsonl");

    let writer = JsonlWriter::<Signal>::new(&path);
    let signals = vec![
        Signal::new(
            "sentry",
            Severity::Critical,
            "Server down",
            "prod-1 unreachable",
        )
        .with_url("https://sentry.io/issues/123")
        .with_dedup_key("sentry-123"),
        Signal::new("github", Severity::Info, "New PR", "PR #42 opened")
            .with_tags(["review", "pr"]),
        Signal::new(
            "reminder",
            Severity::Warning,
            "Standup",
            "Daily standup reminder",
        ),
    ];

    for sig in &signals {
        writer.append(sig).unwrap();
    }

    let mut reader = JsonlReader::<Signal>::new(&path);
    let read_back = reader.poll().unwrap();

    assert_eq!(read_back.len(), 3);
    assert_eq!(read_back[0].title, "Server down");
    assert_eq!(read_back[0].source, "sentry");
    assert_eq!(read_back[0].severity, Severity::Critical);
    assert_eq!(
        read_back[0].url.as_deref(),
        Some("https://sentry.io/issues/123")
    );
    assert_eq!(read_back[0].dedup_key.as_deref(), Some("sentry-123"));

    assert_eq!(read_back[1].title, "New PR");
    assert_eq!(read_back[1].tags, vec!["review", "pr"]);

    assert_eq!(read_back[2].title, "Standup");
    assert_eq!(read_back[2].severity, Severity::Warning);
}

#[test]
fn jsonl_reader_only_returns_new_records() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("signals.jsonl");

    let writer = JsonlWriter::<Signal>::new(&path);
    let mut reader = JsonlReader::<Signal>::new(&path);

    writer
        .append(&Signal::new("test", Severity::Info, "First", "body"))
        .unwrap();

    let batch1 = reader.poll().unwrap();
    assert_eq!(batch1.len(), 1);

    // Second poll with no new data.
    let batch2 = reader.poll().unwrap();
    assert!(batch2.is_empty());

    // Write more.
    writer
        .append(&Signal::new("test", Severity::Warning, "Second", "body"))
        .unwrap();
    writer
        .append(&Signal::new("test", Severity::Critical, "Third", "body"))
        .unwrap();

    let batch3 = reader.poll().unwrap();
    assert_eq!(batch3.len(), 2);
    assert_eq!(batch3[0].title, "Second");
    assert_eq!(batch3[1].title, "Third");
}

#[test]
fn jsonl_reader_nonexistent_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.jsonl");

    let mut reader = JsonlReader::<Signal>::new(&path);
    let result = reader.poll().unwrap();
    assert!(result.is_empty());
}

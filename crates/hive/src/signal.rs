//! Signal type — buzz produces these, hive consumes them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Severity level for a signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Critical => write!(f, "critical"),
            Self::Warning => write!(f, "warning"),
            Self::Info => write!(f, "info"),
        }
    }
}

/// A signal represents an event detected by buzz (from Sentry, GitHub,
/// email, scheduled reminders, etc.) that hive can triage and act upon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    /// Unique identifier for this signal.
    pub id: String,

    /// Where this signal came from (e.g. "sentry", "github", "email", "reminder").
    pub source: String,

    /// How urgent this signal is.
    pub severity: Severity,

    /// Short human-readable title.
    pub title: String,

    /// Full description / body text.
    pub body: String,

    /// Optional URL linking to the upstream resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// When this signal was created.
    pub timestamp: DateTime<Utc>,

    /// Free-form tags for categorisation and filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Optional deduplication key. Signals with the same dedup_key
    /// can be coalesced so hive does not act on duplicates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
}

impl Signal {
    /// Create a new signal with the minimum required fields.
    ///
    /// Generates a UUID v4 for the `id` and stamps `timestamp` to now.
    pub fn new(
        source: impl Into<String>,
        severity: Severity,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            source: source.into(),
            severity,
            title: title.into(),
            body: body.into(),
            url: None,
            timestamp: Utc::now(),
            tags: Vec::new(),
            dedup_key: None,
        }
    }

    /// Set the URL.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the dedup key.
    pub fn with_dedup_key(mut self, key: impl Into<String>) -> Self {
        self.dedup_key = Some(key.into());
        self
    }

    /// Add tags.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_new() {
        let sig = Signal::new(
            "sentry",
            Severity::Critical,
            "Server down",
            "prod-1 is unreachable",
        );
        assert_eq!(sig.source, "sentry");
        assert_eq!(sig.severity, Severity::Critical);
        assert_eq!(sig.title, "Server down");
        assert!(!sig.id.is_empty());
        assert!(sig.url.is_none());
        assert!(sig.tags.is_empty());
        assert!(sig.dedup_key.is_none());
    }

    #[test]
    fn test_signal_builder() {
        let sig = Signal::new("github", Severity::Info, "New PR", "PR #42 opened")
            .with_url("https://github.com/org/repo/pull/42")
            .with_dedup_key("gh-pr-42")
            .with_tags(["pr", "review"]);

        assert_eq!(
            sig.url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        assert_eq!(sig.dedup_key.as_deref(), Some("gh-pr-42"));
        assert_eq!(sig.tags, vec!["pr", "review"]);
    }

    #[test]
    fn test_signal_roundtrip() {
        let sig = Signal::new("email", Severity::Warning, "Alert", "Something happened")
            .with_url("https://example.com")
            .with_tags(["ops"]);

        let json = serde_json::to_string(&sig).unwrap();
        let parsed: Signal = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, sig.id);
        assert_eq!(parsed.source, sig.source);
        assert_eq!(parsed.severity, sig.severity);
        assert_eq!(parsed.title, sig.title);
        assert_eq!(parsed.body, sig.body);
        assert_eq!(parsed.url, sig.url);
        assert_eq!(parsed.tags, sig.tags);
        assert_eq!(parsed.dedup_key, sig.dedup_key);
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(Severity::Critical.to_string(), "critical");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Info.to_string(), "info");
    }

    #[test]
    fn test_signal_deserialize_minimal() {
        // Signals should deserialize even without optional fields
        let json = r#"{
            "id": "abc-123",
            "source": "reminder",
            "severity": "info",
            "title": "Daily standup",
            "body": "Time for standup",
            "timestamp": "2025-01-15T10:00:00Z"
        }"#;

        let sig: Signal = serde_json::from_str(json).unwrap();
        assert_eq!(sig.id, "abc-123");
        assert!(sig.url.is_none());
        assert!(sig.tags.is_empty());
        assert!(sig.dedup_key.is_none());
    }
}

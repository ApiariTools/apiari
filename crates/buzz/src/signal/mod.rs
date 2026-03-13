//! Signal types — the core data model for buzz.
//!
//! Watchers produce `SignalUpdate`s, which are upserted into the SQLite store
//! as `SignalRecord`s. The coordinator reads open signals to stay aware of
//! what's happening.

pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of a signal in the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalStatus {
    Open,
    Updated,
    Resolved,
    Stale,
}

impl std::fmt::Display for SignalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Updated => write!(f, "updated"),
            Self::Resolved => write!(f, "resolved"),
            Self::Stale => write!(f, "stale"),
        }
    }
}

impl SignalStatus {
    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "open" => Self::Open,
            "updated" => Self::Updated,
            "resolved" => Self::Resolved,
            "stale" => Self::Stale,
            _ => Self::Open,
        }
    }
}

/// Severity level for a signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::Error => write!(f, "error"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

impl Severity {
    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "info" => Self::Info,
            "warning" => Self::Warning,
            "error" => Self::Error,
            "critical" => Self::Critical,
            _ => Self::Info,
        }
    }

    /// Numeric priority (higher = more severe).
    pub fn priority(&self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
            Self::Critical => 3,
        }
    }
}

/// A signal record as stored in SQLite.
#[derive(Debug, Clone)]
pub struct SignalRecord {
    pub id: i64,
    pub source: String,
    pub external_id: String,
    pub title: String,
    pub body: Option<String>,
    pub severity: Severity,
    pub status: SignalStatus,
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub metadata: Option<String>,
}

/// An update from a watcher — upserted into the store by (source, external_id).
#[derive(Debug, Clone)]
pub struct SignalUpdate {
    pub source: String,
    pub external_id: String,
    pub title: String,
    pub body: Option<String>,
    pub severity: Severity,
    pub status: SignalStatus,
    pub url: Option<String>,
    pub metadata: Option<String>,
}

impl SignalUpdate {
    /// Create a new signal update.
    pub fn new(
        source: impl Into<String>,
        external_id: impl Into<String>,
        title: impl Into<String>,
        severity: Severity,
    ) -> Self {
        Self {
            source: source.into(),
            external_id: external_id.into(),
            title: title.into(),
            body: None,
            severity,
            status: SignalStatus::Open,
            url: None,
            metadata: None,
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn with_status(mut self, status: SignalStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_display() {
        assert_eq!(Severity::Critical.to_string(), "critical");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Error.to_string(), "error");
        assert_eq!(Severity::Info.to_string(), "info");
    }

    #[test]
    fn test_severity_from_str() {
        assert_eq!(Severity::from_str_loose("critical"), Severity::Critical);
        assert_eq!(Severity::from_str_loose("warning"), Severity::Warning);
        assert_eq!(Severity::from_str_loose("unknown"), Severity::Info);
    }

    #[test]
    fn test_severity_priority() {
        assert!(Severity::Critical.priority() > Severity::Error.priority());
        assert!(Severity::Error.priority() > Severity::Warning.priority());
        assert!(Severity::Warning.priority() > Severity::Info.priority());
    }

    #[test]
    fn test_status_display() {
        assert_eq!(SignalStatus::Open.to_string(), "open");
        assert_eq!(SignalStatus::Resolved.to_string(), "resolved");
        assert_eq!(SignalStatus::Stale.to_string(), "stale");
        assert_eq!(SignalStatus::Updated.to_string(), "updated");
    }

    #[test]
    fn test_status_from_str() {
        assert_eq!(SignalStatus::from_str_loose("open"), SignalStatus::Open);
        assert_eq!(
            SignalStatus::from_str_loose("resolved"),
            SignalStatus::Resolved
        );
        assert_eq!(SignalStatus::from_str_loose("junk"), SignalStatus::Open);
    }

    #[test]
    fn test_signal_update_builder() {
        let update = SignalUpdate::new("sentry", "issue-42", "Server down", Severity::Critical)
            .with_body("prod-1 unreachable")
            .with_url("https://sentry.io/issue/42")
            .with_status(SignalStatus::Open)
            .with_metadata(r#"{"count": 5}"#);

        assert_eq!(update.source, "sentry");
        assert_eq!(update.external_id, "issue-42");
        assert_eq!(update.title, "Server down");
        assert_eq!(update.body.as_deref(), Some("prod-1 unreachable"));
        assert_eq!(update.url.as_deref(), Some("https://sentry.io/issue/42"));
        assert_eq!(update.severity, Severity::Critical);
        assert_eq!(update.status, SignalStatus::Open);
        assert!(update.metadata.is_some());
    }
}

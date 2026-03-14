//! Pipeline rules — match signals and decide actions.

use crate::buzz::signal::Severity;

/// What to do with a signal that matches a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineAction {
    /// Send immediately via Telegram.
    Notify,
    /// Collect into a batch, flush on timer.
    Batch,
    /// Silently drop (don't notify).
    Drop,
}

/// A single rule that matches signals and decides an action.
/// Rules are evaluated top-to-bottom; first match wins.
#[derive(Debug, Clone)]
pub struct PipelineRule {
    pub name: String,
    /// Match on signal source (e.g. "sentry", "swarm", "github"). None = any.
    pub source: Option<String>,
    /// Match on severity. None = any.
    pub severity: Option<Severity>,
    /// Match on external_id prefix (e.g. "swarm-pr-"). None = any.
    pub id_prefix: Option<String>,
    /// What to do when matched.
    pub action: PipelineAction,
    /// Minimum seconds between notifications for the same fingerprint.
    /// 0 = no rate limit (notify every time if new).
    pub rate_limit_secs: u64,
}

impl PipelineRule {
    /// Check if this rule matches a signal.
    pub fn matches(&self, source: &str, severity: &Severity, external_id: &str) -> bool {
        if let Some(ref s) = self.source
            && s != source
        {
            return false;
        }
        if let Some(ref sev) = self.severity
            && sev != severity
        {
            return false;
        }
        if let Some(ref prefix) = self.id_prefix
            && !external_id.starts_with(prefix)
        {
            return false;
        }
        true
    }
}

/// Default rules that replicate sensible behavior.
pub fn default_rules() -> Vec<PipelineRule> {
    vec![
        PipelineRule {
            name: "critical-immediate".into(),
            source: None,
            severity: Some(Severity::Critical),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 0,
        },
        PipelineRule {
            name: "error-immediate".into(),
            source: None,
            severity: Some(Severity::Error),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 300,
        },
        PipelineRule {
            name: "warning-immediate".into(),
            source: None,
            severity: Some(Severity::Warning),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 600,
        },
        PipelineRule {
            name: "info-batch".into(),
            source: None,
            severity: Some(Severity::Info),
            id_prefix: None,
            action: PipelineAction::Batch,
            rate_limit_secs: 0,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_any() {
        let rule = PipelineRule {
            name: "catch-all".into(),
            source: None,
            severity: None,
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 0,
        };
        assert!(rule.matches("sentry", &Severity::Error, "issue-1"));
        assert!(rule.matches("github", &Severity::Info, "pr-42"));
    }

    #[test]
    fn test_match_source() {
        let rule = PipelineRule {
            name: "sentry-only".into(),
            source: Some("sentry".into()),
            severity: None,
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 0,
        };
        assert!(rule.matches("sentry", &Severity::Error, "issue-1"));
        assert!(!rule.matches("github", &Severity::Error, "issue-1"));
    }

    #[test]
    fn test_match_severity() {
        let rule = PipelineRule {
            name: "critical".into(),
            source: None,
            severity: Some(Severity::Critical),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 0,
        };
        assert!(rule.matches("sentry", &Severity::Critical, "x"));
        assert!(!rule.matches("sentry", &Severity::Error, "x"));
    }

    #[test]
    fn test_match_id_prefix() {
        let rule = PipelineRule {
            name: "pr-signals".into(),
            source: None,
            severity: None,
            id_prefix: Some("swarm-pr-".into()),
            action: PipelineAction::Drop,
            rate_limit_secs: 0,
        };
        assert!(rule.matches("swarm", &Severity::Info, "swarm-pr-42"));
        assert!(!rule.matches("swarm", &Severity::Info, "swarm-worker-1"));
    }

    #[test]
    fn test_match_combined() {
        let rule = PipelineRule {
            name: "sentry-error".into(),
            source: Some("sentry".into()),
            severity: Some(Severity::Error),
            id_prefix: None,
            action: PipelineAction::Notify,
            rate_limit_secs: 300,
        };
        assert!(rule.matches("sentry", &Severity::Error, "issue-1"));
        assert!(!rule.matches("sentry", &Severity::Warning, "issue-1"));
        assert!(!rule.matches("github", &Severity::Error, "issue-1"));
    }

    #[test]
    fn test_default_rules() {
        let rules = default_rules();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].name, "critical-immediate");
        assert_eq!(rules[0].action, PipelineAction::Notify);
        assert_eq!(rules[0].rate_limit_secs, 0);
        assert_eq!(rules[3].name, "info-batch");
        assert_eq!(rules[3].action, PipelineAction::Batch);
    }
}

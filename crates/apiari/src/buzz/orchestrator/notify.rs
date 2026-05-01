//! Three-tier notification routing for signals.
//!
//! Each signal source maps to a `NotificationTier` that determines how
//! prominently it is surfaced to the user:
//! - **Silent** — stored in DB, visible in Activity tab only
//! - **Badge** — shows in triage sidebar, no chat message
//! - **Chat** — message appears in coordinator chat

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// How prominently a signal is surfaced to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationTier {
    /// DB only, visible in Activity tab.
    Silent,
    /// Triage sidebar, no chat message.
    Badge,
    /// Appears in coordinator chat.
    Chat,
}

/// Result of routing a signal through the notification system.
#[derive(Debug)]
pub struct NotificationRouting {
    /// The tier this signal was routed to.
    pub tier: NotificationTier,
    /// A formatted message (for Badge/Chat tiers). None for Silent.
    pub message: Option<String>,
}

/// Routes signals to their appropriate notification tier.
pub struct NotificationRouter {
    /// Per-source tier overrides from config.
    overrides: HashMap<String, NotificationTier>,
}

impl NotificationRouter {
    /// Create a new router with optional config overrides.
    pub fn new(overrides: HashMap<String, NotificationTier>) -> Self {
        Self { overrides }
    }

    /// Determine the notification tier for a signal source.
    pub fn tier_for(&self, source: &str) -> NotificationTier {
        // Check config overrides first
        if let Some(tier) = self.overrides.get(source) {
            return tier.clone();
        }
        // Built-in defaults
        default_tier(source)
    }

    /// Route a signal source + title to the appropriate tier with an optional message.
    pub fn route(&self, source: &str, title: &str, url: Option<&str>) -> NotificationRouting {
        let tier = self.tier_for(source);
        let message = match tier {
            NotificationTier::Silent => None,
            NotificationTier::Badge | NotificationTier::Chat => {
                let msg = if let Some(url) = url {
                    format!("[{source}] {title} — {url}")
                } else {
                    format!("[{source}] {title}")
                };
                Some(msg)
            }
        };
        NotificationRouting { tier, message }
    }
}

/// Built-in default tier for a signal source.
fn default_tier(source: &str) -> NotificationTier {
    match source {
        // Silent: routine events that don't need attention
        "github_ci_pass" | "github_pr_push" | "github_bot_review" | "github_merged_pr"
        | "github_release" => NotificationTier::Silent,

        // Badge: worth noting but not chat-worthy
        "github_review_queue" | "swarm_worker_spawned" | "swarm_worker_closed" => {
            NotificationTier::Badge
        }

        // Chat: needs immediate attention
        "github_ci_failure" | "swarm_pr_opened" | "swarm_worker_waiting" => NotificationTier::Chat,

        // Default: anything unrecognized gets Badge
        _ => NotificationTier::Badge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_tiers() {
        let router = NotificationRouter::new(HashMap::new());

        assert_eq!(router.tier_for("github_ci_pass"), NotificationTier::Silent);
        assert_eq!(router.tier_for("github_pr_push"), NotificationTier::Silent);
        assert_eq!(
            router.tier_for("github_bot_review"),
            NotificationTier::Silent
        );
        assert_eq!(
            router.tier_for("github_merged_pr"),
            NotificationTier::Silent
        );
        assert_eq!(router.tier_for("github_release"), NotificationTier::Silent);

        assert_eq!(
            router.tier_for("github_review_queue"),
            NotificationTier::Badge
        );
        assert_eq!(
            router.tier_for("swarm_worker_spawned"),
            NotificationTier::Badge
        );
        assert_eq!(
            router.tier_for("swarm_worker_closed"),
            NotificationTier::Badge
        );

        assert_eq!(router.tier_for("github_ci_failure"), NotificationTier::Chat);
        assert_eq!(router.tier_for("swarm_pr_opened"), NotificationTier::Chat);
        assert_eq!(
            router.tier_for("swarm_worker_waiting"),
            NotificationTier::Chat
        );
    }

    #[test]
    fn test_unknown_source_defaults_to_badge() {
        let router = NotificationRouter::new(HashMap::new());
        assert_eq!(
            router.tier_for("some_unknown_source"),
            NotificationTier::Badge
        );
    }

    #[test]
    fn test_config_override() {
        let mut overrides = HashMap::new();
        overrides.insert("github_ci_pass".to_string(), NotificationTier::Chat);
        overrides.insert("github_ci_failure".to_string(), NotificationTier::Silent);

        let router = NotificationRouter::new(overrides);

        // Overridden
        assert_eq!(router.tier_for("github_ci_pass"), NotificationTier::Chat);
        assert_eq!(
            router.tier_for("github_ci_failure"),
            NotificationTier::Silent
        );

        // Non-overridden still uses defaults
        assert_eq!(router.tier_for("github_pr_push"), NotificationTier::Silent);
    }

    #[test]
    fn test_route_silent_no_message() {
        let router = NotificationRouter::new(HashMap::new());
        let routing = router.route("github_ci_pass", "CI passed", None);
        assert_eq!(routing.tier, NotificationTier::Silent);
        assert!(routing.message.is_none());
    }

    #[test]
    fn test_route_chat_has_message() {
        let router = NotificationRouter::new(HashMap::new());
        let routing = router.route("github_ci_failure", "CI failed on PR #42", None);
        assert_eq!(routing.tier, NotificationTier::Chat);
        assert!(routing.message.is_some());
        assert!(routing.message.unwrap().contains("CI failed on PR #42"));
    }

    #[test]
    fn test_route_with_url() {
        let router = NotificationRouter::new(HashMap::new());
        let routing = router.route(
            "github_ci_failure",
            "CI failed",
            Some("https://github.com/org/repo/actions/runs/123"),
        );
        assert!(routing.message.unwrap().contains("https://"));
    }
}

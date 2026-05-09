//! Notification routing decisions — pure logic for choosing where to send
//! swarm notifications based on channel presence state.

use std::path::Path;

/// Where a notification should be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    /// Send only to the hive TUI inbox.
    UiOnly,
    /// Send only to Telegram.
    TelegramOnly,
    /// Send to both TUI inbox and Telegram (urgent notifications).
    Both,
}

/// Decide where a notification should go based on the active channel and urgency.
///
/// - Urgent notifications (stalls) always go to Telegram, plus UI if active → `Both`
/// - Non-urgent notifications go to UI when active → `UiOnly`
/// - Everything goes to Telegram when UI is inactive → `TelegramOnly`
pub fn decide(ui_active: bool, urgent: bool) -> RoutingDecision {
    match (ui_active, urgent) {
        (_, true) => RoutingDecision::Both,
        (true, false) => RoutingDecision::UiOnly,
        (false, false) => RoutingDecision::TelegramOnly,
    }
}

/// Convenience: read channel state from disk and decide routing.
///
/// Reads `.hive/channel_state.json` via `crate::presence::active_channel()`,
/// then delegates to [`decide()`].
pub fn route_for_workspace(workspace_root: &Path, urgent: bool) -> RoutingDecision {
    let active = crate::presence::active_channel(workspace_root);
    decide(active == "ui", urgent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_active_non_urgent_routes_ui_only() {
        assert_eq!(decide(true, false), RoutingDecision::UiOnly);
    }

    #[test]
    fn ui_active_urgent_routes_both() {
        assert_eq!(decide(true, true), RoutingDecision::Both);
    }

    #[test]
    fn ui_inactive_non_urgent_routes_telegram() {
        assert_eq!(decide(false, false), RoutingDecision::TelegramOnly);
    }

    #[test]
    fn ui_inactive_urgent_routes_both() {
        assert_eq!(decide(false, true), RoutingDecision::Both);
    }
}

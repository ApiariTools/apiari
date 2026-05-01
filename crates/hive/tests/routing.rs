//! Integration tests for notification routing decisions.
//!
//! Each test sets up a temp workspace directory, writes `.hive/channel_state.json`
//! as needed, then calls the routing functions and asserts the result.

use chrono::Utc;
use hive::presence::{self, ChannelEntry, ChannelState};
use hive::routing::{RoutingDecision, decide, route_for_workspace};

/// Helper: write a channel state with the given channels to the temp workspace.
fn write_channel_state(root: &std::path::Path, state: &ChannelState) {
    presence::save(root, state).expect("failed to write channel_state.json");
}

// ---------------------------------------------------------------------------
// Pure decide() tests
// ---------------------------------------------------------------------------

#[test]
fn decide_ui_active_pr_opened_is_ui_only() {
    // Non-urgent notification + UI active → UiOnly
    assert_eq!(decide(true, false), RoutingDecision::UiOnly);
}

#[test]
fn decide_ui_active_stall_is_both() {
    // Urgent notification + UI active → Both
    assert_eq!(decide(true, true), RoutingDecision::Both);
}

#[test]
fn decide_ui_inactive_pr_opened_is_telegram() {
    // Non-urgent notification + UI inactive → TelegramOnly
    assert_eq!(decide(false, false), RoutingDecision::TelegramOnly);
}

#[test]
fn decide_ui_inactive_stall_is_both() {
    // Urgent notification + UI inactive → Both (stalls always go to telegram)
    assert_eq!(decide(false, true), RoutingDecision::Both);
}

// ---------------------------------------------------------------------------
// route_for_workspace() tests — reads channel_state.json from disk
// ---------------------------------------------------------------------------

#[test]
fn route_ui_active_pr_opened_returns_ui_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Touch "ui" channel recently.
    let mut state = ChannelState::default();
    state.touch("ui");
    write_channel_state(root, &state);

    let decision = route_for_workspace(root, false);
    assert_eq!(decision, RoutingDecision::UiOnly);
}

#[test]
fn route_ui_active_agent_stalled_returns_both() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let mut state = ChannelState::default();
    state.touch("ui");
    write_channel_state(root, &state);

    let decision = route_for_workspace(root, true);
    assert_eq!(decision, RoutingDecision::Both);
}

#[test]
fn route_stale_ui_returns_telegram() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // UI channel was active 10 minutes ago — stale (>5 min threshold).
    let mut state = ChannelState::default();
    state.channels.insert(
        "ui".to_string(),
        ChannelEntry {
            last_seen: Utc::now() - chrono::Duration::seconds(600),
        },
    );
    write_channel_state(root, &state);

    let decision = route_for_workspace(root, false);
    assert_eq!(decision, RoutingDecision::TelegramOnly);
}

#[test]
fn route_missing_channel_state_returns_telegram() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // No channel_state.json at all — should default to telegram.
    let decision = route_for_workspace(root, false);
    assert_eq!(decision, RoutingDecision::TelegramOnly);
}

#[test]
fn route_telegram_more_recent_than_ui_returns_telegram() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Both channels active, but telegram is more recent.
    let mut state = ChannelState::default();
    state.channels.insert(
        "ui".to_string(),
        ChannelEntry {
            last_seen: Utc::now() - chrono::Duration::seconds(30),
        },
    );
    state.channels.insert(
        "telegram".to_string(),
        ChannelEntry {
            last_seen: Utc::now(),
        },
    );
    write_channel_state(root, &state);

    let decision = route_for_workspace(root, false);
    assert_eq!(decision, RoutingDecision::TelegramOnly);
}

#[test]
fn route_ui_more_recent_than_telegram_agent_waiting_returns_ui_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Both channels active, but UI is more recent.
    let mut state = ChannelState::default();
    state.channels.insert(
        "telegram".to_string(),
        ChannelEntry {
            last_seen: Utc::now() - chrono::Duration::seconds(60),
        },
    );
    state.touch("ui"); // most recent
    write_channel_state(root, &state);

    // AgentWaiting is non-urgent.
    let decision = route_for_workspace(root, false);
    assert_eq!(decision, RoutingDecision::UiOnly);
}

// ---------------------------------------------------------------------------
// UiEvent display tests (verify the inbox event formatting)
// ---------------------------------------------------------------------------

#[test]
fn ui_event_display_pr_opened() {
    let event = hive::ui::inbox::UiEvent::PrOpened {
        worktree_id: "hive-1".into(),
        pr_url: "https://github.com/test/1".into(),
        pr_title: "Fix bug".into(),
    };
    let display = event.display();
    assert!(display.contains("hive-1"));
    assert!(display.contains("Fix bug"));
}

#[test]
fn ui_event_display_agent_stalled() {
    let event = hive::ui::inbox::UiEvent::AgentStalled {
        worktree_id: "hive-2".into(),
    };
    let display = event.display();
    assert!(display.contains("hive-2"));
    assert!(display.contains("stalled"));
}

#[test]
fn ui_event_push_and_poll_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    hive::ui::inbox::push_event(
        root,
        &hive::ui::inbox::UiEvent::AgentCompleted {
            worktree_id: "hive-3".into(),
        },
    )
    .unwrap();

    let mut pos = 0u64;
    let events = hive::ui::inbox::poll_events(root, &mut pos);
    assert_eq!(events.len(), 1);
    assert!(pos > 0);

    // No new events after reading.
    let events2 = hive::ui::inbox::poll_events(root, &mut pos);
    assert!(events2.is_empty());
}

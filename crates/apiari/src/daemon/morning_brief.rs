//! Morning brief — scheduled daily digest via the coordinator.
//!
//! Checks once per daemon tick whether the configured local time has passed
//! and, if so, builds a prompt from open signals + worker state, invokes
//! a fresh coordinator session, and sends the result via Telegram.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use apiari_claude_sdk::SessionOptions;

use crate::buzz::channel::telegram::TelegramChannel;
use crate::buzz::channel::{Channel, OutboundMessage};
use crate::buzz::coordinator::Coordinator;
use crate::buzz::signal::{Severity, SignalRecord};
use crate::config::MorningBriefConfig;

use super::socket;

// ── Swarm state types (minimal, for reading .swarm/state.json) ──

#[derive(Debug, Deserialize)]
struct SwarmState {
    #[serde(default)]
    worktrees: Vec<WorktreeEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WorktreeEntry {
    pub id: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub pr: Option<PrInfo>,
    #[serde(default)]
    pub agent_kind: Option<String>,
    #[serde(default)]
    pub agent_session_status: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PrInfo {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

// ── Persistence ──

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedState {
    last_sent_date: Option<String>,
}

// ── Scheduler ──

/// Tracks whether the morning brief should fire today.
pub struct MorningBriefScheduler {
    time: NaiveTime,
    tz: Tz,
    state_path: PathBuf,
    last_sent_date: Option<NaiveDate>,
}

impl MorningBriefScheduler {
    /// Create a new scheduler from config, loading persisted state.
    pub fn new(config: &MorningBriefConfig, workspace: &str) -> Result<Self, String> {
        let time = NaiveTime::parse_from_str(&config.time, "%H:%M")
            .map_err(|e| format!("invalid time '{}': {e}", config.time))?;
        let tz: Tz = config
            .timezone
            .parse()
            .map_err(|e| format!("invalid timezone '{}': {e}", config.timezone))?;

        let state_path =
            crate::config::config_dir().join(format!("morning_brief_{workspace}.json"));

        let last_sent_date = load_persisted_date(&state_path);

        Ok(Self {
            time,
            tz,
            state_path,
            last_sent_date,
        })
    }

    /// Returns true if the brief should fire for the given UTC instant.
    pub fn should_fire(&self, now: DateTime<Utc>) -> bool {
        let local_now = now.with_timezone(&self.tz);
        let local_date = local_now.date_naive();
        let local_time = local_now.time();

        // Already fired today?
        if self.last_sent_date == Some(local_date) {
            return false;
        }

        // Past the configured time?
        local_time >= self.time
    }

    /// Mark today as sent and persist to disk.
    pub fn mark_sent(&mut self, now: DateTime<Utc>) {
        let local_now = now.with_timezone(&self.tz);
        let date = local_now.date_naive();
        self.last_sent_date = Some(date);
        save_persisted_date(&self.state_path, date);
    }
}

fn load_persisted_date(path: &Path) -> Option<NaiveDate> {
    let contents = std::fs::read_to_string(path).ok()?;
    let state: PersistedState = serde_json::from_str(&contents).ok()?;
    state
        .last_sent_date
        .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
}

fn save_persisted_date(path: &Path, date: NaiveDate) {
    let state = PersistedState {
        last_sent_date: Some(date.format("%Y-%m-%d").to_string()),
    };
    if let Ok(json) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::write(path, json);
    }
}

// ── Prompt builder ──

/// Build a coordinator prompt that asks for a morning brief.
pub fn build_brief_prompt(
    signals: &[SignalRecord],
    workers: &[WorktreeEntry],
    workspace: &str,
) -> String {
    let mut ctx = String::new();

    ctx.push_str(&format!(
        "You are generating the morning brief for workspace '{workspace}'.\n\n"
    ));

    // ── Signals ──
    ctx.push_str("## Open Signals\n");
    if signals.is_empty() {
        ctx.push_str("No open signals.\n\n");
    } else {
        for severity in [
            Severity::Critical,
            Severity::Error,
            Severity::Warning,
            Severity::Info,
        ] {
            let matching: Vec<_> = signals.iter().filter(|s| s.severity == severity).collect();
            if !matching.is_empty() {
                ctx.push_str(&format!("**{}** ({}):\n", severity, matching.len()));
                for s in &matching {
                    ctx.push_str(&format!("- [{}] {}", s.source, s.title));
                    if let Some(ref url) = s.url {
                        ctx.push_str(&format!(" ({url})"));
                    }
                    ctx.push('\n');
                }
            }
        }
        ctx.push('\n');
    }

    // ── Workers ──
    ctx.push_str("## Swarm Workers\n");
    if workers.is_empty() {
        ctx.push_str("No active workers.\n\n");
    } else {
        for w in workers {
            let status = w
                .agent_session_status
                .as_deref()
                .or(w.phase.as_deref())
                .unwrap_or("unknown");
            let summary = w.summary.as_deref().unwrap_or("(no summary)");
            let branch = w.branch.as_deref().unwrap_or("");
            let mut line = format!("- {} [{}]", w.id, status);
            if !branch.is_empty() {
                line.push_str(&format!(" on {branch}"));
            }
            if let Some(ref pr) = w.pr {
                let pr_state = pr.state.as_deref().unwrap_or("open");
                line.push_str(&format!(" (PR: {pr_state})"));
            }
            line.push_str(&format!(": {summary}"));
            ctx.push_str(&line);
            ctx.push('\n');
        }
        ctx.push('\n');
    }

    // ── Review queue ──
    let review_signals: Vec<_> = signals
        .iter()
        .filter(|s| s.source == "review_queue")
        .collect();
    ctx.push_str("## Review Queue\n");
    if review_signals.is_empty() {
        ctx.push_str("No review queue items.\n\n");
    } else {
        ctx.push_str(&format!("{} item(s):\n", review_signals.len()));
        for s in &review_signals {
            ctx.push_str(&format!("- {}", s.title));
            if let Some(ref url) = s.url {
                ctx.push_str(&format!(" ({url})"));
            }
            ctx.push('\n');
        }
        ctx.push('\n');
    }

    // ── Instructions ──
    ctx.push_str(
        "## Instructions\n\
         Based on the data above, generate a concise morning brief (under 300 words):\n\
         1. **Signal overview** — group by severity, summarize don't list every one\n\
         2. **Workers** — who's running, who's waiting, any PRs ready for review\n\
         3. **Review queue** — count + any urgent items\n\
         4. **Focus suggestion** — one actionable recommendation for today\n\n\
         Be direct and use bullet points.\n",
    );

    ctx
}

// ── Worker loader ──

/// Load worker entries from a swarm state.json file.
pub fn load_workers(state_path: &Path) -> Vec<WorktreeEntry> {
    let contents = match std::fs::read_to_string(state_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let state: SwarmState = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    state.worktrees
}

// ── Executor ──

/// Run the morning brief: invoke a fresh coordinator and send result via Telegram.
pub async fn execute_brief(
    model: &str,
    signals: Vec<SignalRecord>,
    swarm_state_path: Option<&Path>,
    workspace: &str,
    channel: TelegramChannel,
    chat_id: i64,
    topic_id: Option<i64>,
    socket_server: Option<Arc<socket::DaemonSocketServer>>,
) {
    let workers = swarm_state_path.map(load_workers).unwrap_or_default();
    let prompt = build_brief_prompt(&signals, &workers, workspace);

    info!("[{workspace}] generating morning brief");

    let mut coordinator = Coordinator::new(model, 5);

    let opts = SessionOptions {
        system_prompt: Some(prompt),
        max_turns: Some(5),
        model: Some(model.to_string()),
        ..Default::default()
    };

    match coordinator
        .handle_message_with_options("Generate the morning brief for today.", opts, |_| {})
        .await
    {
        Ok(response) => {
            let text = format!("\u{1f305} Morning Brief\n\n{response}");
            let msg = OutboundMessage {
                chat_id,
                text: text.clone(),
                buttons: vec![],
                topic_id,
            };
            if let Err(e) = channel.send_message(&msg).await {
                error!("[{workspace}] failed to send morning brief: {e}");
            }
            if let Some(ref server) = socket_server {
                server.broadcast_activity("system", workspace, "morning_brief", &text);
            }
            info!("[{workspace}] morning brief sent");
        }
        Err(e) => {
            error!("[{workspace}] morning brief failed: {e}");
            let msg = OutboundMessage {
                chat_id,
                text: format!("Morning brief generation failed: {e}"),
                buttons: vec![],
                topic_id,
            };
            let _ = channel.send_message(&msg).await;
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::signal::{Severity, SignalStatus};
    use chrono::TimeZone;

    fn make_signal(source: &str, title: &str, severity: Severity) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.to_string(),
            external_id: "ext-1".to_string(),
            title: title.to_string(),
            body: None,
            severity,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
        }
    }

    fn make_worker(id: &str, status: &str, summary: &str) -> WorktreeEntry {
        WorktreeEntry {
            id: id.to_string(),
            branch: Some(format!("swarm/{id}")),
            summary: Some(summary.to_string()),
            pr: None,
            agent_kind: Some("claude".to_string()),
            agent_session_status: Some(status.to_string()),
            phase: None,
        }
    }

    fn make_scheduler(time: &str, tz: &str) -> (MorningBriefScheduler, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = MorningBriefConfig {
            enabled: true,
            time: time.to_string(),
            timezone: tz.to_string(),
        };
        let mut scheduler = MorningBriefScheduler::new(&config, "test").unwrap();
        scheduler.state_path = dir.path().join("morning_brief_state.json");
        (scheduler, dir)
    }

    // ── should_fire tests ──

    #[test]
    fn test_should_fire_at_configured_time() {
        let (scheduler, _dir) = make_scheduler("09:00", "America/Chicago");
        // 09:01 CDT = 14:01 UTC (CDT = UTC-5)
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 14, 1, 0).unwrap();
        assert!(scheduler.should_fire(now));
    }

    #[test]
    fn test_should_not_fire_before_configured_time() {
        let (scheduler, _dir) = make_scheduler("09:00", "America/Chicago");
        // 08:30 CDT = 13:30 UTC
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 13, 30, 0).unwrap();
        assert!(!scheduler.should_fire(now));
    }

    #[test]
    fn test_should_not_fire_twice_same_day() {
        let (mut scheduler, _dir) = make_scheduler("09:00", "America/Chicago");
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 14, 1, 0).unwrap();
        assert!(scheduler.should_fire(now));

        scheduler.mark_sent(now);
        assert!(!scheduler.should_fire(now));

        // Even later the same day
        let later = Utc.with_ymd_and_hms(2026, 3, 16, 20, 0, 0).unwrap();
        assert!(!scheduler.should_fire(later));
    }

    #[test]
    fn test_should_fire_next_day() {
        let (mut scheduler, _dir) = make_scheduler("09:00", "America/Chicago");
        let day1 = Utc.with_ymd_and_hms(2026, 3, 16, 14, 1, 0).unwrap();
        scheduler.mark_sent(day1);

        // Next day, after 09:00 CDT
        let day2 = Utc.with_ymd_and_hms(2026, 3, 17, 14, 1, 0).unwrap();
        assert!(scheduler.should_fire(day2));
    }

    #[test]
    fn test_timezone_boundary_utc_plus() {
        // Tokyo is UTC+9. 09:00 JST = 00:00 UTC same day.
        let (scheduler, _dir) = make_scheduler("09:00", "Asia/Tokyo");
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 0, 0, 0).unwrap();
        assert!(scheduler.should_fire(now));
    }

    #[test]
    fn test_timezone_boundary_before_midnight_utc() {
        // Chicago CDT (UTC-5). 09:00 CDT = 14:00 UTC.
        // At 13:59 UTC it's 08:59 CDT — should NOT fire.
        let (scheduler, _dir) = make_scheduler("09:00", "America/Chicago");
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 13, 59, 0).unwrap();
        assert!(!scheduler.should_fire(now));

        // At 14:00 UTC it's 09:00 CDT — SHOULD fire.
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 14, 0, 0).unwrap();
        assert!(scheduler.should_fire(now));
    }

    #[test]
    fn test_state_persistence() {
        let (mut scheduler, _dir) = make_scheduler("09:00", "America/Chicago");
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 14, 1, 0).unwrap();
        scheduler.mark_sent(now);

        // Simulate loading persisted state
        let loaded = load_persisted_date(&scheduler.state_path);
        assert_eq!(loaded, Some(NaiveDate::from_ymd_opt(2026, 3, 16).unwrap()));
    }

    // ── build_brief_prompt tests ──

    #[test]
    fn test_prompt_includes_signal_counts() {
        let signals = vec![
            make_signal("sentry", "Server error", Severity::Critical),
            make_signal("sentry", "Memory warning", Severity::Warning),
            make_signal("github", "CI failure", Severity::Error),
        ];
        let prompt = build_brief_prompt(&signals, &[], "test-ws");

        assert!(prompt.contains("critical"));
        assert!(prompt.contains("(1)"));
        assert!(prompt.contains("Server error"));
        assert!(prompt.contains("CI failure"));
    }

    #[test]
    fn test_prompt_includes_worker_status() {
        let workers = vec![
            make_worker("w1", "running", "Fix auth bug"),
            make_worker("w2", "waiting", "Add feature X"),
        ];
        let prompt = build_brief_prompt(&[], &workers, "test-ws");

        assert!(prompt.contains("w1"));
        assert!(prompt.contains("[running]"));
        assert!(prompt.contains("Fix auth bug"));
        assert!(prompt.contains("w2"));
        assert!(prompt.contains("[waiting]"));
    }

    #[test]
    fn test_prompt_includes_review_queue() {
        let signals = vec![
            make_signal("review_queue", "PR #42: Fix login", Severity::Info),
            make_signal("review_queue", "PR #43: Update deps", Severity::Info),
        ];
        let prompt = build_brief_prompt(&signals, &[], "test-ws");

        assert!(prompt.contains("Review Queue"));
        assert!(prompt.contains("2 item(s)"));
        assert!(prompt.contains("PR #42"));
    }

    #[test]
    fn test_prompt_empty_state() {
        let prompt = build_brief_prompt(&[], &[], "test-ws");

        assert!(prompt.contains("No open signals"));
        assert!(prompt.contains("No active workers"));
        assert!(prompt.contains("No review queue items"));
    }

    #[test]
    fn test_prompt_includes_worker_pr_info() {
        let workers = vec![WorktreeEntry {
            id: "w1".to_string(),
            branch: Some("swarm/w1".to_string()),
            summary: Some("Fix bug".to_string()),
            pr: Some(PrInfo {
                url: Some("https://github.com/org/repo/pull/1".to_string()),
                title: Some("Fix bug PR".to_string()),
                state: Some("OPEN".to_string()),
            }),
            agent_kind: None,
            agent_session_status: Some("waiting".to_string()),
            phase: None,
        }];
        let prompt = build_brief_prompt(&[], &workers, "test-ws");

        assert!(prompt.contains("PR: OPEN"));
    }
}

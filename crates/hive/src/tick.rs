//! Unified tick engine — replaces independent pollers with a single coordinated loop.
//!
//! Each watcher implements the `Watcher` trait and fires at a configurable interval
//! (measured in ticks). The engine builds shared `TickContext` once per tick and
//! passes it to all due watchers. Watchers run concurrently via `join_all`, and
//! long-running actions are spawned as background tasks.

use crate::db::Db;
use crate::events::EventHub;
use crate::pr_review::PrReviewCache;
use crate::watcher::WatchedBot;
use async_trait::async_trait;
use std::path::PathBuf;

/// Shared state built once per tick, passed to all watchers.
#[allow(dead_code)]
pub struct TickContext {
    pub tick_number: u64,
}

/// Actions watchers can request — executed by the engine after all watchers run.
pub enum Action {
    /// Log a system message to a bot's conversation
    LogBotMessage {
        workspace: String,
        bot: String,
        message: String,
    },
    /// Dispatch a signal to a bot (runs autonomously in background)
    DispatchSignal {
        bot: WatchedBot,
        signal_source: String,
        signal_title: String,
        signal_body: String,
    },
    /// Run a proactive/scheduled bot (runs autonomously in background)
    RunBot { bot: WatchedBot },
    /// Send a message to a swarm worker
    SendToWorker {
        workspace_root: PathBuf,
        worker_id: String,
        message: String,
    },
    /// Update the Sentry cursor after signals have been dispatched
    UpdateSentryCursor {
        workspace: String,
        bot: String,
        issue_id: String,
    },
    /// Fire a due follow-up (inject action as user message to bot)
    FireFollowup {
        id: String,
        workspace: String,
        bot: String,
        action: String,
        fires_at: String,
    },
}

/// Trait that all watchers implement.
#[async_trait]
pub trait Watcher: Send {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// How often to run: 1 = every tick, 2 = every other tick, etc.
    /// Must be >= 1. A value of 0 is treated as 1.
    fn interval_ticks(&self) -> u64;

    /// Run one tick. Receives shared context, returns actions to execute.
    async fn tick(&mut self, ctx: &TickContext) -> Vec<Action>;
}

/// The engine: holds watchers, runs the loop.
pub struct TickEngine {
    watchers: Vec<Box<dyn Watcher>>,
    tick_interval_secs: u64,
    tick_number: u64,
}

impl TickEngine {
    pub fn new(tick_interval_secs: u64) -> Self {
        Self {
            watchers: Vec::new(),
            tick_interval_secs,
            tick_number: 0,
        }
    }

    pub fn add_watcher(&mut self, watcher: Box<dyn Watcher>) {
        assert!(
            watcher.interval_ticks() >= 1,
            "interval_ticks() must be >= 1, got 0 from watcher '{}'",
            watcher.name()
        );
        self.watchers.push(watcher);
    }

    /// Start the tick loop. Call this with tokio::spawn.
    pub async fn run(mut self, db: Db, events: Option<EventHub>) {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(self.tick_interval_secs));

        // First tick fires immediately (matches old poller behavior)
        interval.tick().await;

        loop {
            self.tick_number += 1;

            let ctx = TickContext {
                tick_number: self.tick_number,
            };

            // Run all due watchers concurrently
            let mut all_actions = Vec::new();
            let mut futures: Vec<_> = Vec::new();

            // Partition watchers into due/not-due. We need &mut for tick(),
            // so we iterate sequentially but the watchers themselves can do
            // concurrent I/O internally.
            for watcher in &mut self.watchers {
                let effective_interval = watcher.interval_ticks().max(1);
                if self.tick_number.is_multiple_of(effective_interval) {
                    futures.push(watcher.tick(&ctx));
                }
            }

            // Await all due watchers concurrently
            let results = futures_util::future::join_all(futures).await;
            for actions in results {
                if !actions.is_empty() {
                    all_actions.extend(actions);
                }
            }

            // Execute actions — spawn long-running ones as background tasks.
            // Collect JoinHandles so UpdateSentryCursor can await signal dispatch.
            let mut signal_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            for action in all_actions {
                if matches!(action, Action::UpdateSentryCursor { .. }) {
                    // Await all preceding signal dispatches before advancing cursor
                    for handle in signal_handles.drain(..) {
                        let _ = handle.await;
                    }
                }
                if let Some(handle) = execute_action(action, &db, events.as_ref()) {
                    signal_handles.push(handle);
                }
            }

            // Wait for next tick (wall-clock aligned, not work-time + sleep)
            interval.tick().await;
        }
    }
}

/// Execute an action. Long-running actions (signal dispatch, bot runs) are spawned
/// as background tasks to avoid blocking the tick loop. Returns the JoinHandle for
/// spawned tasks so callers can await them when ordering matters.
fn execute_action(
    action: Action,
    db: &Db,
    events: Option<&EventHub>,
) -> Option<tokio::task::JoinHandle<()>> {
    match action {
        Action::LogBotMessage {
            workspace,
            bot,
            message,
        } => {
            let _ = db.add_message(&workspace, &bot, "system", &message, None);
            None
        }
        Action::DispatchSignal {
            bot,
            signal_source,
            signal_title,
            signal_body,
        } => {
            let db = db.clone();
            Some(tokio::spawn(async move {
                let signal = crate::watcher::Signal {
                    source: signal_source,
                    title: signal_title,
                    body: signal_body,
                };
                crate::watcher::dispatch_signal(&bot, &db, &signal).await;
            }))
        }
        Action::RunBot { bot } => {
            let db = db.clone();
            Some(tokio::spawn(async move {
                let prompt = bot.proactive_prompt.as_deref().unwrap_or("");
                crate::watcher::run_proactive(&bot, &db, prompt).await;
            }))
        }
        Action::FireFollowup {
            id,
            workspace,
            bot,
            action,
            fires_at,
        } => {
            tracing::info!("[followup] firing {id} for {workspace}/{bot}");
            // Atomically mark as fired only if still pending (handles cancel race)
            match crate::followup::mark_fired_if_pending(db, &id) {
                Ok(true) => {
                    let message = format!("[Scheduled follow-up] {action}");
                    let _ = db.add_message(&workspace, &bot, "user", &message, None);
                    if let Some(hub) = events {
                        hub.send(crate::events::HiveEvent::FollowupFired {
                            id: id.clone(),
                            workspace: workspace.clone(),
                            bot: bot.clone(),
                            action: action.clone(),
                            fires_at,
                        });
                        hub.send(crate::events::HiveEvent::Message {
                            workspace: workspace.clone(),
                            bot: bot.clone(),
                            role: "user".to_string(),
                            content: message,
                        });
                    }
                }
                Ok(false) => {
                    tracing::info!("[followup] {id} was cancelled before firing, skipping");
                }
                Err(e) => {
                    tracing::warn!("[followup] failed to mark {id} as fired: {e}");
                }
            }
            None
        }
        Action::UpdateSentryCursor {
            workspace,
            bot,
            issue_id,
        } => {
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = db.set_sentry_cursor(&workspace, &bot, &issue_id, &now) {
                tracing::warn!("[sentry] failed to update cursor for {workspace}/{bot}: {e}");
            }
            None
        }
        Action::SendToWorker {
            workspace_root,
            worker_id,
            message,
        } => {
            let root = workspace_root.clone();
            Some(tokio::spawn(async move {
                let req = apiari_swarm::client::DaemonRequest::SendMessage {
                    worktree_id: worker_id.clone(),
                    message,
                };
                let result = tokio::task::spawn_blocking(move || {
                    apiari_swarm::client::send_daemon_request(&root, &req)
                })
                .await;
                match result {
                    Ok(Ok(apiari_swarm::client::DaemonResponse::Ok { .. })) => {
                        tracing::info!("[pr-feedback] Sent feedback to {}", worker_id);
                    }
                    Ok(Ok(apiari_swarm::client::DaemonResponse::Error { message })) => {
                        tracing::warn!("[pr-feedback] swarm send failed: {}", message);
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("[pr-feedback] Failed to send to swarm daemon: {e}");
                    }
                    Ok(Ok(other)) => {
                        tracing::warn!("[pr-feedback] unexpected daemon response: {:?}", other);
                    }
                    Err(e) => {
                        tracing::warn!("[pr-feedback] spawn_blocking failed: {e}");
                    }
                }
            }))
        }
    }
}

// --- Watcher implementations ---

/// Watches for GitHub signals (failing CI on open PRs).
pub struct SignalWatcher {
    bots: Vec<WatchedBot>,
}

impl SignalWatcher {
    pub fn new(bots: Vec<WatchedBot>) -> Self {
        // Only keep bots that have watch sources
        let bots = bots.into_iter().filter(|b| !b.watch.is_empty()).collect();
        Self { bots }
    }
}

#[async_trait]
impl Watcher for SignalWatcher {
    fn name(&self) -> &str {
        "signal-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        4 // every 4th tick = ~60s at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let mut actions = Vec::new();
        for bot in &self.bots {
            for source in &bot.watch {
                // "sentry" is handled by SentryWatcher
                if source.as_str() == "github"
                    && let Some(signal) = crate::watcher::poll_github(&bot.working_dir).await
                {
                    actions.push(Action::DispatchSignal {
                        bot: bot.clone(),
                        signal_source: "github".to_string(),
                        signal_title: signal.title,
                        signal_body: signal.body,
                    });
                }
            }
        }
        actions
    }
}

/// Watches for config/prompt file changes.
pub struct ConfigChangeWatcher {
    workspaces: Vec<crate::config_watcher::WatchedWorkspace>,
    hashes: std::collections::HashMap<(String, String), String>,
    initialized: bool,
}

impl ConfigChangeWatcher {
    pub fn new(workspaces: Vec<crate::config_watcher::WatchedWorkspace>) -> Self {
        Self {
            workspaces,
            hashes: std::collections::HashMap::new(),
            initialized: false,
        }
    }
}

#[async_trait]
impl Watcher for ConfigChangeWatcher {
    fn name(&self) -> &str {
        "config-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        2 // every 2nd tick = ~30s at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        // On first tick, just capture initial hashes
        if !self.initialized {
            for ws in &self.workspaces {
                for bot in &ws.bots {
                    let hash = crate::config_watcher::compute_prompt_hash(ws, bot);
                    self.hashes.insert((ws.name.clone(), bot.clone()), hash);
                }
            }
            self.initialized = true;
            return Vec::new();
        }

        let mut actions = Vec::new();
        for ws in &self.workspaces {
            for bot in &ws.bots {
                let new_hash = crate::config_watcher::compute_prompt_hash(ws, bot);
                let key = (ws.name.clone(), bot.clone());

                if let Some(old_hash) = self.hashes.get(&key)
                    && *old_hash != new_hash
                {
                    tracing::info!(
                        "[config-watcher] prompt changed for {}/{}, resetting session",
                        ws.name,
                        bot
                    );
                    actions.push(Action::LogBotMessage {
                        workspace: ws.name.clone(),
                        bot: bot.clone(),
                        message: "Session reset — bot configuration was updated.".to_string(),
                    });
                }

                self.hashes.insert(key, new_hash);
            }
        }
        actions
    }
}

/// Polls GitHub GraphQL for PR review state.
pub struct PrReviewWatcher {
    cache: PrReviewCache,
    workspace_roots: Vec<PathBuf>,
}

impl PrReviewWatcher {
    pub fn new(cache: PrReviewCache, workspace_roots: Vec<PathBuf>) -> Self {
        Self {
            cache,
            workspace_roots,
        }
    }
}

#[async_trait]
impl Watcher for PrReviewWatcher {
    fn name(&self) -> &str {
        "pr-review-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        4 // every 4th tick = ~60s at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        crate::pr_review::poll_once(&self.cache, &self.workspace_roots).await;
        Vec::new()
    }
}

/// A scheduled bot entry with its pre-parsed cron expression (if any).
struct ScheduledBot {
    bot: WatchedBot,
    cron: Option<croner::Cron>,
}

/// Checks if scheduled/proactive bots need to run.
///
/// Supports two schedule formats:
/// - `schedule = "0 9 * * 1-5"` — standard 5-field cron expression (preferred)
/// - `schedule_hours = 24` — deprecated fallback, runs every N hours
///
/// Cron expressions are parsed once at construction time.
/// Persists `last_run_at` to the DB so schedules survive restarts.
pub struct ScheduleWatcher {
    entries: Vec<ScheduledBot>,
    db: Db,
}

impl ScheduleWatcher {
    pub fn new(bots: Vec<WatchedBot>, db: Db) -> Self {
        use std::str::FromStr;
        let entries = bots
            .into_iter()
            .filter(|b| {
                (b.schedule.is_some() || b.schedule_hours.is_some()) && b.proactive_prompt.is_some()
            })
            .map(|bot| {
                let cron =
                    bot.schedule
                        .as_deref()
                        .and_then(|expr| match croner::Cron::from_str(expr) {
                            Ok(c) => Some(c),
                            Err(e) => {
                                tracing::warn!(
                                    "[schedule] invalid cron expression '{}' for {}/{}: {e}",
                                    expr,
                                    bot.workspace,
                                    bot.name
                                );
                                None
                            }
                        });
                ScheduledBot { bot, cron }
            })
            .collect();
        Self { entries, db }
    }

    /// Determine if a bot should run based on its schedule and last run time.
    fn should_run(
        entry: &ScheduledBot,
        last_run_at: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        if let Some(ref cron) = entry.cron {
            Self::should_run_cron(cron, last_run_at, now)
        } else if let Some(hours) = entry.bot.schedule_hours {
            Self::should_run_hours(hours, last_run_at, now)
        } else {
            false
        }
    }

    fn should_run_cron(
        cron: &croner::Cron,
        last_run_at: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let last_run = last_run_at.and_then(|ts| {
            chrono::DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        });

        match last_run {
            Some(last) => {
                // Find next trigger after last run — if it's <= now, fire
                cron.find_next_occurrence(&last, false)
                    .is_ok_and(|next| next <= now)
            }
            None => {
                // Never run before — check if there was a trigger in the last tick window.
                // To avoid firing immediately on first startup, only fire if a cron trigger
                // occurred in the last 60 seconds.
                let window_start = now - chrono::Duration::seconds(60);
                cron.find_next_occurrence(&window_start, false)
                    .is_ok_and(|next| next <= now)
            }
        }
    }

    fn should_run_hours(
        hours: u64,
        last_run_at: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let secs = (hours as i64).saturating_mul(3600);
        let interval = chrono::Duration::seconds(secs);
        match last_run_at.and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok()) {
            Some(last) => now - last.with_timezone(&chrono::Utc) >= interval,
            // Never run before — don't fire on startup
            None => false,
        }
    }
}

#[async_trait]
impl Watcher for ScheduleWatcher {
    fn name(&self) -> &str {
        "schedule-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        1 // every tick, but internally checks schedule
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let mut actions = Vec::new();
        let now = chrono::Utc::now();
        let now_str = now.to_rfc3339();

        for entry in &self.entries {
            let bot = &entry.bot;
            let key = format!("{}/{}", bot.workspace, bot.name);

            let last_run_at = match self.db.get_schedule_last_run(&bot.workspace, &bot.name) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("[schedule] failed to read last_run for {key}: {e}");
                    continue;
                }
            };

            if Self::should_run(entry, last_run_at.as_deref(), now) {
                tracing::info!("[schedule] firing scheduled bot {key}");
                if let Err(e) = self
                    .db
                    .set_schedule_last_run(&bot.workspace, &bot.name, &now_str)
                {
                    tracing::error!("[schedule] failed to persist last_run for {key}: {e}");
                }
                actions.push(Action::RunBot { bot: bot.clone() });
            }
        }
        actions
    }
}

/// Checks for due follow-ups every tick (~15s).
pub struct FollowupWatcher {
    db: Db,
}

impl FollowupWatcher {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Watcher for FollowupWatcher {
    fn name(&self) -> &str {
        "followup-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        1 // every tick (~15s)
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let due = crate::followup::query_due(&self.db);
        due.into_iter()
            .map(|f| Action::FireFollowup {
                id: f.id,
                workspace: f.workspace,
                bot: f.bot,
                action: f.action,
                fires_at: f.fires_at,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct MockWatcher {
        name: String,
        interval: u64,
        tick_count: Arc<AtomicU64>,
    }

    impl MockWatcher {
        fn new(name: &str, interval: u64) -> Self {
            Self {
                name: name.to_string(),
                interval,
                tick_count: Arc::new(AtomicU64::new(0)),
            }
        }

        fn count(&self) -> Arc<AtomicU64> {
            self.tick_count.clone()
        }
    }

    #[async_trait]
    impl Watcher for MockWatcher {
        fn name(&self) -> &str {
            &self.name
        }

        fn interval_ticks(&self) -> u64 {
            self.interval
        }

        async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
            self.tick_count.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }
    }

    #[test]
    fn test_tick_engine_fires_at_correct_intervals() {
        let engine = TickEngine::new(15);

        // Simulate 8 ticks and count how many times each would fire
        let intervals = [1u64, 2, 4];
        let mut fire_counts = vec![0u64; 3];
        for tick_num in 1..=8u64 {
            for (i, interval) in intervals.iter().enumerate() {
                if tick_num.is_multiple_of(*interval) {
                    fire_counts[i] += 1;
                }
            }
        }

        assert_eq!(fire_counts[0], 8); // every tick
        assert_eq!(fire_counts[1], 4); // every 2nd tick
        assert_eq!(fire_counts[2], 2); // every 4th tick
        assert_eq!(engine.tick_interval_secs, 15);
    }

    #[test]
    fn test_watcher_interval_2_only_fires_on_even_ticks() {
        let watcher = MockWatcher::new("even-only", 2);

        for tick_num in 1..=10u64 {
            let should_fire = tick_num.is_multiple_of(watcher.interval_ticks());
            if should_fire {
                assert_eq!(tick_num % 2, 0, "tick {tick_num} should be even");
            } else {
                assert_eq!(tick_num % 2, 1, "tick {tick_num} should be odd");
            }
        }
    }

    #[tokio::test]
    async fn test_mock_watcher_tick_increments() {
        let mut watcher = MockWatcher::new("test", 1);
        let ctx = TickContext { tick_number: 1 };

        assert_eq!(watcher.tick_count.load(Ordering::Relaxed), 0);
        watcher.tick(&ctx).await;
        assert_eq!(watcher.tick_count.load(Ordering::Relaxed), 1);
        watcher.tick(&ctx).await;
        assert_eq!(watcher.tick_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_tick_engine_new() {
        let engine = TickEngine::new(15);
        assert_eq!(engine.tick_interval_secs, 15);
        assert_eq!(engine.tick_number, 0);
        assert!(engine.watchers.is_empty());
    }

    #[test]
    fn test_tick_engine_add_watcher() {
        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(MockWatcher::new("test", 1)));
        assert_eq!(engine.watchers.len(), 1);
    }

    #[test]
    #[should_panic(expected = "interval_ticks() must be >= 1")]
    fn test_tick_engine_rejects_zero_interval() {
        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(MockWatcher::new("bad", 0)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_tick_engine_runs_watchers_on_schedule() {
        let every1 = MockWatcher::new("every-1", 1);
        let every2 = MockWatcher::new("every-2", 2);
        let count1 = every1.count();
        let count2 = every2.count();

        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(every1));
        engine.add_watcher(Box::new(every2));

        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("test.db")).unwrap();

        // Spawn the engine
        let handle = tokio::spawn(engine.run(db, None));

        // Advance time tick-by-tick (15s each) for 4 ticks
        for _ in 0..4 {
            tokio::time::advance(std::time::Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
        }

        // After 4 ticks: every-1 should fire 4 times, every-2 should fire 2 times
        assert_eq!(count1.load(Ordering::Relaxed), 4);
        assert_eq!(count2.load(Ordering::Relaxed), 2);

        handle.abort();
    }

    fn make_test_entry(schedule: Option<&str>, schedule_hours: Option<u64>) -> ScheduledBot {
        use std::str::FromStr;
        let bot = WatchedBot {
            workspace: "test-ws".to_string(),
            name: "test-bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "tester".to_string(),
            watch: vec![],
            working_dir: None,
            schedule: schedule.map(String::from),
            schedule_hours,
            proactive_prompt: Some("do something".to_string()),
            services: vec![],
            response_style: None,
        };
        let cron = schedule.and_then(|s| croner::Cron::from_str(s).ok());
        ScheduledBot { bot, cron }
    }

    #[test]
    fn test_cron_should_run_after_trigger() {
        // Cron: every hour at minute 0
        let entry = make_test_entry(Some("0 * * * *"), None);
        // Last run was at 08:00, now it's 09:01 — should fire (09:00 trigger passed)
        let last = "2026-04-29T08:00:00+00:00";
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-29T09:01:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(ScheduleWatcher::should_run(&entry, Some(last), now));
    }

    #[test]
    fn test_cron_should_not_run_before_trigger() {
        // Cron: every hour at minute 0
        let entry = make_test_entry(Some("0 * * * *"), None);
        // Last run was at 08:00, now it's 08:30 — should NOT fire (next is 09:00)
        let last = "2026-04-29T08:00:00+00:00";
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-29T08:30:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(!ScheduleWatcher::should_run(&entry, Some(last), now));
    }

    #[test]
    fn test_cron_weekday_only() {
        // Cron: 9am weekdays only (mon-fri)
        let entry = make_test_entry(Some("0 9 * * 1-5"), None);
        // Saturday 2026-05-02 at 09:01 — should NOT fire
        let last = "2026-05-01T09:00:00+00:00"; // Friday
        let saturday = chrono::DateTime::parse_from_rfc3339("2026-05-02T09:01:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(!ScheduleWatcher::should_run(&entry, Some(last), saturday));

        // Monday 2026-05-04 at 09:01 — should fire
        let monday = chrono::DateTime::parse_from_rfc3339("2026-05-04T09:01:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(ScheduleWatcher::should_run(&entry, Some(last), monday));
    }

    #[test]
    fn test_cron_no_last_run_within_window() {
        // Never run before, cron just triggered within 60s window
        let entry = make_test_entry(Some("0 9 * * *"), None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-29T09:00:30+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(ScheduleWatcher::should_run(&entry, None, now));
    }

    #[test]
    fn test_cron_no_last_run_outside_window() {
        // Never run before, cron triggered more than 60s ago — don't fire
        let entry = make_test_entry(Some("0 9 * * *"), None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-29T09:02:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(!ScheduleWatcher::should_run(&entry, None, now));
    }

    #[test]
    fn test_invalid_cron_expression() {
        // Invalid cron produces no parsed Cron, no schedule_hours — should not run
        let entry = make_test_entry(Some("not a cron"), None);
        let now = chrono::Utc::now();
        assert!(!ScheduleWatcher::should_run(&entry, None, now));
    }

    #[test]
    fn test_schedule_hours_fallback() {
        let entry = make_test_entry(None, Some(24));
        // Last run 25 hours ago — should fire
        let now = chrono::Utc::now();
        let last = (now - chrono::Duration::hours(25)).to_rfc3339();
        assert!(ScheduleWatcher::should_run(&entry, Some(&last), now));
    }

    #[test]
    fn test_schedule_hours_not_yet() {
        let entry = make_test_entry(None, Some(24));
        // Last run 1 hour ago — should NOT fire
        let now = chrono::Utc::now();
        let last = (now - chrono::Duration::hours(1)).to_rfc3339();
        assert!(!ScheduleWatcher::should_run(&entry, Some(&last), now));
    }

    #[test]
    fn test_schedule_hours_no_last_run() {
        // schedule_hours with no last_run — should NOT fire on startup
        let entry = make_test_entry(None, Some(24));
        let now = chrono::Utc::now();
        assert!(!ScheduleWatcher::should_run(&entry, None, now));
    }

    #[test]
    fn test_schedule_prefers_cron_over_hours() {
        // Bot has both schedule and schedule_hours — cron takes precedence
        let entry = make_test_entry(Some("0 9 * * *"), Some(1));
        // Last run 2 hours ago, but cron says 9am and it's 10am — cron should fire
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-29T10:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let last = "2026-04-29T08:00:00+00:00";
        assert!(ScheduleWatcher::should_run(&entry, Some(last), now));
    }

    #[test]
    fn test_db_schedule_last_run_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("test.db")).unwrap();

        // Initially no last_run
        assert!(db.get_schedule_last_run("ws", "bot").unwrap().is_none());

        // Set and read back
        db.set_schedule_last_run("ws", "bot", "2026-04-29T09:00:00+00:00")
            .unwrap();
        let last = db.get_schedule_last_run("ws", "bot").unwrap();
        assert_eq!(last.as_deref(), Some("2026-04-29T09:00:00+00:00"));

        // Update
        db.set_schedule_last_run("ws", "bot", "2026-04-29T10:00:00+00:00")
            .unwrap();
        let last = db.get_schedule_last_run("ws", "bot").unwrap();
        assert_eq!(last.as_deref(), Some("2026-04-29T10:00:00+00:00"));
    }

    #[tokio::test]
    async fn test_execute_action_cursor_update_writes_after_signal_handles() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Verify UpdateSentryCursor is synchronous (returns None) and writes DB
        let result = execute_action(
            Action::UpdateSentryCursor {
                workspace: "ws".to_string(),
                bot: "bot".to_string(),
                issue_id: "42".to_string(),
            },
            &db,
            None,
        );
        assert!(result.is_none(), "UpdateSentryCursor should be synchronous");
        let cursor = db.get_sentry_cursor("ws", "bot").unwrap();
        assert_eq!(cursor.as_deref(), Some("42"));
    }

    #[tokio::test]
    async fn test_execute_action_dispatch_signal_returns_handle() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("test.db")).unwrap();

        let bot = WatchedBot {
            workspace: "ws".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec![],
            working_dir: None,
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec![],
            response_style: None,
        };

        let result = execute_action(
            Action::DispatchSignal {
                bot,
                signal_source: "sentry".to_string(),
                signal_title: "test".to_string(),
                signal_body: "body".to_string(),
            },
            &db,
            None,
        );
        assert!(
            result.is_some(),
            "DispatchSignal should return a JoinHandle"
        );
    }
}

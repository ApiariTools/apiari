//! Auto bot runner — background task that checks for due auto bots every 60 seconds.
//!
//! For cron bots: parses the cron schedule and fires when the expression is due
//! since the last run. For signal bots: queries the signals table for new unprocessed
//! signals matching the bot's signal_source and fires once per unique signal.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::auto_bot::{AutoBot, AutoBotRun, AutoBotStore};

// ── Runner ─────────────────────────────────────────────────────────────

/// Checks for due auto bots every 60 seconds and fires them in background tasks.
pub struct AutoBotRunner {
    store: Arc<AutoBotStore>,
    /// Shared DB connection used to query the signals table.
    db_conn: Arc<Mutex<rusqlite::Connection>>,
    workspace: String,
    workspace_root: PathBuf,
}

impl AutoBotRunner {
    pub fn new(
        store: Arc<AutoBotStore>,
        db_conn: Arc<Mutex<rusqlite::Connection>>,
        workspace: String,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            store,
            db_conn,
            workspace,
            workspace_root,
        }
    }

    /// Spawn the runner loop. Returns immediately with a JoinHandle.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                if let Err(e) = self.tick() {
                    error!("[auto_bot_runner/{}] tick error: {e}", self.workspace);
                }
            }
        })
    }

    /// Run one tick: check which bots should fire and spawn them.
    fn tick(&self) -> color_eyre::Result<()> {
        let bots = self.store.list(&self.workspace)?;
        for bot in bots {
            if !bot.enabled {
                continue;
            }
            match bot.trigger_type.as_str() {
                "cron" => {
                    if let Err(e) = self.maybe_fire_cron_bot(&bot) {
                        warn!(
                            "[auto_bot_runner/{}] cron check failed for {}: {e}",
                            self.workspace, bot.id
                        );
                    }
                }
                "signal" => {
                    if let Err(e) = self.maybe_fire_signal_bot(&bot) {
                        warn!(
                            "[auto_bot_runner/{}] signal check failed for {}: {e}",
                            self.workspace, bot.id
                        );
                    }
                }
                other => {
                    warn!(
                        "[auto_bot_runner/{}] unknown trigger_type '{}' for bot {}",
                        self.workspace, other, bot.id
                    );
                }
            }
        }
        Ok(())
    }

    /// Check if a cron bot is due and fire it if so.
    fn maybe_fire_cron_bot(&self, bot: &AutoBot) -> color_eyre::Result<()> {
        let schedule_str = match &bot.cron_schedule {
            Some(s) => s,
            None => {
                warn!(
                    "[auto_bot_runner/{}] cron bot {} has no schedule",
                    self.workspace, bot.id
                );
                return Ok(());
            }
        };

        use std::str::FromStr as _;
        let cron = match croner::Cron::from_str(schedule_str) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "[auto_bot_runner/{}] bad cron schedule '{}' for bot {}: {e}",
                    self.workspace, schedule_str, bot.id
                );
                return Ok(());
            }
        };

        let now = Utc::now();
        // Get the previous occurrence — if it falls within the last 60 seconds (our tick
        // interval), the bot is due.
        let prev = match cron.find_previous_occurrence(&now, false) {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "[auto_bot_runner/{}] cron prev-occurrence error for bot {}: {e}",
                    self.workspace, bot.id
                );
                return Ok(());
            }
        };

        let secs_since_prev = (now - prev).num_seconds();
        if secs_since_prev > 60 {
            return Ok(()); // not due this tick
        }

        // Check if we already have a run for this cron fire time window
        // (prevent double-firing within the same minute if the daemon restarts quickly).
        let prev_str = prev.to_rfc3339();
        if self.run_exists_since(&bot.id, &prev_str)? {
            return Ok(());
        }

        info!(
            "[auto_bot_runner/{}] cron bot {} is due (prev={})",
            self.workspace, bot.id, prev_str
        );
        self.spawn_bot_run(bot.clone(), "cron".to_string());
        Ok(())
    }

    /// Check if a signal bot has new unprocessed signals and fire it for each.
    fn maybe_fire_signal_bot(&self, bot: &AutoBot) -> color_eyre::Result<()> {
        let source = match &bot.signal_source {
            Some(s) => s.clone(),
            None => {
                warn!(
                    "[auto_bot_runner/{}] signal bot {} has no signal_source",
                    self.workspace, bot.id
                );
                return Ok(());
            }
        };

        // Find the timestamp of the last run so we only look at newer signals.
        let since = match self.store.latest_run(&bot.id)? {
            Some(run) => run.started_at,
            None => "1970-01-01T00:00:00Z".to_string(),
        };

        // Query signals table for open signals from the target source created after the
        // last run. Use the shared DB connection (same DB file as SignalStore).
        let signals = self.query_new_signals(&source, &since)?;

        for (external_id,) in signals {
            let triggered_by = format!("signal:{source}:{external_id}");
            // Skip if a run with this triggered_by already exists.
            if self.run_exists_for_trigger(&bot.id, &triggered_by)? {
                continue;
            }
            info!(
                "[auto_bot_runner/{}] signal bot {} firing for {triggered_by}",
                self.workspace, bot.id
            );
            self.spawn_bot_run(bot.clone(), triggered_by);
        }

        Ok(())
    }

    /// Query the signals table for open signals from `source` created after `since`.
    fn query_new_signals(&self, source: &str, since: &str) -> color_eyre::Result<Vec<(String,)>> {
        let conn = self.db_conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT external_id FROM signals
             WHERE workspace=?1 AND source=?2
               AND status IN ('open','updated')
               AND created_at > ?3
             ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![self.workspace, source, since], |row| {
                Ok((row.get(0)?,))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Returns true if any run for this bot was started at or after `since`.
    fn run_exists_since(&self, auto_bot_id: &str, since: &str) -> color_eyre::Result<bool> {
        let conn = self.db_conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM auto_bot_runs
             WHERE auto_bot_id=?1 AND started_at >= ?2",
            rusqlite::params![auto_bot_id, since],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Returns true if a run with exactly this triggered_by value already exists.
    fn run_exists_for_trigger(
        &self,
        auto_bot_id: &str,
        triggered_by: &str,
    ) -> color_eyre::Result<bool> {
        let conn = self.db_conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM auto_bot_runs
             WHERE auto_bot_id=?1 AND triggered_by=?2",
            rusqlite::params![auto_bot_id, triggered_by],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Spawn `run_bot` in a background task.
    fn spawn_bot_run(&self, bot: AutoBot, triggered_by: String) {
        let store = Arc::clone(&self.store);
        let workspace = self.workspace.clone();
        let workspace_root = self.workspace_root.clone();

        tokio::spawn(async move {
            run_bot(bot, triggered_by, store, workspace, workspace_root).await;
        });
    }
}

// ── Bot execution ──────────────────────────────────────────────────────

/// Execute one auto bot run, using a pre-existing run record.
///
/// Used by the HTTP trigger endpoint which inserts the run record before calling this.
pub async fn run_bot_external(
    bot: AutoBot,
    triggered_by: String,
    run_id: String,
    store: Arc<AutoBotStore>,
    workspace: String,
    workspace_root: PathBuf,
) {
    info!(
        "[auto_bot_runner/{}] bot {} external run {} (triggered_by={triggered_by})",
        workspace, bot.id, run_id
    );

    let full_prompt = format!(
        "{}\n\nWorkspace: {}\nRoot: {}",
        bot.prompt,
        workspace,
        workspace_root.display()
    );

    let (outcome, summary, worker_id) =
        execute_bot_prompt(&bot, &full_prompt, &workspace, &workspace_root).await;

    info!(
        "[auto_bot_runner/{}] bot {} external run {} finished: outcome={outcome}",
        workspace, bot.id, run_id
    );

    if let Err(e) = store.finish_run(&run_id, &outcome, &summary, worker_id.as_deref()) {
        error!(
            "[auto_bot_runner/{}] failed to finish external run {run_id}: {e}",
            workspace
        );
    }
}

/// Execute one auto bot run.
///
/// 1. Inserts a run record (no finished_at yet).
/// 2. Runs the bot's prompt via claude CLI.
/// 3. Parses output for `dispatch_worker:` directives.
/// 4. Determines outcome and calls finish_run().
async fn run_bot(
    bot: AutoBot,
    triggered_by: String,
    store: Arc<AutoBotStore>,
    workspace: String,
    workspace_root: PathBuf,
) {
    let run_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let run = AutoBotRun {
        id: run_id.clone(),
        auto_bot_id: bot.id.clone(),
        workspace: workspace.clone(),
        triggered_by: triggered_by.clone(),
        started_at: now,
        finished_at: None,
        outcome: None,
        summary: None,
        worker_id: None,
    };

    if let Err(e) = store.insert_run(&run) {
        error!(
            "[auto_bot_runner/{}] failed to insert run for bot {}: {e}",
            workspace, bot.id
        );
        return;
    }

    info!(
        "[auto_bot_runner/{}] bot {} starting run {} (triggered_by={triggered_by})",
        workspace, bot.id, run_id
    );

    // Build the full prompt
    let full_prompt = format!(
        "{}\n\nWorkspace: {}\nRoot: {}",
        bot.prompt,
        workspace,
        workspace_root.display()
    );

    // Run claude CLI
    let (outcome, summary, worker_id) =
        execute_bot_prompt(&bot, &full_prompt, &workspace, &workspace_root).await;

    info!(
        "[auto_bot_runner/{}] bot {} run {} finished: outcome={outcome}",
        workspace, bot.id, run_id
    );

    if let Err(e) = store.finish_run(&run_id, &outcome, &summary, worker_id.as_deref()) {
        error!(
            "[auto_bot_runner/{}] failed to finish run {run_id}: {e}",
            workspace
        );
    }
}

/// Execute the bot prompt via the claude CLI and return (outcome, summary, worker_id).
async fn execute_bot_prompt(
    bot: &AutoBot,
    full_prompt: &str,
    workspace: &str,
    workspace_root: &PathBuf,
) -> (String, String, Option<String>) {
    use tokio::process::Command;

    let output = Command::new("claude")
        .arg("--print")
        .arg("--max-turns")
        .arg("7")
        .arg(full_prompt)
        .current_dir(workspace_root)
        .output()
        .await;

    let raw_output = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            warn!(
                "[auto_bot_runner/{workspace}] bot {} claude exited non-zero: {stderr}",
                bot.id
            );
            return ("error".to_string(), stderr, None);
        }
        Err(e) => {
            warn!(
                "[auto_bot_runner/{workspace}] bot {} failed to run claude: {e}",
                bot.id
            );
            return ("error".to_string(), e.to_string(), None);
        }
    };

    // Parse output for dispatch_worker: directives.
    // Format: a line like `dispatch_worker: <goal here>`
    let mut worker_id: Option<String> = None;
    for line in raw_output.lines() {
        let trimmed = line.trim();
        if let Some(goal) = trimmed.strip_prefix("dispatch_worker:") {
            let goal = goal.trim().to_string();
            match try_dispatch_worker(bot, &goal, workspace, workspace_root).await {
                Ok(id) => {
                    worker_id = Some(id);
                    break;
                }
                Err(e) => {
                    warn!(
                        "[auto_bot_runner/{workspace}] bot {} dispatch_worker failed: {e}",
                        bot.id
                    );
                }
            }
        }
    }

    let outcome = if worker_id.is_some() {
        "dispatched_worker"
    } else if raw_output.trim().is_empty() {
        "noise"
    } else {
        "notified"
    };

    (
        outcome.to_string(),
        raw_output.trim().to_string(),
        worker_id,
    )
}

/// Attempt to dispatch a swarm worker with the given goal.
async fn try_dispatch_worker(
    bot: &AutoBot,
    goal: &str,
    workspace: &str,
    workspace_root: &PathBuf,
) -> color_eyre::Result<String> {
    use tokio::process::Command;

    let worker_id = format!("autobot-{}", Uuid::new_v4());

    // Write a brief prompt to a temp file so swarm can read it.
    let tmp_path = std::env::temp_dir().join(format!("autobot-brief-{worker_id}.txt"));
    tokio::fs::write(
        &tmp_path,
        format!(
            "Auto bot dispatch from '{bot_name}':\n\n{goal}\n\nWorkspace: {workspace}",
            bot_name = bot.name
        ),
    )
    .await?;

    let status = Command::new("swarm")
        .arg("--dir")
        .arg(workspace_root)
        .arg("create")
        .arg("--prompt-file")
        .arg(&tmp_path)
        .status()
        .await?;

    // Clean up temp file regardless.
    let _ = tokio::fs::remove_file(&tmp_path).await;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!("swarm create exited non-zero"));
    }

    Ok(worker_id)
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::auto_bot::{AutoBot, AutoBotStore};
    use chrono::Utc;

    /// Create a shared in-memory connection with all needed tables, and both
    /// an AutoBotStore and a db_conn clone that point at the same DB.
    fn make_shared_setup() -> (Arc<AutoBotStore>, Arc<Mutex<rusqlite::Connection>>) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS signals (
                 id INTEGER PRIMARY KEY,
                 workspace TEXT NOT NULL,
                 source TEXT NOT NULL,
                 external_id TEXT NOT NULL,
                 title TEXT NOT NULL,
                 body TEXT,
                 severity TEXT NOT NULL DEFAULT 'info',
                 status TEXT NOT NULL DEFAULT 'open',
                 url TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 resolved_at TEXT,
                 metadata TEXT,
                 UNIQUE(workspace, source, external_id)
             );
             CREATE TABLE IF NOT EXISTS auto_bots (
                 id TEXT PRIMARY KEY,
                 workspace TEXT NOT NULL,
                 name TEXT NOT NULL,
                 color TEXT NOT NULL DEFAULT '#f5c542',
                 trigger_type TEXT NOT NULL,
                 cron_schedule TEXT,
                 signal_source TEXT,
                 signal_filter TEXT,
                 prompt TEXT NOT NULL,
                 provider TEXT NOT NULL DEFAULT 'claude',
                 model TEXT,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS auto_bot_runs (
                 id TEXT PRIMARY KEY,
                 auto_bot_id TEXT NOT NULL REFERENCES auto_bots(id),
                 workspace TEXT NOT NULL,
                 triggered_by TEXT NOT NULL,
                 started_at TEXT NOT NULL,
                 finished_at TEXT,
                 outcome TEXT,
                 summary TEXT,
                 worker_id TEXT
             );",
        )
        .unwrap();
        let conn = Arc::new(Mutex::new(conn));
        let store = Arc::new(AutoBotStore::new(Arc::clone(&conn)));
        (store, conn)
    }

    fn make_runner(
        store: Arc<AutoBotStore>,
        db_conn: Arc<Mutex<rusqlite::Connection>>,
    ) -> AutoBotRunner {
        AutoBotRunner::new(
            store,
            db_conn,
            "test".to_string(),
            PathBuf::from("/tmp/test-workspace"),
        )
    }

    fn insert_signal(
        conn: &Arc<Mutex<rusqlite::Connection>>,
        workspace: &str,
        source: &str,
        ext_id: &str,
        created_at: &str,
    ) {
        let c = conn.lock().unwrap();
        c.execute(
            "INSERT OR IGNORE INTO signals (workspace, source, external_id, title, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'Test signal', 'open', ?4, ?4)",
            rusqlite::params![workspace, source, ext_id, created_at],
        ).unwrap();
    }

    // ── Cron fire detection tests ──────────────────────────────────────

    #[test]
    fn test_cron_run_exists_since_false_when_empty() {
        let (store, db) = make_shared_setup();
        let runner = make_runner(store, db);
        let now = Utc::now().to_rfc3339();
        assert!(!runner.run_exists_since("nobot", &now).unwrap());
    }

    #[test]
    fn test_cron_run_exists_since_true_after_insert() {
        let (store, db) = make_shared_setup();

        let bot = AutoBot {
            id: "b1".to_string(),
            workspace: "test".to_string(),
            name: "B1".to_string(),
            color: "#fff".to_string(),
            trigger_type: "cron".to_string(),
            cron_schedule: Some("0 * * * *".to_string()),
            signal_source: None,
            signal_filter: None,
            prompt: "do stuff".to_string(),
            provider: "claude".to_string(),
            model: None,
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            status: String::new(),
        };
        store.upsert(&bot).unwrap();

        let run = crate::buzz::auto_bot::AutoBotRun {
            id: "r1".to_string(),
            auto_bot_id: "b1".to_string(),
            workspace: "test".to_string(),
            triggered_by: "cron".to_string(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            outcome: None,
            summary: None,
            worker_id: None,
        };
        store.insert_run(&run).unwrap();

        let runner = make_runner(Arc::clone(&store), Arc::clone(&db));
        let past = (Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
        assert!(runner.run_exists_since("b1", &past).unwrap());
    }

    // ── Signal matching tests ──────────────────────────────────────────

    #[test]
    fn test_signal_query_returns_new_signals() {
        let (store, db) = make_shared_setup();

        let five_min_ago = (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        insert_signal(&db, "test", "github", "pr-101", &Utc::now().to_rfc3339());

        let runner = make_runner(store, db);
        let signals = runner.query_new_signals("github", &five_min_ago).unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].0, "pr-101");
    }

    #[test]
    fn test_signal_query_ignores_old_signals() {
        let (store, db) = make_shared_setup();

        let very_old = "2000-01-01T00:00:00Z".to_string();
        let since_yesterday = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        // Signal created before since_yesterday
        insert_signal(&db, "test", "github", "pr-old", &very_old);

        let runner = make_runner(store, db);
        let signals = runner
            .query_new_signals("github", &since_yesterday)
            .unwrap();
        assert_eq!(signals.len(), 0);
    }

    #[test]
    fn test_signal_query_ignores_different_source() {
        let (store, db) = make_shared_setup();

        let five_min_ago = (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        insert_signal(&db, "test", "sentry", "sentry-1", &Utc::now().to_rfc3339());

        let runner = make_runner(store, db);
        let signals = runner.query_new_signals("github", &five_min_ago).unwrap();
        assert_eq!(signals.len(), 0);
    }

    #[test]
    fn test_run_exists_for_trigger_false_when_not_run() {
        let (store, db) = make_shared_setup();
        let runner = make_runner(store, db);
        assert!(
            !runner
                .run_exists_for_trigger("b1", "signal:github:pr-1")
                .unwrap()
        );
    }

    #[test]
    fn test_run_exists_for_trigger_true_after_insert() {
        let (store, db) = make_shared_setup();

        let bot = AutoBot {
            id: "b2".to_string(),
            workspace: "test".to_string(),
            name: "B2".to_string(),
            color: "#fff".to_string(),
            trigger_type: "signal".to_string(),
            cron_schedule: None,
            signal_source: Some("github".to_string()),
            signal_filter: None,
            prompt: "check signal".to_string(),
            provider: "claude".to_string(),
            model: None,
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            status: String::new(),
        };
        store.upsert(&bot).unwrap();

        let run = crate::buzz::auto_bot::AutoBotRun {
            id: "r2".to_string(),
            auto_bot_id: "b2".to_string(),
            workspace: "test".to_string(),
            triggered_by: "signal:github:pr-99".to_string(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            outcome: None,
            summary: None,
            worker_id: None,
        };
        store.insert_run(&run).unwrap();

        let runner = make_runner(Arc::clone(&store), Arc::clone(&db));
        assert!(
            runner
                .run_exists_for_trigger("b2", "signal:github:pr-99")
                .unwrap()
        );
        assert!(
            !runner
                .run_exists_for_trigger("b2", "signal:github:pr-100")
                .unwrap()
        );
    }

    // ── Cron schedule validation ───────────────────────────────────────

    #[test]
    fn test_valid_cron_schedule_parses() {
        use std::str::FromStr as _;
        let result = croner::Cron::from_str("0 9 * * 1-5");
        assert!(result.is_ok(), "standard cron schedule should parse");
    }

    #[test]
    fn test_invalid_cron_schedule_fails() {
        use std::str::FromStr as _;
        let result = croner::Cron::from_str("not-a-cron");
        assert!(
            result.is_err(),
            "invalid cron schedule should fail to parse"
        );
    }

    #[test]
    fn test_cron_prev_occurrence_within_tick() {
        use std::str::FromStr as _;
        // "every minute" — there should always be a prev occurrence within 60s
        let cron = croner::Cron::from_str("* * * * *").unwrap();
        let now = Utc::now();
        let prev = cron.find_previous_occurrence(&now, false).unwrap();
        let secs = (now - prev).num_seconds();
        assert!(
            secs <= 60,
            "prev occurrence should be within last 60s for '* * * * *'"
        );
    }
}

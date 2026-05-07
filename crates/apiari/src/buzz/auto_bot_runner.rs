//! Auto bot runner — background task that checks for due auto bots every 60 seconds.
//!
//! For cron bots: parses the cron schedule and fires when the expression is due
//! since the last run. For signal bots: queries the signals table for new unprocessed
//! signals matching the bot's signal_source and fires once per unique signal.
//!
//! ## Execution model
//!
//! Each bot run uses the full Coordinator infrastructure: the bot gets the same
//! system prompt as the main bee (open signals, memory, skills), the same tool
//! access (Bash, Read, etc.), and the same action-marker protocol
//! (`[DISMISS: id]`, `[FIX: desc]`, `[TASK: title]`, etc.). This means auto bots
//! can investigate, dispatch workers, and manage signals autonomously.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::Utc;
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::auto_bot::{AutoBot, AutoBotRun, AutoBotStore};
use crate::buzz::{
    coordinator::actions::{BeeAction, parse_actions},
    signal::{Severity, SignalUpdate, store::SignalStore},
};

// ── Runner ─────────────────────────────────────────────────────────────

/// Checks for due auto bots every 60 seconds and fires them in background tasks.
pub struct AutoBotRunner {
    store: Arc<AutoBotStore>,
    /// Shared DB connection used to query the signals table.
    db_conn: Arc<Mutex<rusqlite::Connection>>,
    workspace: String,
    workspace_root: PathBuf,
    /// DB file path — needed to open SignalStore and TaskStore per run.
    db_path: PathBuf,
    /// Workspace config — used to build the coordinator's skills prompt.
    workspace_config: Option<crate::config::WorkspaceConfig>,
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
            db_path: PathBuf::new(),
            workspace_config: None,
        }
    }

    /// Attach workspace config so bot runs use the full coordinator infrastructure.
    pub fn with_config(mut self, db_path: PathBuf, config: crate::config::WorkspaceConfig) -> Self {
        self.db_path = db_path;
        self.workspace_config = Some(config);
        self
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
        let db_path = self.db_path.clone();
        let workspace_config = self.workspace_config.clone();

        tokio::spawn(async move {
            run_bot(
                bot,
                triggered_by,
                store,
                workspace,
                workspace_root,
                db_path,
                workspace_config,
            )
            .await;
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
    db_path: PathBuf,
    workspace_config: Option<crate::config::WorkspaceConfig>,
) {
    info!(
        "[auto_bot_runner/{}] bot {} external run {} (triggered_by={triggered_by})",
        workspace, bot.id, run_id
    );

    let (outcome, summary, worker_id) = execute_bot_prompt(
        &bot,
        &bot.prompt,
        &workspace,
        &workspace_root,
        &db_path,
        workspace_config.as_ref(),
    )
    .await;

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
async fn run_bot(
    bot: AutoBot,
    triggered_by: String,
    store: Arc<AutoBotStore>,
    workspace: String,
    workspace_root: PathBuf,
    db_path: PathBuf,
    workspace_config: Option<crate::config::WorkspaceConfig>,
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

    let (outcome, summary, worker_id) = execute_bot_prompt(
        &bot,
        &bot.prompt,
        &workspace,
        &workspace_root,
        &db_path,
        workspace_config.as_ref(),
    )
    .await;

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

/// Run the bot prompt through the coordinator (or fall back to plain claude --print).
///
/// Returns `(outcome, summary, worker_id)` where outcome is one of:
/// - `"dispatched_worker"` — coordinator dispatched a swarm worker via Bash
/// - `"notified"` — coordinator produced a response (signal actions may have fired)
/// - `"noise"` — empty response
/// - `"error"` — coordinator failed
async fn execute_bot_prompt(
    bot: &AutoBot,
    prompt: &str,
    workspace: &str,
    workspace_root: &Path,
    db_path: &Path,
    workspace_config: Option<&crate::config::WorkspaceConfig>,
) -> (String, String, Option<String>) {
    if let Some(config) = workspace_config
        && !db_path.as_os_str().is_empty()
    {
        return execute_with_coordinator(bot, prompt, workspace, workspace_root, db_path, config)
            .await;
    }
    // Fallback: no config available (e.g. tests, legacy callers).
    execute_plain(bot, prompt, workspace, workspace_root).await
}

/// Run the bot through the full Coordinator: signal context, tools, action markers.
///
/// Uses the two-phase prepare_dispatch / dispatch_message API so that
/// `SignalStore` (which contains a non-Send `rusqlite::Connection`) stays on the
/// blocking thread and never crosses an await boundary.
async fn execute_with_coordinator(
    bot: &AutoBot,
    prompt: &str,
    workspace: &str,
    workspace_root: &Path,
    db_path: &Path,
    config: &crate::config::WorkspaceConfig,
) -> (String, String, Option<String>) {
    use crate::buzz::coordinator::{
        Coordinator, CoordinatorEvent,
        skills::{
            build_skills_prompt, default_coordinator_disallowed_tools, default_coordinator_tools,
        },
    };

    // Build skills context and prompt from workspace config.
    let skill_ctx = crate::config::build_skill_context(workspace, config);
    let extra_context = build_skills_prompt(&skill_ctx);

    // Use bot's own model if set, otherwise fall back to workspace coordinator model.
    let model = bot
        .model
        .clone()
        .unwrap_or_else(|| config.coordinator.model.clone());

    let mut coordinator = Coordinator::new(&model, 10);
    coordinator.set_extra_context(extra_context);
    coordinator.set_tools(default_coordinator_tools());
    coordinator.set_disallowed_tools(default_coordinator_disallowed_tools());
    coordinator.set_working_dir(workspace_root.to_path_buf());
    if let Some(preamble) = &config.coordinator.prompt {
        coordinator.set_prompt_preamble(preamble.clone());
    }

    info!(
        "[auto_bot_runner/{workspace}] bot {} running via coordinator (model={model})",
        bot.id
    );

    // Phase 1 (sync): open SignalStore and build session options.
    // block_in_place lets us run blocking DB code without crossing a Send boundary.
    let db_path_owned = db_path.to_path_buf();
    let workspace_owned = workspace.to_string();
    let bundle_result = tokio::task::block_in_place(|| {
        let signal_store = SignalStore::open(&db_path_owned, &workspace_owned)?;
        coordinator.prepare_dispatch(&signal_store)
    });

    let bundle = match bundle_result {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "[auto_bot_runner/{workspace}] bot {} failed to prepare coordinator session: {e}",
                bot.id
            );
            return execute_plain(bot, prompt, workspace, workspace_root).await;
        }
    };

    // Phase 2 (async): run the LLM turn — no SignalStore reference needed.
    let mut response_buf = String::new();
    let result = coordinator
        .dispatch_message(prompt, bundle, &[], |event| {
            if let CoordinatorEvent::Token(tok) = event {
                response_buf.push_str(&tok);
            }
        })
        .await;

    match result {
        Ok(full_response) => {
            // full_response is the complete assembled text returned by dispatch_message.
            let response = if full_response.is_empty() {
                response_buf
            } else {
                full_response
            };
            let actions = parse_actions(&response);

            // Execute actions against the signal/task stores (blocking DB work).
            let db_path_owned2 = db_path.to_path_buf();
            let workspace_owned2 = workspace.to_string();
            let bot_id = bot.id.clone();
            let bot_name = bot.name.clone();
            let actions_clone = actions.clone();
            tokio::task::block_in_place(|| {
                if let Ok(signal_store) = SignalStore::open(&db_path_owned2, &workspace_owned2) {
                    tokio::runtime::Handle::current().block_on(execute_actions(
                        &actions_clone,
                        &signal_store,
                        &workspace_owned2,
                        workspace_root,
                        &bot_id,
                        &bot_name,
                    ));
                }
            });

            let outcome = if response.trim().is_empty() {
                "noise"
            } else {
                "notified"
            };

            (outcome.to_string(), response.trim().to_string(), None)
        }
        Err(e) => {
            warn!(
                "[auto_bot_runner/{workspace}] bot {} coordinator error: {e}",
                bot.id
            );
            ("error".to_string(), e.to_string(), None)
        }
    }
}

/// Execute parsed BeeAction markers: dismiss/snooze signals, create tasks, etc.
///
/// Returns a worker_id if a worker was dispatched (via coordinator Bash call, not directly
/// from action markers — worker dispatch goes through the coordinator's Bash tool use).
async fn execute_actions(
    actions: &[BeeAction],
    signal_store: &SignalStore,
    workspace: &str,
    _workspace_root: &Path,
    bot_id: &str,
    bot_name: &str,
) -> Option<String> {
    for action in actions {
        match action {
            BeeAction::Dismiss { signal_id } => match signal_store.resolve_signal(*signal_id) {
                Ok(()) => {
                    info!("[auto_bot_runner/{workspace}] bot {bot_id} dismissed signal {signal_id}")
                }
                Err(e) => warn!(
                    "[auto_bot_runner/{workspace}] bot {bot_id} failed to dismiss signal {signal_id}: {e}"
                ),
            },
            BeeAction::Snooze { signal_id, hours } => {
                let until = Utc::now() + chrono::Duration::hours(*hours as i64);
                match signal_store.snooze_signal(*signal_id, until) {
                    Ok(()) => info!(
                        "[auto_bot_runner/{workspace}] bot {bot_id} snoozed signal {signal_id} for {hours}h"
                    ),
                    Err(e) => warn!(
                        "[auto_bot_runner/{workspace}] bot {bot_id} failed to snooze signal {signal_id}: {e}"
                    ),
                }
            }
            BeeAction::Escalate { message } => {
                let external_id = format!(
                    "escalation-{}-{}",
                    bot_name.to_lowercase().replace(' ', "_"),
                    Utc::now().timestamp_millis()
                );
                let update =
                    SignalUpdate::new("escalation", &external_id, message, Severity::Critical);
                match signal_store.upsert_signal(&update) {
                    Ok((id, _)) => info!(
                        "[auto_bot_runner/{workspace}] bot {bot_id} escalated → signal id={id}: {message}"
                    ),
                    Err(e) => {
                        warn!("[auto_bot_runner/{workspace}] bot {bot_id} failed to escalate: {e}")
                    }
                }
            }
            BeeAction::Fix { description } => {
                let fingerprint = normalize_fingerprint(description);
                let source = format!("bee_{}", bot_name.to_lowercase().replace(' ', "_"));
                let external_id = format!("fix-{bot_id}-{fingerprint}");
                let update = SignalUpdate::new(&source, &external_id, description, Severity::Error);
                match signal_store.upsert_signal(&update) {
                    Ok((id, _)) => info!(
                        "[auto_bot_runner/{workspace}] bot {bot_id} fix signal id={id}: {description}"
                    ),
                    Err(e) => warn!(
                        "[auto_bot_runner/{workspace}] bot {bot_id} failed to create fix signal: {e}"
                    ),
                }
            }
            BeeAction::Task { title } => {
                if let Ok(task_store) =
                    crate::buzz::task::store::TaskStore::open(signal_store.db_path())
                {
                    let task = crate::buzz::task::Task {
                        id: Uuid::new_v4().to_string(),
                        workspace: workspace.to_string(),
                        title: title.clone(),
                        stage: crate::buzz::task::TaskStage::Triage,
                        source: Some(format!("bot_{bot_id}")),
                        source_url: None,
                        worker_id: None,
                        pr_url: None,
                        pr_number: None,
                        repo: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        resolved_at: None,
                        metadata: serde_json::json!({"bot": bot_id}),
                    };
                    match task_store.create_task(&task) {
                        Ok(()) => info!(
                            "[auto_bot_runner/{workspace}] bot {bot_id} created task: {title}"
                        ),
                        Err(e) => warn!(
                            "[auto_bot_runner/{workspace}] bot {bot_id} failed to create task: {e}"
                        ),
                    }
                }
            }
            // Canvas, Research, Followup — logged but not acted on here.
            BeeAction::Canvas { .. } | BeeAction::Research { .. } | BeeAction::Followup { .. } => {}
        }
    }

    // Worker dispatch happens via the coordinator's Bash tool use (swarm create).
    // We don't parse a separate worker_id from action markers.
    None
}

/// Normalize a string into a short stable fingerprint for deduplication.
fn normalize_fingerprint(s: &str) -> String {
    let lower = s.to_lowercase();
    let alphanum: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let deduped: String = alphanum
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    deduped.chars().take(40).collect()
}

/// Fallback: run `claude --print` with no tools, no signal context.
///
/// Used when workspace config is unavailable (tests, legacy callers).
async fn execute_plain(
    bot: &AutoBot,
    prompt: &str,
    workspace: &str,
    workspace_root: &Path,
) -> (String, String, Option<String>) {
    use tokio::process::Command;

    let full_prompt = format!(
        "{prompt}\n\nWorkspace: {workspace}\nRoot: {}",
        workspace_root.display()
    );

    let output = Command::new("claude")
        .arg("--print")
        .arg("--max-turns")
        .arg("7")
        .arg(&full_prompt)
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

    let outcome = if raw_output.trim().is_empty() {
        "noise"
    } else {
        "notified"
    };

    (outcome.to_string(), raw_output.trim().to_string(), None)
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

    // ── Fingerprint normalization ──────────────────────────────────────

    #[test]
    fn test_normalize_fingerprint_basic() {
        assert_eq!(
            normalize_fingerprint("Fix the login bug"),
            "fix-the-login-bug"
        );
    }

    #[test]
    fn test_normalize_fingerprint_dedupes_separators() {
        assert_eq!(
            normalize_fingerprint("multiple   spaces & symbols!!"),
            "multiple-spaces-symbols"
        );
    }

    #[test]
    fn test_normalize_fingerprint_truncates() {
        let long = "a".repeat(100);
        assert_eq!(normalize_fingerprint(&long).len(), 40);
    }
}

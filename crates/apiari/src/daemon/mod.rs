//! Multi-workspace daemon — event loop for all workspaces.
//!
//! Discovers workspace configs, builds per-workspace watcher registries,
//! shares Telegram connections by bot_token, and routes messages by (chat_id, topic_id).

pub mod morning_brief;
pub mod socket;

use color_eyre::eyre::{Result, WrapErr};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::git_safety::GitSafetyHooks;

use crate::buzz::channel::telegram::TelegramChannel;
use crate::buzz::channel::{Channel, ChannelEvent, OutboundMessage};
use crate::buzz::conversation::ConversationStore;
use crate::buzz::coordinator::prompt::format_signal_summary;
use crate::buzz::coordinator::skills::{
    SkillContext, build_skills_prompt, default_coordinator_disallowed_tools,
    default_coordinator_tools,
};
use crate::buzz::coordinator::{Coordinator, CoordinatorEvent};
use crate::buzz::daemon::config as buzz_daemon_config;
use crate::buzz::pipeline::Pipeline;
use crate::buzz::signal::store::SignalStore;
use crate::buzz::watcher::WatcherRegistry;
use crate::buzz::watcher::email::EmailWatcher;
use crate::buzz::watcher::github::GithubWatcher;
use crate::buzz::watcher::linear::LinearWatcher;
use crate::buzz::watcher::notion::NotionWatcher;
use crate::buzz::watcher::review_queue::ReviewQueueWatcher;
use crate::buzz::watcher::sentry::SentryWatcher;
use crate::buzz::watcher::swarm::SwarmWatcher;

use crate::config::{
    self, Workspace, WorkspaceConfig, db_path, log_path, pid_path, to_buzz_config,
    to_pipeline_rules,
};

/// Why the event loop exited.
enum ExitReason {
    /// Clean shutdown (Ctrl+C).
    Shutdown,
    /// Error — daemon should restart.
    Error(color_eyre::eyre::Error),
    /// Self-update — exec the new binary.
    Restart,
}

/// A workspace slot in the daemon — holds per-workspace state.
struct WorkspaceSlot {
    name: String,
    config: WorkspaceConfig,
    registry: WatcherRegistry,
    coord_tx: mpsc::UnboundedSender<CoordinatorJob>,
    store: SignalStore,
    pipeline: Pipeline,
    morning_brief: Option<morning_brief::MorningBriefScheduler>,
}

/// Key for routing Telegram messages to workspaces.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct RouteKey {
    chat_id: i64,
    topic_id: Option<i64>,
}

/// A job to be processed by a workspace's dedicated coordinator task.
enum CoordinatorJob {
    /// Handle a Telegram user message.
    TelegramMessage {
        text: String,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        channel: TelegramChannel,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        slot_name: String,
    },
    /// Handle a TUI chat message with streaming tokens.
    TuiChat {
        text: String,
        responder: mpsc::UnboundedSender<socket::DaemonResponse>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        ws_name: String,
    },
    /// Reset the coordinator session.
    ResetSession,
    /// Compact the coordinator session (reset + respond with confirmation).
    Compact {
        /// If Some, send confirmation via Telegram.
        telegram: Option<(TelegramChannel, i64, Option<i64>)>,
        /// If Some, send confirmation via TUI responder.
        tui_responder: Option<mpsc::UnboundedSender<socket::DaemonResponse>>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        slot_name: String,
    },
    /// Coordinator follow-through triggered by a signal hook.
    SignalFollowThrough {
        signals: Vec<String>,
        source: String,
        prompt_override: Option<String>,
        queued_at: std::time::Instant,
        ttl_secs: u64,
        telegram: Option<(TelegramChannel, i64, Option<i64>)>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        slot_name: String,
    },
}

/// Helper: look up the Telegram channel for a workspace slot.
fn get_channel<'a>(
    slot: &WorkspaceSlot,
    channels: &'a HashMap<String, TelegramChannel>,
) -> Option<&'a TelegramChannel> {
    slot.config
        .telegram
        .as_ref()
        .and_then(|tg| channels.get(&tg.bot_token))
}

/// Per-workspace coordinator task — processes jobs serially to preserve session ordering.
async fn run_coordinator_task(
    mut coordinator: Coordinator,
    store: SignalStore,
    mut job_rx: mpsc::UnboundedReceiver<CoordinatorJob>,
    max_session_turns: u32,
) {
    let mut turn_count: u32 = 0;

    while let Some(job) = job_rx.recv().await {
        match job {
            CoordinatorJob::TelegramMessage {
                text,
                chat_id,
                topic_id,
                message_id,
                channel,
                socket_server,
                slot_name,
            } => {
                channel.send_reaction(chat_id, message_id, "👀").await;

                // Start typing indicator loop
                let typing_cancel = CancellationToken::new();
                {
                    let typing_token = typing_cancel.clone();
                    let typing_channel = channel.clone();
                    tokio::spawn(async move {
                        loop {
                            typing_channel.send_typing(chat_id, topic_id).await;
                            tokio::select! {
                                _ = typing_token.cancelled() => break,
                                _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {}
                            }
                        }
                    });
                }

                let opts = match coordinator.build_options(&store) {
                    Ok(opts) => opts,
                    Err(e) => {
                        error!("[{slot_name}] failed to build coordinator options: {e}");
                        typing_cancel.cancel();
                        continue;
                    }
                };

                let name_for_cb = slot_name.clone();
                let mut alerts: Vec<String> = Vec::new();

                let result = coordinator
                    .handle_message_with_options(&text, opts, |event| match event {
                        CoordinatorEvent::BashAudit {
                            command,
                            matched_pattern,
                        } => {
                            warn!(
                                "[{name_for_cb}] coordinator bash MUTATING ({matched_pattern}): {command}"
                            );
                        }
                        CoordinatorEvent::FilesModified { files } => {
                            let file_list: Vec<String> = files
                                .iter()
                                .map(|(repo, file)| format!("{repo}/{file}"))
                                .collect();
                            warn!(
                                "[{name_for_cb}] coordinator modified files: {}",
                                file_list.join(", ")
                            );
                            alerts.push(format!(
                                "⚠️ Coordinator modified workspace files:\n{}",
                                file_list
                                    .iter()
                                    .map(|f| format!("- `{f}`"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ));
                        }
                        CoordinatorEvent::Token(_) => {}
                    })
                    .await;

                // Stop typing indicator
                typing_cancel.cancel();

                // Persist messages to DB (scoped to drop before await)
                {
                    let conv = ConversationStore::new(store.conn(), store.workspace());
                    if let Err(e) = conv.save_message("user", &text, Some("telegram"), None, None) {
                        warn!("[{slot_name}] failed to save user message: {e}");
                    }
                    if let Ok(ref response) = result {
                        let session_id = coordinator.session_token().map(|t| t.token.as_str());
                        let provider = Some(coordinator.provider());
                        if let Err(e) = conv.save_message(
                            "assistant",
                            response,
                            Some("system"),
                            provider,
                            session_id,
                        ) {
                            warn!("[{slot_name}] failed to save assistant message: {e}");
                        }
                    }
                }

                match result {
                    Ok(response) => {
                        turn_count += 1;

                        if let Some(ref server) = socket_server {
                            server.broadcast_activity(
                                "telegram",
                                &slot_name,
                                "assistant_message",
                                &response,
                            );
                        }

                        for alert in alerts {
                            let alert_msg = OutboundMessage {
                                chat_id,
                                text: alert.clone(),
                                buttons: vec![],
                                topic_id,
                            };
                            let _ = channel.send_message(&alert_msg).await;
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity(
                                    "system",
                                    &slot_name,
                                    "safety_alert",
                                    &alert,
                                );
                            }
                        }

                        let msg = OutboundMessage {
                            chat_id,
                            text: response,
                            buttons: vec![],
                            topic_id,
                        };
                        if let Err(e) = channel.send_message(&msg).await {
                            error!("[{slot_name}] failed to send response: {e}");
                        }

                        // Auto-compact if turn limit exceeded
                        if max_session_turns > 0 && turn_count >= max_session_turns {
                            info!("[{slot_name}] session compacted after {turn_count} turns");
                            coordinator.reset_session();
                            turn_count = 0;
                        }
                    }
                    Err(e) => {
                        error!("[{slot_name}] coordinator error: {e}");
                        // If session resume failed, reset and try fresh next time
                        if coordinator.has_session() {
                            warn!(
                                "[{slot_name}] resetting session after error (possible expired resume token)"
                            );
                            coordinator.reset_session();
                            turn_count = 0;
                        }
                        let msg = OutboundMessage {
                            chat_id,
                            text: format!("Error: {e}"),
                            buttons: vec![],
                            topic_id,
                        };
                        let _ = channel.send_message(&msg).await;
                    }
                }
            }

            CoordinatorJob::TuiChat {
                text,
                responder,
                socket_server,
                ws_name,
            } => {
                let opts = match coordinator.build_options(&store) {
                    Ok(opts) => opts,
                    Err(e) => {
                        let _ = responder.send(socket::DaemonResponse::Error {
                            text: format!("{e}"),
                        });
                        continue;
                    }
                };

                let name_for_cb = ws_name.clone();
                let responder_for_cb = responder.clone();

                let result = coordinator
                    .handle_message_with_options(&text, opts, |event| match event {
                        CoordinatorEvent::Token(t) => {
                            let _ =
                                responder_for_cb.send(socket::DaemonResponse::Token { text: t });
                        }
                        CoordinatorEvent::BashAudit {
                            command,
                            matched_pattern,
                        } => {
                            warn!(
                                "[{name_for_cb}] coordinator bash MUTATING ({matched_pattern}): {command}"
                            );
                        }
                        CoordinatorEvent::FilesModified { files } => {
                            let file_list: Vec<String> = files
                                .iter()
                                .map(|(repo, file)| format!("{repo}/{file}"))
                                .collect();
                            warn!(
                                "[{name_for_cb}] coordinator modified files: {}",
                                file_list.join(", ")
                            );
                        }
                    })
                    .await;

                // Persist messages to DB (scoped to drop before any further borrows)
                {
                    let conv = ConversationStore::new(store.conn(), store.workspace());
                    if let Err(e) = conv.save_message("user", &text, Some("tui"), None, None) {
                        warn!("[{ws_name}] failed to save user message: {e}");
                    }
                    if let Ok(ref response) = result {
                        let session_id = coordinator.session_token().map(|t| t.token.as_str());
                        let provider = Some(coordinator.provider());
                        if let Err(e) = conv.save_message(
                            "assistant",
                            response,
                            Some("system"),
                            provider,
                            session_id,
                        ) {
                            warn!("[{ws_name}] failed to save assistant message: {e}");
                        }
                    }
                }

                match result {
                    Ok(response) => {
                        turn_count += 1;
                        let _ = responder.send(socket::DaemonResponse::Done);
                        if let Some(ref server) = socket_server {
                            server.broadcast_activity(
                                "tui",
                                &ws_name,
                                "assistant_message",
                                &response,
                            );
                        }

                        // Auto-compact if turn limit exceeded
                        if max_session_turns > 0 && turn_count >= max_session_turns {
                            info!("[{ws_name}] session compacted after {turn_count} turns");
                            coordinator.reset_session();
                            turn_count = 0;
                        }
                    }
                    Err(e) => {
                        // If session resume failed, reset and try fresh next time
                        if coordinator.has_session() {
                            warn!(
                                "[{ws_name}] resetting session after error (possible expired resume token)"
                            );
                            coordinator.reset_session();
                            turn_count = 0;
                        }
                        let _ = responder.send(socket::DaemonResponse::Error {
                            text: format!("{e}"),
                        });
                    }
                }
            }

            CoordinatorJob::ResetSession => {
                coordinator.reset_session();
                turn_count = 0;
            }

            CoordinatorJob::Compact {
                telegram,
                tui_responder,
                socket_server,
                slot_name,
            } => {
                coordinator.reset_session();
                turn_count = 0;
                info!("[{slot_name}] session compacted via /compact command");

                let msg_text = "\u{1f5dc}\u{fe0f} Session compacted. Starting fresh.";
                if let Some(ref server) = socket_server {
                    server.broadcast_activity("system", &slot_name, "assistant_message", msg_text);
                }
                if let Some((channel, chat_id, topic_id)) = telegram {
                    let msg = OutboundMessage {
                        chat_id,
                        text: msg_text.to_string(),
                        buttons: vec![],
                        topic_id,
                    };
                    if let Err(e) = channel.send_message(&msg).await {
                        error!("[{slot_name}] failed to send /compact confirmation: {e}");
                    }
                }
                if let Some(responder) = tui_responder {
                    let _ = responder.send(socket::DaemonResponse::Token {
                        text: msg_text.to_string(),
                    });
                    let _ = responder.send(socket::DaemonResponse::Done);
                }
            }

            CoordinatorJob::SignalFollowThrough {
                signals,
                source,
                prompt_override,
                queued_at,
                ttl_secs,
                telegram,
                socket_server,
                slot_name,
            } => {
                let elapsed = queued_at.elapsed().as_secs();
                if elapsed >= ttl_secs {
                    info!(
                        "[{slot_name}] dropping stale signal follow-through ({source}, queued {elapsed}s ago)"
                    );
                    continue;
                }

                if !coordinator.has_session() {
                    continue;
                }

                let notification = if let Some(ref tpl) = prompt_override {
                    format_hook_notification(&source, &signals, tpl)
                } else {
                    format_system_notification(&source, &signals)
                };
                info!(
                    "[{slot_name}] triggering coordinator follow-through ({source}, {} event(s))",
                    signals.len()
                );

                let saved_turns = coordinator.max_turns();
                coordinator.set_max_turns(3);

                let opts = match coordinator.build_options(&store) {
                    Ok(opts) => opts,
                    Err(e) => {
                        warn!("[{slot_name}] failed to build coordinator options: {e}");
                        coordinator.set_max_turns(saved_turns);
                        continue;
                    }
                };

                match coordinator
                    .handle_message_with_options(&notification, opts, |_| {})
                    .await
                {
                    Ok(response) => {
                        let response = response.trim().to_string();
                        if !response.is_empty() && response.to_lowercase() != "ack" {
                            if let Some((ref channel, chat_id, topic_id)) = telegram {
                                let msg = OutboundMessage {
                                    chat_id,
                                    text: response.clone(),
                                    buttons: vec![],
                                    topic_id,
                                };
                                if let Err(e) = channel.send_message(&msg).await {
                                    error!("[{slot_name}] failed to send follow-through: {e}");
                                }
                            }
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity(
                                    "signal",
                                    &slot_name,
                                    "assistant_message",
                                    &response,
                                );
                            }
                        } else {
                            info!(
                                "[{slot_name}] coordinator ack'd {source} events (no message sent)"
                            );
                        }
                    }
                    Err(e) => {
                        warn!("[{slot_name}] coordinator follow-through failed: {e}");
                    }
                }

                coordinator.set_max_turns(saved_turns);
            }
        }
    }
}

/// Run the daemon in the foreground with auto-restart on errors.
pub async fn run_foreground() -> Result<()> {
    if let Some(pid) = read_pid()
        && is_process_alive(pid)
    {
        eprintln!("daemon already running (pid: {pid})");
        return Ok(());
    }
    write_pid()?;

    loop {
        let workspaces = config::discover_workspaces()?;
        if workspaces.is_empty() {
            eprintln!(
                "No workspace configs found in {}",
                config::workspaces_dir().display()
            );
            eprintln!("Run `apiari init` in a project directory to create one.");
            remove_pid();
            return Ok(());
        }

        info!("discovered {} workspace(s)", workspaces.len());

        match run_event_loop(workspaces).await {
            ExitReason::Shutdown => {
                info!("clean shutdown");
                break;
            }
            ExitReason::Restart => {
                use std::os::unix::process::CommandExt;
                info!("exec'ing new binary...");
                remove_pid();
                let exe = std::env::current_exe()?;
                let err = std::process::Command::new(&exe)
                    .args(["daemon", "start", "--foreground"])
                    .exec();
                // exec only returns on error
                error!("exec failed: {err}");
                break;
            }
            ExitReason::Error(e) => {
                error!("event loop error: {e:#}");
                info!("restarting in 5 seconds...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }

    remove_pid();
    Ok(())
}

/// Spawn the daemon in the background.
pub fn spawn_background() -> Result<()> {
    if let Some(pid) = read_pid()
        && is_process_alive(pid)
    {
        eprintln!("daemon already running (pid: {pid})");
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let log = log_path();
    std::fs::create_dir_all(config::config_dir())?;

    let log_file = std::fs::File::create(&log)?;
    let stderr_file = log_file.try_clone()?;

    let child = std::process::Command::new(exe)
        .args(["daemon", "start", "--foreground"])
        .stdout(log_file)
        .stderr(stderr_file)
        .stdin(std::process::Stdio::null())
        .spawn()?;

    eprintln!("apiari daemon started (pid {})", child.id());
    eprintln!("log: {}", log.display());
    Ok(())
}

/// Stop the running daemon via PID file.
pub fn stop_daemon() -> Result<()> {
    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            // Wait briefly for the process to exit
            for _ in 0..20 {
                if !is_process_alive(pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            eprintln!("daemon stopped (pid: {pid})");
        } else {
            eprintln!("daemon not running (stale pid file)");
        }
        remove_pid();
    } else {
        eprintln!("daemon not running");
    }
    Ok(())
}

/// Main event loop across all workspaces.
async fn run_event_loop(workspaces: Vec<Workspace>) -> ExitReason {
    let db = db_path();
    if let Err(e) = std::fs::create_dir_all(db.parent().unwrap()) {
        return ExitReason::Error(e.into());
    }

    // Build workspace slots
    let mut slots: Vec<WorkspaceSlot> = Vec::new();
    // Route (chat_id, topic_id) → workspace index
    let mut route_map: HashMap<RouteKey, usize> = HashMap::new();
    // Workspace name → slot index
    let mut name_map: HashMap<String, usize> = HashMap::new();

    for ws in &workspaces {
        let store = match SignalStore::open(&db, &ws.name) {
            Ok(s) => s,
            Err(e) => return ExitReason::Error(e),
        };
        let buzz_config = to_buzz_config(&ws.config);

        let mut registry = WatcherRegistry::new();

        if let Some(gh_config) = &buzz_config.watchers.github
            && gh_config.enabled
        {
            info!(
                "[{}] github watcher: watching {} repo(s): {}",
                ws.name,
                gh_config.repos.len(),
                gh_config.repos.join(", ")
            );
            let mut gh_watcher = GithubWatcher::new(gh_config.clone());
            gh_watcher.load_cursors(&store);
            registry.add_with_interval(Box::new(gh_watcher), gh_config.interval_secs);

            if !gh_config.review_queue.is_empty() {
                let query_names: Vec<&str> = gh_config
                    .review_queue
                    .iter()
                    .map(|q| q.name.as_str())
                    .collect();
                info!(
                    "[{}] review queue watcher: {} quer{}: {}",
                    ws.name,
                    gh_config.review_queue.len(),
                    if gh_config.review_queue.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    },
                    query_names.join(", ")
                );
                registry.add_with_interval(
                    Box::new(ReviewQueueWatcher::new(gh_config)),
                    gh_config.interval_secs,
                );
            }
        }

        if let Some(sentry_config) = &buzz_config.watchers.sentry
            && sentry_config.enabled
        {
            info!(
                "[{}] enabling sentry watcher ({}/{})",
                ws.name, sentry_config.org, sentry_config.project
            );
            registry.add_with_interval(
                Box::new(SentryWatcher::new(sentry_config.clone())),
                sentry_config.interval_secs,
            );
        }

        if let Some(swarm_config) = &buzz_config.watchers.swarm
            && swarm_config.enabled
        {
            info!(
                "[{}] enabling swarm watcher ({})",
                ws.name,
                swarm_config.state_path.display()
            );
            registry.add_with_interval(
                Box::new(SwarmWatcher::new(swarm_config.clone())),
                swarm_config.interval_secs,
            );
        }

        for email_config in &buzz_config.watchers.email {
            let mut watcher = EmailWatcher::new(email_config.clone());
            // Pre-load cursor from store so first poll skips already-seen UIDs
            if let Ok(Some(val)) = store.get_cursor(watcher.cursor_key())
                && let Ok(uid) = val.parse::<u32>()
            {
                watcher.set_initial_uid(uid);
                info!(
                    "[{}] email watcher '{}' resuming from UID {}",
                    ws.name, email_config.name, uid
                );
            }
            info!(
                "[{}] enabling email watcher '{}' ({})",
                ws.name, email_config.name, email_config.host
            );
            registry.add_with_interval(Box::new(watcher), email_config.interval_secs);
        }

        for notion_config in &buzz_config.watchers.notion {
            let mut watcher = NotionWatcher::new(notion_config.clone());
            // Pre-load cursor from store so first poll skips already-seen data
            if let Ok(Some(val)) = store.get_cursor(watcher.last_poll_key()) {
                watcher.set_initial_last_poll(val.clone());
                info!(
                    "[{}] notion watcher '{}' resuming from {}",
                    ws.name, notion_config.name, val
                );
            }
            info!(
                "[{}] enabling notion watcher '{}'",
                ws.name, notion_config.name
            );
            registry.add_with_interval(Box::new(watcher), notion_config.interval_secs);
        }

        for linear_config in &buzz_config.watchers.linear {
            let mut watcher = LinearWatcher::new(linear_config.clone());
            // Pre-load seen map from cursor store so first poll skips unchanged issues
            if let Ok(Some(json)) = store.get_cursor(watcher.cursor_key())
                && let Ok(map) =
                    serde_json::from_str::<std::collections::HashMap<String, String>>(&json)
            {
                info!(
                    "[{}] linear watcher '{}' restored {} seen issue(s)",
                    ws.name,
                    linear_config.name,
                    map.len()
                );
                watcher.set_initial_seen(map);
            }
            let query_names: Vec<&str> = linear_config
                .review_queue
                .iter()
                .map(|q| q.name.as_str())
                .collect();
            info!(
                "[{}] enabling linear watcher '{}' ({} quer{}): {}",
                ws.name,
                linear_config.name,
                linear_config.review_queue.len(),
                if linear_config.review_queue.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                query_names.join(", ")
            );
            registry.add_with_interval(Box::new(watcher), linear_config.poll_interval_secs);
        }

        let mut coordinator = Coordinator::new(
            &ws.config.coordinator.model,
            ws.config.coordinator.max_turns,
        );
        coordinator.set_name(ws.config.coordinator.name.clone());
        let skill_ctx = build_skill_context(&ws.name, &ws.config);
        coordinator.set_extra_context(build_skills_prompt(&skill_ctx));
        if let Some(ref preamble) = skill_ctx.prompt_preamble {
            coordinator.set_prompt_preamble(preamble.clone());
        }
        coordinator.set_tools(default_coordinator_tools());
        coordinator.set_disallowed_tools(default_coordinator_disallowed_tools());
        coordinator.set_working_dir(ws.config.root.clone());
        if let Some(settings) = config::coordinator_settings_json() {
            coordinator.set_settings(settings);
        }
        coordinator.set_safety_hooks(Box::new(GitSafetyHooks {
            workspace_root: ws.config.root.clone(),
        }));

        // Build route key
        if let Some(tg) = &ws.config.telegram {
            let key = RouteKey {
                chat_id: tg.chat_id,
                topic_id: tg.topic_id,
            };
            route_map.insert(key, slots.len());
        }

        let pipeline_rules = to_pipeline_rules(&ws.config.pipeline);
        let pipeline = Pipeline::new(pipeline_rules, ws.config.pipeline.batch_window_secs);

        // Restore session from DB if available
        {
            let conv = ConversationStore::new(store.conn(), &ws.name);
            match conv.last_session() {
                Ok(Some(token)) if token.provider == coordinator.provider() => {
                    info!("[{}] restoring {} session from DB", ws.name, token.provider);
                    coordinator.restore_session(token);
                }
                Ok(Some(token)) => {
                    info!(
                        "[{}] skipping session restore: provider mismatch (db={}, current={})",
                        ws.name,
                        token.provider,
                        coordinator.provider()
                    );
                }
                Ok(None) => {
                    info!("[{}] no previous session to restore", ws.name);
                }
                Err(e) => {
                    warn!("[{}] failed to query last session: {e}", ws.name);
                }
            }
        }

        // Spawn dedicated coordinator task for this workspace
        let coord_store = match SignalStore::open(&db, &ws.name) {
            Ok(s) => s,
            Err(e) => return ExitReason::Error(e),
        };
        let (coord_tx, coord_rx) = mpsc::unbounded_channel::<CoordinatorJob>();
        let max_session_turns = ws.config.coordinator.max_session_turns;
        tokio::spawn(run_coordinator_task(
            coordinator,
            coord_store,
            coord_rx,
            max_session_turns,
        ));

        // Morning brief scheduler
        let morning_brief_scheduler = ws
            .config
            .morning_brief
            .as_ref()
            .filter(|mb| mb.enabled)
            .and_then(
                |mb| match morning_brief::MorningBriefScheduler::new(mb, &ws.name) {
                    Ok(s) => {
                        info!(
                            "[{}] morning brief enabled at {} {}",
                            ws.name, mb.time, mb.timezone
                        );
                        Some(s)
                    }
                    Err(e) => {
                        warn!("[{}] morning brief config error: {e}", ws.name);
                        None
                    }
                },
            );

        name_map.insert(ws.name.clone(), slots.len());
        info!("[{}] {} watcher(s) enabled", ws.name, registry.len());
        slots.push(WorkspaceSlot {
            name: ws.name.clone(),
            config: ws.config.clone(),
            registry,
            coord_tx,
            store,
            pipeline,
            morning_brief: morning_brief_scheduler,
        });
    }

    // Start TUI socket server (optional — warn on failure)
    let socket_path = config::socket_path();
    let (mut tui_rx, socket_server) = match socket::DaemonSocketServer::start(&socket_path) {
        Ok((rx, req_tx, server)) => {
            // Start TCP listener if any workspace has daemon_tcp_port configured.
            // Only one TCP listener is started; warn if multiple differ.
            let tcp_configs: Vec<_> = slots
                .iter()
                .filter_map(|s| {
                    s.config.daemon_tcp_port.map(|port| {
                        let bind = s.config.daemon_tcp_bind.as_deref().unwrap_or("127.0.0.1");
                        (port, bind.to_string())
                    })
                })
                .collect();
            if tcp_configs.len() > 1 {
                let ports: Vec<_> = tcp_configs.iter().map(|(p, _)| *p).collect();
                warn!(
                    "[daemon] multiple workspaces configure daemon_tcp_port {:?}; using first",
                    ports
                );
            }
            if let Some((port, bind_addr)) = tcp_configs.into_iter().next() {
                match server.start_tcp(port, &bind_addr, req_tx.clone()) {
                    Ok(()) => {
                        info!("[daemon] TCP listener started on {bind_addr}:{port}")
                    }
                    Err(e) => {
                        warn!("[daemon] failed to start TCP listener on {bind_addr}:{port}: {e}")
                    }
                }
            }
            (Some(rx), Some(server))
        }
        Err(e) => {
            warn!("failed to start TUI socket server: {e}");
            (None, None)
        }
    };

    // Deduplicate Telegram connections by bot_token
    let (tx, mut rx) = mpsc::channel::<ChannelEvent>(64);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let mut telegram_channels: HashMap<String, TelegramChannel> = HashMap::new();

    for slot in &slots {
        if let Some(tg) = &slot.config.telegram
            && !telegram_channels.contains_key(&tg.bot_token)
        {
            let channel = TelegramChannel::new(tg.bot_token.clone());
            let channel_for_run = TelegramChannel::new(tg.bot_token.clone());
            let tx_clone = tx.clone();
            let cancel_rx_clone = cancel_rx.clone();

            tokio::spawn(async move {
                channel_for_run.run(tx_clone, cancel_rx_clone).await;
            });

            info!(
                "telegram channel started for bot_token ...{}",
                &tg.bot_token[tg.bot_token.len().saturating_sub(6)..]
            );
            telegram_channels.insert(tg.bot_token.clone(), channel);
        }
    }

    // Validate Telegram bot tokens at startup
    for slot in &slots {
        if let Some(tg) = &slot.config.telegram
            && let Some(channel) = telegram_channels.get(&tg.bot_token)
        {
            let channel = channel.clone();
            let ws_name = slot.name.clone();
            tokio::spawn(async move {
                match channel.validate().await {
                    Ok(username) => {
                        info!("[{ws_name}] Telegram bot @{username} connected");
                    }
                    Err(description) => {
                        warn!(
                            "[{ws_name}] Telegram bot token appears invalid (getMe failed: {description}). \
                             Notifications will not be delivered. Check your bot_token in ~/.config/apiari/workspaces/{ws_name}.toml"
                        );
                    }
                }
            });
        }
    }

    // Compute min poll interval across all workspaces
    let min_interval = slots
        .iter()
        .map(|s| buzz_daemon_config::min_watcher_interval(&to_buzz_config(&s.config)))
        .min()
        .unwrap_or(60);

    let poll_interval = std::time::Duration::from_secs(min_interval);
    let mut poll_timer = tokio::time::interval(poll_interval);
    poll_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    info!(
        "apiari daemon running ({} workspace(s), poll interval: {}s)",
        slots.len(),
        poll_interval.as_secs()
    );

    loop {
        // Helper: recv from tui_rx if it exists, else pend forever
        let tui_recv = async {
            match tui_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            _ = &mut shutdown => {
                info!("shutting down");
                let _ = cancel_tx.send(true);
                drop(socket_server); // clean up socket file
                return ExitReason::Shutdown;
            }

            _ = poll_timer.tick() => {
                for slot in &mut slots {
                    // Morning brief check (independent of watchers — runs even
                    // for workspaces with no watcher registry entries).
                    if let Some(ref mut scheduler) = slot.morning_brief {
                        let now = chrono::Utc::now();
                        if scheduler.should_fire(now)
                            && let Some(tg) = &slot.config.telegram
                            && let Some(channel) = telegram_channels.get(&tg.bot_token)
                        {
                            let params = morning_brief::BriefParams {
                                model: slot.config.coordinator.model.clone(),
                                signals: slot.store.get_open_signals().unwrap_or_default(),
                                swarm_state_path: slot.config.watchers.swarm.as_ref()
                                    .map(|s| s.state_path.clone()),
                                workspace: slot.name.clone(),
                                channel: channel.clone(),
                                chat_id: tg.chat_id,
                                topic_id: tg.topic_id,
                                socket_server: socket_server.clone(),
                            };
                            tokio::spawn(morning_brief::execute_brief(params));
                            scheduler.mark_sent(now);
                        }
                    }

                    if slot.registry.is_empty() {
                        continue;
                    }

                    // signal source → (descriptions, hook config)
                    let mut hook_events: HashMap<String, (Vec<String>, config::SignalHookConfig)> = HashMap::new();
                    let mut ci_pass_batch: Vec<(String, String)> = Vec::new(); // (pr_ref, title)

                    // NOTE: Watchers are polled sequentially within each slot because
                    // ThrottledWatcher::poll takes &mut self and SignalStore (rusqlite)
                    // is !Send, so tokio::spawn per-watcher isn't possible without
                    // restructuring to Arc<Mutex<Connection>>. Each poll IS async and
                    // yields at await points so it doesn't block the OS thread.
                    // The GitHub watcher fans out repo polling concurrently internally.
                    for throttled in slot.registry.watchers_mut() {
                        if !throttled.should_poll() {
                            continue;
                        }
                        let watcher_name = throttled.watcher().name().to_string();
                        match throttled.watcher_mut().poll(&slot.store).await {
                            Ok(updates) => {
                                if !updates.is_empty() {
                                    info!("[{}] [{}] polled {} update(s)", slot.name, watcher_name, updates.len());
                                }
                                // Collect emitted IDs for auto-reconciliation
                                throttled.set_poll_ids(
                                    updates.iter().map(|u| u.external_id.clone()).collect(),
                                );
                                for update in &updates {
                                    match slot.store.upsert_signal(update) {
                                        Ok((id, is_new)) => {
                                            // Collect new signals matching a hook for coordinator follow-through
                                            if is_new {
                                                if let Some(hook) = slot.config.coordinator.signal_hooks
                                                    .iter()
                                                    .find(|h| update.source == h.source || update.source.starts_with(&format!("{}_", h.source)))
                                                {
                                                    if let Ok(Some(record)) = slot.store.get_signal(id) {
                                                        let desc = if let Some(ref url) = record.url {
                                                            format!("{} ({})", record.title, url)
                                                        } else if let Some(ref body) = record.body {
                                                            format!("{} — {}", record.title, body.lines().next().unwrap_or(""))
                                                        } else {
                                                            record.title.clone()
                                                        };
                                                        let entry = hook_events
                                                            .entry(hook.source.clone())
                                                            .or_insert_with(|| (Vec::new(), hook.clone()));
                                                        entry.0.push(desc);
                                                    }
                                                }
                                            }

                                            // Determine notification text:
                                            // - github_merged_pr: DB-only, no Telegram
                                            // - github_ci_pass: collected for batched message
                                            // - github_release: immediate real-time
                                            // - Other new signals go through pipeline rules
                                            let text = if is_new {
                                                slot.store.get_signal(id).ok().flatten().and_then(|record| {
                                                    match update.source.as_str() {
                                                        "github_merged_pr" => None,
                                                        "github_release" => {
                                                            slot.pipeline.process_force_notify(&record)
                                                        }
                                                        "github_ci_pass" => {
                                                            // Extract PR # from external_id (ci-pass-{repo}-{pr}-{run})
                                                            let pr_ref = record
                                                                .external_id
                                                                .rsplit('-')
                                                                .nth(1)
                                                                .map(|n| format!("#{n}"))
                                                                .unwrap_or_default();
                                                            ci_pass_batch
                                                                .push((pr_ref, record.title.clone()));
                                                            None
                                                        }
                                                        _ => slot.pipeline.process(&record),
                                                    }
                                                })
                                            } else {
                                                None
                                            };

                                            if let Some(text) = text
                                                && let Some(tg) = &slot.config.telegram
                                                && let Some(channel) = telegram_channels.get(&tg.bot_token)
                                            {
                                                let msg = OutboundMessage {
                                                    chat_id: tg.chat_id,
                                                    text,
                                                    buttons: vec![],
                                                    topic_id: tg.topic_id,
                                                };
                                                if let Err(e) = channel.send_message(&msg).await {
                                                    error!("[{}] failed to send notification: {e}", slot.name);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("[{}] failed to upsert signal: {e}", slot.name);
                                        }
                                    }
                                }
                                // Reconcile: resolve signals no longer in the source
                                if let Err(e) = throttled.reconcile(&slot.store) {
                                    error!("[{}] [{}] reconcile failed: {e}", slot.name, watcher_name);
                                }
                                // Update cursor timestamp so TUI shows watcher as healthy
                                let _ = slot.store.set_cursor(&watcher_name, "ok");
                                throttled.mark_polled();
                            }
                            Err(e) => {
                                error!("[{}] [{}] poll failed: {e}", slot.name, watcher_name);
                                // Still mark polled on error to avoid hammering a failing source
                                throttled.mark_polled();
                            }
                        }
                    }

                    // Send batched CI pass notification
                    if !ci_pass_batch.is_empty() {
                        let text = if ci_pass_batch.len() == 1 {
                            ci_pass_batch[0].1.clone()
                        } else {
                            let pr_refs: Vec<&str> =
                                ci_pass_batch.iter().map(|(r, _)| r.as_str()).collect();
                            format!(
                                "\u{2705} CI passed on {} PRs: {}",
                                ci_pass_batch.len(),
                                pr_refs.join(", ")
                            )
                        };
                        if let Some(tg) = &slot.config.telegram
                            && let Some(channel) = telegram_channels.get(&tg.bot_token)
                        {
                            let msg = OutboundMessage {
                                chat_id: tg.chat_id,
                                text,
                                buttons: vec![],
                                topic_id: tg.topic_id,
                            };
                            if let Err(e) = channel.send_message(&msg).await {
                                error!("[{}] failed to send CI pass notification: {e}", slot.name);
                            }
                        }
                    }

                    // Flush any pending batched notifications
                    if let Some(text) = slot.pipeline.flush_batches()
                        && let Some(tg) = &slot.config.telegram
                        && let Some(channel) = telegram_channels.get(&tg.bot_token)
                    {
                        let msg = OutboundMessage {
                            chat_id: tg.chat_id,
                            text,
                            buttons: vec![],
                            topic_id: tg.topic_id,
                        };
                        if let Err(e) = channel.send_message(&msg).await {
                            error!("[{}] failed to send batch notification: {e}", slot.name);
                        }
                    }

                    // Periodically evict old notification log entries
                    slot.pipeline.evict_old_log_entries();

                    // Coordinator follow-through for signal hook events (non-blocking)
                    for (source, (signals, hook)) in hook_events {
                        let telegram_info = slot.config.telegram.as_ref().and_then(|tg| {
                            telegram_channels.get(&tg.bot_token).map(|ch| {
                                (ch.clone(), tg.chat_id, tg.topic_id)
                            })
                        });
                        let prompt_override = if hook.prompt.is_empty() {
                            None
                        } else {
                            Some(hook.prompt.clone())
                        };
                        let _ = slot.coord_tx.send(CoordinatorJob::SignalFollowThrough {
                            signals,
                            source,
                            prompt_override,
                            queued_at: std::time::Instant::now(),
                            ttl_secs: hook.ttl_secs,
                            telegram: telegram_info,
                            socket_server: socket_server.clone(),
                            slot_name: slot.name.clone(),
                        });
                    }

                }
            }

            // ── TUI socket requests ──
            Some(client_req) = tui_recv => {
                match client_req.request {
                    socket::DaemonRequest::Chat { ref workspace, ref text } => {
                        let ws_name = workspace.clone();
                        let user_text = text.clone();

                        if let Some(&idx) = name_map.get(&ws_name) {
                            let slot = &mut slots[idx];
                            info!("[{}] TUI chat: {user_text}", slot.name);

                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("tui", &ws_name, "user_message", &user_text);
                            }

                            // Check for slash commands in TUI chat
                            if let Some(rest) = user_text.strip_prefix('/') {
                                let (command, _args) = match rest.split_once(' ') {
                                    Some((cmd, args)) => (cmd, args.trim()),
                                    None => (rest, ""),
                                };
                                let handled = handle_tui_command(
                                    command,
                                    slot,
                                    &client_req.responder,
                                    &socket_server,
                                    &telegram_channels,
                                ).await;
                                if handled {
                                    continue;
                                }
                                // Not a built-in command — fall through to coordinator
                            }

                            let job = CoordinatorJob::TuiChat {
                                text: user_text,
                                responder: client_req.responder.clone(),
                                socket_server: socket_server.clone(),
                                ws_name,
                            };
                            if slot.coord_tx.send(job).is_err() {
                                let _ = client_req.responder.send(socket::DaemonResponse::Error {
                                    text: "coordinator task shut down".to_string(),
                                });
                            }
                        } else {
                            let _ = client_req.responder.send(socket::DaemonResponse::Error {
                                text: format!("workspace '{ws_name}' not found"),
                            });
                        }
                    }
                }
            }

            Some(event) = rx.recv() => {
                match event {
                    ChannelEvent::Message { chat_id, user_name, text, topic_id, message_id, .. } => {
                        let key = RouteKey { chat_id, topic_id };
                        let slot_idx = route_map.get(&key).copied()
                            .or_else(|| route_map.get(&RouteKey { chat_id, topic_id: None }).copied());

                        if let Some(idx) = slot_idx {
                            let slot = &slots[idx];
                            info!("[{}] message from {user_name}: {text}", slot.name);

                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("telegram", &slot.name, "user_message", &text);
                            }

                            if let Some(channel) = get_channel(slot, &telegram_channels) {
                                let job = CoordinatorJob::TelegramMessage {
                                    text,
                                    chat_id,
                                    topic_id,
                                    message_id,
                                    channel: channel.clone(),
                                    socket_server: socket_server.clone(),
                                    slot_name: slot.name.clone(),
                                };
                                if let Err(e) = slot.coord_tx.send(job) {
                                    error!("[{}] coordinator job send failed: {e}", slot.name);
                                }
                            }
                        } else {
                            warn!("no workspace route for chat_id={chat_id} topic_id={topic_id:?}");
                        }
                    }

                    ChannelEvent::Command { chat_id, command, topic_id, .. } => {
                        let key = RouteKey { chat_id, topic_id };
                        let slot_idx = route_map.get(&key).copied()
                            .or_else(|| route_map.get(&RouteKey { chat_id, topic_id: None }).copied());

                        if let Some(idx) = slot_idx {
                            let slot = &mut slots[idx];
                            info!("[{}] command: /{command}", slot.name);

                            // Broadcast command to TUI
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("telegram", &slot.name, "user_message", &format!("/{command}"));
                            }

                            if let Some(channel) = get_channel(slot, &telegram_channels) {
                                match command.as_str() {
                                    "status" => {
                                        let signals = slot.store.get_open_signals().unwrap_or_default();
                                        let summary = format_signal_summary(&signals);
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &summary);
                                        }
                                        let msg = OutboundMessage {
                                            chat_id,
                                            text: summary,
                                            buttons: vec![],
                                            topic_id,
                                        };
                                        let _ = channel.send_message(&msg).await;
                                    }
                                    "reset" => {
                                        let _ = slot.coord_tx.send(CoordinatorJob::ResetSession);
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", "Session reset.");
                                        }
                                        let msg = OutboundMessage {
                                            chat_id,
                                            text: "Session reset.".to_string(),
                                            buttons: vec![],
                                            topic_id,
                                        };
                                        let _ = channel.send_message(&msg).await;
                                    }
                                    "compact" => {
                                        if slot.coord_tx.send(CoordinatorJob::Compact {
                                            telegram: Some((channel.clone(), chat_id, topic_id)),
                                            tui_responder: None,
                                            socket_server: socket_server.clone(),
                                            slot_name: slot.name.clone(),
                                        }).is_err() {
                                            let msg = OutboundMessage {
                                                chat_id,
                                                text: "Error: coordinator task shut down".to_string(),
                                                buttons: vec![],
                                                topic_id,
                                            };
                                            let _ = channel.send_message(&msg).await;
                                        }
                                    }
                                    "update" => {
                                        info!("[{}] running /update", slot.name);
                                        let updating_msg = OutboundMessage {
                                            chat_id,
                                            text: "Updating apiari + swarm from crates.io...".to_string(),
                                            buttons: vec![],
                                            topic_id,
                                        };
                                        let _ = channel.send_message(&updating_msg).await;

                                        let script = "source /Users/josh/.cargo/env 2>/dev/null; \
                                            /Users/josh/.cargo/bin/cargo install --force apiari 2>&1 && \
                                            codesign -f -s - /Users/josh/.cargo/bin/apiari 2>&1 && \
                                            /Users/josh/.cargo/bin/cargo install --force swarm 2>&1 && \
                                            codesign -f -s - /Users/josh/.cargo/bin/swarm 2>&1";

                                        let output = tokio::process::Command::new("sh")
                                            .arg("-c")
                                            .arg(script)
                                            .output()
                                            .await;

                                        match output {
                                            Ok(out) => {
                                                let stdout = String::from_utf8_lossy(&out.stdout);
                                                let stderr = String::from_utf8_lossy(&out.stderr);
                                                let status_icon = if out.status.success() { "✅" } else { "❌" };
                                                let mut text = format!("{status_icon} /update");
                                                let combined = format!("{stdout}{stderr}");
                                                let tail: String = combined
                                                    .lines()
                                                    .rev()
                                                    .take(20)
                                                    .collect::<Vec<_>>()
                                                    .into_iter()
                                                    .rev()
                                                    .collect::<Vec<_>>()
                                                    .join("\n");
                                                if !tail.is_empty() {
                                                    text.push_str(&format!("\n```\n{tail}\n```"));
                                                }
                                                if let Some(ref server) = socket_server {
                                                    server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                                }
                                                let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;

                                                if out.status.success() {
                                                    info!("[{}] /update succeeded, restarting", slot.name);
                                                    let _ = channel.send_message(&OutboundMessage {
                                                        chat_id,
                                                        text: "Restarting daemon...".to_string(),
                                                        buttons: vec![],
                                                        topic_id,
                                                    }).await;
                                                    return ExitReason::Restart;
                                                }
                                            }
                                            Err(e) => {
                                                let _ = channel.send_message(&OutboundMessage {
                                                    chat_id,
                                                    text: format!("❌ /update failed: {e}"),
                                                    buttons: vec![],
                                                    topic_id,
                                                }).await;
                                            }
                                        }
                                    }
                                    "brief" => {
                                        channel.send_typing(chat_id, topic_id).await;

                                        let params = morning_brief::BriefParams {
                                            model: slot.config.coordinator.model.clone(),
                                            signals: slot.store.get_open_signals().unwrap_or_default(),
                                            swarm_state_path: slot.config.watchers.swarm.as_ref()
                                                .map(|s| s.state_path.clone()),
                                            workspace: slot.name.clone(),
                                            channel: channel.clone(),
                                            chat_id,
                                            topic_id,
                                            socket_server: socket_server.clone(),
                                        };
                                        tokio::spawn(morning_brief::execute_brief(params));
                                    }
                                    "help" => {
                                        let mut text = "Built-in commands:\n/status — show open signals\n/brief — generate morning brief on demand\n/reset — reset coordinator session\n/compact — compact coordinator session (clear context, start fresh)\n/update — install latest apiari + swarm from crates.io\n/help — this message".to_string();
                                        if !slot.config.commands.is_empty() {
                                            text.push_str("\n\nCustom commands:");
                                            for cmd in &slot.config.commands {
                                                let desc = cmd.description.as_deref().unwrap_or("(no description)");
                                                text.push_str(&format!("\n/{} — {}", cmd.name, desc));
                                            }
                                        }
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                        }
                                        let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;
                                    }
                                    _ => {
                                        if let Some(cmd_cfg) = slot.config.commands.iter().find(|c| c.name == command) {
                                            info!("[{}] running custom command: /{}", slot.name, command);
                                            let output = tokio::process::Command::new("sh")
                                                .arg("-c")
                                                .arg(&cmd_cfg.script)
                                                .output()
                                                .await;

                                            match output {
                                                Ok(out) => {
                                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                                    let status_icon = if out.status.success() { "✅" } else { "❌" };
                                                    let mut text = format!("{status_icon} /{command}");
                                                    let combined = format!("{stdout}{stderr}");
                                                    let tail: String = combined
                                                        .lines()
                                                        .rev()
                                                        .take(20)
                                                        .collect::<Vec<_>>()
                                                        .into_iter()
                                                        .rev()
                                                        .collect::<Vec<_>>()
                                                        .join("\n");
                                                    if !tail.is_empty() {
                                                        text.push_str(&format!("\n```\n{tail}\n```"));
                                                    }
                                                    if let Some(ref server) = socket_server {
                                                        server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                                    }
                                                    let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;

                                                    if cmd_cfg.restart && out.status.success() {
                                                        info!("[{}] command /{} requested restart", slot.name, command);
                                                        let _ = channel.send_message(&OutboundMessage {
                                                            chat_id,
                                                            text: "Restarting daemon...".to_string(),
                                                            buttons: vec![],
                                                            topic_id,
                                                        }).await;
                                                        // Exec the new binary
                                                        return ExitReason::Restart;
                                                    }
                                                }
                                                Err(e) => {
                                                    let _ = channel.send_message(&OutboundMessage {
                                                        chat_id,
                                                        text: format!("❌ /{command} failed: {e}"),
                                                        buttons: vec![],
                                                        topic_id,
                                                    }).await;
                                                }
                                            }
                                        } else {
                                            let _ = channel.send_message(&OutboundMessage {
                                                chat_id,
                                                text: format!("Unknown command: /{command}"),
                                                buttons: vec![],
                                                topic_id,
                                            }).await;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    ChannelEvent::CallbackQuery { callback_query_id, .. } => {
                        if let Some(channel) = telegram_channels.values().next() {
                            channel.answer_callback_query(&callback_query_id).await;
                        }
                    }
                }
            }
        }
    }
}

/// Show open signals from the database.
pub fn show_status(workspace_filter: Option<&str>) -> Result<()> {
    let db = db_path();
    std::fs::create_dir_all(db.parent().unwrap())?;

    let workspaces = config::discover_workspaces()?;
    if workspaces.is_empty() {
        eprintln!("No workspace configs found.");
        return Ok(());
    }

    for ws in &workspaces {
        if let Some(filter) = workspace_filter
            && ws.name != filter
        {
            continue;
        }

        let store = SignalStore::open(&db, &ws.name)?;
        let signals = store.get_open_signals()?;

        println!("=== {} ===", ws.name);
        if signals.is_empty() {
            println!("  No open signals.\n");
        } else {
            println!("  {} open signal(s):\n", signals.len());
            for signal in &signals {
                println!(
                    "  [{severity}] [{source}] {title}",
                    severity = signal.severity,
                    source = signal.source,
                    title = signal.title,
                );
                if let Some(ref url) = signal.url {
                    println!("    {url}");
                }
                if let Some(ref body) = signal.body {
                    let first_line = body.lines().next().unwrap_or("");
                    if !first_line.is_empty() {
                        println!("    {first_line}");
                    }
                }
            }
            println!();
        }

        let counts = store.count_by_status()?;
        if !counts.is_empty() {
            for (status, count) in &counts {
                println!("  {status}: {count}");
            }
            println!();
        }
    }

    Ok(())
}

/// Run a CLI chat with a workspace's coordinator.
pub async fn run_chat(workspace_name: &str, message: Option<String>) -> Result<()> {
    let db = db_path();
    std::fs::create_dir_all(db.parent().unwrap())?;

    let workspaces = config::discover_workspaces()?;
    let ws = workspaces
        .iter()
        .find(|w| w.name == workspace_name)
        .ok_or_else(|| color_eyre::eyre::eyre!("workspace '{}' not found", workspace_name))?;

    let store = SignalStore::open(&db, workspace_name)?;
    let mut coordinator = Coordinator::new(
        &ws.config.coordinator.model,
        ws.config.coordinator.max_turns,
    );
    coordinator.set_name(ws.config.coordinator.name.clone());

    let skill_ctx = build_skill_context(workspace_name, &ws.config);
    coordinator.set_extra_context(build_skills_prompt(&skill_ctx));
    if let Some(ref preamble) = skill_ctx.prompt_preamble {
        coordinator.set_prompt_preamble(preamble.clone());
    }
    coordinator.set_tools(default_coordinator_tools());
    coordinator.set_disallowed_tools(default_coordinator_disallowed_tools());
    coordinator.set_working_dir(ws.config.root.clone());
    if let Some(settings) = config::coordinator_settings_json() {
        coordinator.set_settings(settings);
    }
    coordinator.set_safety_hooks(Box::new(GitSafetyHooks {
        workspace_root: ws.config.root.clone(),
    }));

    if !Coordinator::is_available().await {
        eprintln!("claude CLI not found — coordinator requires it");
        return Ok(());
    }

    if let Some(msg) = message {
        eprintln!("Thinking...");
        let response = coordinator
            .handle_message(&msg, &store, |event| {
                print_event_to_stderr(&event);
            })
            .await?;
        println!("{response}");
    } else {
        println!("apiari chat [{workspace_name}] (type 'quit' to exit)\n");
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            eprint!("> ");
            line.clear();
            if stdin.read_line(&mut line)? == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "quit" || trimmed == "exit" {
                break;
            }

            eprintln!("Thinking...");
            match coordinator
                .handle_message(trimmed, &store, |event| {
                    print_event_to_stderr(&event);
                })
                .await
            {
                Ok(response) => println!("\n{response}\n"),
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }

    Ok(())
}

/// Print safety events to stderr for CLI chat.
fn print_event_to_stderr(event: &CoordinatorEvent) {
    match event {
        CoordinatorEvent::BashAudit {
            command,
            matched_pattern,
        } => {
            eprintln!("Bash audit ({matched_pattern}): {command}");
        }
        CoordinatorEvent::FilesModified { files } => {
            let list: Vec<String> = files
                .iter()
                .map(|(repo, file)| format!("  - {repo}/{file}"))
                .collect();
            eprintln!(
                "Warning: coordinator modified workspace files:\n{}",
                list.join("\n")
            );
        }
        CoordinatorEvent::Token(_) => {}
    }
}

/// Check if the daemon is currently running.
pub fn is_daemon_running() -> bool {
    if let Some(pid) = read_pid() {
        is_process_alive(pid)
    } else {
        false
    }
}

/// Ensure the daemon is running, starting it in the background if needed.
/// If the daemon isn't running, starts it and waits up to ~3 seconds for the
/// socket to become available.
pub fn ensure_daemon() -> Result<()> {
    if is_daemon_running() {
        return Ok(());
    }
    eprintln!("Starting daemon...");
    spawn_background()?;

    // Wait for the daemon socket to appear (up to ~3 seconds)
    let socket = config::socket_path();
    for _ in 0..30 {
        if socket.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Ok(())
}

fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn is_process_alive(pid: u32) -> bool {
    // kill -0 checks if process exists without sending a signal
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Build a SkillContext, logging auto-discovered repos.
fn build_skill_context(workspace_name: &str, config: &WorkspaceConfig) -> SkillContext {
    let ctx = config::build_skill_context(workspace_name, config);
    if config.repos.is_empty() && !ctx.repos.is_empty() {
        info!(
            "[{workspace_name}] auto-discovered {} repo(s): {}",
            ctx.repos.len(),
            ctx.repos.join(", ")
        );
    }
    ctx
}

fn write_pid() -> Result<()> {
    let dir = config::config_dir();
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("failed to create {}", dir.display()))?;
    std::fs::write(pid_path(), std::process::id().to_string())?;
    Ok(())
}

fn remove_pid() {
    let _ = std::fs::remove_file(pid_path());
}

/// Handle a TUI slash command. Returns `true` if the command was handled.
async fn handle_tui_command(
    command: &str,
    slot: &mut WorkspaceSlot,
    responder: &mpsc::UnboundedSender<socket::DaemonResponse>,
    socket_server: &Option<Arc<socket::DaemonSocketServer>>,
    telegram_channels: &HashMap<String, TelegramChannel>,
) -> bool {
    /// Send a text response back to the TUI client.
    fn reply(
        responder: &mpsc::UnboundedSender<socket::DaemonResponse>,
        socket_server: &Option<Arc<socket::DaemonSocketServer>>,
        ws_name: &str,
        text: &str,
    ) {
        let _ = responder.send(socket::DaemonResponse::Token {
            text: text.to_string(),
        });
        let _ = responder.send(socket::DaemonResponse::Done);
        if let Some(server) = socket_server {
            server.broadcast_activity("tui", ws_name, "assistant_message", text);
        }
    }

    match command {
        "status" => {
            let signals = slot.store.get_open_signals().unwrap_or_default();
            let summary = format_signal_summary(&signals);
            reply(responder, socket_server, &slot.name, &summary);
            true
        }
        "reset" => {
            let _ = slot.coord_tx.send(CoordinatorJob::ResetSession);
            reply(responder, socket_server, &slot.name, "Session reset.");
            true
        }
        "compact" => {
            if slot
                .coord_tx
                .send(CoordinatorJob::Compact {
                    telegram: None,
                    tui_responder: Some(responder.clone()),
                    socket_server: socket_server.clone(),
                    slot_name: slot.name.clone(),
                })
                .is_err()
            {
                reply(
                    responder,
                    socket_server,
                    &slot.name,
                    "Error: coordinator task shut down",
                );
            }
            true
        }
        "brief" => {
            let channel = slot
                .config
                .telegram
                .as_ref()
                .and_then(|tg| telegram_channels.get(&tg.bot_token));
            if let Some(channel) = channel {
                if let Some(tg) = &slot.config.telegram {
                    let params = morning_brief::BriefParams {
                        model: slot.config.coordinator.model.clone(),
                        signals: slot.store.get_open_signals().unwrap_or_default(),
                        swarm_state_path: slot
                            .config
                            .watchers
                            .swarm
                            .as_ref()
                            .map(|s| s.state_path.clone()),
                        workspace: slot.name.clone(),
                        channel: channel.clone(),
                        chat_id: tg.chat_id,
                        topic_id: tg.topic_id,
                        socket_server: socket_server.clone(),
                    };
                    tokio::spawn(morning_brief::execute_brief(params));
                    reply(
                        responder,
                        socket_server,
                        &slot.name,
                        "Generating morning brief...",
                    );
                } else {
                    reply(
                        responder,
                        socket_server,
                        &slot.name,
                        "No Telegram channel configured for briefs.",
                    );
                }
            } else {
                reply(
                    responder,
                    socket_server,
                    &slot.name,
                    "No Telegram channel configured for briefs.",
                );
            }
            true
        }
        "help" => {
            let mut text = "Built-in commands:\n/status — show open signals\n/brief — generate morning brief on demand\n/reset — reset coordinator session\n/compact — compact coordinator session (clear context, start fresh)\n/update — install latest apiari + swarm from crates.io\n/help — this message"
                .to_string();
            if !slot.config.commands.is_empty() {
                text.push_str("\n\nCustom commands:");
                for cmd in &slot.config.commands {
                    let desc = cmd.description.as_deref().unwrap_or("(no description)");
                    text.push_str(&format!("\n/{} — {}", cmd.name, desc));
                }
            }
            reply(responder, socket_server, &slot.name, &text);
            true
        }
        "update" => {
            // Send initial status as a streaming token (Done sent after completion)
            let _ = responder.send(socket::DaemonResponse::Token {
                text: "Updating apiari + swarm from crates.io...\n".to_string(),
            });

            let script = "source /Users/josh/.cargo/env 2>/dev/null; \
                /Users/josh/.cargo/bin/cargo install --force apiari 2>&1 && \
                codesign -f -s - /Users/josh/.cargo/bin/apiari 2>&1 && \
                /Users/josh/.cargo/bin/cargo install --force swarm 2>&1 && \
                codesign -f -s - /Users/josh/.cargo/bin/swarm 2>&1";

            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .output()
                .await;

            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let status_icon = if out.status.success() { "✅" } else { "❌" };
                    let mut text = format!("{status_icon} /update");
                    let combined = format!("{stdout}{stderr}");
                    let tail: String = combined
                        .lines()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !tail.is_empty() {
                        text.push_str(&format!("\n```\n{tail}\n```"));
                    }
                    let _ = responder.send(socket::DaemonResponse::Token { text: text.clone() });
                    let _ = responder.send(socket::DaemonResponse::Done);
                    if let Some(server) = socket_server {
                        server.broadcast_activity("tui", &slot.name, "assistant_message", &text);
                    }
                }
                Err(e) => {
                    let text = format!("❌ /update failed: {e}");
                    let _ = responder.send(socket::DaemonResponse::Token { text: text.clone() });
                    let _ = responder.send(socket::DaemonResponse::Done);
                    if let Some(server) = socket_server {
                        server.broadcast_activity("tui", &slot.name, "assistant_message", &text);
                    }
                }
            }
            true
        }
        _ => {
            // Check custom commands
            if let Some(cmd_cfg) = slot.config.commands.iter().find(|c| c.name == command) {
                info!("[{}] running custom command: /{}", slot.name, command);
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd_cfg.script)
                    .output()
                    .await;

                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let status_icon = if out.status.success() { "✅" } else { "❌" };
                        let mut text = format!("{status_icon} /{command}");
                        let combined = format!("{stdout}{stderr}");
                        let tail: String = combined
                            .lines()
                            .rev()
                            .take(20)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !tail.is_empty() {
                            text.push_str(&format!("\n```\n{tail}\n```"));
                        }
                        reply(responder, socket_server, &slot.name, &text);
                    }
                    Err(e) => {
                        reply(
                            responder,
                            socket_server,
                            &slot.name,
                            &format!("❌ /{command} failed: {e}"),
                        );
                    }
                }
                true
            } else {
                // Not a known command — let the coordinator handle it
                false
            }
        }
    }
}

/// Format signal events into a system notification for the coordinator.
fn format_system_notification(source: &str, events: &[String]) -> String {
    let mut msg = format!(
        "[System notification — {source} activity]\n\
         The following events just occurred:\n",
    );
    for e in events {
        msg.push_str(&format!("- {e}\n"));
    }
    msg.push_str(
        "\nIf any of these are relevant to your recent conversations, \
         provide a brief contextual update. Otherwise respond with just \"ack\".",
    );
    msg
}

/// Format a hook notification using a custom prompt template.
/// Supports {source} and {events} placeholders.
fn format_hook_notification(source: &str, events: &[String], template: &str) -> String {
    let event_list = events
        .iter()
        .map(|e| format!("- {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    let result = template
        .replace("{source}", source)
        .replace("{events}", &event_list);
    if result.is_empty() {
        format_system_notification(source, events)
    } else {
        result
    }
}

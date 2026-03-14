//! Multi-workspace daemon — event loop for all workspaces.
//!
//! Discovers workspace configs, builds per-workspace watcher registries,
//! shares Telegram connections by bot_token, and routes messages by (chat_id, topic_id).

pub mod socket;

use color_eyre::eyre::{Result, WrapErr};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::git_safety::GitSafetyHooks;

use buzz::channel::telegram::TelegramChannel;
use buzz::channel::{Channel, ChannelEvent, OutboundMessage};
use buzz::coordinator::prompt::format_signal_summary;
use buzz::coordinator::skills::{
    SkillContext, build_skills_prompt, default_coordinator_disallowed_tools,
    default_coordinator_tools,
};
use buzz::coordinator::{Coordinator, CoordinatorEvent};
use buzz::daemon::config as buzz_daemon_config;
use buzz::pipeline::Pipeline;
use buzz::signal::store::SignalStore;
use buzz::watcher::WatcherRegistry;
use buzz::watcher::github::GithubWatcher;
use buzz::watcher::sentry::SentryWatcher;
use buzz::watcher::swarm::SwarmWatcher;

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
}

/// A workspace slot in the daemon — holds per-workspace state.
struct WorkspaceSlot {
    name: String,
    config: WorkspaceConfig,
    registry: WatcherRegistry,
    coordinator: Coordinator,
    store: SignalStore,
    pipeline: Pipeline,
}

/// Key for routing Telegram messages to workspaces.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct RouteKey {
    chat_id: i64,
    topic_id: Option<i64>,
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

/// Run the daemon in the foreground with auto-restart on errors.
pub async fn run_foreground() -> Result<()> {
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
    let exe = std::env::current_exe()?;
    let log = log_path();
    std::fs::create_dir_all(config::config_dir())?;

    let log_file = std::fs::File::create(&log)?;
    let stderr_file = log_file.try_clone()?;

    let child = std::process::Command::new(exe)
        .args(["daemon"])
        .stdout(log_file)
        .stderr(stderr_file)
        .stdin(std::process::Stdio::null())
        .spawn()?;

    eprintln!("apiari daemon started (pid {})", child.id());
    eprintln!("log: {}", log.display());
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
            registry.add(Box::new(GithubWatcher::new(gh_config.clone())));
        }

        if let Some(sentry_config) = &buzz_config.watchers.sentry
            && sentry_config.enabled
        {
            info!(
                "[{}] enabling sentry watcher ({}/{})",
                ws.name, sentry_config.org, sentry_config.project
            );
            registry.add(Box::new(SentryWatcher::new(sentry_config.clone())));
        }

        if let Some(swarm_config) = &buzz_config.watchers.swarm
            && swarm_config.enabled
        {
            info!(
                "[{}] enabling swarm watcher ({})",
                ws.name,
                swarm_config.state_path.display()
            );
            registry.add(Box::new(SwarmWatcher::new(swarm_config.clone())));
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

        name_map.insert(ws.name.clone(), slots.len());
        info!("[{}] {} watcher(s) enabled", ws.name, registry.len());
        slots.push(WorkspaceSlot {
            name: ws.name.clone(),
            config: ws.config.clone(),
            registry,
            coordinator,
            store,
            pipeline,
        });
    }

    // Start TUI socket server (optional — warn on failure)
    let socket_path = config::socket_path();
    let (mut tui_rx, socket_server) = match socket::DaemonSocketServer::start(&socket_path) {
        Ok((rx, server)) => (Some(rx), Some(server)),
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
                    if slot.registry.is_empty() {
                        continue;
                    }

                    let mut new_swarm_events: Vec<String> = Vec::new();

                    for watcher in slot.registry.watchers_mut() {
                        let watcher_name = watcher.name().to_string();
                        match watcher.poll(&slot.store).await {
                            Ok(updates) => {
                                if !updates.is_empty() {
                                    info!("[{}] [{}] polled {} update(s)", slot.name, watcher_name, updates.len());
                                }
                                for update in &updates {
                                    match slot.store.upsert_signal(update) {
                                        Ok((id, true)) => {
                                            // Collect new swarm signals for coordinator follow-through
                                            if watcher_name == "swarm"
                                                && let Ok(Some(record)) = slot.store.get_signal(id)
                                            {
                                                let desc = if let Some(ref url) = record.url {
                                                    format!("{} ({})", record.title, url)
                                                } else if let Some(ref body) = record.body {
                                                    format!("{} — {}", record.title, body.lines().next().unwrap_or(""))
                                                } else {
                                                    record.title.clone()
                                                };
                                                new_swarm_events.push(desc);
                                            }

                                            if let Ok(Some(record)) = slot.store.get_signal(id)
                                                && let Some(text) = slot.pipeline.process(&record)
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
                                        Ok((_, false)) => {} // existing signal updated, no notification
                                        Err(e) => {
                                            error!("[{}] failed to upsert signal: {e}", slot.name);
                                        }
                                    }
                                }
                                // Reconcile: resolve signals no longer in the source
                                if let Err(e) = watcher.reconcile(&slot.store) {
                                    error!("[{}] [{}] reconcile failed: {e}", slot.name, watcher_name);
                                }
                                // Update cursor timestamp so TUI shows watcher as healthy
                                let _ = slot.store.set_cursor(&watcher_name, "ok");
                            }
                            Err(e) => {
                                error!("[{}] [{}] poll failed: {e}", slot.name, watcher_name);
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

                    // Coordinator follow-through for swarm events
                    if !new_swarm_events.is_empty() && slot.coordinator.has_session() {
                        let notification = format_system_notification(&new_swarm_events);
                        info!("[{}] triggering coordinator follow-through ({} swarm event(s))", slot.name, new_swarm_events.len());

                        let saved_turns = slot.coordinator.max_turns();
                        slot.coordinator.set_max_turns(3);

                        match slot.coordinator.handle_message(&notification, &slot.store, |_| {}).await {
                            Ok(response) => {
                                let response = response.trim().to_string();
                                if !response.is_empty() && response.to_lowercase() != "ack" {
                                    // Send to Telegram
                                    if let Some(tg) = &slot.config.telegram
                                        && let Some(channel) = telegram_channels.get(&tg.bot_token)
                                    {
                                        let msg = OutboundMessage {
                                            chat_id: tg.chat_id,
                                            text: response.clone(),
                                            buttons: vec![],
                                            topic_id: tg.topic_id,
                                        };
                                        if let Err(e) = channel.send_message(&msg).await {
                                            error!("[{}] failed to send follow-through: {e}", slot.name);
                                        }
                                    }
                                    // Broadcast to TUI
                                    if let Some(ref server) = socket_server {
                                        server.broadcast_activity("system", &slot.name, "assistant_message", &response);
                                    }
                                } else {
                                    info!("[{}] coordinator ack'd swarm events (no message sent)", slot.name);
                                }
                            }
                            Err(e) => {
                                warn!("[{}] coordinator follow-through failed: {e}", slot.name);
                            }
                        }

                        slot.coordinator.set_max_turns(saved_turns);
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

                            // Broadcast user message to all TUI clients
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("tui", &ws_name, "user_message", &user_text);
                            }

                            let slot_name = slot.name.clone();
                            let responder = client_req.responder.clone();

                            match slot.coordinator.handle_message(&user_text, &slot.store, |event| {
                                match event {
                                    CoordinatorEvent::Token(t) => {
                                        let _ = responder.send(socket::DaemonResponse::Token { text: t });
                                    }
                                    CoordinatorEvent::BashAudit { command, matched_pattern } => {
                                        warn!(
                                            "[{slot_name}] coordinator bash MUTATING ({matched_pattern}): {command}"
                                        );
                                    }
                                    CoordinatorEvent::FilesModified { files } => {
                                        let file_list: Vec<String> = files
                                            .iter()
                                            .map(|(repo, file)| format!("{repo}/{file}"))
                                            .collect();
                                        warn!(
                                            "[{slot_name}] coordinator modified files: {}",
                                            file_list.join(", ")
                                        );
                                    }
                                }
                            }).await {
                                Ok(response) => {
                                    let _ = client_req.responder.send(socket::DaemonResponse::Done);
                                    // Broadcast completed assistant message to all TUI clients
                                    if let Some(ref server) = socket_server {
                                        server.broadcast_activity("tui", &ws_name, "assistant_message", &response);
                                    }
                                }
                                Err(e) => {
                                    let _ = client_req.responder.send(socket::DaemonResponse::Error {
                                        text: format!("{e}"),
                                    });
                                }
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
                            let slot = &mut slots[idx];
                            info!("[{}] message from {user_name}: {text}", slot.name);

                            // Broadcast Telegram user message to TUI clients
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("telegram", &slot.name, "user_message", &text);
                            }

                            if let Some(channel) = get_channel(slot, &telegram_channels) {
                                channel.send_typing(chat_id, topic_id).await;
                                channel.send_reaction(chat_id, message_id, "🧠").await;

                                let slot_name = slot.name.clone();
                                let mut alerts: Vec<String> = Vec::new();

                                match slot.coordinator.handle_message(&text, &slot.store, |event| {
                                    match event {
                                        CoordinatorEvent::BashAudit { command, matched_pattern } => {
                                            warn!(
                                                "[{slot_name}] coordinator bash MUTATING ({matched_pattern}): {command}"
                                            );
                                        }
                                        CoordinatorEvent::FilesModified { files } => {
                                            let file_list: Vec<String> = files
                                                .iter()
                                                .map(|(repo, file)| format!("{repo}/{file}"))
                                                .collect();
                                            warn!(
                                                "[{slot_name}] coordinator modified files: {}",
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
                                    }
                                }).await {
                                    Ok(response) => {
                                        // Broadcast Telegram assistant response to TUI clients
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &response);
                                        }

                                        for alert in alerts {
                                            let alert_msg = OutboundMessage {
                                                chat_id,
                                                text: alert.clone(),
                                                buttons: vec![],
                                                topic_id,
                                            };
                                            let _ = channel.send_message(&alert_msg).await;
                                            // Broadcast safety alerts to TUI
                                            if let Some(ref server) = socket_server {
                                                server.broadcast_activity("system", &slot.name, "safety_alert", &alert);
                                            }
                                        }

                                        let msg = OutboundMessage {
                                            chat_id,
                                            text: response,
                                            buttons: vec![],
                                            topic_id,
                                        };
                                        if let Err(e) = channel.send_message(&msg).await {
                                            error!("[{}] failed to send response: {e}", slot.name);
                                        }
                                    }
                                    Err(e) => {
                                        error!("[{}] coordinator error: {e}", slot.name);
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
                                        slot.coordinator.reset_session();
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
                                    "help" => {
                                        let mut text = "Built-in commands:\n/status — show open signals\n/reset — reset coordinator session\n/help — this message".to_string();
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
                                                        // Return error to trigger run_foreground() restart loop
                                                        return ExitReason::Error(color_eyre::eyre::eyre!("restart requested by /{command}"));
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
pub fn ensure_daemon() -> Result<()> {
    if is_daemon_running() {
        return Ok(());
    }
    eprintln!("Starting daemon in the background...");
    spawn_background()
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

/// Format swarm events into a system notification for the coordinator.
fn format_system_notification(events: &[String]) -> String {
    let mut msg = String::from(
        "[System notification — swarm activity]\n\
         The following worker events just occurred:\n",
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

//! Multi-workspace daemon — event loop for all workspaces.
//!
//! Discovers workspace configs, builds per-workspace watcher registries,
//! shares Telegram connections by bot_token, and routes messages by (chat_id, topic_id).

pub mod doctor;
#[allow(dead_code)]
pub mod http;
pub mod morning_brief;
pub mod socket;

use std::{collections::HashMap, sync::Arc};

use color_eyre::eyre::{Result, WrapErr};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    buzz::{
        channel::{Channel, ChannelEvent, OutboundMessage, telegram::TelegramChannel},
        conversation::ConversationStore,
        coordinator::{
            Coordinator, CoordinatorEvent, DispatchBundle,
            prompt::format_signal_summary,
            skills::{
                SkillContext, build_skills_prompt, default_coordinator_disallowed_tools,
                default_coordinator_tools, observe_coordinator_disallowed_tools,
                observe_coordinator_tools,
            },
        },
        daemon::config as buzz_daemon_config,
        orchestrator::Orchestrator,
        signal::{Severity, store::SignalStore},
        watcher::{
            WatcherRegistry, email::EmailWatcher, github::GithubWatcher, linear::LinearWatcher,
            notion::NotionWatcher, review_queue::ReviewQueueWatcher, script::ScriptWatcher,
            sentry::SentryWatcher, swarm::SwarmWatcher,
        },
    },
    config::{
        self, BeeConfig, Workspace, WorkspaceConfig, db_path, log_path, pid_path, to_buzz_config,
    },
    git_safety::GitSafetyHooks,
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

/// How long the user must be idle before we send a nudge (10 minutes).
const IDLE_NUDGE_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Minimum time between consecutive nudges (30 minutes).
const IDLE_NUDGE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30 * 60);

struct BeeSlot {
    name: String,
    coord_tx: mpsc::UnboundedSender<CoordinatorJob>,
    coord_handle: Option<tokio::task::JoinHandle<()>>,
    max_session_turns: u32,
    coord_respawn_count: u32,
    coord_last_respawn: Option<std::time::Instant>,
    last_user_input: Option<std::time::Instant>,
    last_nudge: Option<std::time::Instant>,
    /// Heartbeat interval. None = no heartbeat.
    heartbeat_interval: Option<std::time::Duration>,
    /// Heartbeat prompt to send.
    heartbeat_prompt: Option<String>,
    /// When the last heartbeat fired (initialized to now so first fire waits one full interval).
    last_heartbeat: Option<std::time::Instant>,
}

/// A workspace slot in the daemon — holds per-workspace state.
struct WorkspaceSlot {
    name: String,
    config: WorkspaceConfig,
    registry: WatcherRegistry,
    bees: Vec<BeeSlot>,
    bee_map: HashMap<String, usize>,
    store: SignalStore,
    orchestrator: Orchestrator,
    morning_brief: Option<morning_brief::MorningBriefScheduler>,
    /// DB path for reopening SignalStore on coordinator respawn.
    db_path: std::path::PathBuf,
    /// Broadcast sender for web UI WebSocket updates.
    web_updates_tx: Option<tokio::sync::broadcast::Sender<http::WsUpdate>>,
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
        bee_name: String,
    },
    /// Handle a TUI chat message with streaming tokens.
    TuiChat {
        text: String,
        responder: mpsc::UnboundedSender<socket::DaemonResponse>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        ws_name: String,
        bee_name: String,
    },
    /// Reset the coordinator session.
    ResetSession,
    /// Clear the coordinator session (hard reset, no context carried forward).
    Clear {
        /// If Some, send confirmation via Telegram.
        telegram: Option<(TelegramChannel, i64, Option<i64>)>,
        /// If Some, send confirmation via TUI responder.
        tui_responder: Option<mpsc::UnboundedSender<socket::DaemonResponse>>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        slot_name: String,
    },
    /// Compact the coordinator session (summarize context to memory, then reset).
    Compact {
        /// If Some, send confirmation via Telegram.
        telegram: Option<(TelegramChannel, i64, Option<i64>)>,
        /// If Some, send confirmation via TUI responder.
        tui_responder: Option<mpsc::UnboundedSender<socket::DaemonResponse>>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        slot_name: String,
    },
    /// Queue context to be prepended to the next TUI user message. Used for
    /// built-in TUI command output (e.g. /doctor) that the coordinator should
    /// see without triggering a separate LLM turn. Only consumed by the
    /// `TuiChat` path; Telegram dispatches are unaffected.
    InjectContext { text: String },
    /// Coordinator follow-through triggered by a signal hook.
    SignalFollowThrough {
        signals: Vec<String>,
        source: String,
        prompt_override: Option<String>,
        action: Option<String>,
        queued_at: std::time::Instant,
        ttl_secs: u64,
        telegram: Option<(TelegramChannel, i64, Option<i64>)>,
        socket_server: Option<Arc<socket::DaemonSocketServer>>,
        slot_name: String,
        /// Playbook skill names to load for this session.
        skill_names: Vec<String>,
        /// Workspace root for loading playbook files.
        workspace_root: std::path::PathBuf,
        /// Name of the Bee that owns this coordinator.
        bee_name: String,
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

fn conversation_scope(ws_name: &str, bee_name: &str) -> String {
    format!("{ws_name}/{bee_name}")
}

fn bee_matches_signal_source(bee: &BeeConfig, source: &str) -> bool {
    bee.signal_hooks
        .iter()
        .any(|hook| source == hook.source || source.starts_with(&format!("{}_", hook.source)))
}

/// Per-workspace coordinator task — processes jobs serially to preserve session ordering.
async fn run_coordinator_task(
    mut coordinator: Coordinator,
    store: SignalStore,
    mut job_rx: mpsc::UnboundedReceiver<CoordinatorJob>,
    max_session_turns: u32,
    authority: crate::config::WorkspaceAuthority,
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
                bee_name,
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

                let bundle = match coordinator.prepare_dispatch(&store) {
                    Ok(b) => b,
                    Err(e) => {
                        error!("[{slot_name}] failed to build coordinator options: {e}");
                        typing_cancel.cancel();
                        continue;
                    }
                };

                let name_for_cb = slot_name.clone();
                let mut alerts: Vec<String> = Vec::new();

                let result = coordinator
                    .dispatch_message(&text, bundle, |event| match event {
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
                        CoordinatorEvent::Token(_) | CoordinatorEvent::Usage(_) => {}
                    })
                    .await;

                // Stop typing indicator
                typing_cancel.cancel();

                // Persist messages to DB (scoped to drop before await)
                {
                    let conv_scope = conversation_scope(&slot_name, &bee_name);
                    let conv = ConversationStore::new(store.conn(), &conv_scope);
                    if let Err(e) = conv.save_message("user", &text, Some("telegram"), None, None) {
                        warn!("[{slot_name}] failed to save user message: {e}");
                    }
                    if let Ok(ref response) = result
                        && !response.trim().is_empty()
                    {
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

                        if !response.trim().is_empty()
                            && let Some(ref server) = socket_server
                        {
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

                        // If the coordinator only used tools and produced no text,
                        // send a brief fallback so the user knows the run completed.
                        let final_text = if response.trim().is_empty() {
                            "✅ Done.".to_string()
                        } else {
                            response
                        };

                        let msg = OutboundMessage {
                            chat_id,
                            text: final_text,
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
                bee_name,
            } => {
                let bundle = match coordinator.prepare_dispatch(&store) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = responder.send(socket::DaemonResponse::Error {
                            workspace: ws_name.clone(),
                            text: format!("{e}"),
                        });
                        continue;
                    }
                };

                // Prepend any pending context (e.g. /doctor output) to
                // the user message so the coordinator sees it inline.
                let pending_ctx = coordinator.take_pending_context();
                let dispatch_text = if let Some(ref ctx) = pending_ctx {
                    format!("{ctx}\n\n---\n\nUser message: {text}")
                } else {
                    text.clone()
                };

                let name_for_cb = ws_name.clone();
                let model_for_cb = coordinator.model().to_string();
                let responder_for_cb = responder.clone();

                let result = coordinator
                    .dispatch_message(&dispatch_text, bundle, |event| match event {
                        CoordinatorEvent::Token(t) => {
                            let _ = responder_for_cb.send(socket::DaemonResponse::Token {
                                workspace: name_for_cb.clone(),
                                text: t,
                            });
                        }
                        CoordinatorEvent::Usage(stats) => {
                            let _ = responder_for_cb.send(socket::DaemonResponse::Usage {
                                workspace: name_for_cb.clone(),
                                input_tokens: stats.input_tokens,
                                output_tokens: stats.output_tokens,
                                cache_read_tokens: stats.cache_read_tokens,
                                total_cost_usd: stats.total_cost_usd,
                                context_window: crate::buzz::coordinator::max_context_tokens(
                                    &model_for_cb,
                                ),
                            });
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
                    let conv_scope = conversation_scope(&ws_name, &bee_name);
                    let conv = ConversationStore::new(store.conn(), &conv_scope);
                    if let Err(e) = conv.save_message("user", &text, Some("tui"), None, None) {
                        warn!("[{ws_name}] failed to save user message: {e}");
                    }
                    // Only persist non-empty assistant responses (tool-only turns
                    // produce empty text which clutters history).
                    if let Ok(ref response) = result
                        && !response.trim().is_empty()
                    {
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
                        let _ = responder.send(socket::DaemonResponse::Done {
                            workspace: ws_name.clone(),
                        });
                        // Only broadcast non-empty responses (tool-only turns
                        // have no text to show).
                        if !response.trim().is_empty()
                            && let Some(ref server) = socket_server
                        {
                            server.broadcast_activity(
                                "tui",
                                &ws_name,
                                "assistant_message",
                                &response,
                            );
                        }

                        // Parse and execute action markers from chat responses
                        let bee_actions =
                            crate::buzz::coordinator::actions::parse_actions(&response);
                        if !bee_actions.is_empty() {
                            let ws_root = crate::config::discover_workspaces()
                                .ok()
                                .and_then(|ws| {
                                    ws.into_iter()
                                        .find(|w| w.name == ws_name)
                                        .map(|w| w.config.root)
                                })
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            execute_bee_actions(
                                &bee_actions,
                                &store,
                                &ws_name,
                                &bee_name,
                                &ws_root,
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
                        // Restore pending context so it's available on the
                        // next attempt — the coordinator never ingested it.
                        if let Some(ctx) = pending_ctx {
                            coordinator.set_pending_context(ctx);
                        }
                        // If session resume failed, reset and try fresh next time
                        if coordinator.has_session() {
                            warn!(
                                "[{ws_name}] resetting session after error (possible expired resume token)"
                            );
                            coordinator.reset_session();
                            turn_count = 0;
                        }
                        let _ = responder.send(socket::DaemonResponse::Error {
                            workspace: ws_name.clone(),
                            text: format!("{e}"),
                        });
                    }
                }
            }

            CoordinatorJob::InjectContext { text } => {
                // Store context on the coordinator so it is prepended to the
                // next real user message. No LLM turn is triggered here —
                // the coordinator will see the context when the user sends
                // their next message.
                coordinator.set_pending_context(text);
            }

            CoordinatorJob::ResetSession => {
                coordinator.reset_session();
                turn_count = 0;
            }

            CoordinatorJob::Clear {
                telegram,
                tui_responder,
                socket_server,
                slot_name,
            } => {
                coordinator.reset_session();
                turn_count = 0;
                info!("[{slot_name}] session cleared via /clear command");

                let msg_text = "\u{1f5d1}\u{fe0f} Session cleared. Starting fresh.";
                if let Some(ref server) = socket_server {
                    // Broadcast session_reset so TUI can reset turn counter
                    server.broadcast_activity("system", &slot_name, "session_reset", msg_text);
                }
                if let Some((channel, chat_id, topic_id)) = telegram {
                    let msg = OutboundMessage {
                        chat_id,
                        text: msg_text.to_string(),
                        buttons: vec![],
                        topic_id,
                    };
                    if let Err(e) = channel.send_message(&msg).await {
                        error!("[{slot_name}] failed to send /clear confirmation: {e}");
                    }
                }
                if let Some(responder) = tui_responder {
                    let _ = responder.send(socket::DaemonResponse::Token {
                        workspace: slot_name.clone(),
                        text: msg_text.to_string(),
                    });
                    let _ = responder.send(socket::DaemonResponse::Done {
                        workspace: slot_name,
                    });
                }
            }

            CoordinatorJob::Compact {
                telegram,
                tui_responder,
                socket_server,
                slot_name,
            } => {
                info!("[{slot_name}] session compact via /compact command");

                // If we have an active session, ask the coordinator to summarize
                let mut saved_to_memory = false;
                if coordinator.has_session() {
                    let summary_prompt = "Summarize the current session in 3-5 bullet points of key context: decisions made, tasks in flight, important state. Output ONLY the bullet points, nothing else.";

                    let bundle = coordinator.prepare_dispatch(&store);
                    if let Ok(bundle) = bundle {
                        match coordinator
                            .dispatch_message(summary_prompt, bundle, |_| {})
                            .await
                        {
                            Ok(summary) => {
                                let summary = summary.trim();
                                if !summary.is_empty() {
                                    // Save summary to memory store
                                    let mem = crate::buzz::coordinator::memory::MemoryStore::new(
                                        store.conn(),
                                        store.workspace(),
                                    );
                                    let entry = format!(
                                        "Session compact ({}): {}",
                                        chrono::Local::now().format("%Y-%m-%d %H:%M"),
                                        summary
                                    );
                                    match mem.add(
                                        crate::buzz::coordinator::memory::MemoryCategory::Observation,
                                        &entry,
                                    ) {
                                        Ok(_) => saved_to_memory = true,
                                        Err(e) => warn!("[{slot_name}] failed to save compact summary to memory: {e}"),
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("[{slot_name}] failed to get compact summary: {e}");
                            }
                        }
                    }
                }

                coordinator.reset_session();
                turn_count = 0;

                let msg_text = if saved_to_memory {
                    "\u{1f5dc}\u{fe0f} Session compacted \u{2014} key context saved to memory. Starting fresh."
                } else {
                    "\u{1f5dc}\u{fe0f} Session compacted. Starting fresh."
                };
                if let Some(ref server) = socket_server {
                    // Broadcast session_reset so TUI can reset turn counter
                    server.broadcast_activity("system", &slot_name, "session_reset", msg_text);
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
                        workspace: slot_name.clone(),
                        text: msg_text.to_string(),
                    });
                    let _ = responder.send(socket::DaemonResponse::Done {
                        workspace: slot_name,
                    });
                }
            }

            CoordinatorJob::SignalFollowThrough {
                signals,
                source,
                prompt_override,
                action,
                queued_at,
                ttl_secs,
                telegram,
                socket_server,
                slot_name,
                skill_names,
                workspace_root,
                bee_name,
            } => {
                let has_session = coordinator.has_session();
                info!(
                    "[follow-through] hook matched: source={source} signal_count={} has_session={has_session} ttl_secs={ttl_secs} model={}",
                    signals.len(),
                    coordinator.model()
                );
                let elapsed = queued_at.elapsed().as_secs();
                if elapsed >= ttl_secs {
                    info!(
                        "[follow-through] skipped (TTL expired): source={source} signal_count={} queued_ago_secs={elapsed} ttl_secs={ttl_secs}",
                        signals.len()
                    );
                    continue;
                }

                let has_action = action
                    .as_deref()
                    .map(|a| !a.trim().is_empty())
                    .unwrap_or(false);
                if !has_action && !has_session {
                    info!(
                        "[follow-through] skipped (no session, no action): source={source} signal_count={}",
                        signals.len()
                    );
                    continue;
                }
                if has_action && !has_session {
                    info!(
                        "[follow-through] firing without active session (action hook): source={source} signal_count={}",
                        signals.len()
                    );
                }

                let mut notification = if let Some(ref tpl) = prompt_override {
                    format_hook_notification(&source, &signals, tpl)
                } else {
                    format_system_notification(&source, &signals)
                };

                // Append action instruction so the coordinator knows what to DO
                if let Some(ref action_str) = action {
                    notification.push_str("\n\n[Action] ");
                    notification.push_str(action_str);
                }
                // Load hook-triggered playbooks
                let playbook_content = if !skill_names.is_empty() {
                    let mut content = String::new();
                    for name in &skill_names {
                        if let Some(pb) =
                            crate::buzz::coordinator::skills::load_playbook(&workspace_root, name)
                        {
                            if !content.is_empty() {
                                content.push_str("\n---\n\n");
                            }
                            content.push_str(&format!("### Playbook: {name}\n\n"));
                            content.push_str(&pb);
                        } else {
                            warn!("[{slot_name}] playbook not found: {name}");
                        }
                    }
                    if content.is_empty() {
                        None
                    } else {
                        Some(content)
                    }
                } else {
                    None
                };

                let saved_turns = coordinator.max_turns();
                let max_turns = 15;
                coordinator.set_max_turns(max_turns);

                let mut bundle = match coordinator
                    .prepare_dispatch_with_playbooks(&store, playbook_content.as_deref())
                {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("[{slot_name}] failed to build coordinator options: {e}");
                        coordinator.set_max_turns(saved_turns);
                        continue;
                    }
                };

                // For Claude: run follow-throughs in a fresh session so any
                // per-session overrides stay isolated.
                if let DispatchBundle::Claude(ref mut opts) = bundle {
                    opts.resume = None;
                    // Strip the validate-bash PreToolUse hook from follow-through
                    // sessions. Hook deny decisions are cached by Claude Code and
                    // can bleed into the user's interactive session if left in.
                    // Follow-throughs have limited max_turns and don't need bash
                    // auditing; the hook stays active on the main interactive
                    // coordinator session only.
                    opts.settings = None;

                    // In observe mode, strip Bash to enforce read-only.
                    if authority == crate::config::WorkspaceAuthority::Observe {
                        if !opts.disallowed_tools.iter().any(|t| t == "Bash") {
                            opts.disallowed_tools.push("Bash".to_string());
                        }
                        opts.allowed_tools.retain(|t| t != "Bash");
                    }
                }

                // Save the user's session token so we can restore it after the
                // follow-through (dispatch_message overwrites session_id
                // as a side-effect).
                let saved_session_token = coordinator.session_token().cloned();

                let action_snippet = action
                    .as_deref()
                    .map(|a| a.chars().take(80).collect::<String>())
                    .unwrap_or_default();
                info!(
                    "[follow-through] executing: source={source} signal_count={} action=\"{action_snippet}\"",
                    signals.len(),
                );

                let name_for_cb = slot_name.clone();
                let source_for_cb = source.clone();
                match coordinator
                    .dispatch_message(&notification, bundle, |event| match event {
                        CoordinatorEvent::BashAudit {
                            command,
                            matched_pattern,
                        } => {
                            let sanitized: String = command
                                .chars()
                                .take(120)
                                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                                .collect();
                            warn!(
                                "[{name_for_cb}] signal follow-through bash MUTATING ({matched_pattern}): {sanitized}"
                            );
                        }
                        CoordinatorEvent::FilesModified { files } => {
                            let file_list: Vec<String> = files
                                .iter()
                                .map(|(repo, file)| format!("{repo}/{file}"))
                                .collect();
                            warn!(
                                "[{name_for_cb}] signal follow-through MUTATED FILES source={source_for_cb} files={file_list:?}"
                            );
                        }
                        _ => {}
                    })
                    .await
                {
                    Ok(response) => {
                        let response = response.trim().to_string();
                        info!(
                            "[follow-through] completed: source={source} signal_count={} response_len={} empty={}",
                            signals.len(),
                            response.len(),
                            response.is_empty()
                        );
                        if !response.is_empty() && response.to_lowercase() != "ack" {
                            let notification_text = format!(
                                "[signal: {source}] {response}"
                            );

                            // Broadcast to TUI clients as an assistant message so
                            // the response renders as a normal assistant chat bubble
                            // rather than a dim system status line.
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity(
                                    "signal",
                                    &slot_name,
                                    "assistant_message",
                                    &notification_text,
                                );
                            }

                            // Always send to Telegram when configured.
                            if let Some((ref channel, chat_id, topic_id)) = telegram {
                                let msg = OutboundMessage {
                                    chat_id,
                                    text: notification_text,
                                    buttons: vec![],
                                    topic_id,
                                };
                                if let Err(e) = channel.send_message(&msg).await {
                                    error!("[{slot_name}] failed to send follow-through: {e}");
                                }
                            }
                        } else {
                            info!(
                                "[{slot_name}] coordinator ack'd {source} events (no message sent)"
                            );
                        }
                        // Check if the follow-through exhausted its turn budget.
                        let used = coordinator.last_num_turns();
                        if used >= max_turns as u64 {
                            warn!(
                                "signal follow-through exhausted max_turns ({max_turns}) for source={source}"
                            );
                        }

                        // Parse and execute action markers from the Bee's response.
                        let bee_actions =
                            crate::buzz::coordinator::actions::parse_actions(&response);
                        if !bee_actions.is_empty() {
                            info!(
                                "[{slot_name}] executing {} action marker(s) from {bee_name}",
                                bee_actions.len()
                            );
                            // Look up workspace root for canvas writes
                            let ws_root = crate::config::discover_workspaces()
                                .ok()
                                .and_then(|ws| {
                                    ws.into_iter()
                                        .find(|w| w.name == slot_name)
                                        .map(|w| w.config.root)
                                })
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            execute_bee_actions(
                                &bee_actions,
                                &store,
                                &slot_name,
                                &bee_name,
                                &ws_root,
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[follow-through] error: source={source} signal_count={} err={e}",
                            signals.len()
                        );
                    }
                }

                // Restore the user's session so subsequent messages resume
                // the original conversation (not the follow-through's session).
                if let Some(token) = saved_session_token {
                    coordinator.restore_session(token);
                }

                coordinator.set_max_turns(saved_turns);
            }
        }
    }
}

/// Execute parsed Bee action markers against the signal/task stores.
fn execute_bee_actions(
    actions: &[crate::buzz::coordinator::actions::BeeAction],
    store: &crate::buzz::signal::store::SignalStore,
    slot_name: &str,
    bee_name: &str,
    config_root: &std::path::Path,
) {
    use crate::buzz::coordinator::actions::BeeAction;
    use crate::buzz::signal::{Severity, SignalUpdate};

    for action in actions {
        match action {
            BeeAction::Dismiss { signal_id } => match store.resolve_signal(*signal_id) {
                Ok(()) => info!("[{slot_name}] action: dismissed signal {signal_id}"),
                Err(e) => warn!("[{slot_name}] action: failed to dismiss signal {signal_id}: {e}"),
            },
            BeeAction::Escalate { message } => {
                let external_id = format!(
                    "escalation-{}-{}",
                    bee_name,
                    chrono::Utc::now().timestamp_millis()
                );
                let update =
                    SignalUpdate::new("escalation", &external_id, message, Severity::Critical);
                match store.upsert_signal(&update) {
                    Ok((id, _)) => {
                        info!("[{slot_name}] action: escalated signal id={id}: {message}")
                    }
                    Err(e) => warn!("[{slot_name}] action: failed to escalate: {e}"),
                }
            }
            BeeAction::Fix { description } => {
                let source = format!("bee_{bee_name}");
                let external_id =
                    format!("fix-{}-{}", bee_name, chrono::Utc::now().timestamp_millis());
                let update = SignalUpdate::new(&source, &external_id, description, Severity::Error);
                match store.upsert_signal(&update) {
                    Ok((id, _)) => info!(
                        "[{slot_name}] action: fix signal id={id} source={source}: {description}"
                    ),
                    Err(e) => warn!("[{slot_name}] action: failed to create fix signal: {e}"),
                }
            }
            BeeAction::Snooze { signal_id, hours } => {
                let until = chrono::Utc::now() + chrono::Duration::hours(*hours as i64);
                match store.snooze_signal(*signal_id, until) {
                    Ok(()) => {
                        info!("[{slot_name}] action: snoozed signal {signal_id} for {hours}h")
                    }
                    Err(e) => {
                        warn!("[{slot_name}] action: failed to snooze signal {signal_id}: {e}")
                    }
                }
            }
            BeeAction::Task { title } => {
                // Open a TaskStore on the same DB and create a task in Triage stage.
                let task = crate::buzz::task::Task {
                    id: uuid::Uuid::new_v4().to_string(),
                    workspace: store.workspace().to_string(),
                    title: title.clone(),
                    stage: crate::buzz::task::TaskStage::Triage,
                    source: Some(format!("bee_{bee_name}")),
                    source_url: None,
                    worker_id: None,
                    pr_url: None,
                    pr_number: None,
                    repo: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    resolved_at: None,
                    metadata: serde_json::Value::Object(serde_json::Map::new()),
                };
                match crate::buzz::task::store::TaskStore::open(store.db_path()) {
                    Ok(task_store) => match task_store.create_task(&task) {
                        Ok(()) => info!("[{slot_name}] action: created task \"{title}\""),
                        Err(e) => warn!("[{slot_name}] action: failed to create task: {e}"),
                    },
                    Err(e) => warn!("[{slot_name}] action: failed to open task store: {e}"),
                }
            }
            BeeAction::Research { topic } => {
                // Research is handled by the web UI / ResearchBee — just log it.
                info!("[{slot_name}] action: research requested (handled elsewhere): {topic}");
            }
            BeeAction::Canvas { content } => {
                // Write canvas content to .apiari/canvas/{bee_name}.md
                let canvas_dir = config_root.join(".apiari/canvas");
                if let Err(e) = std::fs::create_dir_all(&canvas_dir) {
                    warn!("[{slot_name}] action: failed to create canvas dir: {e}");
                } else {
                    let path = canvas_dir.join(format!("{bee_name}.md"));
                    // Prepend new content with date header
                    let date = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
                    let new_section = format!("## {date}\n\n{content}\n\n---\n\n");
                    let existing = std::fs::read_to_string(&path).unwrap_or_default();
                    match std::fs::write(&path, format!("{new_section}{existing}")) {
                        Ok(()) => info!(
                            "[{slot_name}/{bee_name}] canvas updated ({} bytes)",
                            content.len()
                        ),
                        Err(e) => warn!("[{slot_name}/{bee_name}] failed to write canvas: {e}"),
                    }
                }
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
    // Route (chat_id, topic_id) → (workspace index, bee index)
    let mut route_map: HashMap<RouteKey, (usize, usize)> = HashMap::new();
    // Workspace name → slot index
    let mut name_map: HashMap<String, usize> = HashMap::new();

    for ws in &workspaces {
        let store = match SignalStore::open(&db, &ws.name) {
            Ok(s) => s,
            Err(e) => return ExitReason::Error(e),
        };
        let buzz_config = to_buzz_config(&ws.config);

        // Validate workspace-level schedule once at startup (warns on malformed active_hours).
        if let Some(ref ws_sched) = ws.config.schedule {
            crate::buzz::schedule::warn_if_invalid(ws_sched);
        }

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
            let gh_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .github
                    .as_ref()
                    .and_then(|g| g.active_hours.as_deref()),
                "github",
            );
            registry.add_with_interval_and_schedule(
                Box::new(gh_watcher),
                gh_config.interval_secs,
                gh_sched.clone(),
            );

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
                registry.add_with_interval_and_schedule(
                    Box::new(ReviewQueueWatcher::new(gh_config)),
                    gh_config.interval_secs,
                    gh_sched,
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
            let sentry_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .sentry
                    .as_ref()
                    .and_then(|s| s.active_hours.as_deref()),
                "sentry",
            );
            registry.add_with_interval_and_schedule(
                Box::new(SentryWatcher::new(sentry_config.clone())),
                sentry_config.interval_secs,
                sentry_sched,
            );
        }

        if let Some(swarm_config) = &buzz_config.watchers.swarm
            && swarm_config.enabled
        {
            // Auto-start the swarm daemon if it isn't running
            ensure_swarm_daemon(&ws.config.root).await;

            info!(
                "[{}] enabling swarm watcher (daemon IPC, workspace: {})",
                ws.name,
                ws.config.root.display()
            );
            let swarm_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .swarm
                    .as_ref()
                    .and_then(|s| s.active_hours.as_deref()),
                "swarm",
            );
            registry.add_with_interval_and_schedule(
                Box::new(SwarmWatcher::new(ws.config.root.clone())),
                swarm_config.interval_secs,
                swarm_sched,
            );
        }

        for (i, email_config) in buzz_config.watchers.email.iter().enumerate() {
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
            let email_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .email
                    .get(i)
                    .and_then(|e| e.active_hours.as_deref()),
                &email_config.name,
            );
            registry.add_with_interval_and_schedule(
                Box::new(watcher),
                email_config.interval_secs,
                email_sched,
            );
        }

        for (i, notion_config) in buzz_config.watchers.notion.iter().enumerate() {
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
            let notion_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .notion
                    .get(i)
                    .and_then(|n| n.active_hours.as_deref()),
                &notion_config.name,
            );
            registry.add_with_interval_and_schedule(
                Box::new(watcher),
                notion_config.interval_secs,
                notion_sched,
            );
        }

        for (i, linear_config) in buzz_config.watchers.linear.iter().enumerate() {
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
            let linear_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .linear
                    .get(i)
                    .and_then(|l| l.active_hours.as_deref()),
                &linear_config.name,
            );
            registry.add_with_interval_and_schedule(
                Box::new(watcher),
                linear_config.poll_interval_secs,
                linear_sched,
            );
        }

        for (i, script_config) in buzz_config.watchers.script.iter().enumerate() {
            info!(
                "[{}] enabling script watcher '{}'",
                ws.name, script_config.name
            );
            let script_sched = effective_watcher_schedule(
                ws.config.schedule.as_ref(),
                ws.config
                    .watchers
                    .script
                    .get(i)
                    .and_then(|s| s.active_hours.as_deref()),
                &script_config.name,
            );
            registry.add_with_interval_and_schedule(
                Box::new(ScriptWatcher::new(script_config.clone())),
                script_config.interval_secs,
                script_sched,
            );
        }

        let bees = ws.config.resolved_bees();
        let mut bee_slots = Vec::with_capacity(bees.len());
        let mut bee_map = HashMap::with_capacity(bees.len());
        let slot_idx = slots.len();

        if let Some(tg) = &ws.config.telegram {
            route_map.insert(
                RouteKey {
                    chat_id: tg.chat_id,
                    topic_id: tg.topic_id,
                },
                (slot_idx, 0),
            );
        }

        for (bee_idx, bee) in bees.iter().enumerate() {
            let mut coordinator = build_bee_coordinator(&ws.name, bee, &ws.config);

            if let Some(tg) = &ws.config.telegram
                && let Some(topic_id) = bee.topic_id
            {
                route_map.insert(
                    RouteKey {
                        chat_id: tg.chat_id,
                        topic_id: Some(topic_id),
                    },
                    (slot_idx, bee_idx),
                );
            }

            restore_coordinator_session(&mut coordinator, &store, &ws.name, &bee.name);

            let coord_store = match SignalStore::open(&db, &ws.name) {
                Ok(s) => s,
                Err(e) => return ExitReason::Error(e),
            };
            let (coord_tx, coord_rx) = mpsc::unbounded_channel::<CoordinatorJob>();
            let coord_handle = tokio::spawn(run_coordinator_task(
                coordinator,
                coord_store,
                coord_rx,
                bee.max_session_turns,
                ws.config.authority,
            ));

            bee_map.insert(bee.name.clone(), bee_idx);
            let hb_interval = bee.heartbeat_duration();
            let hb_prompt = bee.heartbeat_prompt.clone();
            bee_slots.push(BeeSlot {
                name: bee.name.clone(),
                coord_tx,
                coord_handle: Some(coord_handle),
                max_session_turns: bee.max_session_turns,
                coord_respawn_count: 0,
                coord_last_respawn: None,
                last_user_input: None,
                last_nudge: None,
                heartbeat_interval: hb_interval,
                heartbeat_prompt: hb_prompt,
                // Initialize to now so Bees wait one full interval before first heartbeat.
                last_heartbeat: if hb_interval.is_some() {
                    Some(std::time::Instant::now())
                } else {
                    None
                },
            });
        }

        // Load workflow graph from workspace, falling back to builtin
        let workflow_yaml_path = ws.config.root.join(".apiari/workflow.yaml");
        let workflow_graph =
            crate::buzz::orchestrator::graph::builtin::load_workflow(Some(&workflow_yaml_path))
                .unwrap_or_else(|e| {
                    warn!(
                        "[{}] failed to load workflow.yaml: {e}, using builtin",
                        ws.name
                    );
                    crate::buzz::orchestrator::graph::builtin::builtin_workflow()
                });
        info!(
            "[{}] workflow graph '{}' loaded ({} nodes, {} edges)",
            ws.name,
            workflow_graph.name,
            workflow_graph.nodes.len(),
            workflow_graph.edges.len(),
        );
        let orchestrator = Orchestrator::with_graph(&ws.config.orchestrator, workflow_graph);

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
            bees: bee_slots,
            bee_map,
            store,
            orchestrator,
            morning_brief: morning_brief_scheduler,
            db_path: db.clone(),
            web_updates_tx: None,
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

    // Start web UI HTTP server (port 7422) for the first workspace.
    // The web UI shows the workflow graph and live task state.
    let mut web_signal_rx: Option<mpsc::UnboundedReceiver<http::InjectSignal>> = None;
    let mut web_chat_rx: Option<mpsc::UnboundedReceiver<http::WebChatRequest>> = None;
    if let Some(first_slot) = slots.first() {
        let graph = first_slot.orchestrator.workflow_graph().clone();
        let yaml_path = first_slot.config.root.join(".apiari/workflow.yaml");
        match http::start_http_server(
            graph,
            Some(yaml_path),
            first_slot.db_path.clone(),
            first_slot.name.clone(),
            7422,
        )
        .await
        {
            Ok((updates_tx, signal_rx, chat_rx)) => {
                // Store the broadcast sender on the slot so signal processing can push updates
                // (we'll set it on the mutable slot below)
                if let Some(slot) = slots.first_mut() {
                    slot.web_updates_tx = Some(updates_tx);
                }
                web_signal_rx = Some(signal_rx);
                web_chat_rx = Some(chat_rx);
                info!("[daemon] web UI server started on http://127.0.0.1:7422");
            }
            Err(e) => {
                warn!("[daemon] failed to start web UI server: {e}");
            }
        }
    }

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

    let mut idle_timer = tokio::time::interval(std::time::Duration::from_secs(60));
    idle_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut prune_timer = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
    prune_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Prune old activity events on startup.
    for slot in &slots {
        let retention_days = slot.config.activity.retention_days;
        if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(&db)
            && let Err(e) = ae.prune(&slot.name, retention_days)
        {
            warn!("[{}] failed to prune activity events: {e}", slot.name);
        }
    }

    // Spawn background reconciliation tasks for each workspace
    let reconcile_cancel = cancel_rx.clone();
    for slot in &slots {
        let db_for_reconcile = slot.db_path.clone();
        let ws_name = slot.name.clone();
        let ws_name_log = ws_name.clone();
        let interval_secs = slot.config.orchestrator.reconcile_interval_secs;
        let mut cancel_watch = reconcile_cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Skip the first immediate tick
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = cancel_watch.changed() => break,
                    _ = interval.tick() => {
                        match crate::buzz::orchestrator::reconcile::run_reconciliation(
                            &db_for_reconcile, &ws_name,
                        ).await {
                            Ok(actions) => {
                                if !actions.is_empty() {
                                    info!(
                                        "[{ws_name}] reconciliation applied {} action(s)",
                                        actions.len()
                                    );
                                }
                            }
                            Err(e) => {
                                warn!("[{ws_name}] reconciliation error: {e}");
                            }
                        }
                    }
                }
            }
        });
        info!("[{ws_name_log}] reconciliation task started (interval: {interval_secs}s)");
    }

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

        // Helper: recv from web signal injection, else pend forever
        let web_recv = async {
            match web_signal_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };

        // Helper: recv from web chat, else pend forever
        let web_chat_recv = async {
            match web_chat_rx.as_mut() {
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

            // ── Web UI signal injection ──
            Some(sig) = web_recv => {
                if let Some(slot) = slots.first_mut() {
                    let now = chrono::Utc::now();
                    let signal = crate::buzz::signal::SignalRecord {
                        id: now.timestamp_millis(),
                        source: sig.source.clone(),
                        external_id: format!("web-{}", now.timestamp_millis()),
                        title: sig.title.clone(),
                        body: None,
                        severity: crate::buzz::signal::Severity::Info,
                        status: crate::buzz::signal::SignalStatus::Open,
                        url: None,
                        created_at: now,
                        updated_at: now,
                        resolved_at: None,
                        metadata: sig.metadata.map(|m| m.to_string()),
                        snoozed_until: None,
                    };
                    if let Ok(task_store) = crate::buzz::task::store::TaskStore::open(&slot.db_path) {
                        match slot.orchestrator.process_signal(&task_store, &slot.name, &signal) {
                            Ok(result) => {
                                info!(
                                    "[web] processed injected signal '{}': transitioned={}",
                                    sig.source, result.engine_result.transitioned,
                                );
                                // Broadcast to web UI clients
                                if let Some(ref tx) = slot.web_updates_tx {
                                    if let Some(task) = &result.engine_result.task {
                                        let _ = tx.send(http::WsUpdate::TaskUpdated {
                                            task: http::task_to_view(task),
                                        });
                                    }
                                    let _ = tx.send(http::WsUpdate::SignalProcessed {
                                        source: sig.source,
                                        title: sig.title,
                                    });
                                }
                            }
                            Err(e) => warn!("[web] failed to process signal: {e}"),
                        }
                    }
                }
            }

            // ── Web UI chat ──
            Some(chat_req) = web_chat_recv => {
                let ws_name = &chat_req.workspace;
                if let Some(&slot_idx) = name_map.get(ws_name) {
                    // Find the target bee
                    let bee_idx = chat_req.bee.as_deref()
                        .and_then(|name| slots[slot_idx].bee_map.get(name).copied())
                        .unwrap_or(0);
                    // Track last user input for heartbeat/nudge
                    if let Some(bee) = slots[slot_idx].bees.get_mut(bee_idx) {
                        bee.last_user_input = Some(std::time::Instant::now());
                        bee.last_nudge = None;
                    }
                    let slot = &slots[slot_idx];
                    if let Some(bee) = slot.bees.get(bee_idx) {
                        // Create a socket responder that bridges to the web chat response channel
                        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<socket::DaemonResponse>();
                        let response_tx = chat_req.response_tx.clone();

                        // Forward daemon responses to web chat events
                        tokio::spawn(async move {
                            while let Some(resp) = resp_rx.recv().await {
                                match resp {
                                    socket::DaemonResponse::Token { text, .. } => {
                                        let _ = response_tx.send(http::WebChatEvent::Token { text });
                                    }
                                    socket::DaemonResponse::Done { .. } => {
                                        let _ = response_tx.send(http::WebChatEvent::Done);
                                        break;
                                    }
                                    socket::DaemonResponse::Error { text, .. } => {
                                        let _ = response_tx.send(http::WebChatEvent::Error { text });
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        });

                        let bee_name = slot.bees.get(bee_idx)
                            .map(|b| b.name.clone())
                            .unwrap_or_default();
                        let job = CoordinatorJob::TuiChat {
                            text: chat_req.text,
                            responder: resp_tx,
                            socket_server: socket_server.clone(),
                            ws_name: ws_name.clone(),
                            bee_name,
                        };
                        if bee.coord_tx.send(job).is_err() {
                            let _ = chat_req.response_tx.send(http::WebChatEvent::Error {
                                text: "coordinator not running".to_string(),
                            });
                        }
                    } else {
                        let _ = chat_req.response_tx.send(http::WebChatEvent::Error {
                            text: "bee not found".to_string(),
                        });
                    }
                } else {
                    let _ = chat_req.response_tx.send(http::WebChatEvent::Error {
                        text: format!("workspace '{ws_name}' not found"),
                    });
                }
            }

            _ = poll_timer.tick() => {
                // ── Coordinator health check (before watchers so hook dispatches don't get dropped) ──
                for slot in &mut slots {
                    let resolved_bees = slot.config.resolved_bees();
                    for bee in &mut slot.bees {
                        let needs_respawn = match &bee.coord_handle {
                            Some(h) => h.is_finished(),
                            None => true,
                        };
                        if !needs_respawn {
                            continue;
                        }

                        if let Some(old_handle) = bee.coord_handle.take() {
                            match old_handle.await {
                                Ok(()) => {
                                    warn!("[{}/{}] coordinator task exited unexpectedly", slot.name, bee.name);
                                }
                                Err(e) if e.is_panic() => {
                                    let payload = e.into_panic();
                                    let msg = payload
                                        .downcast_ref::<&str>()
                                        .map(|s| s.to_string())
                                        .or_else(|| payload.downcast_ref::<String>().cloned())
                                        .unwrap_or_else(|| "(non-string panic)".to_string());
                                    error!("[{}/{}] coordinator task panicked: {msg}", slot.name, bee.name);
                                }
                                Err(e) => {
                                    error!("[{}/{}] coordinator task cancelled: {e}", slot.name, bee.name);
                                }
                            }
                        }

                        if let Some(last) = bee.coord_last_respawn
                            && last.elapsed() > std::time::Duration::from_secs(300)
                        {
                            bee.coord_respawn_count = 0;
                        }
                        let backoff_secs =
                            15u64.saturating_mul(1u64 << bee.coord_respawn_count.min(4));
                        if let Some(last) = bee.coord_last_respawn
                            && last.elapsed() < std::time::Duration::from_secs(backoff_secs)
                        {
                            warn!(
                                "[{}/{}] coordinator respawn backoff ({backoff_secs}s) — skipping this tick",
                                slot.name, bee.name
                            );
                            continue;
                        }

                        let Some(bee_config) = resolved_bees.iter().find(|cfg| cfg.name == bee.name) else {
                            error!("[{}/{}] missing bee config during respawn", slot.name, bee.name);
                            continue;
                        };
                        let mut coordinator =
                            build_bee_coordinator(&slot.name, bee_config, &slot.config);

                        let coord_store = match SignalStore::open(&slot.db_path, &slot.name) {
                            Ok(s) => s,
                            Err(e) => {
                                error!(
                                    "[{}/{}] failed to reopen SignalStore for respawn: {e}",
                                    slot.name, bee.name
                                );
                                continue;
                            }
                        };

                        restore_coordinator_session(&mut coordinator, &coord_store, &slot.name, &bee.name);

                        let (new_tx, new_rx) = mpsc::unbounded_channel::<CoordinatorJob>();
                        bee.coord_tx = new_tx;
                        bee.coord_handle = Some(tokio::spawn(run_coordinator_task(
                            coordinator,
                            coord_store,
                            new_rx,
                            bee.max_session_turns,
                            slot.config.authority,
                        )));
                        bee.coord_respawn_count += 1;
                        bee.coord_last_respawn = Some(std::time::Instant::now());
                        info!(
                            "[{}/{}] coordinator task respawned (attempt {})",
                            slot.name, bee.name, bee.coord_respawn_count
                        );
                    }
                }

                // ── Per-Bee heartbeats ──
                for slot in &mut slots {
                    let ws_name = slot.name.clone();
                    let workspace_root = slot.config.root.clone();
                    for bee in &mut slot.bees {
                        let Some(interval) = bee.heartbeat_interval else {
                            continue;
                        };
                        let Some(ref prompt) = bee.heartbeat_prompt else {
                            continue;
                        };

                        // Check if enough time has passed since last heartbeat
                        let should_fire = bee
                            .last_heartbeat
                            .is_none_or(|last| last.elapsed() >= interval);
                        if !should_fire {
                            continue;
                        }

                        bee.last_heartbeat = Some(std::time::Instant::now());

                        let bee_name = bee.name.clone();
                        let mut prompt = prompt.clone();

                        // Append canvas update instruction to heartbeat
                        prompt.push_str(
                            "\n\nIf you have notable findings or status updates, write a clean summary inside [CANVAS]...[/CANVAS] tags for your canvas display. If nothing notable, skip the canvas update."
                        );

                        info!("[{ws_name}/{bee_name}] heartbeat firing");

                        // Send heartbeat prompt as a coordinator follow-through job.
                        // The action field carries the heartbeat prompt so it fires
                        // even without an active session.
                        let job = CoordinatorJob::SignalFollowThrough {
                            signals: vec![],
                            source: "heartbeat".to_string(),
                            prompt_override: Some("Periodic heartbeat check:".to_string()),
                            action: Some(prompt),
                            queued_at: std::time::Instant::now(),
                            ttl_secs: 600,
                            telegram: slot.config.telegram.as_ref().and_then(|tg| {
                                telegram_channels
                                    .get(&tg.bot_token)
                                    .map(|ch| (ch.clone(), tg.chat_id, tg.topic_id))
                            }),
                            socket_server: socket_server.clone(),
                            slot_name: ws_name.clone(),
                            skill_names: vec![],
                            workspace_root: workspace_root.clone(),
                            bee_name: bee_name.clone(),
                        };

                        if bee.coord_tx.send(job).is_err() {
                            warn!("[{ws_name}/{bee_name}] heartbeat: coordinator not available");
                        }
                    }
                }

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
                                model: slot.config.resolved_bees()[0].model.clone(),
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

                    // Collect orchestrator results for follow-through actions
                    let mut orchestrator_matched_actions: Vec<crate::buzz::orchestrator::MatchedAction> = Vec::new();
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

                        // Schedule check: effective_schedule was precomputed at registration.
                        if !crate::buzz::schedule::is_within_active_hours(throttled.effective_schedule()) {
                            tracing::trace!(
                                "[{}] [{}] skipping poll: outside active hours",
                                slot.name,
                                watcher_name
                            );
                            continue;
                        }
                        let poll_result = tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            throttled.watcher_mut().poll(&slot.store),
                        )
                        .await;
                        let poll_result = match poll_result {
                            Ok(inner) => inner,
                            Err(_) => {
                                error!("[{}] [{}] poll timed out after 30s", slot.name, watcher_name);
                                let _ = slot.store.set_cursor(&watcher_name, "error: poll timed out");
                                throttled.mark_polled();
                                continue;
                            }
                        };
                        match poll_result {
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
                                            // Process signal through orchestrator (task engine + notification routing + action matching)
                                            if is_new
                                                && let Ok(Some(record)) = slot.store.get_signal(id)
                                                && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                            {
                                                match slot.orchestrator.process_signal(
                                                    &task_store,
                                                    &slot.name,
                                                    &record,
                                                ) {
                                                    Ok(orch_result) => {
                                                        // Collect matched actions for follow-through dispatch
                                                        orchestrator_matched_actions.extend(orch_result.matched_actions);

                                                        // Execute workflow actions (system PR creation, reviewer dispatch, etc.)
                                                        for wf_action in &orch_result.workflow_actions {
                                                            execute_workflow_action(
                                                                wf_action,
                                                                &slot.config.root,
                                                                slot.store.db_path(),
                                                                &slot.name,
                                                            );
                                                        }

                                                        let engine_result = orch_result.engine_result;
                                                        for (worker_id, message) in engine_result.worker_messages {
                                                            info!("[task-engine] forwarding to worker {worker_id}: {message}");
                                                            let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(
                                                                slot.config.root.clone(),
                                                            );
                                                            tokio::spawn(async move {
                                                                if let Err(e) = swarm.send_message(&worker_id, &message).await {
                                                                    tracing::warn!(
                                                                        "[task-engine] failed to forward to worker {worker_id}: {e}"
                                                                    );
                                                                }
                                                            });
                                                        }
                                                        for notification in &engine_result.notifications {
                                                            if let Some(ref server) = socket_server {
                                                                server.broadcast_activity(
                                                                    "task",
                                                                    &slot.name,
                                                                    "transition",
                                                                    notification,
                                                                );
                                                            }
                                                        }
                                                        // Broadcast to web UI clients
                                                        if let Some(ref web_tx) = slot.web_updates_tx {
                                                            if let Some(ref task) = engine_result.task {
                                                                let _ = web_tx.send(http::WsUpdate::TaskUpdated {
                                                                    task: http::task_to_view(task),
                                                                });
                                                            }
                                                            let _ = web_tx.send(http::WsUpdate::SignalProcessed {
                                                                source: record.source.clone(),
                                                                title: record.title.clone(),
                                                            });
                                                            let _ = web_tx.send(http::WsUpdate::Signal {
                                                                id: record.id,
                                                                workspace: slot.name.clone(),
                                                                source: record.source.clone(),
                                                                title: record.title.clone(),
                                                                severity: format!("{:?}", record.severity),
                                                                url: record.url.clone(),
                                                                created_at: record.created_at.to_rfc3339(),
                                                            });
                                                        }
                                                        // Log activity events for signal match and stage change.
                                                        if let Some(ref task) = engine_result.task
                                                            && let Ok(ae) = crate::buzz::task::ActivityEventStore::open(slot.store.db_path()) {
                                                                // Log signal event
                                                                let _ = ae.log_event(
                                                                    &slot.name,
                                                                    Some(&task.id),
                                                                    "signal",
                                                                    &format!("Signal: {}", record.title),
                                                                    record.body.as_deref(),
                                                                    Some(&record.source),
                                                                    Some(record.id),
                                                                    None,
                                                                );
                                                                // Log stage_change if a transition occurred
                                                                if engine_result.transitioned
                                                                    && let Some(ref from) = engine_result.from_stage
                                                                {
                                                                    let to = &task.stage;
                                                                    if from != to {
                                                                        let meta = serde_json::json!({
                                                                            "from": from.as_str(),
                                                                            "to": to.as_str(),
                                                                            "reason": record.source,
                                                                        });
                                                                        let _ = ae.log_event(
                                                                            &slot.name,
                                                                            Some(&task.id),
                                                                            "stage_change",
                                                                            &format!("{} → {}", from.as_str(), to.as_str()),
                                                                            None,
                                                                            Some(&record.source),
                                                                            Some(record.id),
                                                                            Some(&meta.to_string()),
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("[task-engine] error processing signal: {e}");
                                                    }
                                                }
                                            }

                                            // Create task for new swarm workers (skip reviewer workers)
                                            if is_new && (update.source == "swarm_worker_spawned" || (update.source == "swarm" && update.external_id.starts_with("swarm-spawned-"))) {
                                                let worker_id = update.external_id.strip_prefix("swarm-spawned-").unwrap_or("").to_string();
                                                let is_reviewer = update.body.as_ref()
                                                    .and_then(|b| b.lines().nth(1))
                                                    .map(|l| l.trim_start().starts_with("Review PR"))
                                                    .unwrap_or(false);
                                                if !is_reviewer && !worker_id.is_empty()
                                                    && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                        && let Ok(None) = task_store.find_task_by_worker(&slot.name, &worker_id) {
                                                            let title = update.body.as_ref()
                                                                .and_then(|b| b.lines().nth(1))
                                                                .map(|s| s.trim().to_string())
                                                                .filter(|s| !s.is_empty())
                                                                .unwrap_or_else(|| format!("Worker {worker_id}"));
                                                            let title = if title.len() > 80 { format!("{}…", &title[..79]) } else { title };
                                                            let now = chrono::Utc::now();
                                                            let task = crate::buzz::task::Task {
                                                                id: uuid::Uuid::new_v4().to_string(),
                                                                workspace: slot.name.clone(),
                                                                title,
                                                                stage: crate::buzz::task::TaskStage::InProgress,
                                                                source: Some("swarm".to_string()),
                                                                source_url: None,
                                                                worker_id: Some(worker_id.clone()),
                                                                pr_url: None,
                                                                pr_number: None,
                                                                repo: None,
                                                                created_at: now,
                                                                updated_at: now,
                                                                resolved_at: None,
                                                                metadata: serde_json::json!({}),
                                                            };
                                                            if let Err(e) = task_store.create_task(&task) {
                                                                tracing::warn!("[task-engine] failed to create task for worker {worker_id}: {e}");
                                                            } else {
                                                                info!("[task-engine] created task '{}' for worker {worker_id}", task.title);
                                                                // Log worker spawn and initial stage to activity feed.
                                                                if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(slot.store.db_path()) {
                                                                    let meta = serde_json::json!({
                                                                        "from": serde_json::Value::Null,
                                                                        "to": "In Progress",
                                                                        "worker_id": worker_id,
                                                                    });
                                                                    let _ = ae.log_event(
                                                                        &slot.name,
                                                                        Some(&task.id),
                                                                        "stage_change",
                                                                        &format!("Task created: {}", task.title),
                                                                        None,
                                                                        Some("swarm"),
                                                                        None,
                                                                        Some(&meta.to_string()),
                                                                    );
                                                                    let _ = ae.log_event(
                                                                        &slot.name,
                                                                        Some(&task.id),
                                                                        "worker",
                                                                        &format!("Worker {} spawned", worker_id),
                                                                        None,
                                                                        Some("swarm"),
                                                                        None,
                                                                        Some(&serde_json::json!({"worker_id": worker_id}).to_string()),
                                                                    );
                                                                }
                                                            }
                                                        }
                                            }

                                            // Update task when worker opens a PR
                                            if is_new && (update.source == "swarm_pr_opened" || (update.source == "swarm" && update.external_id.starts_with("swarm-pr-"))) {
                                                let worker_id = update.external_id.strip_prefix("swarm-pr-").unwrap_or("").to_string();
                                                if !worker_id.is_empty()
                                                    && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                        && let Ok(Some(task)) = task_store.find_task_by_worker(&slot.name, &worker_id) {
                                                            let pr_url = update.url.clone();
                                                            let pr_number = pr_url.as_ref()
                                                                .and_then(|u| crate::buzz::orchestrator::extract_github_pr_from_url(u))
                                                                .map(|(_, num)| num);
                                                            let repo = pr_url.as_ref()
                                                                .and_then(|u| crate::buzz::orchestrator::extract_github_pr_from_url(u))
                                                                .map(|(r, _)| r);

                                                            if let Some(ref url) = pr_url
                                                                && let Some(num) = pr_number
                                                                    && let Err(e) = task_store.update_task_pr(&task.id, url, num) {
                                                                        tracing::warn!("[task-engine] failed to update PR on task: {e}");
                                                                    }
                                                            if let Some(ref r) = repo
                                                                && let Err(e) = task_store.update_task_repo(&task.id, r) {
                                                                    tracing::warn!("[task-engine] failed to update repo on task: {e}");
                                                                }
                                                            if task.stage == crate::buzz::task::TaskStage::InProgress {
                                                                if let Err(e) = task_store.transition_task(
                                                                    &task.id,
                                                                    &crate::buzz::task::TaskStage::InProgress,
                                                                    &crate::buzz::task::TaskStage::InAiReview,
                                                                    Some("PR opened".to_string()),
                                                                ) {
                                                                    tracing::warn!("[task-engine] failed to transition task to InAiReview: {e}");
                                                                } else {
                                                                    info!("[task-engine] task '{}' → In AI Review (PR opened)", task.title);
                                                                    // Log PR opened + stage change to activity feed.
                                                                    if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(slot.store.db_path()) {
                                                                        let pr_label = pr_url.as_deref().unwrap_or("(unknown)");
                                                                        let meta = serde_json::json!({
                                                                            "from": "In Progress",
                                                                            "to": "In AI Review",
                                                                            "reason": "PR opened",
                                                                            "pr_url": pr_label,
                                                                        });
                                                                        let _ = ae.log_event(
                                                                            &slot.name,
                                                                            Some(&task.id),
                                                                            "pr",
                                                                            &format!("PR opened: {pr_label}"),
                                                                            None,
                                                                            Some("swarm"),
                                                                            None,
                                                                            Some(&serde_json::json!({"pr_url": pr_label}).to_string()),
                                                                        );
                                                                        let _ = ae.log_event(
                                                                            &slot.name,
                                                                            Some(&task.id),
                                                                            "stage_change",
                                                                            "In Progress → In AI Review",
                                                                            None,
                                                                            Some("swarm"),
                                                                            None,
                                                                            Some(&meta.to_string()),
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                        }
                                            }

                                            // Dispatch reviewer worker when task enters InAiReview
                                            if is_new && (update.source == "swarm_pr_opened" || (update.source == "swarm" && update.external_id.starts_with("swarm-pr-"))) {
                                                let worker_id = update.external_id.strip_prefix("swarm-pr-").unwrap_or("").to_string();
                                                if !worker_id.is_empty()
                                                    && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                        && let Ok(Some(task)) = task_store.find_task_by_worker(&slot.name, &worker_id)
                                                            && task.stage == crate::buzz::task::TaskStage::InAiReview
                                                            && task.metadata.get("reviewer_worker_id").is_none()
                                                            && let (Some(pr_number), Some(repo)) = (task.pr_number, &task.repo)
                                                {
                                                    // Use the short repo name (e.g. "apiari" from "ApiariTools/apiari")
                                                    let short_repo = repo
                                                        .split('/')
                                                        .next_back()
                                                        .unwrap_or(repo.as_str())
                                                        .to_string();
                                                    let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(
                                                        slot.config.root.clone(),
                                                    );
                                                    let task_id = task.id.clone();
                                                    let task_title = task.title.clone();
                                                    let mut meta = task.metadata.clone();
                                                    let db_path = slot.store.db_path().to_path_buf();
                                                    let ws_name_for_review = slot.name.clone();
                                                    tokio::spawn(async move {
                                                        match swarm.create_reviewer_worker(&short_repo, pr_number).await {
                                                            Ok(reviewer_id) if !reviewer_id.is_empty() => {
                                                                meta["reviewer_worker_id"] = serde_json::Value::String(reviewer_id.clone());
                                                                if let Ok(ts) = crate::buzz::task::store::TaskStore::open(&db_path) {
                                                                    if let Err(e) = ts.update_task_metadata(&task_id, &meta) {
                                                                        tracing::warn!("[task-engine] failed to store reviewer_worker_id: {e}");
                                                                    } else {
                                                                        info!("[task-engine] dispatched reviewer {reviewer_id} for task '{task_title}'");
                                                                        if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(&db_path) {
                                                                            let _ = ae.log_event(
                                                                                &ws_name_for_review,
                                                                                Some(&task_id),
                                                                                "worker",
                                                                                &format!("Reviewer {} dispatched", reviewer_id),
                                                                                None,
                                                                                Some("swarm"),
                                                                                None,
                                                                                Some(&serde_json::json!({"reviewer_worker_id": reviewer_id}).to_string()),
                                                                            );
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            Ok(_) => {
                                                                tracing::warn!("[task-engine] reviewer dispatch for '{task_title}' returned empty id");
                                                            }
                                                            Err(e) => {
                                                                tracing::warn!("[task-engine] failed to dispatch reviewer for '{task_title}': {e}");
                                                            }
                                                        }
                                                    });
                                                }
                                            }

                                            // Auto-close the worker that opened the PR — it has delivered its work
                                            if is_new && (update.source == "swarm_pr_opened" || (update.source == "swarm" && update.external_id.starts_with("swarm-pr-"))) {
                                                let worker_id = update.external_id.strip_prefix("swarm-pr-").unwrap_or("").to_string();
                                                if !worker_id.is_empty() {
                                                    let swarm_for_close = crate::buzz::coordinator::swarm_client::SwarmClient::new(slot.config.root.clone());
                                                    let wid = worker_id.clone();
                                                    tokio::spawn(async move {
                                                        match swarm_for_close.list_workers().await {
                                                            Ok(workers) => {
                                                                if let Some(w) = workers.iter().find(|w| w.id == wid)
                                                                    && should_auto_close_pr_worker(w) {
                                                                        tracing::info!("auto-closing worker {wid} after PR opened");
                                                                        if let Err(e) = swarm_for_close.close_worker(&wid).await {
                                                                            tracing::warn!("failed to auto-close worker {wid} after PR opened: {e}");
                                                                        }
                                                                    }
                                                            }
                                                            Err(e) => {
                                                                tracing::warn!("failed to list workers for auto-close after PR: {e}");
                                                            }
                                                        }
                                                    });
                                                }
                                            }

                                            // Dispatch reviewer on branch when task enters InAiReview via branch-ready flow
                                            if is_new && update.source == "swarm_branch_ready" && update.external_id.starts_with("swarm-branch-ready-") {
                                                let worker_id = update.external_id.strip_prefix("swarm-branch-ready-").unwrap_or("").to_string();
                                                if !worker_id.is_empty()
                                                    && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                        && let Ok(Some(task)) = task_store.find_task_by_worker(&slot.name, &worker_id)
                                                            && task.stage == crate::buzz::task::TaskStage::InAiReview
                                                            && task.metadata.get("reviewer_worker_id").is_none()
                                                {
                                                    // Extract branch_name from signal metadata
                                                    let branch_name = update.metadata.as_ref()
                                                        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                                                        .and_then(|v| v.get("branch_name").and_then(|b| b.as_str()).map(String::from))
                                                        .unwrap_or_default();

                                                    if !branch_name.is_empty() {
                                                        // Store ready_branch in task metadata for later PR-open step
                                                        let mut meta = task.metadata.clone();
                                                        meta["ready_branch"] = serde_json::Value::String(branch_name.clone());
                                                        let db_path = slot.store.db_path().to_path_buf();
                                                        if let Err(e) = task_store.update_task_metadata(&task.id, &meta) {
                                                            tracing::warn!("[task-engine] failed to store ready_branch in task metadata: {e}");
                                                        }

                                                        // Derive a short repo name from the signal metadata repo_path
                                                        // (e.g. "/home/user/project/apiari" → "apiari")
                                                        let short_repo = update.metadata.as_ref()
                                                            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                                                            .and_then(|v| v.get("repo").and_then(|r| r.as_str()).map(String::from))
                                                            .unwrap_or_default();
                                                        let short_repo = short_repo
                                                            .trim_end_matches('/')
                                                            .rsplit('/')
                                                            .next()
                                                            .unwrap_or(&short_repo)
                                                            .to_string();

                                                        let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(
                                                            slot.config.root.clone(),
                                                        );
                                                        let task_id = task.id.clone();
                                                        let task_title = task.title.clone();
                                                        let mut meta2 = meta;
                                                        let ws_name_branch = slot.name.clone();
                                                        tokio::spawn(async move {
                                                            match swarm.create_reviewer_worker_for_branch(&short_repo, &branch_name).await {
                                                                Ok(reviewer_id) if !reviewer_id.is_empty() => {
                                                                    meta2["reviewer_worker_id"] = serde_json::Value::String(reviewer_id.clone());
                                                                    if let Ok(ts) = crate::buzz::task::store::TaskStore::open(&db_path) {
                                                                        if let Err(e) = ts.update_task_metadata(&task_id, &meta2) {
                                                                            tracing::warn!("[task-engine] failed to store reviewer_worker_id: {e}");
                                                                        } else {
                                                                            info!("[task-engine] dispatched branch reviewer {reviewer_id} for task '{task_title}'");
                                                                            if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(&db_path) {
                                                                                let _ = ae.log_event(
                                                                                    &ws_name_branch,
                                                                                    Some(&task_id),
                                                                                    "worker",
                                                                                    &format!("Branch reviewer {} dispatched", reviewer_id),
                                                                                    None,
                                                                                    Some("swarm"),
                                                                                    None,
                                                                                    Some(&serde_json::json!({"reviewer_worker_id": reviewer_id, "branch": branch_name}).to_string()),
                                                                                );
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                Ok(_) => {
                                                                    tracing::warn!("[task-engine] branch reviewer dispatch for '{task_title}' returned empty id");
                                                                }
                                                                Err(e) => {
                                                                    tracing::warn!("[task-engine] failed to dispatch branch reviewer for '{task_title}': {e}");
                                                                }
                                                            }
                                                        });
                                                    }
                                                }
                                            }

                                            // Process reviewer worker completion — emit verdict signal
                                            if is_new && (update.source == "swarm_worker_closed" || (update.source == "swarm" && update.external_id.starts_with("swarm-completed-"))) {
                                                let worker_id = update.external_id.strip_prefix("swarm-completed-")
                                                    .or_else(|| update.external_id.strip_prefix("swarm-worker-closed-"))
                                                    .unwrap_or("").to_string();
                                                if !worker_id.is_empty()
                                                    && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                        && let Ok(Some(task)) = task_store.find_task_by_reviewer_worker(&slot.name, &worker_id)
                                                {
                                                    // Read verdict from swarm state file while worker is still present
                                                    let state_path = slot.config.root.join(".swarm").join("state.json");
                                                    if let Ok(raw) = std::fs::read_to_string(&state_path)
                                                        && let Ok(state_json) = serde_json::from_str::<serde_json::Value>(&raw)
                                                    {
                                                        let worktree = state_json
                                                            .get("worktrees")
                                                            .and_then(|wts| wts.as_array())
                                                            .and_then(|arr| {
                                                                arr.iter().find(|wt| {
                                                                    wt.get("id")
                                                                        .and_then(|id| id.as_str())
                                                                        == Some(worker_id.as_str())
                                                                })
                                                            });
                                                        let review_verdict_obj = worktree
                                                            .and_then(|wt| wt.get("review_verdict"));
                                                        let verdict = review_verdict_obj
                                                            .and_then(|v| {
                                                                let approved = v.get("approved").and_then(|a| a.as_bool())?;
                                                                if approved {
                                                                    Some("APPROVED".to_string())
                                                                } else {
                                                                    Some("CHANGES_REQUESTED".to_string())
                                                                }
                                                            });
                                                        let comments = review_verdict_obj
                                                            .and_then(|v| v.get("comments"))
                                                            .and_then(|c| c.as_array())
                                                            .map(|arr| {
                                                                arr.iter()
                                                                    .filter_map(|item| item.as_str())
                                                                    .collect::<Vec<_>>()
                                                                    .join("\n")
                                                            })
                                                            .unwrap_or_default();

                                                        if let Some(verdict) = verdict {
                                                            // Build metadata and title for PR flow or branch-first flow
                                                            let is_branch_flow = task.pr_number.is_none();
                                                            let (metadata, signal_title) = if let (Some(pr_number), Some(repo)) = (task.pr_number, task.repo.as_deref()) {
                                                                // Old PR flow
                                                                (
                                                                    serde_json::json!({
                                                                        "verdict": verdict,
                                                                        "comments": comments,
                                                                        "repo": repo,
                                                                        "pr_number": pr_number,
                                                                        "reviewer_worker_id": worker_id,
                                                                    }),
                                                                    format!("Review verdict for PR #{pr_number}: {verdict}"),
                                                                )
                                                            } else {
                                                                // Branch-first flow: no PR yet
                                                                let ready_branch = task.metadata
                                                                    .get("ready_branch")
                                                                    .and_then(|v| v.as_str())
                                                                    .unwrap_or("unknown");
                                                                (
                                                                    serde_json::json!({
                                                                        "verdict": verdict,
                                                                        "comments": comments,
                                                                        "reviewer_worker_id": worker_id,
                                                                        "ready_branch": ready_branch,
                                                                    }),
                                                                    format!("Review verdict for branch {ready_branch}: {verdict}"),
                                                                )
                                                            };
                                                            let verdict_signal = crate::buzz::signal::SignalUpdate::new(
                                                                "swarm_review_verdict",
                                                                format!("swarm-review-verdict-{worker_id}"),
                                                                signal_title,
                                                                crate::buzz::signal::Severity::Info,
                                                            )
                                                            .with_metadata(metadata.to_string());

                                                            match slot.store.upsert_signal(&verdict_signal) {
                                                                Ok((vid, true)) => {
                                                                    info!("[task-engine] emitted review verdict '{verdict}' for task '{}'", task.title);
                                                                    // Log review event to activity feed.
                                                                    if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(slot.store.db_path()) {
                                                                        let review_meta = serde_json::json!({
                                                                            "verdict": verdict,
                                                                            "reviewer_worker_id": worker_id,
                                                                        });
                                                                        let _ = ae.log_event(
                                                                            &slot.name,
                                                                            Some(&task.id),
                                                                            "review",
                                                                            &format!("Review verdict: {verdict}"),
                                                                            if comments.is_empty() { None } else { Some(comments.as_str()) },
                                                                            Some("swarm"),
                                                                            Some(vid),
                                                                            Some(&review_meta.to_string()),
                                                                        );
                                                                    }
                                                                    // Process immediately through orchestrator
                                                                    if let Ok(Some(vrecord)) = slot.store.get_signal(vid)
                                                                        && let Ok(ts2) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                                    {
                                                                        match slot.orchestrator.process_signal(&ts2, &slot.name, &vrecord) {
                                                                            Ok(verdict_orch_result) => {
                                                                                let ve_result = verdict_orch_result.engine_result;
                                                                                for (wid, msg) in ve_result.worker_messages {
                                                                                    let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(slot.config.root.clone());
                                                                                    tokio::spawn(async move {
                                                                                        if let Err(e) = swarm.send_message(&wid, &msg).await {
                                                                                            tracing::warn!("[task-engine] failed to send review feedback to worker {wid}: {e}");
                                                                                        }
                                                                                    });
                                                                                }
                                                                                for notification in &ve_result.notifications {
                                                                                    if let Some(ref server) = socket_server {
                                                                                        server.broadcast_activity("task", &slot.name, "transition", notification);
                                                                                    }
                                                                                }

                                                                                // Branch-first flow: after approval, system creates the PR
                                                                                if verdict == "APPROVED" && is_branch_flow {
                                                                                    let ready_branch = task.metadata
                                                                                        .get("ready_branch")
                                                                                        .and_then(|v| v.as_str())
                                                                                        .unwrap_or("")
                                                                                        .to_string();
                                                                                    if !ready_branch.is_empty() {
                                                                                        let task_id = task.id.clone();
                                                                                        let task_title = task.title.clone();
                                                                                        let work_dir = slot.config.root.clone();
                                                                                        let db_path = slot.store.db_path().to_path_buf();
                                                                                        let ws_name_pr = slot.name.clone();
                                                                                        tokio::spawn(async move {
                                                                                            match crate::buzz::orchestrator::workflow::create_system_pr(
                                                                                                &work_dir, &ready_branch, &task_title, "Approved by AI reviewer",
                                                                                            ).await {
                                                                                                Ok(pr_result) => {
                                                                                                    info!("[workflow] system PR created for task '{}': {}", task_title, pr_result.pr_url);
                                                                                                    if let Ok(ts) = crate::buzz::task::store::TaskStore::open(&db_path) {
                                                                                                        if let Some(num) = pr_result.pr_number {
                                                                                                            let _ = ts.update_task_pr(&task_id, &pr_result.pr_url, num);
                                                                                                        }
                                                                                                        let _ = ts.transition_task(
                                                                                                            &task_id,
                                                                                                            &crate::buzz::task::TaskStage::InAiReview,
                                                                                                            &crate::buzz::task::TaskStage::HumanReview,
                                                                                                            Some("System PR created after review approval".to_string()),
                                                                                                        );
                                                                                                    }
                                                                                                    // Emit swarm_pr_opened signal
                                                                                                    if let Ok(ss) = crate::buzz::signal::store::SignalStore::open(&db_path, &ws_name_pr) {
                                                                                                        let pr_signal = crate::buzz::signal::SignalUpdate::new(
                                                                                                            "swarm_pr_opened",
                                                                                                            format!("swarm-system-pr-{task_id}"),
                                                                                                            format!("System PR created: {}", pr_result.pr_url),
                                                                                                            crate::buzz::signal::Severity::Info,
                                                                                                        )
                                                                                                        .with_url(&pr_result.pr_url)
                                                                                                        .with_metadata(serde_json::json!({
                                                                                                            "pr_url": pr_result.pr_url,
                                                                                                            "pr_number": pr_result.pr_number,
                                                                                                            "task_id": task_id,
                                                                                                            "system_created": true,
                                                                                                        }).to_string());
                                                                                                        let _ = ss.upsert_signal(&pr_signal);
                                                                                                    }
                                                                                                }
                                                                                                Err(e) => {
                                                                                                    tracing::warn!("[workflow] system PR creation failed for task '{}': {e}", task_title);
                                                                                                }
                                                                                            }
                                                                                        });
                                                                                    }
                                                                                }

                                                                                // Auto-close the reviewer worker — it has delivered its verdict
                                                                                let swarm_for_close = crate::buzz::coordinator::swarm_client::SwarmClient::new(slot.config.root.clone());
                                                                                let reviewer_id_to_close = worker_id.clone();
                                                                                tokio::spawn(async move {
                                                                                    if let Err(e) = swarm_for_close.close_worker(&reviewer_id_to_close).await {
                                                                                        tracing::warn!("failed to auto-close reviewer {reviewer_id_to_close}: {e}");
                                                                                    } else {
                                                                                        tracing::info!("auto-closed reviewer worker {reviewer_id_to_close}");
                                                                                    }
                                                                                });
                                                                            }
                                                                            Err(e) => {
                                                                                tracing::warn!("[task-engine] error processing verdict signal: {e}");
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                Ok((_, false)) => {} // already seen
                                                                Err(e) => {
                                                                    tracing::warn!("[task-engine] failed to upsert verdict signal: {e}");
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            // Handle worker closed — dismiss task if no PR was merged
                                            if is_new && (update.source == "swarm_worker_closed" || (update.source == "swarm" && update.external_id.starts_with("swarm-closed-"))) {
                                                let worker_id = update.external_id.strip_prefix("swarm-closed-")
                                                    .or_else(|| update.external_id.strip_prefix("swarm-worker-closed-"))
                                                    .unwrap_or("").to_string();
                                                if !worker_id.is_empty()
                                                    && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                        && let Ok(Some(task)) = task_store.find_task_by_worker(&slot.name, &worker_id)
                                                            && !task.stage.is_terminal() && task.pr_url.is_none() {
                                                                let from_stage = task.stage.clone();
                                                                if let Err(e) = task_store.transition_task(
                                                                    &task.id,
                                                                    &task.stage,
                                                                    &crate::buzz::task::TaskStage::Dismissed,
                                                                    Some("Worker closed without PR".to_string()),
                                                                ) {
                                                                    tracing::warn!("[task-engine] failed to dismiss task for closed worker: {e}");
                                                                } else {
                                                                    info!("[task-engine] dismissed task '{}' (worker closed without PR)", task.title);
                                                                    if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(slot.store.db_path()) {
                                                                        let meta = serde_json::json!({
                                                                            "from": from_stage.as_str(),
                                                                            "to": "Dismissed",
                                                                            "reason": "Worker closed without PR",
                                                                        });
                                                                        let _ = ae.log_event(
                                                                            &slot.name,
                                                                            Some(&task.id),
                                                                            "stage_change",
                                                                            &format!("{} → Dismissed", from_stage.as_str()),
                                                                            None,
                                                                            Some("swarm"),
                                                                            None,
                                                                            Some(&meta.to_string()),
                                                                        );
                                                                    }
                                                                }
                                                            }
                                            }

                                            // Auto-forward CI failure to the matching swarm worker
                                            if is_new
                                                && update.source == "github_ci_failure"
                                                && let Some(ref meta_str) = update.metadata
                                                && let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str)
                                                && let Some(repo) = meta.get("repo").and_then(|v| v.as_str())
                                                && let Some(pr_num_u64) = meta.get("pr_number").and_then(|v| v.as_u64())
                                                && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                && let Ok(Some(task)) = task_store.find_task_by_pr(&slot.name, repo, pr_num_u64 as i64)
                                                && let Some(worker_id) = task.worker_id
                                            {
                                                let pr_number = pr_num_u64;
                                                let job_url = update.url.clone().or_else(|| update.body.clone());
                                                let error_msg = format!(
                                                    "CI failed on your PR. {}Please fix the failing checks and push.",
                                                    job_url.map(|u| format!("Error details: {}. ", u)).unwrap_or_default()
                                                );
                                                info!("auto-forwarded CI failure to worker {worker_id} for PR #{pr_number}");
                                                let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(slot.config.root.clone());
                                                tokio::spawn(async move {
                                                    if let Err(e) = swarm.send_message(&worker_id, &error_msg).await {
                                                        tracing::warn!("failed to auto-forward CI failure to worker {worker_id}: {e}");
                                                    }
                                                });
                                            }

                                            // Auto-forward CI pass to the matching swarm worker
                                            if is_new
                                                && update.source == "github_ci_pass"
                                                && let Some(ref meta_str) = update.metadata
                                                && let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str)
                                                && let Some(repo) = meta.get("repo").and_then(|v| v.as_str())
                                                && let Some(pr_num_u64) = meta.get("pr_number").and_then(|v| v.as_u64())
                                                && let Ok(task_store) = crate::buzz::task::store::TaskStore::open(slot.store.db_path())
                                                && let Ok(Some(task)) = task_store.find_task_by_pr(&slot.name, repo, pr_num_u64 as i64)
                                                && let Some(worker_id) = task.worker_id
                                            {
                                                let pr_number = pr_num_u64;
                                                let pass_msg = format!(
                                                    "CI is green on your PR #{pr_number}. All checks passed!"
                                                );
                                                info!("auto-forwarded CI pass to worker {worker_id} for PR #{pr_number}");
                                                let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(slot.config.root.clone());
                                                tokio::spawn(async move {
                                                    if let Err(e) = swarm.send_message(&worker_id, &pass_msg).await {
                                                        tracing::warn!("failed to auto-forward CI pass to worker {worker_id}: {e}");
                                                    }
                                                });
                                            }

                                            // Determine notification via orchestrator tier routing
                                            // (orchestrator already matched actions above)
                                            let notification = if is_new {
                                                slot.store.get_signal(id).ok().flatten().and_then(|record| {
                                                    let severity = record.severity.clone();
                                                    let tier = slot.orchestrator.process_signal(
                                                        &crate::buzz::task::store::TaskStore::open(slot.store.db_path()).ok()?,
                                                        &slot.name,
                                                        &record,
                                                    ).ok()?.notification_tier;
                                                    use crate::buzz::orchestrator::notify::NotificationTier;
                                                    match tier {
                                                        NotificationTier::Silent => {
                                                            // CI pass: still batch for TUI display
                                                            if update.source == "github_ci_pass" {
                                                                let pr_ref = record
                                                                    .external_id
                                                                    .rsplit('-')
                                                                    .nth(1)
                                                                    .map(|n| format!("#{n}"))
                                                                    .unwrap_or_default();
                                                                ci_pass_batch
                                                                    .push((pr_ref, record.title.clone()));
                                                            }
                                                            None
                                                        }
                                                        NotificationTier::Badge => {
                                                            // Badge: broadcast to TUI but not Telegram
                                                            let text = format!("[{}] {}", record.source, record.title);
                                                            if let Some(ref server) = socket_server {
                                                                server.broadcast_activity("signal", &slot.name, "badge", &text);
                                                            }
                                                            None // Don't return as (text, severity) — not for Telegram
                                                        }
                                                        NotificationTier::Chat => {
                                                            let text = format!("[{}] {}", record.source, record.title);
                                                            Some((text, severity))
                                                        }
                                                    }
                                                })
                                            } else {
                                                None
                                            };

                                            if let Some((text, severity)) = notification {
                                                // Always broadcast to TUI
                                                if let Some(ref server) = socket_server {
                                                    server.broadcast_activity("signal", &slot.name, "notification", &text);
                                                }
                                                // Only send to Telegram if severity is Warning or higher
                                                if severity.priority() >= Severity::Warning.priority()
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
                                let _ = slot.store.set_cursor(&watcher_name, "error: poll failed");
                                // Still mark polled on error to avoid hammering a failing source
                                throttled.mark_polled();
                            }
                        }
                    }

                    // Send batched CI pass notification (TUI-preferred, Telegram fallback)
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
                        let receivers = socket_server
                            .as_ref()
                            .map(|server| server.broadcast_activity("signal", &slot.name, "ci_pass", &text))
                            .unwrap_or(0);
                        if receivers == 0
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
                                error!("[{}] failed to send CI pass notification: {e}", slot.name);
                            }
                        }
                    }

                    // Broadcast watcher poll heartbeat to TUI clients so remote
                    // clients can update their "last updated" display even without
                    // direct SQLite access.
                    if let Some(ref server) = socket_server
                        && let Ok(cursors) = slot.store.get_watcher_cursors()
                    {
                        let cursor_summary: Vec<String> = cursors
                            .iter()
                            .map(|(name, ts)| format!("{name}={ts}"))
                            .collect();
                        server.broadcast_activity(
                            "daemon",
                            &slot.name,
                            "watcher_poll_complete",
                            &cursor_summary.join(","),
                        );
                    }

                    // Coordinator follow-through for orchestrator matched actions (non-blocking)

                    // Check workspace schedule before firing any follow-throughs.
                    let follow_through_active = slot.config.schedule.as_ref()
                        .map(crate::buzz::schedule::is_within_active_hours)
                        .unwrap_or(true);
                    if !follow_through_active && !orchestrator_matched_actions.is_empty() {
                        info!(
                            "[{}] skipping signal follow-through: outside active hours ({} action(s) dropped)",
                            slot.name,
                            orchestrator_matched_actions.len()
                        );
                    }

                    // Group matched actions by trigger (same behavior as old hook_events)
                    let mut grouped_actions: HashMap<String, Vec<crate::buzz::orchestrator::MatchedAction>> = HashMap::new();
                    for action in orchestrator_matched_actions {
                        grouped_actions.entry(action.trigger.clone()).or_default().push(action);
                    }

                    for (source, actions) in grouped_actions {
                        if !follow_through_active {
                            continue;
                        }
                        let first = &actions[0];
                        let signals: Vec<String> = actions.iter().map(|a| a.signal_description.clone()).collect();
                        info!(
                            "[follow-through] dispatching: workspace={} source={source} signal_count={} ttl_secs={}",
                            slot.name,
                            signals.len(),
                            first.ttl_secs,
                        );
                        let telegram_info = slot.config.telegram.as_ref().and_then(|tg| {
                            telegram_channels.get(&tg.bot_token).map(|ch| {
                                (ch.clone(), tg.chat_id, tg.topic_id)
                            })
                        });
                        let resolved_bees = slot.config.resolved_bees();
                        let bee_idx = resolved_bees
                            .iter()
                            .position(|bee| bee_matches_signal_source(bee, &source))
                            .unwrap_or(0);
                        let matched_bee_name = resolved_bees.get(bee_idx)
                            .map(|b| b.name.clone())
                            .unwrap_or_else(|| "coordinator".to_string());
                        let _ = slot.bees[bee_idx].coord_tx.send(CoordinatorJob::SignalFollowThrough {
                            signals,
                            source,
                            prompt_override: None,
                            action: Some(first.action.clone()),
                            queued_at: std::time::Instant::now(),
                            ttl_secs: first.ttl_secs,
                            telegram: telegram_info,
                            socket_server: socket_server.clone(),
                            slot_name: slot.name.clone(),
                            skill_names: first.skills.clone(),
                            workspace_root: slot.config.root.clone(),
                            bee_name: matched_bee_name,
                        });
                    }

                }

            }

            // ── TUI socket requests ──
            Some(client_req) = tui_recv => {
                match client_req.request {
                    socket::DaemonRequest::Chat { ref workspace, ref text, ref bee } => {
                        let ws_name = workspace.clone();
                        let user_text = text.clone();

                        if let Some(&idx) = name_map.get(&ws_name) {
                            let slot = &mut slots[idx];
                            info!("[{}] TUI chat: {user_text}", slot.name);

                            let bee_idx = match bee {
                                Some(bee_name) => match slot.bee_map.get(bee_name).copied() {
                                    Some(idx) => idx,
                                    None => {
                                        let _ = client_req.responder.send(socket::DaemonResponse::Error {
                                            workspace: ws_name.clone(),
                                            text: format!("bee '{bee_name}' not found in workspace '{ws_name}'"),
                                        });
                                        continue;
                                    }
                                },
                                None => 0,
                            };

                            slot.bees[bee_idx].last_user_input = Some(std::time::Instant::now());
                            slot.bees[bee_idx].last_nudge = None;

                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("tui", &ws_name, "user_message", &user_text);
                            }

                            // Check for slash commands in TUI chat
                            if let Some(rest) = user_text.strip_prefix('/') {
                                let (command, args) = match rest.split_once(' ') {
                                    Some((cmd, args)) => (cmd, args.trim()),
                                    None => (rest, ""),
                                };
                                let (handled, inject_context) = handle_tui_command(
                                    command,
                                    args,
                                    slot,
                                    &client_req.responder,
                                    &socket_server,
                                    &telegram_channels,
                                ).await;
                                if handled {
                                    // If the command produced context for the coordinator
                                    // (e.g. /doctor output), inject it so the coordinator
                                    // can reference the results in future turns.
                                    if let Some(context) = inject_context {
                                        let job = CoordinatorJob::InjectContext {
                                            text: context,
                                        };
                                        if slot.bees[0].coord_tx.send(job).is_err() {
                                            warn!("[{ws_name}] failed to inject command context: coordinator task shut down");
                                        }
                                    }
                                    continue;
                                }
                                // Not a built-in command — fall through to coordinator
                            }

                            let ws_name_for_err = ws_name.clone();
                            let job = CoordinatorJob::TuiChat {
                                text: user_text,
                                responder: client_req.responder.clone(),
                                socket_server: socket_server.clone(),
                                ws_name,
                                bee_name: slot.bees[bee_idx].name.clone(),
                            };
                            if slot.bees[bee_idx].coord_tx.send(job).is_err() {
                                let _ = client_req.responder.send(socket::DaemonResponse::Error {
                                    workspace: ws_name_for_err,
                                    text: "coordinator task shut down".to_string(),
                                });
                            }
                        } else {
                            let _ = client_req.responder.send(socket::DaemonResponse::Error {
                                workspace: ws_name.clone(),
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
                        let route = route_map.get(&key).copied()
                            .or_else(|| route_map.get(&RouteKey { chat_id, topic_id: None }).copied());

                        if let Some((slot_idx, bee_idx)) = route {
                            let slot = &mut slots[slot_idx];
                            info!("[{}] message from {user_name}: {text}", slot.name);

                            slot.bees[bee_idx].last_user_input = Some(std::time::Instant::now());
                            slot.bees[bee_idx].last_nudge = None;

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
                                    bee_name: slot.bees[bee_idx].name.clone(),
                                };
                                if let Err(e) = slot.bees[bee_idx].coord_tx.send(job) {
                                    error!("[{}] coordinator job send failed: {e}", slot.name);
                                }
                            }
                        } else {
                            warn!("no workspace route for chat_id={chat_id} topic_id={topic_id:?}");
                        }
                    }

                    ChannelEvent::Command { chat_id, command, args, topic_id, .. } => {
                        let key = RouteKey { chat_id, topic_id };
                        let route = route_map.get(&key).copied()
                            .or_else(|| route_map.get(&RouteKey { chat_id, topic_id: None }).copied());

                        if let Some((slot_idx, bee_idx)) = route {
                            let slot = &mut slots[slot_idx];
                            info!("[{}] command: /{command}", slot.name);

                            slot.bees[bee_idx].last_user_input = Some(std::time::Instant::now());
                            slot.bees[bee_idx].last_nudge = None;

                            // Broadcast command to TUI
                            if let Some(ref server) = socket_server {
                                server.broadcast_activity("telegram", &slot.name, "user_message", &format!("/{command}"));
                            }

                            if let Some(channel) = get_channel(slot, &telegram_channels) {
                                match command.as_str() {
                                    "status" => {
                                        let summary = build_full_status(slot).await;
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
                                        let _ = slot.bees[bee_idx].coord_tx.send(CoordinatorJob::ResetSession);
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
                                    "clear" => {
                                        if slot.bees[bee_idx].coord_tx.send(CoordinatorJob::Clear {
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
                                    "compact" => {
                                        if slot.bees[bee_idx].coord_tx.send(CoordinatorJob::Compact {
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
                                    "update" | "reinstall" => {
                                        info!("[{}] running /reinstall", slot.name);
                                        let updating_msg = OutboundMessage {
                                            chat_id,
                                            text: "Syncing repos and rebuilding apiari...".to_string(),
                                            buttons: vec![],
                                            topic_id,
                                        };
                                        let _ = channel.send_message(&updating_msg).await;

                                        let (text, success) = run_reinstall(&slot.config.root).await;
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                        }
                                        let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;

                                        if success {
                                            info!("[{}] /reinstall succeeded, restarting", slot.name);
                                            let _ = channel.send_message(&OutboundMessage {
                                                chat_id,
                                                text: "Restarting daemon...".to_string(),
                                                buttons: vec![],
                                                topic_id,
                                            }).await;
                                            return ExitReason::Restart;
                                        }
                                    }
                                    "brief" => {
                                        channel.send_typing(chat_id, topic_id).await;

                                        let params = morning_brief::BriefParams {
                                            model: slot.config.resolved_bees()[0].model.clone(),
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
                                    "config" => {
                                        let skill_ctx = build_skill_context(&slot.name, &slot.config);
                                        let text = crate::buzz::coordinator::skills::config::build_config_summary(&skill_ctx);
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                        }
                                        let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;
                                    }
                                    "devmode" => {
                                        let text = crate::buzz::coordinator::devmode::handle_command(&args);
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                        }
                                        let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;
                                    }
                                    "doctor" => {
                                        let fix = args.trim() == "--fix";
                                        let ws_name = slot.name.clone();
                                        let ws_config = slot.config.clone();
                                        let text = tokio::task::spawn_blocking(move || {
                                            doctor::run(&ws_name, &ws_config, fix)
                                        }).await.unwrap_or_else(|e| format!("doctor failed: {e}"));
                                        if let Some(ref server) = socket_server {
                                            server.broadcast_activity("telegram", &slot.name, "assistant_message", &text);
                                        }
                                        let _ = channel.send_message(&OutboundMessage { chat_id, text, buttons: vec![], topic_id }).await;
                                    }
                                    "help" => {
                                        let mut text = "Built-in commands:\n/status — show open signals\n/config — show workspace configuration summary\n/brief — generate morning brief on demand\n/doctor — check workspace health (--fix to scaffold missing files)\n/reset — reset coordinator session\n/clear — clear session (hard reset, no context carried forward)\n/compact — compact session (summarize key context to memory, then reset)\n/devmode — toggle dev mode (on/off/status)\n/reinstall — sync repos and rebuild apiari from source (/update also works)\n/help — this message".to_string();
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

            _ = idle_timer.tick() => {
                // Check each workspace slot for idle nudge eligibility.
                for slot in &mut slots {
                    for bee in &mut slot.bees {
                        let last_input = match bee.last_user_input {
                            Some(t) => t,
                            None => continue,
                        };

                        if last_input.elapsed() < IDLE_NUDGE_THRESHOLD {
                            continue;
                        }

                        if let Some(last) = bee.last_nudge
                            && last.elapsed() < IDLE_NUDGE_COOLDOWN
                        {
                            continue;
                        }

                        bee.last_nudge = Some(std::time::Instant::now());

                        let ws_name = slot.name.clone();
                        let bee_name = bee.name.clone();
                        let swarm_state_path = slot
                            .config
                            .watchers
                            .swarm
                            .as_ref()
                            .map(|s| s.state_path.clone());
                        let repos = slot.config.repos.clone();
                        let server = socket_server.clone();
                        let web_tx = slot.web_updates_tx.clone();

                        tokio::spawn(async move {
                            let nudge = tokio::time::timeout(
                                std::time::Duration::from_secs(30),
                                build_idle_nudge_detached(&swarm_state_path, &repos),
                            )
                            .await;

                            let text = match nudge {
                                Ok(Some(t)) => t,
                                Ok(None) => return,
                                Err(_) => {
                                    warn!("[{ws_name}] idle nudge check timed out");
                                    return;
                                }
                            };

                            info!("[{ws_name}/{bee_name}] sending idle nudge");
                            // Broadcast to TUI clients
                            if let Some(ref server) = server {
                                server.broadcast_activity(
                                    "system",
                                    &ws_name,
                                    "assistant_message",
                                    &text,
                                );
                            }
                            // Broadcast to web UI clients
                            if let Some(ref tx) = web_tx {
                                let _ = tx.send(http::WsUpdate::Signal {
                                    id: chrono::Utc::now().timestamp_millis(),
                                    workspace: ws_name,
                                    source: format!("heartbeat/{bee_name}"),
                                    title: text,
                                    severity: "Info".to_string(),
                                    url: None,
                                    created_at: chrono::Utc::now().to_rfc3339(),
                                });
                            }
                        });
                    }
                }
            }
            _ = prune_timer.tick() => {
                for slot in &slots {
                    let retention_days = slot.config.activity.retention_days;
                    if let Ok(ae) = crate::buzz::task::ActivityEventStore::open(slot.store.db_path())
                        && let Err(e) = ae.prune(&slot.name, retention_days)
                    {
                        warn!("[{}] failed to prune activity events: {e}", slot.name);
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

        // Show schedule status if configured and currently outside active hours.
        if let Some(ref schedule) = ws.config.schedule
            && !crate::buzz::schedule::is_within_active_hours(schedule)
        {
            let hours_str = schedule.active_hours.as_deref().unwrap_or("all hours");
            let days_str = match schedule.active_days.as_ref() {
                None => "all days".to_string(),
                Some(d) if d.is_empty() => "no days".to_string(),
                Some(d) => d
                    .iter()
                    .map(|s| {
                        let mut c = s.chars();
                        match c.next() {
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            println!("  Schedule: paused (active {hours_str}, {days_str})");
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
    let (allowed, disallowed) = tools_for_authority(ws.config.authority);
    info!(
        "[{workspace_name}] coordinator authority={:?} allowed_tools: {allowed:?}, disallowed_tools: {disallowed:?}",
        ws.config.authority
    );
    coordinator.set_tools(allowed);
    coordinator.set_disallowed_tools(disallowed);
    coordinator.set_working_dir(ws.config.root.clone());
    if let Some(settings) = config::coordinator_settings_json() {
        coordinator.set_settings(settings);
    }
    coordinator.set_safety_hooks(Box::new(GitSafetyHooks {
        workspace_root: ws.config.root.clone(),
    }));

    if !Coordinator::is_available(&ws.config.coordinator.provider).await {
        eprintln!(
            "{} CLI not found — coordinator requires it",
            ws.config.coordinator.provider
        );
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
        CoordinatorEvent::Token(_) | CoordinatorEvent::Usage(_) => {}
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

/// Ensure the swarm daemon is running for a workspace, starting it if needed.
///
/// Pings the daemon over its Unix socket. If unreachable, starts it with
/// `swarm --dir <root> daemon start` and waits up to ~2 seconds for it
/// to respond to ping.
async fn ensure_swarm_daemon(workspace_root: &std::path::Path) {
    use crate::buzz::coordinator::swarm_client::SwarmClient;

    let root_display = workspace_root.display();

    // Check if swarm daemon is already running via socket ping
    let dir = workspace_root.to_path_buf();
    let is_running = tokio::task::spawn_blocking(move || SwarmClient::ping_sync(&dir))
        .await
        .unwrap_or(false);

    if is_running {
        info!("swarm daemon already running for {}", root_display);
        return;
    }

    // Daemon not running — start it
    info!("swarm daemon not running for {}, starting...", root_display);
    let result = tokio::process::Command::new("swarm")
        .arg("--dir")
        .arg(workspace_root)
        .args(["daemon", "start"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .output()
        .await;

    match &result {
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "swarm daemon start returned {}: {}",
                o.status,
                stderr.trim()
            );
            return;
        }
        Err(e) => {
            warn!("failed to start swarm daemon for {}: {}", root_display, e);
            return;
        }
        _ => {}
    }

    // Wait up to ~2 seconds for daemon to respond to ping
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let dir = workspace_root.to_path_buf();
        let alive = tokio::task::spawn_blocking(move || SwarmClient::ping_sync(&dir))
            .await
            .unwrap_or(false);
        if alive {
            info!("swarm daemon started for {}", root_display);
            return;
        }
    }
    warn!(
        "swarm daemon may not have started in time for {}",
        root_display
    );
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

/// Ensure the `.apiari/` scaffold exists in the workspace root.
///
/// Creates `.apiari/context.md` (minimal stub) and `.apiari/skills/` if they
/// don't already exist. This lets the coordinator assume these paths are
/// available for writing from the start.
fn ensure_apiari_scaffold(workspace_root: &std::path::Path, ws_name: &str) {
    let apiari_dir = workspace_root.join(".apiari");
    let skills_dir = apiari_dir.join("skills");
    let context_file = apiari_dir.join("context.md");

    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        warn!("[{ws_name}] failed to create .apiari/skills/: {e}");
        return;
    }

    if !context_file.exists()
        && let Err(e) = std::fs::write(&context_file, "# Workspace Context\n")
    {
        warn!("[{ws_name}] failed to create .apiari/context.md: {e}");
    }
}

/// Build a fresh Coordinator for a bee (used at startup and on respawn).
fn build_bee_coordinator(
    ws_name: &str,
    bee: &BeeConfig,
    ws_config: &WorkspaceConfig,
) -> Coordinator {
    ensure_apiari_scaffold(&ws_config.root, ws_name);

    let provider = &bee.provider;
    if !matches!(provider.as_str(), "claude" | "codex" | "gemini") {
        warn!("[{ws_name}] unknown coordinator provider \"{provider}\" — falling back to claude");
    }

    let mut coordinator = Coordinator::new(&bee.model, bee.max_turns);
    coordinator.set_provider(bee.provider.clone());
    coordinator.set_name(bee.name.clone());
    let skill_ctx = build_skill_context(ws_name, ws_config);
    let mut extra_context = build_skills_prompt(&skill_ctx);

    // Inject workflow description so the Bee knows its process
    let workflow_path = ws_config.root.join(".apiari/workflow.yaml");
    if let Ok(yaml) = std::fs::read_to_string(&workflow_path) {
        extra_context.push_str("\n\n## Your Workflow\n\n");
        extra_context.push_str("You operate within a workflow graph that defines your process. ");
        extra_context.push_str("Here is the workflow definition:\n\n```yaml\n");
        extra_context.push_str(&yaml);
        extra_context.push_str("\n```\n\n");
        extra_context.push_str(
            "When users ask about your process, steps, or workflow, refer to this graph. ",
        );
        extra_context
            .push_str("You can explain what happens at each node, what triggers transitions, ");
        extra_context.push_str("and where a task currently is in the process.\n");
    }

    coordinator.set_extra_context(extra_context);
    if let Some(ref prompt) = bee.prompt {
        coordinator.set_prompt_preamble(prompt.clone());
    } else if let Some(ref preamble) = skill_ctx.prompt_preamble {
        coordinator.set_prompt_preamble(preamble.clone());
    }
    let (allowed, disallowed) = tools_for_authority(ws_config.authority);
    info!(
        "[{ws_name}] coordinator authority={:?} allowed_tools: {allowed:?}, disallowed_tools: {disallowed:?}",
        ws_config.authority
    );
    coordinator.set_tools(allowed);
    coordinator.set_disallowed_tools(disallowed);
    coordinator.set_working_dir(ws_config.root.clone());
    if let Some(settings) = config::coordinator_settings_json() {
        coordinator.set_settings(settings);
    }
    coordinator.set_safety_hooks(Box::new(GitSafetyHooks {
        workspace_root: ws_config.root.clone(),
    }));
    coordinator
}

/// Return (allowed, disallowed) tool lists based on authority level.
fn tools_for_authority(authority: crate::config::WorkspaceAuthority) -> (Vec<String>, Vec<String>) {
    match authority {
        crate::config::WorkspaceAuthority::Observe => (
            observe_coordinator_tools(),
            observe_coordinator_disallowed_tools(),
        ),
        crate::config::WorkspaceAuthority::Autonomous => (
            default_coordinator_tools(),
            default_coordinator_disallowed_tools(),
        ),
    }
}

/// Try to restore the last coordinator session from the database.
fn restore_coordinator_session(
    coordinator: &mut Coordinator,
    store: &SignalStore,
    ws_name: &str,
    bee_name: &str,
) {
    let scope = conversation_scope(ws_name, bee_name);
    let conv = ConversationStore::new(store.conn(), &scope);
    match conv.last_session() {
        Ok(Some(token)) if token.provider == coordinator.provider() => {
            info!("[{ws_name}/{bee_name}] restoring session from DB");
            coordinator.restore_session(token);
        }
        Ok(Some(token)) => {
            info!(
                "[{ws_name}/{bee_name}] skipping session restore: provider mismatch (db={}, current={})",
                token.provider,
                coordinator.provider()
            );
        }
        Ok(None) => {
            info!("[{ws_name}/{bee_name}] no previous session to restore");
        }
        Err(e) => {
            warn!("[{ws_name}/{bee_name}] failed to query last session: {e}");
        }
    }
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

/// Handle a TUI slash command. Returns `(handled, inject_context)` where
/// `handled` is `true` if the command was recognized and processed, and
/// `inject_context` is an optional string to forward into the coordinator
/// session so it can reference the output in future turns (e.g. /doctor).
async fn handle_tui_command(
    command: &str,
    args: &str,
    slot: &mut WorkspaceSlot,
    responder: &mpsc::UnboundedSender<socket::DaemonResponse>,
    socket_server: &Option<Arc<socket::DaemonSocketServer>>,
    telegram_channels: &HashMap<String, TelegramChannel>,
) -> (bool, Option<String>) {
    /// Send a text response back to the TUI client.
    fn reply(
        responder: &mpsc::UnboundedSender<socket::DaemonResponse>,
        socket_server: &Option<Arc<socket::DaemonSocketServer>>,
        ws_name: &str,
        text: &str,
    ) {
        let _ = responder.send(socket::DaemonResponse::Token {
            workspace: ws_name.to_string(),
            text: text.to_string(),
        });
        let _ = responder.send(socket::DaemonResponse::Done {
            workspace: ws_name.to_string(),
        });
        if let Some(server) = socket_server {
            server.broadcast_activity("tui", ws_name, "assistant_message", text);
        }
    }

    match command {
        "status" => {
            let summary = build_full_status(slot).await;
            reply(responder, socket_server, &slot.name, &summary);
            (true, None)
        }
        "reset" => {
            let _ = slot.bees[0].coord_tx.send(CoordinatorJob::ResetSession);
            reply(responder, socket_server, &slot.name, "Session reset.");
            (true, None)
        }
        "clear" => {
            if slot.bees[0]
                .coord_tx
                .send(CoordinatorJob::Clear {
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
            (true, None)
        }
        "compact" => {
            if slot.bees[0]
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
            (true, None)
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
                        model: slot.config.resolved_bees()[0].model.clone(),
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
            (true, None)
        }
        "config" => {
            let skill_ctx = build_skill_context(&slot.name, &slot.config);
            let text = crate::buzz::coordinator::skills::config::build_config_summary(&skill_ctx);
            reply(responder, socket_server, &slot.name, &text);
            (true, None)
        }
        "devmode" => {
            let text = crate::buzz::coordinator::devmode::handle_command(args);
            reply(responder, socket_server, &slot.name, &text);
            (true, None)
        }
        "doctor" => {
            let fix = args.trim() == "--fix";
            let ws_name = slot.name.clone();
            let ws_config = slot.config.clone();
            let text = tokio::task::spawn_blocking(move || doctor::run(&ws_name, &ws_config, fix))
                .await
                .unwrap_or_else(|e| format!("doctor failed: {e}"));
            reply(responder, socket_server, &slot.name, &text);
            let context = format!("The user ran /doctor and got the following output:\n\n{text}");
            (true, Some(context))
        }
        "help" => {
            let mut text = "Built-in commands:\n/status — show open signals\n/config — show workspace configuration summary\n/brief — generate morning brief on demand\n/doctor — check workspace health (--fix to scaffold missing files)\n/reset — reset coordinator session\n/clear — clear session (hard reset, no context carried forward)\n/compact — compact session (summarize key context to memory, then reset)\n/devmode — toggle dev mode (on/off/status)\n/reinstall — sync repos and rebuild apiari from source (/update also works)\n/help — this message"
                .to_string();
            if !slot.config.commands.is_empty() {
                text.push_str("\n\nCustom commands:");
                for cmd in &slot.config.commands {
                    let desc = cmd.description.as_deref().unwrap_or("(no description)");
                    text.push_str(&format!("\n/{} — {}", cmd.name, desc));
                }
            }
            reply(responder, socket_server, &slot.name, &text);
            (true, None)
        }
        "update" | "reinstall" => {
            // Send initial status as a streaming token (Done sent after completion)
            let _ = responder.send(socket::DaemonResponse::Token {
                workspace: slot.name.clone(),
                text: "Syncing repos and rebuilding apiari...\n".to_string(),
            });

            let (text, _success) = run_reinstall(&slot.config.root).await;
            let _ = responder.send(socket::DaemonResponse::Token {
                workspace: slot.name.clone(),
                text: text.clone(),
            });
            let _ = responder.send(socket::DaemonResponse::Done {
                workspace: slot.name.clone(),
            });
            if let Some(server) = socket_server {
                server.broadcast_activity("tui", &slot.name, "assistant_message", &text);
            }
            (true, None)
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
                (true, None)
            } else {
                // Not a known command — let the coordinator handle it
                (false, None)
            }
        }
    }
}

/// Build an idle nudge message, or `None` if nothing is pending.
///
/// This is a detached version that takes owned/cloned data so it can run
/// inside a spawned task without borrowing `WorkspaceSlot`.
///
/// Checks for:
/// - Swarm workers in "waiting" state (via state file)
/// - Open PRs with all CI checks passing (via `gh`)
async fn build_idle_nudge_detached(
    swarm_state_path: &Option<std::path::PathBuf>,
    repos: &[String],
) -> Option<String> {
    let mut items: Vec<String> = Vec::new();

    // 1. Check for waiting workers from swarm state file
    if let Some(path) = swarm_state_path
        && let Ok(contents) = tokio::fs::read_to_string(path).await
        && let Ok(state) = serde_json::from_str::<serde_json::Value>(&contents)
        && let Some(worktrees) = state.get("worktrees").and_then(|v| v.as_array())
    {
        for wt in worktrees {
            let phase = wt.get("phase").and_then(|v| v.as_str()).unwrap_or("");
            if phase == "waiting" {
                // Skip reviewer workers — auto-dispatched, not user-actionable
                let prompt = wt.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                if prompt.starts_with("Review PR") {
                    continue;
                }
                // Skip workers that opened a PR — they're done, not stuck
                let has_pr = wt.get("pr").and_then(|v| v.as_object()).is_some();
                if has_pr {
                    continue;
                }
                let id = wt.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                items.push(format!("Worker {id} is waiting for input (no PR yet)"));
            }
        }
    }

    // 2. Check for open PRs with all CI checks passing (only if gh is available)
    if !repos.is_empty() && is_gh_available().await {
        for repo in repos {
            let output = tokio::process::Command::new("gh")
                .args([
                    "pr",
                    "list",
                    "--repo",
                    repo,
                    "--state",
                    "open",
                    "--json",
                    "number,title,statusCheckRollup",
                    "--jq",
                    ".[] | select(.statusCheckRollup != null and (.statusCheckRollup | length > 0) and all(.statusCheckRollup[]; .conclusion == \"SUCCESS\")) | \"#\\(.number) \\(.title)\"",
                ])
                .output()
                .await;

            if let Ok(out) = output
                && out.status.success()
            {
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        items.push(format!("PR {line} — CI green, mergeable"));
                    }
                }
            }
        }
    }

    if items.is_empty() {
        return None;
    }

    let mut msg = String::from("Pending items:\n");
    for item in &items {
        msg.push_str(&format!("• {item}\n"));
    }
    Some(msg.trim_end().to_string())
}

/// Check if the `gh` CLI is installed and authenticated.
async fn is_gh_available() -> bool {
    let which = tokio::process::Command::new("which")
        .arg("gh")
        .output()
        .await;
    if !matches!(which, Ok(ref o) if o.status.success()) {
        return false;
    }
    let auth = tokio::process::Command::new("gh")
        .args(["auth", "status"])
        .output()
        .await;
    matches!(auth, Ok(ref o) if o.status.success())
}

/// Build a full status summary: open signals + worker states + PR queue.
async fn build_full_status(slot: &WorkspaceSlot) -> String {
    let signals = slot.store.get_open_signals().unwrap_or_default();
    let mut summary = format_signal_summary(&signals);

    // Worker states from swarm state file
    if let Some(ref swarm_cfg) = slot.config.watchers.swarm
        && let Ok(contents) = tokio::fs::read_to_string(&swarm_cfg.state_path).await
        && let Ok(state) = serde_json::from_str::<serde_json::Value>(&contents)
        && let Some(worktrees) = state.get("worktrees").and_then(|v| v.as_array())
        && !worktrees.is_empty()
    {
        summary.push_str(&format!("\n{} worker(s):\n", worktrees.len()));
        for wt in worktrees {
            let id = wt.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let phase = wt
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let branch = wt.get("branch").and_then(|v| v.as_str()).unwrap_or("");
            let has_pr = wt.get("pr").and_then(|v| v.as_object()).is_some();
            let pr_str = if has_pr { " [PR]" } else { "" };
            summary.push_str(&format!("  [{phase}] {id} ({branch}){pr_str}\n"));
        }

        // PR queue
        let prs: Vec<_> = worktrees
            .iter()
            .filter_map(|wt| {
                let pr = wt.get("pr")?.as_object()?;
                let number = pr.get("number")?.as_u64()?;
                let title = pr.get("title")?.as_str()?;
                let state = pr.get("state").and_then(|v| v.as_str()).unwrap_or("open");
                Some((number, title.to_string(), state.to_string()))
            })
            .collect();
        if !prs.is_empty() {
            summary.push_str(&format!("\n{} PR(s):\n", prs.len()));
            for (number, title, state) in &prs {
                summary.push_str(&format!("  #{number} [{state}] {title}\n"));
            }
        }
    }

    summary
}

/// Compute the effective schedule for a single watcher and validate the
/// per-watcher override (if any).
///
/// A per-watcher `active_hours` string overrides the workspace-level `active_hours`.
/// `active_days` is always inherited from the workspace schedule — per-watcher configs
/// cannot override it.  The result is precomputed once at registration time so the
/// poll loop never allocates.
///
/// When a per-watcher `active_hours` override is provided it is validated here
/// (emitting a `warn!` if malformed).  The workspace-level schedule must be
/// validated separately at startup (once, before any watchers are registered) to
/// avoid duplicate warnings.
fn effective_watcher_schedule(
    workspace: Option<&crate::config::Schedule>,
    watcher_hours: Option<&str>,
    watcher_name: &str,
) -> crate::config::Schedule {
    match watcher_hours {
        Some(ah) => {
            // Validate only the per-watcher override; workspace hours were validated at startup.
            // Pass the watcher name so any warning identifies which watcher is misconfigured.
            if crate::buzz::schedule::parse_active_hours(ah).is_none() {
                warn!(
                    "[{}] active_hours {:?} is malformed (expected HH:MM-HH:MM); \
                     hours constraint will be ignored",
                    watcher_name, ah
                );
            }
            crate::config::Schedule {
                active_hours: Some(ah.to_string()),
                active_days: workspace.and_then(|s| s.active_days.clone()),
            }
        }
        None => workspace.cloned().unwrap_or_default(),
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

/// Run the full reinstall sequence for the `/reinstall` (a.k.a. `/update`) command.
///
/// Steps:
/// 1. For each git repo under `workspace_root` (root itself + direct subdirs):
///    - If it contains `crates/apiari/`, run `git checkout Cargo.lock` first.
///    - Run `git pull origin main` (errors are reported but do not abort).
/// 2. Find the repo containing `crates/apiari/` and run
///    `cargo install --force --path <repo>/crates/apiari`.
///
/// Returns `(report_text, install_success)`.
async fn run_reinstall(workspace_root: &std::path::Path) -> (String, bool) {
    let mut report = String::new();

    // Collect git repos: workspace root itself + direct child directories.
    let mut repo_paths: Vec<std::path::PathBuf> = Vec::new();

    if workspace_root.join(".git").exists() {
        repo_paths.push(workspace_root.to_path_buf());
    }

    if let Ok(entries) = std::fs::read_dir(workspace_root) {
        let mut child_dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_ok_and(|ft| ft.is_dir())
                    && e.path().join(".git").exists()
                    && !e.file_name().to_string_lossy().starts_with('.')
            })
            .map(|e| e.path())
            .collect();
        child_dirs.sort();
        repo_paths.extend(child_dirs);
    }

    // Identify the apiari source repo (first one that contains crates/apiari/).
    let apiari_src: Option<std::path::PathBuf> = repo_paths
        .iter()
        .find(|p| p.join("crates/apiari").exists())
        .cloned();

    // Pull all repos, doing git checkout Cargo.lock first in the apiari source repo.
    for repo in &repo_paths {
        let dir_name = repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo.display().to_string());

        if apiari_src.as_deref() == Some(repo.as_path()) {
            // Discard Cargo.lock changes caused by [patch.crates-io] dev overrides.
            let co = tokio::process::Command::new("git")
                .args(["checkout", "Cargo.lock"])
                .current_dir(repo)
                .output()
                .await;
            match co {
                Ok(out) if out.status.success() => {
                    report.push_str(&format!("✅ git checkout Cargo.lock ({dir_name})\n"));
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    report.push_str(&format!(
                        "⚠️ git checkout Cargo.lock ({dir_name}): {}\n",
                        stderr.trim()
                    ));
                }
                Err(e) => {
                    report.push_str(&format!("⚠️ git checkout Cargo.lock ({dir_name}): {e}\n"));
                }
            }
        }

        let pull = tokio::process::Command::new("git")
            .args(["pull", "origin", "main"])
            .current_dir(repo)
            .output()
            .await;
        match pull {
            Ok(out) => {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let icon = if out.status.success() { "✅" } else { "❌" };
                let short = combined.trim().lines().last().unwrap_or("").to_string();
                report.push_str(&format!("{icon} git pull ({dir_name}): {short}\n"));
            }
            Err(e) => {
                report.push_str(&format!("❌ git pull ({dir_name}): {e}\n"));
            }
        }
    }

    if repo_paths.is_empty() {
        report.push_str("⚠️ No git repos found under workspace root\n");
    }

    // Build and install from the apiari source repo.
    let install_path = apiari_src
        .map(|p| p.join("crates/apiari"))
        .unwrap_or_else(|| std::path::PathBuf::from("crates/apiari"));

    report.push_str("\nBuilding and installing...\n");

    let script = format!(
        ". \"$HOME/.cargo/env\" 2>/dev/null; cargo install --force --path '{}' 2>&1",
        install_path.display()
    );
    let install = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .await;

    match install {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let status_icon = if out.status.success() { "✅" } else { "❌" };
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
                report.push_str(&format!("{status_icon} cargo install\n```\n{tail}\n```"));
            } else {
                report.push_str(&format!("{status_icon} cargo install"));
            }
            (report, out.status.success())
        }
        Err(e) => {
            report.push_str(&format!("❌ cargo install failed: {e}"));
            (report, false)
        }
    }
}

/// Returns true if a worker that opened a PR should be auto-closed.
///
/// Only headless `claude` workers in `Waiting` phase are eligible.
/// `claude-tui` workers are interactive and must stay alive.
fn should_auto_close_pr_worker(worker: &apiari_swarm::daemon::protocol::WorkerInfo) -> bool {
    worker.phase == apiari_swarm::WorkerPhase::Waiting && worker.agent == "claude"
}

/// Execute a workflow action produced by the orchestrator.
///
/// Spawns async tasks for PR creation, reviewer dispatch, and rework dispatch.
/// This is the execution layer for the orchestrator's workflow engine.
fn execute_workflow_action(
    action: &crate::buzz::orchestrator::workflow::WorkflowAction,
    work_dir: &std::path::Path,
    db_path: &std::path::Path,
    _workspace_name: &str,
) {
    use crate::buzz::orchestrator::workflow::WorkflowAction;

    match action {
        WorkflowAction::CreatePr {
            task_id,
            branch_name,
        }
        | WorkflowAction::ForceCreatePr {
            task_id,
            branch_name,
            ..
        } => {
            let task_id = task_id.clone();
            let branch_name = branch_name.clone();
            let work_dir = work_dir.to_path_buf();
            let db_path = db_path.to_path_buf();

            // Look up task title for the PR
            let title = crate::buzz::task::store::TaskStore::open(&db_path)
                .ok()
                .and_then(|ts| ts.get_task(&task_id).ok().flatten())
                .map(|t| t.title.clone())
                .unwrap_or_else(|| format!("PR for {branch_name}"));

            let is_forced = matches!(action, WorkflowAction::ForceCreatePr { .. });
            let body = if is_forced {
                "Created by apiari orchestrator (max review cycles exceeded)".to_string()
            } else {
                "Created by apiari orchestrator".to_string()
            };

            tokio::spawn(async move {
                match crate::buzz::orchestrator::workflow::create_system_pr(
                    &work_dir,
                    &branch_name,
                    &title,
                    &body,
                )
                .await
                {
                    Ok(pr_result) => {
                        tracing::info!(
                            "[workflow] system PR created for task {task_id}: {}",
                            pr_result.pr_url
                        );
                        if let Ok(ts) = crate::buzz::task::store::TaskStore::open(&db_path) {
                            if let Some(num) = pr_result.pr_number {
                                let _ = ts.update_task_pr(&task_id, &pr_result.pr_url, num);
                            }
                            let _ = ts.transition_task(
                                &task_id,
                                &crate::buzz::task::TaskStage::InAiReview,
                                &crate::buzz::task::TaskStage::HumanReview,
                                Some("System PR created".to_string()),
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[workflow] system PR creation failed for task {task_id}: {e}"
                        );
                    }
                }
            });
        }
        WorkflowAction::DispatchReviewer {
            task_id,
            branch_name,
            worker_id: _,
        } => {
            let task_id = task_id.clone();
            let branch_name = branch_name.clone();
            let work_dir = work_dir.to_path_buf();
            let db_path = db_path.to_path_buf();

            // Derive short repo name from branch or work_dir
            let short_repo = work_dir
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("repo")
                .to_string();

            let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(work_dir.clone());
            tokio::spawn(async move {
                match swarm
                    .create_reviewer_worker_for_branch(&short_repo, &branch_name)
                    .await
                {
                    Ok(reviewer_id) if !reviewer_id.is_empty() => {
                        tracing::info!(
                            "[workflow] dispatched reviewer {reviewer_id} for task {task_id}"
                        );
                        if let Ok(ts) = crate::buzz::task::store::TaskStore::open(&db_path)
                            && let Ok(Some(task)) = ts.get_task(&task_id)
                        {
                            let mut meta = task.metadata.clone();
                            meta["reviewer_worker_id"] =
                                serde_json::Value::String(reviewer_id.clone());
                            meta["ready_branch"] = serde_json::Value::String(branch_name.clone());
                            let _ = ts.update_task_metadata(&task_id, &meta);
                        }
                    }
                    Ok(_) => {
                        tracing::warn!(
                            "[workflow] reviewer dispatch for task {task_id} returned empty id"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[workflow] failed to dispatch reviewer for task {task_id}: {e}"
                        );
                    }
                }
            });
        }
        WorkflowAction::DispatchRework { task_id, feedback } => {
            let task_id = task_id.clone();
            let feedback = feedback.clone();
            let db_path = db_path.to_path_buf();
            let work_dir = work_dir.to_path_buf();

            tokio::spawn(async move {
                // Find the original worker and send feedback
                if let Ok(ts) = crate::buzz::task::store::TaskStore::open(&db_path)
                    && let Ok(Some(task)) = ts.get_task(&task_id)
                    && let Some(ref worker_id) = task.worker_id
                {
                    let swarm = crate::buzz::coordinator::swarm_client::SwarmClient::new(work_dir);
                    let msg = format!(
                        "Review requested changes. Please address the feedback and push again:\n\n{feedback}"
                    );
                    if let Err(e) = swarm.send_message(worker_id, &msg).await {
                        tracing::warn!(
                            "[workflow] failed to send rework feedback to worker {worker_id}: {e}"
                        );
                    } else {
                        tracing::info!(
                            "[workflow] sent rework feedback to worker {worker_id} for task {task_id}"
                        );
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::buzz::task::{Task, TaskStage, store::TaskStore};
    use chrono::Utc;

    fn make_task(workspace: &str, worker_id: &str, repo: &str, pr_number: i64) -> Task {
        Task {
            id: uuid::Uuid::new_v4().to_string(),
            workspace: workspace.to_string(),
            title: format!("Task for PR #{pr_number}"),
            stage: TaskStage::InProgress,
            source: None,
            source_url: None,
            worker_id: Some(worker_id.to_string()),
            pr_url: Some(format!("https://github.com/{repo}/pull/{pr_number}")),
            pr_number: Some(pr_number),
            repo: Some(repo.to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: serde_json::Value::Object(Default::default()),
        }
    }

    #[test]
    fn test_find_worker_for_ci_signal() {
        let store = TaskStore::open_memory().unwrap();
        let ws = "my-workspace";

        // Insert two tasks with different repos/PR numbers
        let task_a = make_task(ws, "worker-aaa", "org/repo-a", 42);
        let task_b = make_task(ws, "worker-bbb", "org/repo-b", 7);
        let task_c = make_task(ws, "worker-ccc", "org/repo-a", 99);
        store.create_task(&task_a).unwrap();
        store.create_task(&task_b).unwrap();
        store.create_task(&task_c).unwrap();

        // Matching by repo + pr_number should return the right worker
        let found = store.find_task_by_pr(ws, "org/repo-a", 42).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().worker_id.as_deref(), Some("worker-aaa"));

        let found_b = store.find_task_by_pr(ws, "org/repo-b", 7).unwrap();
        assert!(found_b.is_some());
        assert_eq!(found_b.unwrap().worker_id.as_deref(), Some("worker-bbb"));

        let found_c = store.find_task_by_pr(ws, "org/repo-a", 99).unwrap();
        assert!(found_c.is_some());
        assert_eq!(found_c.unwrap().worker_id.as_deref(), Some("worker-ccc"));

        // Non-existent PR should return None
        let not_found = store.find_task_by_pr(ws, "org/repo-a", 999).unwrap();
        assert!(not_found.is_none());
    }

    /// Verify the auto-close condition: waiting claude workers close, others don't.
    #[test]
    fn test_should_auto_close_pr_worker() {
        use apiari_swarm::{WorkerPhase, daemon::protocol::WorkerInfo};

        let make = |agent: &str, phase: WorkerPhase| -> WorkerInfo {
            WorkerInfo {
                id: "w1".to_string(),
                branch: "swarm/w1".to_string(),
                prompt: "test task".to_string(),
                agent: agent.to_string(),
                phase,
                session_id: None,
                pr_url: None,
                pr_number: None,
                pr_title: None,
                pr_state: None,
                restart_count: 0,
                created_at: None,
                role: None,
                review_verdict: None,
                agent_card: None,
            }
        };

        // Should close: claude + waiting
        assert!(super::should_auto_close_pr_worker(&make(
            "claude",
            WorkerPhase::Waiting
        )));

        // Should not close: claude but still running
        assert!(!super::should_auto_close_pr_worker(&make(
            "claude",
            WorkerPhase::Running
        )));

        // Should not close: claude-tui (interactive, must stay alive)
        assert!(!super::should_auto_close_pr_worker(&make(
            "claude-tui",
            WorkerPhase::Waiting
        )));

        // Should not close: claude-tui + running
        assert!(!super::should_auto_close_pr_worker(&make(
            "claude-tui",
            WorkerPhase::Running
        )));
    }
}

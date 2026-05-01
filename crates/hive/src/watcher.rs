//! Signal watchers — poll external sources and dispatch to specialty bots.
//!
//! Each bot with a `watch` list gets a background loop that checks for new signals.
//! When a signal fires, the bot processes it autonomously and the result
//! appears in the bot's chat thread.

use crate::db::Db;
use std::path::PathBuf;
use tokio::time::{Duration, interval};
use tracing::info;

const DEFAULT_RESPONSE_STYLE: &str = "Be concise. Lead with the answer.";

/// Configuration for a watched bot.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WatchedBot {
    pub workspace: String,
    pub name: String,
    pub provider: String,
    pub model: Option<String>,
    pub role: String,
    pub watch: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub schedule: Option<String>,
    pub schedule_hours: Option<u64>,
    pub proactive_prompt: Option<String>,
    pub services: Vec<String>,
    pub response_style: Option<String>,
}

/// Start watcher loops for all bots that have watch sources or schedules.
#[allow(dead_code)]
pub fn start_watchers(bots: Vec<WatchedBot>, db: Db) {
    for bot in bots {
        let has_watch = !bot.watch.is_empty();
        let has_schedule = (bot.schedule.is_some() || bot.schedule_hours.is_some())
            && bot.proactive_prompt.is_some();

        if !has_watch && !has_schedule {
            continue;
        }

        if has_watch {
            info!(
                "[watcher] starting signal watchers for {} ({:?})",
                bot.name, bot.watch
            );
        }
        if has_schedule {
            if let Some(ref cron) = bot.schedule {
                info!(
                    "[watcher] starting proactive schedule for {} (cron: {})",
                    bot.name, cron
                );
            } else {
                info!(
                    "[watcher] starting proactive schedule for {} (every {}h)",
                    bot.name,
                    bot.schedule_hours.unwrap_or(24)
                );
            }
        }

        tokio::spawn(run_watcher(bot, db.clone()));
    }
}

#[allow(dead_code)]
async fn run_watcher(bot: WatchedBot, db: Db) {
    let signal_interval = Duration::from_secs(60);
    // Use schedule_hours for legacy interval; cron bots should use ScheduleWatcher instead
    let proactive_secs = bot.schedule_hours.unwrap_or(24).saturating_mul(3600);
    let proactive_interval = Duration::from_secs(proactive_secs);

    let mut signal_tick = interval(signal_interval);
    let mut proactive_tick = interval(proactive_interval);

    // Skip the first proactive tick (don't fire on startup)
    proactive_tick.tick().await;

    loop {
        tokio::select! {
            _ = signal_tick.tick() => {
                for source in &bot.watch {
                    match source.as_str() {
                        "github" => {
                            if let Some(signal) = poll_github(&bot.working_dir).await {
                                dispatch_signal(&bot, &db, &signal).await;
                            }
                        }
                        "sentry" => {
                            // Sentry polling — placeholder
                        }
                        _ => {}
                    }
                }
            }
            _ = proactive_tick.tick() => {
                if let Some(ref prompt) = bot.proactive_prompt {
                    run_proactive(&bot, &db, prompt).await;
                }
            }
        }
    }
}

pub(crate) async fn run_proactive(bot: &WatchedBot, db: &Db, prompt: &str) {
    info!("[watcher] running proactive task for {}", bot.name);

    let report_path = format!("/tmp/hive-report-{}-{}.md", bot.workspace, bot.name);

    // Clean up any old report
    let _ = std::fs::remove_file(&report_path);

    let schedule_desc = if let Some(ref cron) = bot.schedule {
        format!("cron `{cron}`")
    } else {
        format!("every {}h", bot.schedule_hours.unwrap_or(24))
    };
    let _ = db.add_message(
        &bot.workspace,
        &bot.name,
        "system",
        &format!("**Proactive check** — scheduled {schedule_desc}"),
        None,
    );

    // Build service credentials section if the bot has services configured
    let services_section = if !bot.services.is_empty() {
        if let Some(ref dir) = bot.working_dir {
            crate::routes::build_services_prompt(dir, &bot.services)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let full_prompt = format!(
        "You are {}, a specialty bot for the {} workspace.\n\
         Your role: {}\n\
         {services}\n\
         This is a scheduled proactive check. Do the following:\n\n\
         {}\n\n\
         IMPORTANT: Do your research silently using tools. Do NOT narrate your process.\n\
         If nothing noteworthy happened, do NOT publish a report. Just exit silently.\n\
         When you have findings worth reporting, publish your report using this command:\n\
         ```\n\
         hive publish --workspace {ws} --bot {bot_name} --file /tmp/hive-report-{ws}-{bot_name}.md\n\
         ```\n\
         First write your report to /tmp/hive-report-{ws}-{bot_name}.md, then run the command above.\n\n\
         ## Response Style\n\
         {style}\n\n\
         After publishing, say DONE.",
        bot.name,
        bot.workspace,
        bot.role,
        prompt,
        services = services_section,
        ws = bot.workspace,
        bot_name = bot.name,
        style = bot
            .response_style
            .as_deref()
            .unwrap_or(DEFAULT_RESPONSE_STYLE),
    );

    // Resume session if we have one — saves tokens by not re-sending system prompt
    let proactive_session_key = format!("proactive_{}", bot.name);
    let resume_id = db
        .get_session_id(&bot.workspace, &proactive_session_key, "proactive")
        .unwrap_or(None);

    let response = match bot.provider.as_str() {
        "codex" => run_codex_autonomous(&full_prompt, &bot.working_dir)
            .await
            .map(|text| AutonomousResult {
                text,
                session_id: None,
            }),
        "gemini" => run_gemini_autonomous(&full_prompt, &bot.working_dir)
            .await
            .map(|text| AutonomousResult {
                text,
                session_id: None,
            }),
        _ => run_claude_autonomous(&full_prompt, &bot.working_dir, resume_id.as_deref()).await,
    };

    // Save session ID for next run
    if let Ok(ref result) = response
        && let Some(ref sid) = result.session_id
    {
        let _ = db.set_session(&bot.workspace, &proactive_session_key, sid, "proactive");
    }

    // Read the report file if it exists, otherwise fall back to streaming output
    let report = std::fs::read_to_string(&report_path).ok();
    let _ = std::fs::remove_file(&report_path);

    let response_text = response.map(|r| r.text);

    match (report, response_text) {
        (Some(text), _) if !text.trim().is_empty() => {
            // Report file exists — hive publish already stored it in the DB, so just log.
            info!(
                "[watcher] {} report published ({} chars)",
                bot.name,
                text.len()
            );
        }
        (_, Ok(text)) if !text.trim().is_empty() => {
            // Fallback: bot didn't write the file, use streaming output
            let _ = db.add_message(&bot.workspace, &bot.name, "assistant", text.trim(), None);
            info!(
                "[watcher] {} fallback output ({} chars)",
                bot.name,
                text.len()
            );
        }
        (_, Err(e)) => {
            let _ = db.add_message(
                &bot.workspace,
                &bot.name,
                "assistant",
                &format!("Proactive check failed: {e}"),
                None,
            );
        }
        _ => {
            info!("[watcher] {} proactive check: nothing to report", bot.name);
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct Signal {
    pub source: String,
    pub title: String,
    pub body: String,
}

pub(crate) async fn poll_github(working_dir: &Option<PathBuf>) -> Option<Signal> {
    let dir = working_dir.as_ref()?;

    // Check for open PRs that need attention
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,title,reviewDecision,statusCheckRollup",
            "--limit",
            "5",
        ])
        .current_dir(dir)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let prs: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;

    // Find PRs that need review or have failing CI
    for pr in &prs {
        let number = pr.get("number")?.as_u64()?;
        let title = pr.get("title")?.as_str()?;

        // Check for failing CI
        if let Some(checks) = pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
            let failing = checks
                .iter()
                .filter(|c| {
                    c.get("conclusion")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s == "FAILURE")
                })
                .count();

            if failing > 0 {
                return Some(Signal {
                    source: "github".to_string(),
                    title: format!("CI failing on PR #{number}: {title}"),
                    body: format!(
                        "{failing} check(s) failing on PR #{number} \"{title}\". Please investigate."
                    ),
                });
            }
        }
    }

    None
}

pub(crate) async fn dispatch_signal(bot: &WatchedBot, db: &Db, signal: &Signal) {
    info!("[watcher] dispatching to {}: {}", bot.name, signal.title);

    // Store the signal as a system message in the bot's chat
    let _ = db.add_message(
        &bot.workspace,
        &bot.name,
        "system",
        &format!("**Signal: {}**\n\n{}", signal.title, signal.body),
        None,
    );

    // Build prompt for the bot
    let style = bot
        .response_style
        .as_deref()
        .unwrap_or(DEFAULT_RESPONSE_STYLE);
    let prompt = format!(
        "You are {}, a specialty bot. Your role: {}\n\n\
         A signal just fired:\n\
         **{}**\n\n{}\n\n\
         Investigate this and take appropriate action.\n\n\
         ## Response Style\n\
         {style}",
        bot.name, bot.role, signal.title, signal.body
    );

    // Dispatch to the right provider — resume session if available
    let signal_session_key = format!("signal_{}", bot.name);
    let resume_id = db
        .get_session_id(&bot.workspace, &signal_session_key, "signal")
        .unwrap_or(None);

    let response = match bot.provider.as_str() {
        "codex" => run_codex_autonomous(&prompt, &bot.working_dir)
            .await
            .map(|text| AutonomousResult {
                text,
                session_id: None,
            }),
        "gemini" => run_gemini_autonomous(&prompt, &bot.working_dir)
            .await
            .map(|text| AutonomousResult {
                text,
                session_id: None,
            }),
        _ => run_claude_autonomous(&prompt, &bot.working_dir, resume_id.as_deref()).await,
    };

    // Save session ID for next signal
    if let Ok(ref result) = response
        && let Some(ref sid) = result.session_id
    {
        let _ = db.set_session(&bot.workspace, &signal_session_key, sid, "signal");
    }

    match response {
        Ok(result) => {
            let _ = db.add_message(&bot.workspace, &bot.name, "assistant", &result.text, None);
            info!(
                "[watcher] {} responded ({} chars)",
                bot.name,
                result.text.len()
            );
        }
        Err(e) => {
            let _ = db.add_message(
                &bot.workspace,
                &bot.name,
                "assistant",
                &format!("Error processing signal: {e}"),
                None,
            );
        }
    }
}

/// Result of an autonomous run — includes response text and session ID for reuse.
pub(crate) struct AutonomousResult {
    pub text: String,
    pub session_id: Option<String>,
}

pub(crate) async fn run_claude_autonomous(
    prompt: &str,
    working_dir: &Option<PathBuf>,
    resume_session: Option<&str>,
) -> Result<AutonomousResult, String> {
    use apiari_claude_sdk::{
        ClaudeClient, Event, SessionOptions, streaming::AssembledEvent, types::ContentBlock,
    };

    let opts = SessionOptions {
        dangerously_skip_permissions: true,
        include_partial_messages: false, // proactive bots don't need streaming
        working_dir: working_dir.clone(),
        max_turns: Some(10),
        resume: resume_session.map(String::from),
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client.spawn(opts).await.map_err(|e| e.to_string())?;
    session
        .send_message(prompt)
        .await
        .map_err(|e| e.to_string())?;

    let mut full_text = String::new();
    let mut session_id = None;
    loop {
        match session.next_event().await {
            Ok(Some(event)) => match event {
                Event::Stream { assembled, .. } => {
                    for asm in assembled {
                        if let AssembledEvent::TextDelta { text, .. } = asm {
                            full_text.push_str(&text);
                        }
                    }
                }
                Event::Assistant { message, .. } => {
                    for block in &message.message.content {
                        if let ContentBlock::Text { text } = block
                            && full_text.is_empty()
                        {
                            full_text.push_str(text);
                        }
                    }
                }
                Event::Result(result) => {
                    session_id = Some(result.session_id.clone());
                    break;
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }

    Ok(AutonomousResult {
        text: full_text,
        session_id,
    })
}

pub(crate) async fn run_codex_autonomous(
    prompt: &str,
    working_dir: &Option<PathBuf>,
) -> Result<String, String> {
    let client = apiari_codex_sdk::CodexClient::new();
    let mut execution = client
        .exec(
            prompt,
            apiari_codex_sdk::ExecOptions {
                full_auto: true,
                working_dir: working_dir.clone(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    let mut response = String::new();
    while let Ok(Some(event)) = execution.next_event().await {
        if let apiari_codex_sdk::Event::ItemCompleted { item } = &event
            && let Some(text) = item.text()
        {
            response = text.to_string();
        }
    }
    Ok(response)
}

pub(crate) async fn run_gemini_autonomous(
    prompt: &str,
    working_dir: &Option<PathBuf>,
) -> Result<String, String> {
    let client = apiari_gemini_sdk::GeminiClient::new();
    let mut execution = client
        .exec(
            prompt,
            apiari_gemini_sdk::GeminiOptions {
                working_dir: working_dir.clone(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    let mut response = String::new();
    while let Ok(Some(event)) = execution.next_event().await {
        if let apiari_gemini_sdk::Event::ItemCompleted { item } = &event
            && let Some(text) = item.text()
        {
            response = text.to_string();
        }
    }
    Ok(response)
}

//! Coordinator agent — the main chat loop that helps plan and manage quests.
//!
//! The coordinator is a Claude agent session that has access to workspace
//! context, active quests, and incoming signals. It helps the user plan work,
//! break quests into tasks, and dispatch workers.

use crate::daemon::config::DaemonConfig;
use crate::quest::{Quest, QuestStore, TaskStatus};
use crate::worker::Worker;
use crate::workspace::Workspace;
use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use color_eyre::eyre::{Result, bail};
use std::io::{self, BufRead, Write};

/// The coordinator agent manages quests and orchestrates workers.
pub struct Coordinator {
    workspace: Workspace,
    quest_store: QuestStore,
}

impl Coordinator {
    /// Create a new coordinator for the given workspace.
    pub fn new(workspace: Workspace, quest_store: QuestStore) -> Self {
        Self {
            workspace,
            quest_store,
        }
    }

    /// Build the system prompt for the coordinator agent.
    ///
    /// Includes workspace context, active quests, and conventions so the
    /// Claude agent can make informed decisions.
    pub fn build_system_prompt(&self) -> Result<String> {
        let quests = self.quest_store.list()?;
        let active_quests = quests
            .iter()
            .filter(|q| {
                matches!(
                    q.status,
                    crate::quest::QuestStatus::Active | crate::quest::QuestStatus::Planning
                )
            })
            .collect::<Vec<_>>();

        let mut prompt = String::new();

        // Use the soul as the primary personality if available,
        // otherwise fall back to a minimal default.
        if let Some(soul) = &self.workspace.soul {
            prompt.push_str(soul);
            prompt.push('\n');
        } else {
            prompt.push_str(
                "You are the Hive coordinator — the orchestration brain of the Apiari project.\n\n",
            );
        }

        prompt.push_str("## Identity\n");
        if self.workspace.soul.is_some() {
            prompt.push_str("CRITICAL IDENTITY RULE: You ARE the persona defined in the Soul section above. Stay in character at ALL times.\n");
            prompt.push_str("You are NOT Claude, NOT an AI assistant, NOT Claude Code. You do not mention Claude when referring to yourself.\n");
            prompt.push_str("If asked who you are, respond as your Soul persona. If asked about Claude, treat it as a separate thing — Claude is the AI that powers swarm workers, not you.\n");
            prompt.push_str("You are talking to the user right now via `hive chat` or the `hive daemon` Telegram bot.\n");
        } else {
            prompt.push_str("You ARE hive. The user is talking to you right now via `hive chat` or the `hive daemon` Telegram bot.\n");
        }
        prompt.push_str("Hive is the orchestration layer — it plans quests, dispatches work to swarm agents, and manages the workspace.\n");
        prompt.push_str("Buzz is a SEPARATE tool — it's a signal aggregator that polls external sources (Sentry, GitHub) and writes signals.\n");
        prompt.push_str("Hive's daemon mode can READ buzz signals and auto-triage them, but buzz and hive are different things.\n");
        prompt.push_str("When the user asks about setting up Telegram, they mean the hive daemon, not buzz.\n\n");
        prompt.push_str("CRITICAL: You already know everything about hive's features, daemon mode, config format, and setup steps.\n");
        prompt.push_str("This information is in YOUR system prompt below. Do NOT use Read, Glob, Grep, or Bash to look up\n");
        prompt.push_str("hive/daemon/channel code. You already have it. Just answer directly from your system prompt.\n");
        prompt.push_str("Only use tools when the user asks you to DO something (write a file, run a command, search THEIR code).\n\n");

        // Workspace context
        prompt.push_str("## Workspace\n");
        prompt.push_str(&format!("Name: {}\n", self.workspace.name));
        if !self.workspace.repos.is_empty() {
            prompt.push_str("Repositories:\n");
            for repo in &self.workspace.repos {
                prompt.push_str(&format!("  - {repo}\n"));
            }
        }
        if let Some(conventions) = &self.workspace.conventions {
            prompt.push_str(&format!("\n## Conventions\n{conventions}\n"));
        }

        // Available tools
        prompt.push_str("\n## Tools\n");
        prompt.push_str("You have access to Bash, Read, Glob, and Grep.\n");
        prompt.push_str("You can dispatch work to swarm agents using these commands:\n");
        prompt.push_str(&format!(
            "  swarm -d '{}' create \"task description\"  — spin up a new agent in its own worktree\n",
            self.workspace.root.display()
        ));
        prompt.push_str(&format!(
            "  swarm -d '{}' status --json              — check status of all worktrees\n",
            self.workspace.root.display()
        ));
        prompt.push_str(&format!(
            "  swarm -d '{}' send <worktree-id> \"msg\"   — send a message to a running agent\n",
            self.workspace.root.display()
        ));
        prompt.push_str(&format!(
            "  swarm -d '{}' merge <worktree-id>        — merge a worktree's branch into base\n",
            self.workspace.root.display()
        ));
        prompt.push_str(&format!(
            "  swarm -d '{}' close <worktree-id>        — close and clean up a worktree\n",
            self.workspace.root.display()
        ));
        prompt.push_str(
            "IMPORTANT: Always use the -d flag with swarm commands to target the workspace root.\n",
        );
        prompt.push_str("When the user asks you to do work, use swarm create to dispatch it.\n");
        prompt.push_str("IMPORTANT: Swarm workers must plan and execute in one go. When dispatching tasks, include this in the task description: ");
        prompt.push_str("'Plan and implement this completely in one session — do not pause mid-task to ask for confirmation or approval. ");
        prompt.push_str("Use the Plan subagent internally if helpful, but then immediately proceed to implement, commit, and open a PR without waiting for input.'\n\n");

        prompt.push_str("## Multi-repo Dispatch\n");
        prompt.push_str("When you receive a message starting with [DISPATCH], the user wants to dispatch a task to multiple repos.\n");
        prompt.push_str("For each listed target repo:\n");
        prompt.push_str("1. Consider the repo's role (backend vs SDK, provider vs consumer, library vs binary)\n");
        prompt.push_str("2. Produce a ```dispatch:<repo> block with a clear, actionable prompt tailored to that repo\n");
        prompt.push_str("3. Include relevant context about shared API contracts or interfaces\n");
        prompt.push_str(
            "4. Each prompt must be self-contained — workers cannot see other repos' prompts\n",
        );
        prompt.push_str("Do NOT research the codebase or use tools when handling [DISPATCH]. Focus solely on producing dispatch blocks.\n\n");

        // Hive CLI subcommands the coordinator can invoke directly.
        prompt.push_str("## Hive CLI Subcommands\n");
        prompt.push_str("You can run these directly via Bash:\n");
        prompt.push_str(
            "  hive status                              — show workspace and quest status\n",
        );
        prompt.push_str("  hive plan \"<description>\"                — plan a new quest\n");
        prompt.push_str(
            "  hive start <quest-id>                    — dispatch quest tasks to swarm\n",
        );
        prompt.push_str(
            "  hive buzz --once                         — single poll of signal sources\n",
        );
        prompt.push_str(
            "  hive dashboard --once                    — print dashboard status and exit\n\n",
        );

        prompt.push_str("### Reminders\n");
        prompt.push_str("Schedule one-shot or repeating reminders that fire as buzz signals:\n");
        prompt
            .push_str("  hive remind <duration> <message>         — one-shot (e.g. 30m, 2h, 1d)\n");
        prompt.push_str(
            "  hive remind --cron \"<expr>\" <message>    — repeating (cron expression)\n",
        );
        prompt.push_str("  hive reminders                           — list pending reminders\n");
        prompt
            .push_str("  hive reminders cancel <id>               — cancel a specific reminder\n");
        prompt.push_str(
            "Use reminders proactively: set follow-ups after dispatching work, schedule\n",
        );
        prompt.push_str("check-ins on long-running tasks, remind yourself to review PRs, etc.\n\n");

        prompt.push_str("### Sending Telegram Messages\n");
        prompt.push_str(
            "You can message the user directly via Telegram for proactive communication —\n",
        );
        prompt.push_str(
            "when a long task finishes, when you notice an issue, or for any async notification.\n",
        );
        // Inject actual bot token and chat_id if daemon config is available.
        if let Ok(cfg) = DaemonConfig::load(&self.workspace.root) {
            prompt.push_str(&format!(
                "  curl -s -X POST \"https://api.telegram.org/bot{}/sendMessage\" \\\n",
                cfg.telegram.bot_token
            ));
            prompt.push_str(&format!(
                "    -d chat_id={} -d text=\"Your message here\"\n\n",
                cfg.telegram.alert_chat_id
            ));
        } else {
            prompt.push_str(
                "  curl -s -X POST \"https://api.telegram.org/bot<BOT_TOKEN>/sendMessage\" \\\n",
            );
            prompt.push_str("    -d chat_id=<CHAT_ID> -d text=\"Your message here\"\n");
            prompt.push_str(
                "Read bot_token and alert_chat_id from .hive/daemon.toml [telegram] section.\n\n",
            );
        }

        // Daemon setup help — give the coordinator full context so it doesn't need to read files.
        let daemon_config_path = self.workspace.root.join(".hive/daemon.toml");
        prompt.push_str("\n## Daemon Mode (hive daemon)\n");
        prompt.push_str(
            "Hive has a fully built Telegram bot daemon. Run `hive daemon` to start it.\n",
        );
        prompt.push_str("It connects to Telegram via long-polling, routes messages to Claude coordinator sessions,\n");
        prompt.push_str("and auto-triages buzz signals (Critical/Warning) by sending them to an ephemeral Claude\n");
        prompt.push_str("session and forwarding the assessment to Telegram.\n\n");

        prompt.push_str("Features:\n");
        prompt.push_str("- Each chat/group gets its own independent Claude session (persisted across daemon restarts)\n");
        prompt.push_str("- Bot commands: /reset (archive session, start fresh), /history (list past sessions),\n");
        prompt.push_str("  /resume <id-prefix> (switch back to old session), /status (workspace + session info), /help\n");
        prompt.push_str("- Turn nudge: after 50+ turns suggests /reset for better performance\n");
        prompt.push_str("- Works in DMs and groups (one bot, multiple chats)\n");
        prompt.push_str("- Security: allowed_user_ids and allowed_chat_ids filtering\n");
        prompt.push_str(
            "- For groups: must disable privacy mode via @BotFather → /setprivacy → Disable\n\n",
        );

        if daemon_config_path.exists() {
            prompt.push_str(
                "Status: CONFIGURED — .hive/daemon.toml exists. Run `hive daemon` to start.\n\n",
            );
        } else {
            prompt.push_str("Status: NOT YET CONFIGURED — .hive/daemon.toml does not exist.\n\n");
        }

        prompt.push_str("### Setup Steps (walk the user through these)\n");
        prompt.push_str("1. Open Telegram, message @BotFather\n");
        prompt.push_str("2. Send /newbot, choose a display name (e.g. 'Hive Coordinator'), choose a username ending in 'bot'\n");
        prompt.push_str(
            "3. BotFather replies with a bot token like 7000000000:AAxxxxxxxxxxxxxxxxx — copy it\n",
        );
        prompt.push_str(
            "4. Message the new bot (send it anything like 'hello'), or add it to a group\n",
        );
        prompt.push_str("5. Get the chat ID by running:\n");
        prompt.push_str("   curl -s \"https://api.telegram.org/bot<TOKEN>/getUpdates\" | jq '.result[0].message.chat.id'\n");
        prompt.push_str("   (Group IDs are negative numbers starting with -100)\n");
        prompt.push_str(
            "6. For groups: message @BotFather, /setprivacy, select bot, choose Disable\n",
        );
        prompt.push_str("7. Give the token and chat ID to this coordinator and it will write the config file\n\n");

        prompt.push_str("### Config File (.hive/daemon.toml)\n");
        prompt.push_str("```toml\n");
        prompt.push_str("model = \"sonnet\"                           # optional, default\n");
        prompt.push_str("nudge_turn_threshold = 50                   # optional, default\n");
        prompt.push_str("buzz_signals_path = \".buzz/signals.jsonl\"   # optional, default\n");
        prompt.push_str("buzz_poll_interval_secs = 30                # optional, default\n");
        prompt.push_str("allowed_chat_ids = []                       # empty = allow all chats\n");
        prompt.push_str("allowed_user_ids = []                       # empty = allow all users\n");
        prompt.push_str("\n[telegram]\n");
        prompt.push_str(
            "bot_token = \"7000000000:AAxxxxxxxxxxxxxxxxx\" # REQUIRED from @BotFather\n",
        );
        prompt.push_str(
            "alert_chat_id = 123456789                   # REQUIRED: where buzz alerts go\n",
        );
        prompt.push_str("```\n\n");
        prompt.push_str("IMPORTANT: When the user gives you a bot token and chat ID, write .hive/daemon.toml for them.\n");
        prompt.push_str("Do NOT search the codebase for this info — you already have everything you need above.\n\n");

        // Active quests
        if !active_quests.is_empty() {
            prompt.push_str("\n## Active Quests\n");
            for quest in &active_quests {
                prompt.push_str(&format!("- {}\n", quest.summary()));
            }
        }

        Ok(prompt)
    }

    /// Build a planning-focused system prompt for quest decomposition.
    fn build_planning_prompt(&self) -> Result<String> {
        let base = self.build_system_prompt()?;
        let mut prompt = base;
        prompt.push_str("\n## Planning Mode\n");
        prompt.push_str("You are in planning mode. The user will describe a goal.\n");
        prompt.push_str("Break it down into concrete, actionable tasks.\n");
        prompt.push_str("Respond with a list of task titles, one per line, prefixed with '- '.\n");
        prompt.push_str("Keep each task title short and specific (under 80 characters).\n");
        prompt.push_str("Order tasks by dependency — independent tasks first.\n");
        prompt.push_str("Aim for 2-8 tasks. Each task should be completable by a single agent.\n");
        Ok(prompt)
    }

    /// Attempt to spawn a Claude session with the given options.
    ///
    /// Returns `Ok(Some(session))` on success, or `Ok(None)` if the Claude
    /// CLI cannot be found or spawned, allowing the caller to fall back
    /// gracefully.
    async fn try_spawn_session(
        &self,
        opts: SessionOptions,
    ) -> Result<Option<apiari_claude_sdk::Session>> {
        let client = ClaudeClient::new();
        match client.spawn(opts).await {
            Ok(session) => Ok(Some(session)),
            Err(apiari_claude_sdk::SdkError::ProcessSpawn(e)) => {
                tracing::error!(error = %e, "Could not spawn Claude CLI");
                tracing::error!("Make sure `claude` is installed and on your PATH");
                Ok(None)
            }
            Err(e) => Err(color_eyre::eyre::eyre!(e).wrap_err("failed to spawn Claude session")),
        }
    }

    /// Run an interactive chat loop using the Claude SDK.
    ///
    /// Spawns a Claude agent session with the system prompt and relays
    /// messages between stdin and the session. Falls back to a simple
    /// echo mode if the Claude CLI cannot be spawned.
    pub async fn run_chat(&mut self) -> Result<()> {
        let system_prompt = self.build_system_prompt()?;

        let opts = SessionOptions {
            system_prompt: Some(system_prompt),
            model: Some("sonnet".into()),
            working_dir: Some(self.workspace.root.clone()),
            allowed_tools: vec![
                "Bash".into(),
                "Read".into(),
                "Write".into(),
                "Edit".into(),
                "Glob".into(),
                "Grep".into(),
                "WebSearch".into(),
                "WebFetch".into(),
            ],
            ..Default::default()
        };

        eprint!("\x1b[2mConnecting to Claude...\x1b[0m");
        let session = self.try_spawn_session(opts).await?;
        eprint!("\r\x1b[K");
        match session {
            Some(session) => self.run_chat_with_session(session).await,
            None => self.run_chat_fallback(),
        }
    }

    /// Run the chat loop, spawning a new Claude session per turn with --resume.
    ///
    /// The `--print` mode is single-turn, so we spawn a fresh session for each
    /// user message and resume the previous conversation by session ID.
    async fn run_chat_with_session(&self, mut session: apiari_claude_sdk::Session) -> Result<()> {
        eprintln!("\x1b[33mHive Coordinator\x1b[0m — Ctrl-D to exit\n");

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        // We need to read stdin in a background thread because it's blocking.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(32);

        // Spawn a blocking thread to read stdin lines.
        let stdin_handle = tokio::task::spawn_blocking(move || {
            for line in stdin.lock().lines() {
                match line {
                    Ok(line) => {
                        let trimmed = line.trim().to_owned();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if tx.blocking_send(trimmed).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break, // EOF or error
                }
            }
        });

        // Print the initial prompt immediately.
        print!("\x1b[1;33m> \x1b[0m");
        stdout.flush()?;

        // Track the session ID across turns for --resume.
        let mut session_id: Option<String> = None;
        let mut first_turn = true;

        loop {
            tokio::select! {
                user_input = rx.recv() => {
                    match user_input {
                        Some(input) => {
                            // For subsequent turns, spawn a new session resuming the previous one.
                            if !first_turn {
                                let opts = SessionOptions {
                                    resume: session_id.clone(),
                                    model: Some("sonnet".into()),
                                    working_dir: Some(self.workspace.root.clone()),
                                    allowed_tools: vec![
                                        "Bash".into(),
                                        "Read".into(),
                                        "Glob".into(),
                                        "Grep".into(),
                                        "WebSearch".into(),
                                        "WebFetch".into(),
                                    ],
                                    ..Default::default()
                                };
                                match self.try_spawn_session(opts).await? {
                                    Some(s) => session = s,
                                    None => {
                                        eprintln!("\x1b[31mFailed to resume session.\x1b[0m");
                                        break;
                                    }
                                }
                            }

                            // Send the message.
                            if let Err(e) = session.send_message(&input).await {
                                eprintln!("\x1b[31mError: {e}\x1b[0m");
                                break;
                            }

                            eprint!("\x1b[2;33m  thinking...\x1b[0m");

                            // Read events until the assistant turn completes.
                            let turn_session_id = self.drain_assistant_turn(&mut session, &mut stdout).await?;
                            if let Some(sid) = turn_session_id {
                                session_id = Some(sid);
                            }
                            first_turn = false;

                            // Print prompt for next input.
                            print!("\x1b[1;33m> \x1b[0m");
                            stdout.flush()?;
                        }
                        None => {
                            eprintln!("\n\x1b[2mSession closed.\x1b[0m");
                            break;
                        }
                    }
                }
            }
        }

        // Clean up the session.
        session.close_stdin();
        stdin_handle.abort();

        Ok(())
    }

    /// Read events from the session until the current assistant turn ends.
    ///
    /// Prints text content to stdout and logs tool use activity to stderr.
    /// Returns the session_id from the Result event, if any.
    async fn drain_assistant_turn(
        &self,
        session: &mut apiari_claude_sdk::Session,
        stdout: &mut io::Stdout,
    ) -> Result<Option<String>> {
        let mut has_output = false;
        let mut result_session_id = None;

        loop {
            let event = session.next_event().await.map_err(|e| {
                color_eyre::eyre::eyre!(e).wrap_err("error reading event from Claude")
            })?;

            match event {
                Some(Event::Assistant { message, tool_uses }) => {
                    // Print text content blocks in cyan.
                    for block in &message.message.content {
                        if let ContentBlock::Text { text } = block {
                            if !has_output {
                                // Clear the "thinking..." line before first output.
                                eprint!("\r\x1b[K");
                                has_output = true;
                            }
                            writeln!(stdout, "\x1b[36m{text}\x1b[0m")?;
                        }
                    }
                    stdout.flush()?;

                    // Show a single-line status when using tools (overwrites in place).
                    if !tool_uses.is_empty() {
                        let names: Vec<_> = tool_uses.iter().map(|t| t.name.as_str()).collect();
                        eprint!("\r\x1b[K\x1b[2;33m  using {}...\x1b[0m", names.join(", "));
                    }
                }
                Some(Event::Result(result)) => {
                    if !has_output {
                        eprint!("\r\x1b[K");
                    }
                    // Clear any leftover status line.
                    eprint!("\r\x1b[K");
                    result_session_id = Some(result.session_id);
                    break;
                }
                Some(Event::RateLimit(_))
                | Some(Event::Stream { .. })
                | Some(Event::User(_))
                | Some(Event::System(_)) => {
                    // Informational — keep reading.
                }
                None => {
                    eprint!("\r\x1b[K");
                    eprintln!("\x1b[31mSession ended unexpectedly.\x1b[0m");
                    break;
                }
            }
        }
        Ok(result_session_id)
    }

    /// Fallback chat loop when Claude CLI is not available.
    fn run_chat_fallback(&self) -> Result<()> {
        eprintln!("\x1b[33mHive Coordinator\x1b[0m — \x1b[2moffline mode\x1b[0m");
        eprintln!("\x1b[2mClaude CLI not available. Ctrl-D to exit.\x1b[0m\n");

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        print!("\x1b[1;33m> \x1b[0m");
        stdout.flush()?;

        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim();

            if trimmed.is_empty() {
                print!("\x1b[1;33m> \x1b[0m");
                stdout.flush()?;
                continue;
            }

            writeln!(
                stdout,
                "\x1b[36m[offline] Would send to Claude: {trimmed:?}\x1b[0m"
            )?;
            print!("\x1b[1;33m> \x1b[0m");
            stdout.flush()?;
        }

        Ok(())
    }

    /// Plan a new quest from a description.
    ///
    /// Sends the description to Claude to produce a quest with tasks.
    /// Falls back to stub tasks if the Claude CLI is unavailable.
    pub async fn plan_quest(&mut self, description: &str) -> Result<Quest> {
        tracing::info!(description, "Planning quest");

        let planning_prompt = self.build_planning_prompt()?;

        let opts = SessionOptions {
            system_prompt: Some(planning_prompt),
            model: Some("sonnet".into()),
            max_turns: Some(7),
            working_dir: Some(self.workspace.root.clone()),
            ..Default::default()
        };

        let session = self.try_spawn_session(opts).await?;
        let tasks = match session {
            Some(session) => self.plan_with_session(session, description).await?,
            None => {
                tracing::warn!("Claude CLI not available, creating stub quest");
                self.stub_tasks()
            }
        };

        let mut quest = Quest::new(description, description);
        for title in &tasks {
            quest.add_task(title);
        }

        self.quest_store.save(&quest)?;
        Ok(quest)
    }

    /// Use a live Claude session to break a description into tasks.
    async fn plan_with_session(
        &self,
        mut session: apiari_claude_sdk::Session,
        description: &str,
    ) -> Result<Vec<String>> {
        let prompt = format!("Plan the following quest and list the tasks:\n\n{description}");

        session
            .send_message(&prompt)
            .await
            .map_err(|e| color_eyre::eyre::eyre!(e).wrap_err("failed to send planning message"))?;

        // Close stdin so Claude knows we're done sending.
        session.close_stdin();

        let mut response_text = String::new();

        // Read all events until the session completes.
        loop {
            let event = session.next_event().await.map_err(|e| {
                color_eyre::eyre::eyre!(e).wrap_err("error reading planning response")
            })?;

            match event {
                Some(Event::Assistant { message, .. }) => {
                    for block in &message.message.content {
                        if let ContentBlock::Text { text } = block {
                            response_text.push_str(text);
                        }
                    }
                }
                Some(Event::Result(result)) => {
                    tracing::info!(
                        turns = result.num_turns,
                        cost = result.total_cost_usd.unwrap_or(0.0),
                        "Planning complete"
                    );
                    // Also capture the result text if present.
                    if let Some(text) = &result.result
                        && response_text.is_empty()
                    {
                        response_text = text.clone();
                    }
                    break;
                }
                Some(_) => {
                    // System, User, Stream, RateLimit — continue.
                }
                None => break,
            }
        }

        // Parse task titles from the response.
        let tasks = self.parse_task_list(&response_text);

        if tasks.is_empty() {
            tracing::warn!("Claude did not return parseable tasks, using stub tasks");
            Ok(self.stub_tasks())
        } else {
            Ok(tasks)
        }
    }

    /// Parse a list of task titles from Claude's response.
    ///
    /// Looks for lines starting with `- ` or `* ` or numbered lists like
    /// `1. `, `2. `, etc.
    fn parse_task_list(&self, text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                // Match "- task", "* task", "1. task", "2. task", etc.
                let title = if let Some(rest) = trimmed.strip_prefix("- ") {
                    Some(rest.trim().to_owned())
                } else if let Some(rest) = trimmed.strip_prefix("* ") {
                    Some(rest.trim().to_owned())
                } else {
                    // Try numbered list: "1. ", "2. ", "10. ", etc.
                    // Find the first non-digit character.
                    let digit_end = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
                    if digit_end > 0 {
                        let after_digits = &trimmed[digit_end..];
                        after_digits
                            .strip_prefix(". ")
                            .or_else(|| after_digits.strip_prefix(") "))
                            .map(|rest| rest.trim().to_owned())
                    } else {
                        None
                    }
                };

                title.filter(|t| !t.is_empty() && t.len() < 200).map(|t| {
                    // Strip any leading bold markdown like **task**
                    let t = t.trim_start_matches("**");
                    let t = if let Some(idx) = t.find("**") {
                        // Take just what's between the markers, or the
                        // whole string if only opening markers.
                        let inside = &t[..idx];
                        if inside.is_empty() { t } else { inside }
                    } else {
                        t
                    };
                    t.trim().to_owned()
                })
            })
            .collect()
    }

    /// Generate fallback stub tasks when Claude is unavailable.
    fn stub_tasks(&self) -> Vec<String> {
        vec![
            "Analyze requirements".to_owned(),
            "Implement solution".to_owned(),
            "Write tests".to_owned(),
            "Review and merge".to_owned(),
        ]
    }

    /// Start working on a quest by transitioning it to Active status
    /// and dispatching workers for pending tasks.
    pub async fn start_quest(&mut self, quest_id: &str) -> Result<Quest> {
        let mut quest = self
            .quest_store
            .load(quest_id)?
            .ok_or_else(|| color_eyre::eyre::eyre!("quest not found: {quest_id}"))?;

        if quest.status == crate::quest::QuestStatus::Complete {
            bail!("quest {quest_id} is already complete");
        }

        quest.status = crate::quest::QuestStatus::Active;
        quest.updated_at = chrono::Utc::now();
        self.quest_store.save(&quest)?;

        tracing::info!(quest_id = &quest.id[..8], "Quest is now active");

        // Dispatch workers for pending tasks.
        let worker = Worker::new();
        for task in &mut quest.tasks {
            if task.status == TaskStatus::Pending {
                tracing::info!(task = %task.title, "Dispatching worker");
                match worker.spawn_worker(task).await {
                    Ok(worktree_id) => {
                        tracing::info!(task = %task.title, worktree_id, "Worker spawned");
                        task.assigned_to = Some(worktree_id);
                        task.status = TaskStatus::InProgress;
                    }
                    Err(e) => {
                        tracing::error!(task = %task.title, error = %e, "Failed to spawn worker");
                        // Don't fail the whole quest — just log and continue.
                    }
                }
            }
        }

        // Save updated task statuses.
        self.quest_store.save(&quest)?;

        Ok(quest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quest::QuestStore;
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn make_coordinator() -> Coordinator {
        let workspace = Workspace::default();
        // QuestStore path is never accessed by parse_task_list.
        let quest_store = QuestStore::new(PathBuf::from("/tmp/hive_test_coordinator_quests"));
        Coordinator::new(workspace, quest_store)
    }

    #[test]
    fn parse_dash_list() {
        let c = make_coordinator();
        let tasks = c.parse_task_list("- Task one\n- Task two\n- Task three\n");
        assert_eq!(tasks, vec!["Task one", "Task two", "Task three"]);
    }

    #[test]
    fn parse_asterisk_list() {
        let c = make_coordinator();
        let tasks = c.parse_task_list("* First task\n* Second task\n");
        assert_eq!(tasks, vec!["First task", "Second task"]);
    }

    #[test]
    fn parse_numbered_dot_list() {
        let c = make_coordinator();
        let tasks = c.parse_task_list("1. Task A\n2. Task B\n10. Task C\n");
        assert_eq!(tasks, vec!["Task A", "Task B", "Task C"]);
    }

    #[test]
    fn parse_numbered_paren_list() {
        let c = make_coordinator();
        let tasks = c.parse_task_list("1) First\n2) Second\n");
        assert_eq!(tasks, vec!["First", "Second"]);
    }

    #[test]
    fn parse_strips_bold_markdown() {
        let c = make_coordinator();
        let tasks = c.parse_task_list("- **Bold task**\n- **Another bold** with more\n");
        assert_eq!(tasks, vec!["Bold task", "Another bold"]);
    }

    #[test]
    fn parse_skips_non_list_lines() {
        let c = make_coordinator();
        let text = "- Task one\n\n- Task two\n\nsome prose\n- Task three\n";
        let tasks = c.parse_task_list(text);
        assert_eq!(tasks, vec!["Task one", "Task two", "Task three"]);
    }

    #[test]
    fn parse_filters_too_long_titles() {
        let c = make_coordinator();
        let long = "x".repeat(201);
        let text = format!("- {long}\n- Short task\n");
        let tasks = c.parse_task_list(&text);
        assert_eq!(tasks, vec!["Short task"]);
    }

    #[test]
    fn parse_returns_empty_for_blank_text() {
        let c = make_coordinator();
        assert!(c.parse_task_list("").is_empty());
    }

    #[test]
    fn parse_mixed_formats() {
        let c = make_coordinator();
        let text = "Intro text\n\n- Task from dash\n* Task from asterisk\n1. Numbered task\n\nConclusion\n";
        let tasks = c.parse_task_list(text);
        assert_eq!(
            tasks,
            vec!["Task from dash", "Task from asterisk", "Numbered task"]
        );
    }
}

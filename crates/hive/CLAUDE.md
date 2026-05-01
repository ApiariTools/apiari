# Hive

Orchestration brain — plans quests, dispatches tasks to swarm agents, aggregates signals, and provides a dashboard.

## Quick Reference

```bash
cargo build -p hive                      # Build
cargo test -p hive                       # Run tests (101 unit tests)
cargo run -p hive -- init                # Initialize workspace
cargo run -p hive -- chat                # Interactive coordinator chat
cargo run -p hive -- plan "..."          # Plan a quest
cargo run -p hive -- status              # Show quest status
cargo run -p hive -- start ID            # Dispatch quest tasks to swarm
cargo run -p hive -- daemon start        # Start Telegram bot daemon
cargo run -p hive -- buzz --once         # Single signal poll
cargo run -p hive -- buzz --daemon       # Continuous signal polling
cargo run -p hive -- dashboard           # Launch TUI dashboard
cargo run -p hive -- dashboard --once    # Print status and exit
```

## Swarm Worker Rules

1. **You are working in a git worktree.** Always create a new branch (`swarm/*`), never commit directly to `main`.
2. **Only modify files within this repo (`hive/`).** Do not touch other repos in the workspace (e.g., `common/`, `claude-sdk/`, `swarm/`).
3. **When done, create a PR:**
   ```bash
   gh pr create --repo ApiariTools/hive --title "..." --body "..."
   ```
4. **Always run `cargo fmt -p hive` before committing.** CI enforces formatting — unformatted code will fail the build.
5. **After cloning or entering a worktree, run `bash hooks/install.sh` to activate the pre-commit hook.**
6. **Do not run `cargo install` or modify system state.** No global installs, no modifying dotfiles, no system-level changes.
7. **Plan and execute in one go.** When you receive a task via `swarm create`, plan and implement it completely in one session — do not pause mid-task to ask for confirmation or approval. Use the Plan subagent internally if helpful, but then immediately proceed to implement, commit, and open a PR without waiting for input.

## Git Workflow

- You are working in a swarm worktree on a `swarm/*` branch. Stay on this branch.
- NEVER push to or merge into `main` directly.
- NEVER run `git push origin main` or `git checkout main`.
- When done, push your branch and open a PR. Swarm will handle merging.

## Architecture

```
src/
  main.rs               # CLI (clap) with subcommands
  signal.rs             # Signal/Severity types (shared across buzz and daemon)
  workspace/
    mod.rs              # Workspace config (YAML), load/init, config search
  quest/
    mod.rs              # Quest, Task, QuestStatus, TaskStatus, QuestStore
  coordinator/
    mod.rs              # Claude SDK chat loop, planning, system prompt
  worker/
    mod.rs              # Subprocess calls to swarm CLI
  github/
    mod.rs              # gh CLI wrapper (issues, PRs)
  channel/
    mod.rs              # Channel trait + ChannelEvent types
    telegram.rs         # Telegram Bot API client
  daemon/
    mod.rs              # Telegram bot + buzz auto-triage event loop
    config.rs           # Daemon config (daemon.toml)
    session_store.rs    # Per-chat Claude session tracking
  buzz/
    mod.rs              # Signal aggregator entry point (run logic)
    config.rs           # BuzzConfig TOML schema + validation
    output.rs           # OutputMode (stdout, file, webhook)
    reminder.rs         # Scheduled reminder signals
    signal.rs           # deduplicate(), prioritize() functions
    watcher/
      mod.rs            # Watcher trait, state persistence, registry
      github.rs         # GitHub watcher (gh CLI)
      sentry.rs         # Sentry watcher (HTTP API)
      webhook.rs        # Webhook receiver (stub)
  keeper/
    mod.rs              # Dashboard entry point (run logic)
    discovery.rs        # Swarm session discovery, buzz signal reading, PR queries
    tui/
      mod.rs            # TUI event loop (crossterm key handling)
      app.rs            # App state (sessions, selection, panels, overlays)
      render.rs         # Ratatui rendering
      theme.rs          # Honey/amber bee color palette
```

## Key Concepts

- **Quest**: High-level goal with tasks. Persisted as `.hive/quests/<id>.json`.
- **Task**: Discrete work unit within a quest. Assigned to swarm worktrees.
- **Coordinator**: Uses `apiari-claude-sdk` to run Claude sessions for chat and planning. Falls back to offline mode if `claude` CLI is unavailable.
- **Worker**: Calls `swarm create`, `swarm status`, `swarm send`, `swarm close`, `swarm merge` as subprocesses.
- **QuestStore**: CRUD over `.hive/quests/` directory using `apiari_common::state` for atomic writes.
- **Signal**: Event from external sources (Sentry, GitHub, reminders). Defined in `signal.rs`, produced by buzz watchers, consumed by daemon for triage.
- **Watcher**: Pluggable source that polls for new signals. Trait in `buzz::watcher`.
- **Keeper**: Read-only TUI dashboard that discovers swarm sessions and displays their status.

### Inline Buzz Watchers (daemon mode)

Add to `.hive/daemon.toml` to run buzz watchers directly in the daemon:

```toml
[buzz]
enabled = true
# config_path = ".buzz/config.toml"  # default
```

Configure `.buzz/config.toml` as normal (sentry, github, etc.).
When enabled, the daemon polls watchers on the `buzz_poll_interval_secs` cadence
and auto-triages Critical/Warning signals to Telegram.
Cursor state is persisted to `<workspace_root>/.buzz/state.json` between restarts.

## CLI Subcommands

| Command | Description |
|---------|-------------|
| `init` | Creates `.hive/` dir with `workspace.yaml` and `quests/` |
| `status` | Lists repos and all quests with task progress |
| `chat` | Interactive Claude session with workspace context |
| `plan <desc>` | Claude generates task list, saves as quest |
| `start [id]` | Activates quest, spawns swarm worktree per task |
| `daemon start` | Start persistent Telegram bot + buzz auto-triage |
| `daemon stop` | Stop the running daemon |
| `daemon restart` | Restart the daemon |
| `buzz` | Poll signal sources (Sentry, GitHub, webhooks, reminders) |
| `dashboard` | Launch read-only TUI for monitoring swarm sessions |
| `remind <duration> <message>` | Schedule a one-shot reminder (e.g. 30m, 2h, 1d) |
| `remind --cron "<expr>" <message>` | Schedule a repeating reminder (cron expression) |
| `reminders` | List pending reminders |
| `reminders cancel <id>` | Cancel a specific reminder |

## Available Tools for Proactive Communication

The coordinator (and swarm workers via Bash) can use these tools for scheduling follow-ups and notifying the user.

### Reminders

```bash
hive remind 30m "Check PR #42 status"         # one-shot, fires in 30 minutes
hive remind 2h "Follow up on deploy"           # one-shot, fires in 2 hours
hive remind 1d "Review weekly metrics"         # one-shot, fires in 1 day
hive remind --cron "0 9 * * 1" "Weekly standup reminder"  # repeating
hive reminders                                 # list all pending
hive reminders cancel <id>                     # cancel one
```

Reminders fire as buzz signals, which are picked up by the daemon for auto-triage and Telegram delivery.

### Sending Telegram Messages

Send messages directly to the user's Telegram for proactive notifications (task completion, errors, async updates):

```bash
curl -s -X POST "https://api.telegram.org/bot<BOT_TOKEN>/sendMessage" \
  -d chat_id=<CHAT_ID> -d text="Your message here"
```

Bot token and chat ID are in `.hive/daemon.toml` under `[telegram]`:
- `bot_token` — from @BotFather
- `alert_chat_id` — the user's chat/group ID

## Workspace Config (`.hive/workspace.yaml`)

```yaml
name: "my-project"          # Workspace name [default: "apiari"]
repos:                       # Repos to manage
  - "owner/repo"
default_agent: "claude"      # "claude" or "codex"
soul: |                      # Coordinator personality
  You are a senior engineer...
conventions: |               # Coding standards
  Use Rust, test everything...
```

Config search walks upward from cwd looking for `workspace.yaml`, `workspace.yml`, `.hive/workspace.yaml`, or `.hive/workspace.yml`.

## Integration Map

```
hive plan "Add auth"
  |
  v (Claude SDK)
claude CLI session
  |
  v (creates quest)
.hive/quests/<id>.json

hive start <id>
  |
  v (subprocess)
swarm create "task prompt"    # One per task
  |
  v
.swarm/inbox.jsonl -> swarm TUI -> agent daemon

hive buzz --daemon
  |
  v (writes)
.buzz/signals.jsonl
  |
  +---> hive daemon (reads for auto-triage)
  +---> hive dashboard (reads for display)

hive dashboard
  |
  v (reads)
.swarm/state.json + .buzz/signals.jsonl
```

- **Uses**: `apiari-common` (IPC, state persistence), `apiari-claude-sdk` (coordinator/planning/chat)
- **Calls**: `swarm` CLI via Claude's Bash tool (create, status, send, close, merge)
- **Reads**: buzz signals (daemon auto-triage + dashboard display)
- **GitHub**: `gh` CLI wrapper for issues/PRs

## Files on Disk

```
.hive/
  workspace.yaml              # Workspace configuration
  daemon.toml                 # Daemon configuration
  daemon.pid                  # Daemon PID file
  daemon.log                  # Daemon log output
  sessions.json               # Per-chat session state
  quests/
    <quest-uuid>.json          # Quest with embedded tasks

.buzz/
  config.toml                 # Buzz watcher configuration
  state.json                  # Watcher cursors (auto-managed)
  signals.jsonl               # Signal output (when mode = "file")
```

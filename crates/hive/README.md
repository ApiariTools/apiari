# hive

Orchestration brain for Apiari — plans quests, dispatches work to swarm agents, monitors signals, and runs a Telegram bot daemon.

## What is hive?

Hive is the coordination layer of the [Apiari](https://github.com/ApiariTools) toolchain. It uses a Claude-powered **coordinator** to break high-level goals into structured **quests** (plans with discrete tasks), then dispatches each task to a [swarm](https://github.com/ApiariTools/swarm) agent running in an isolated git worktree. A built-in **buzz** signal aggregator polls external sources (Sentry errors, GitHub issues/PRs) and feeds them into an auto-triage pipeline. The **daemon** ties it all together — a persistent Telegram bot that lets you chat with the coordinator, receive notifications when agents finish or open PRs, and trigger custom commands, all from your phone.

## Requirements

- **Rust 1.93+** (edition 2024)
- **`claude` CLI** — required for coordinator features (chat, plan, auto-triage). Hive falls back gracefully to offline mode if unavailable.
- **`gh` CLI** — required for GitHub buzz watcher and PR queries. Install and authenticate with `gh auth login`.
- **swarm** — for managing parallel agents in git worktrees.

## Install

```bash
cargo install --path .
codesign -s - ~/.cargo/bin/hive   # macOS only — re-signs after install
```

## Quick Start

```bash
# 1. Initialize a workspace
hive init
# Edit .hive/workspace.yaml to add your repos

# 2. Chat with the coordinator
hive chat

# 3. Plan a quest
hive plan "Add user authentication to the API"

# 4. Dispatch quest tasks to swarm agents
hive start <quest-id>

# 5. Monitor progress
hive status
hive dashboard

# 6. Start the Telegram daemon (optional)
# First, create .hive/daemon.toml with your bot token (see Configuration below)
hive daemon start
```

## Subcommands

| Command | Description |
|---------|-------------|
| `init` | Create `.hive/` directory with `workspace.yaml` and `quests/` |
| `status` | Show workspace info, repos, and all quests with task progress |
| `chat` | Interactive Claude session with full workspace context |
| `plan <description>` | Claude generates a structured task list, saved as a quest |
| `start [quest-id]` | Dispatch quest tasks to swarm agents (one worktree per task). Omit ID to list quests. Supports prefix matching. |
| `daemon start [--foreground]` | Start persistent Telegram bot + swarm watcher. Backgrounds by default. |
| `daemon stop` | Stop the running daemon |
| `daemon restart` | Stop and restart the daemon |
| `buzz [--daemon] [--once]` | Poll signal sources (Sentry, GitHub, reminders). `--daemon` runs continuously, `--once` polls and exits. |
| `dashboard [--once]` | Launch read-only TUI for monitoring swarm sessions. `--once` prints status and exits. |
| `remind <duration> <message>` | Schedule a one-shot reminder (e.g. `30m`, `2h`, `1d`) |
| `remind --cron "<expr>" <message>` | Schedule a repeating reminder (cron expression) |
| `reminders` | List all pending reminders |
| `reminders cancel <id>` | Cancel a reminder by ID or ID prefix |

Global flag: `-C <dir>` / `--dir <dir>` sets the working directory (defaults to cwd).

## Configuration

### Workspace (`.hive/workspace.yaml`)

Created by `hive init`. Hive searches upward from cwd for `workspace.yaml`, `workspace.yml`, `.hive/workspace.yaml`, or `.hive/workspace.yml`.

```yaml
name: "my-project"
repos:
  - "myorg/backend"
  - "myorg/frontend"
default_agent: "claude"        # agent kind passed to swarm (default: "claude")
soul: |                        # coordinator personality (or use .hive/soul.md)
  You are a senior engineer who values simplicity and testing.
conventions: |                 # coding standards injected into coordinator context
  Use Rust, write tests for all public functions, prefer color_eyre for errors.
```

If `.hive/soul.md` exists, its contents override the inline `soul` field.

### Daemon (`.hive/daemon.toml`)

Required for `hive daemon start`. Minimal config needs only `[telegram]`:

```toml
# Coordinator settings
model = "sonnet"                   # Claude model (default: "sonnet")
max_turns = 20                     # max agentic turns per message (default: 20)
nudge_turn_threshold = 50          # suggest /reset after this many turns (default: 50)

# Access control (empty = allow all)
allowed_user_ids = []
allowed_chat_ids = []

# Buzz signal ingestion
buzz_signals_path = ".buzz/signals.jsonl"   # default
buzz_poll_interval_secs = 30                # default

[telegram]
bot_token = "7000000000:AAxxxxxxxxxxxxxxxxx"   # from @BotFather
alert_chat_id = -1001234567890                 # chat/group for unprompted notifications

[swarm_watch]
enabled = true                     # watch .swarm/state.json (default: true)
poll_interval_secs = 15            # how often to poll (default: 15)
state_path = ".swarm/state.json"   # default
stall_timeout_secs = 300           # seconds before agent is "stalled" (default: 300, 0 = disabled)
waiting_debounce_secs = 30         # debounce before AgentWaiting fires (default: 30, 0 = disabled)
auto_triage = false                # auto-invoke coordinator on PrOpened/AgentWaiting (default: false)

[buzz]
enabled = false                         # run buzz watchers inline in daemon (default: false)
config_path = ".buzz/config.toml"       # default

# Custom slash commands (available in Telegram)
[commands.restart]
run = "./install.sh"                    # shell command, runs from workspace root
description = "Pull, build, and restart"
then = "restart"                        # post-action: "restart" restarts the daemon

[commands.deploy]
run = "./deploy.sh"
```

### Buzz (`.buzz/config.toml`)

Used by `hive buzz` and optionally by the daemon when `[buzz] enabled = true`.

```toml
poll_interval_secs = 60   # default

[output]
mode = "file"                         # "stdout" | "file" | "webhook"
path = ".buzz/signals.jsonl"          # required for mode = "file"
# url = "https://..."                 # required for mode = "webhook"

[sentry]
token = "sntrys_..."
org = "my-org"
project = "my-project"

[github]
repos = ["myorg/backend", "myorg/frontend"]
watch_labels = ["critical", "P0", "incident"]   # optional

# Webhook receiver (stub — not yet implemented)
# [webhook]
# port = 8088

[[reminders]]
message = "Daily standup"
interval_secs = 86400
```

## How It Works

### Quest Workflow

```
hive plan "Add auth"
  → Claude generates tasks
  → Saved to .hive/quests/<uuid>.json

hive start <id>
  → For each task: swarm create --repo <repo> --prompt-file <task>
  → Each task runs in an isolated git worktree with its own Claude agent
  → Agents commit, push, and open PRs autonomously
```

### Daemon

The daemon runs a Telegram bot alongside background watchers:

- **Telegram bot** — receives messages, routes them to the Claude coordinator, streams replies back. Supports custom `/commands` defined in config.
- **SwarmWatcher** — polls `.swarm/state.json` and emits notifications:
  - `AgentSpawned` — new worker appeared
  - `AgentCompleted` — worker finished its task
  - `AgentClosed` — worker was closed
  - `AgentStalled` — worker inactive beyond threshold (skips `claude-tui` agents)
  - `PrOpened` — worker opened a pull request
  - `AgentWaiting` — worker is waiting for input (includes PR link if available)
- **Auto-triage** — when enabled, automatically invokes the coordinator after certain events to suggest next actions.
- **Reminders** — one-shot and cron-based reminders fire as buzz signals and get delivered via Telegram.

### Buzz

Buzz is a pluggable signal aggregator:

- **Sentry watcher** — polls Sentry for unresolved issues via HTTP API
- **GitHub watcher** — uses `gh` CLI to check for assigned issues, review requests, and labeled items
- **Reminders** — scheduled signals at fixed intervals
- **Webhook receiver** — HTTP endpoint for external integrations (stub)

Signals are deduplicated, prioritized, and written to `.buzz/signals.jsonl`. The daemon reads this file for auto-triage.

## Architecture

```
src/
  main.rs               # CLI entry point (clap subcommands)
  signal.rs             # Signal + Severity types
  workspace/            # Workspace config (YAML), loader, init
  quest/                # Quest, Task, QuestStore (CRUD over .hive/quests/)
  coordinator/          # Claude SDK chat loop, planning, system prompt
  worker/               # Subprocess calls to swarm CLI
  github/               # gh CLI wrapper (issues, PRs)
  channel/              # Channel trait + Telegram Bot API client
  daemon/               # Telegram bot + swarm watcher + auto-triage
  buzz/                 # Signal aggregator (watchers, output, config)
  keeper/               # Dashboard TUI (ratatui + crossterm)
  reminder/             # ReminderStore, one-shot + cron scheduling
```

### Files on Disk

```
.hive/
  workspace.yaml          # Workspace configuration
  daemon.toml             # Daemon configuration
  daemon.pid              # Daemon PID file (created at runtime)
  daemon.log              # Daemon log output
  sessions.json           # Per-chat Claude session state
  reminders.json          # Pending reminders
  pr_notified.json        # Tracks which PRs have been notified
  quests/
    <uuid>.json           # Quest with embedded tasks

.buzz/
  config.toml             # Buzz watcher configuration
  state.json              # Watcher cursors (auto-managed)
  signals.jsonl           # Signal output (when mode = "file")
```

## Development

```bash
cargo build -p hive
cargo test -p hive
cargo fmt -p hive
bash hooks/install.sh     # install pre-commit hook (runs cargo fmt)
```

## License

[MIT](LICENSE)

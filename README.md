# apiari

**Your AI coding fleet, managed from your pocket.**

Apiari is a Rust CLI that runs a persistent daemon to watch your AI coding agents and GitHub repos, then keeps you in the loop via Telegram. When CI breaks, a worker stalls, or a PR lands — you'll know instantly, and you can respond right from your phone through an AI-powered coordinator.

```
┌──────────────────────────────────────────────────────────────┐
│                        Telegram                              │
│                                                              │
│  🐝 Bee (apiari)                                    12:34 PM │
│  ┌──────────────────────────────────────────────┐            │
│  │ ⚠️  CI failure in ApiariTools/swarm            │            │
│  │ Branch: feat/new-watcher                      │            │
│  │ Run: #482 — test_signal_pipeline failed       │            │
│  │ https://github.com/ApiariTools/swarm/actions   │            │
│  └──────────────────────────────────────────────┘            │
│                                                              │
│  🐝 Bee (apiari)                                    12:35 PM │
│  ┌──────────────────────────────────────────────┐            │
│  │ 🔴 Worker stalled: cli-a3f2                   │            │
│  │ Task: refactor auth middleware                │            │
│  │ Last activity: 8 minutes ago                  │            │
│  └──────────────────────────────────────────────┘            │
│                                                              │
│  You:  @bee what's the status of the swarm?      12:36 PM   │
│                                                              │
│  🐝 Bee (apiari)                                    12:36 PM │
│  3 workers active, 1 stalled. The auth refactor             │
│  worker (cli-a3f2) stopped producing output 8 min           │
│  ago. Want me to investigate?                                │
│                                                              │
└──────────────────────────────────────────────────────────────┘

┌──────────────────── apiari ui ───────────────────────────────┐
│ Workspaces: [swarm] [apiari] [web]            Signals: 4 open│
│─────────────────────────────────────────────────────────────-│
│ SRC     │ SIGNAL                    │ SEV  │ STATUS │ AGE    │
│─────────┼───────────────────────────┼──────┼────────┼────────│
│ github  │ CI failure: test_pipeline │ err  │ open   │ 12m    │
│ swarm   │ Worker stalled: cli-a3f2  │ warn │ open   │ 8m     │
│ github  │ New PR #47: fix auth flow │ info │ open   │ 3m     │
│ sentry  │ NullRef in /api/checkout  │ crit │ open   │ 1m     │
└──────────────────────────────────────────────────────────────┘
```

## Install

Apiari is installed from source (not yet on crates.io):

```sh
git clone https://github.com/ApiariTools/apiari.git
cd apiari
cargo install --path crates/apiari
```

On macOS, codesign the binary so it can access the keychain and network:

```sh
codesign -f -s - ~/.cargo/bin/apiari
```

## Quick Start

```sh
# 1. Initialize a workspace from your project directory
cd ~/projects/my-app
apiari init

# 2. Edit the generated config to add your Telegram bot + repos
$EDITOR ~/.config/apiari/workspaces/my-app.toml

# 3. Start the daemon
apiari daemon --background

# 4. Check signal status
apiari status
```

## Features

- **📡 GitHub Watcher** — Monitors CI runs, detects failures and recoveries, tracks new PRs (uses the `gh` CLI)
- **🤖 Swarm Watcher** — Tracks AI coding agent status, detects stalled workers, reports lifecycle events
- **🔍 Sentry Watcher** — Surfaces unresolved production errors from Sentry
- **💬 Telegram Notifications** — Real-time alerts with severity levels, batching, and rate limiting
- **🧠 AI Coordinator** — Chat with your workspace from Telegram using a Claude-powered assistant that has full signal context
- **📊 TUI Dashboard** — Terminal UI for viewing signals across all workspaces
- **🗄️ Signal Store** — All events persisted to a local SQLite database for querying and history
- **⚡ Custom Commands** — Define shell scripts as Telegram slash commands for remote ops

## Commands

| Command | Description |
|---|---|
| `apiari init [--name NAME]` | Initialize a workspace config from the current directory |
| `apiari daemon [--background]` | Start the daemon (watches all workspaces) |
| `apiari status [WORKSPACE]` | Show open signals, optionally filtered by workspace |
| `apiari chat WORKSPACE [MESSAGE]` | Chat with a workspace's AI coordinator (omit message for interactive mode) |
| `apiari ui [--workspace NAME]` | Launch the TUI dashboard |

## Configuration

Workspace configs live at `~/.config/apiari/workspaces/{name}.toml`. Run `apiari init` inside a project directory to generate one.

### Example workspace config

```toml
root = "/Users/you/projects/my-app"
repos = ["YourOrg/my-app", "YourOrg/my-app-api"]

[telegram]
bot_token = "123456:ABC-DEF..."
chat_id = -1001234567890
topic_id = 42                       # optional — for forum-style groups
allowed_user_ids = [123456789]      # restrict who can interact

[coordinator]
name = "Bee"                        # bot display name (default: "Bee")
model = "sonnet"                    # Claude model (default: "sonnet")
max_turns = 20                      # max conversation turns (default: 20)
# prompt = "Custom system prompt"   # optional override for coordinator identity

[watchers.github]
repos = ["YourOrg/my-app"]         # repos to watch (defaults to top-level repos)
interval_secs = 120                 # poll interval (default: 120)

[watchers.swarm]
state_path = "/Users/you/.swarm/state.json"
interval_secs = 15                  # poll interval (default: 15)

[watchers.sentry]
org = "your-org"
project = "my-app"
token = "sntrys_..."
interval_secs = 120

[pipeline]
batch_window_secs = 60              # flush batched signals every N seconds

[[pipeline.rules]]
name = "drop-info-github"
source = "github"
severity = "info"
action = "drop"                     # "notify", "batch", or "drop"

[[pipeline.rules]]
name = "rate-limit-sentry"
source = "sentry"
action = "notify"
rate_limit_secs = 300

[[commands]]
name = "deploy"
script = "cd /app && ./deploy.sh"
description = "Deploy to production"
restart = false                     # restart daemon after script runs

[[commands]]
name = "update"
script = "cd /app && git pull && cargo install --path crates/apiari"
description = "Pull latest and reinstall"
restart = true
```

### Config paths

| Path | Purpose |
|---|---|
| `~/.config/apiari/workspaces/` | Workspace TOML configs |
| `~/.config/apiari/apiari.db` | SQLite signal database |
| `~/.config/apiari/daemon.pid` | Daemon PID file |
| `~/.config/apiari/daemon.log` | Daemon log output |
| `~/.config/apiari/daemon.sock` | Unix socket for TUI ↔ daemon IPC |

## Signals

Apiari's core abstraction is the **signal** — a structured event from any watcher, upserted into a local SQLite database keyed by `(workspace, source, external_id)`.

Each signal has:
- **Source** — `github`, `swarm`, or `sentry`
- **Severity** — `info`, `warning`, `error`, or `critical`
- **Status** — `open`, `updated`, `resolved`, or `stale`
- **Metadata** — URL, title, body, timestamps

Signals are routed through a **notification pipeline** where configurable rules can `notify` (send immediately), `batch` (group in a flush window), or `drop` (suppress). Rate limiting per rule prevents alert fatigue.

Use `apiari status` to view open signals or query the SQLite database directly at `~/.config/apiari/apiari.db`.

## Ecosystem

Apiari is part of a toolkit for managing AI coding workflows:

| Crate | Description |
|---|---|
| [**swarm**](https://github.com/ApiariTools/swarm) | TUI agent multiplexer — the workers that apiari watches |
| [**apiari-common**](https://crates.io/crates/apiari-common) | Shared IPC and state primitives |
| [**apiari-tui**](https://crates.io/crates/apiari-tui) | Shared TUI design system (built on ratatui) |
| [**apiari-claude-sdk**](https://crates.io/crates/apiari-claude-sdk) | Claude API SDK for the coordinator |

## License

MIT

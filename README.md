# apiari

**Bee is your AI ops coordinator — all the coordination, zero babysitting.**

Apiari runs a persistent daemon that dispatches and monitors AI coding agents (Claude Code, Codex, Gemini), watches your GitHub repos, and pings you on Telegram when something needs attention. CI failures, code reviews, PR status — signals route automatically through configurable hooks. You can respond right from your phone through Bee, an AI-powered coordinator that has full context on your workspace.

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

```sh
cargo install apiari
```

## Quick Start

```sh
# 1. Initialize a workspace from your project directory
cd ~/projects/my-app
apiari init

# 2. Edit the generated config to add your Telegram bot + repos
$EDITOR ~/.config/apiari/workspaces/my-app.toml

# 3. Start the daemon
apiari daemon start

# 4. Check signal status
apiari status
```

## Features

- **📡 GitHub Watcher** — Monitors CI runs, detects failures and recoveries, tracks new PRs (uses the `gh` CLI)
- **🤖 Swarm Watcher** — Tracks AI coding agent status, detects stalled workers, reports lifecycle events
- **🔍 Sentry Watcher** — Surfaces unresolved production errors from Sentry
- **💬 Telegram Notifications** — Real-time alerts with severity levels, batching, and rate limiting
- **🧠 Multi-Provider Coordinator** — Chat with your workspace from Telegram via Bee, powered by Claude, Codex, or Gemini — with full signal context
- **🪝 Signal Hooks** — Event-driven follow-throughs: when a signal arrives (CI fail, bot review, worker stall), the coordinator automatically runs a session with the hook's prompt and action
- **📖 Playbooks** — Reusable runbooks (`.apiari/skills/*.md`) that signal hooks can optionally reference via `skills` to give the coordinator step-by-step procedures
- **👤 Soul & Context** — Customize coordinator personality (`.apiari/soul.md`) and project knowledge (`.apiari/context.md`), loaded into every session
- **🔐 Authority Model** — `authority = "autonomous"` (full toolset) or `"observe"` (read-only), plus fine-grained `[capabilities]` like `dispatch_workers` and `merge_prs`
- **📊 TUI Dashboard** — Terminal UI with workers panel and coordinator chat, connected to the daemon via Unix socket
- **🗄️ Signal Store** — All events persisted to a local SQLite database for querying and history
- **⚡ Custom Commands** — Define `[[commands]]` in your workspace config to add Telegram slash commands (e.g. `name = "deploy"` becomes `/deploy`). Scripts run via `sh -c` on the daemon host

## Commands

| Command | Description |
|---|---|
| `apiari init [--name NAME]` | Initialize a workspace config from the current directory |
| `apiari daemon start` | Start the daemon (watches all workspaces) |
| `apiari status [WORKSPACE]` | Show open signals, optionally filtered by workspace |
| `apiari chat WORKSPACE [MESSAGE]` | Chat with a workspace's AI coordinator (omit message for interactive mode) |
| `apiari ui [--workspace NAME]` | Launch the TUI dashboard |
| `apiari config set KEY VALUE` | Set a config value using dot-separated key paths |
| `apiari config validate` | Validate workspace config files |

## Configuration

Workspace configs live at `~/.config/apiari/workspaces/{name}.toml`. Run `apiari init` inside a project directory to generate one.

### Example workspace config

```toml
root = "/Users/you/projects/my-app"
repos = ["YourOrg/my-app", "YourOrg/my-app-api"]
authority = "autonomous"               # "autonomous" (default) or "observe"

[capabilities]
dispatch_workers = true                # default: true in autonomous mode
merge_prs = false                      # true, false, or list of target branches

[telegram]
bot_token = "123456:ABC-DEF..."
chat_id = -1001234567890
topic_id = 42                          # optional — for forum-style groups
allowed_user_ids = [123456789]         # restrict who can interact

[coordinator]
name = "Bee"                           # bot display name (default: "Bee")
provider = "claude"                    # "claude", "codex", or "gemini"
model = "sonnet"                       # model name (default: "sonnet")
max_turns = 20                         # max conversation turns (default: 20)

[[coordinator.signal_hooks]]
source = "swarm"
prompt = "Swarm activity: {events}"
action = "Assess the situation. If a worker opened a PR, check reviews and forward comments."
ttl_secs = 300
skills = ["pr-review"]                 # loads .apiari/skills/pr-review.md

[[coordinator.signal_hooks]]
source = "github_bot_review"
prompt = "Bot review received: {events}"
action = "Forward the review comments to the relevant swarm worker."
ttl_secs = 300

[[coordinator.signal_hooks]]
source = "github"
prompt = "CI failed: {events}"
action = "Find the relevant worker and send it the CI error details."
ttl_secs = 300

[watchers.github]
repos = ["YourOrg/my-app"]            # repos to watch (defaults to top-level repos)
interval_secs = 120                    # poll interval (default: 120)

[watchers.swarm]
state_path = "/Users/you/.swarm/state.json"
interval_secs = 15                     # poll interval (default: 15)

[watchers.sentry]
org = "your-org"
project = "my-app"
token = "sntrys_..."
interval_secs = 120

[pipeline]
batch_window_secs = 60                 # flush batched signals every N seconds

[[pipeline.rules]]
name = "drop-info-github"
source = "github"
severity = "info"
action = "drop"                        # "notify", "batch", or "drop"

[[pipeline.rules]]
name = "rate-limit-sentry"
source = "sentry"
action = "notify"
rate_limit_secs = 300
```

### Workspace files

Place these in your repo's `.apiari/` directory:

| File | Purpose |
|---|---|
| `.apiari/soul.md` | Coordinator personality and communication style |
| `.apiari/context.md` | Project knowledge — stack, conventions, repo structure |
| `.apiari/skills/*.md` | Playbooks — reusable runbooks referenced by signal hooks |

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
- **Source** — `github`, `swarm`, `sentry`, etc.
- **Severity** — `info`, `warning`, `error`, or `critical`
- **Status** — `open`, `updated`, `resolved`, or `stale`
- **Metadata** — URL, title, body, timestamps

Signals are routed through two independent systems:

- **Notification pipeline** (`[pipeline]`) — configurable rules that `notify` (send immediately), `batch` (group in a flush window), or `drop` (suppress) Telegram notifications. Rate limiting per rule prevents alert fatigue.
- **Signal hooks** (`[[coordinator.signal_hooks]]`) — evaluated independently of the pipeline. Each hook matches a signal source. Hooks with an `action` always dispatch a coordinator session to handle them, even with no active conversation. Hooks without an `action` are skipped when no session is active. Hooks can reference playbooks from `.apiari/skills/` via the `skills` field for multi-step procedures.

Use `apiari status` to view open signals or query the SQLite database directly at `~/.config/apiari/apiari.db`.

## Ecosystem

Apiari is part of a toolkit for managing AI coding workflows:

| Crate | Description |
|---|---|
| [**swarm**](https://github.com/ApiariTools/swarm) | TUI agent multiplexer — the workers that apiari watches |
| [**apiari-common**](https://crates.io/crates/apiari-common) | Shared IPC and state primitives |
| [**apiari-tui**](https://crates.io/crates/apiari-tui) | Shared TUI design system (built on ratatui) |
| [**apiari-claude-sdk**](https://crates.io/crates/apiari-claude-sdk) | Claude API SDK for the coordinator |
| [**apiari-codex-sdk**](https://crates.io/crates/apiari-codex-sdk) | Codex API SDK for the coordinator |
| [**apiari-gemini-sdk**](https://crates.io/crates/apiari-gemini-sdk) | Gemini API SDK for the coordinator |

## License

MIT

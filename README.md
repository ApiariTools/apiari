# apiari

**Bee is your AI ops coordinator — all the coordination, zero babysitting.**

Apiari runs a persistent daemon that dispatches and monitors AI coding agents (Claude Code, Codex, Gemini), watches your GitHub repos, and pings you on Telegram when something needs attention. CI failures, code reviews, PR status — signals route through configurable event hooks. You can respond right from your phone through Bee, an AI-powered coordinator that has full context on your workspace.

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
apiari daemon --background

# 4. Check signal status
apiari status
```

## Features

- **📡 Multi-watcher Support** — GitHub (CI runs, PRs, failures), Swarm (agent status, stalls), Sentry (production errors) — extensible architecture for custom watchers
- **🔔 Signal Hooks** — Respond to signals by triggering playbooks from `.apiari/skills/` — automate remediation, dispatch workers, merge PRs
- **📚 Playbooks** — Script signal handlers in Bash, Rust, or use coordinator tools to respond intelligently
- **🤖 AI Coordinator** — Chat with your workspace from Telegram via Bee, powered by Claude — reads signal context, uses composable tools (Bash, GitHub, Swarm dispatch)
- **💬 Notifications** — Real-time Telegram alerts with severity levels, batching, rate limiting, and rich formatting
- **📊 TUI Dashboard** — Terminal UI for viewing signals across all workspaces, filtering by source and severity
- **🗄️ Signal Store** — All events persisted to a local SQLite database for querying and history
- **🔐 Workspace Context** — Per-workspace soul.md (identity), context.md (runbooks), and skills/ (playbooks) for isolated coordination
- **👤 Authority Model** — [capabilities] config to control what coordinator can do (merge_prs, dispatch_workers, etc.)

## Commands

| Command | Description |
|---|---|
| `apiari init [--name NAME]` | Initialize a workspace config from the current directory |
| `apiari daemon [--background]` | Start the daemon (watches all workspaces) |
| `apiari status [WORKSPACE]` | Show open signals, optionally filtered by workspace |
| `apiari chat WORKSPACE [MESSAGE]` | Chat with a workspace's AI coordinator (omit message for interactive mode) |
| `apiari ui [--workspace NAME]` | Launch the TUI dashboard |
| `apiari config set <KEY> <VALUE>` | Set a workspace config value |
| `apiari config validate` | Validate all workspace config files |

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

[coordinator.signal_hooks]
gh_ci_failure = "respond-to-ci-failure"
swarm_agent_waiting = "dispatch-next-task"

[capabilities]
merge_prs = true
dispatch_workers = true


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

```


### Workspace directories
Apiari creates a `.apiari/` directory in your workspace root to store identity, context, and playbooks:

```
.apiari/
├── soul.md
├── context.md
└── skills/
    ├── respond-to-ci-failure.sh
    ├── dispatch-next-task.rs
    └── merge-and-deploy.sh
```

**soul.md** — Coordinator identity (example):
```markdown
# Souls Soul
You are Bee, the teams AI operations coordinator.
```

**context.md** — Domain knowledge (example):
```markdown
# Workspace Context
```

**skills/** — Playbooks (example):
```bash
#!/bin/bash
BRANCH="$1"
```

### Config paths

| Path | Purpose |
|---|---|
| `~/.config/apiari/workspaces/` | Workspace TOML configs |
| `~/.config/apiari/apiari.db` | SQLite signal database |
| `~/.config/apiari/daemon.pid` | Daemon PID file |
| `~/.config/apiari/daemon.log` | Daemon log output |
| `~/.config/apiari/daemon.sock` | Unix socket for TUI ↔ daemon IPC |
| `.apiari/soul.md` | Workspace coordinator identity |
| `.apiari/context.md` | Workspace runbooks and knowledge |
| `.apiari/skills/` | Playbooks for signal hooks |

## Signals

Apiari's core abstraction is the **signal** — a structured event from any watcher, upserted into a local SQLite database keyed by `(workspace, source, external_id)`.

Each signal has:
- **Source** — `github`, `swarm`, or `sentry`
- **Severity** — `info`, `warning`, `error`, or `critical`
- **Status** — `open`, `updated`, `resolved`, or `stale`
- **Metadata** — URL, title, body, timestamps

Signals are routed through **signal hooks** defined in your workspace config `[coordinator.signal_hooks]`. When a signal fires, the coordinator can trigger a playbook from `.apiari/skills/` to automate response — dispatch workers, merge PRs, page on-call, or run Bash scripts with full context.

Use `apiari status` to view open signals or query the SQLite database directly at `~/.config/apiari/apiari.db`.


## Authority Model

The `[capabilities]` section of your workspace config controls what your coordinator is authorized to do:

```toml
[capabilities]
merge_prs = true
dispatch_workers = true
run_scripts = true
```

Each capability defaults to `false` for safety. Enable only what your workspace needs. The coordinator will refuse to perform unauthorized actions even if asked directly in chat.

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

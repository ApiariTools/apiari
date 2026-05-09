# apiari (binary crate)

The main `apiari` CLI. Entrypoint for the HTTP daemon, TUI dashboard, chat, signals, config
management, and swarm integration. This is the largest crate in the workspace.

## CLI Commands

| Command | Description |
|---------|-------------|
| `apiari` / `apiari ui` | Launch the ratatui TUI dashboard |
| `apiari daemon start` | Start the HTTP+WebSocket daemon (background) |
| `apiari daemon stop` | Stop a running daemon |
| `apiari daemon status` | Check daemon health |
| `apiari chat <workspace>` | Chat with a workspace bot (TUI or one-shot) |
| `apiari status [workspace]` | Show open signals |
| `apiari config set <key> <value>` | Update a config field |
| `apiari config validate` | Validate workspace TOML files |
| `apiari config migrate` | Migrate configs to current schema |
| `apiari init` | Scaffold a workspace config from the current directory |

## Module Structure

```
src/
  main.rs               — CLI parser (clap), logging setup, command dispatch
  config.rs             — Config types, loading, and config-dir resolution
  config_migrate.rs     — Schema migration helpers
  config_set.rs         — Dot-path config mutation (apiari config set)
  config_validate.rs    — Config validation (apiari config validate)
  git_safety.rs         — GH_TOKEN stripping, git env hygiene
  validate_bash.rs      — PreToolUse hook: validates Bash commands for coordinators
  daemon/
    mod.rs              — Daemon startup, lifecycle, shutdown
    http.rs             — All axum HTTP+WebSocket routes (~366 KB, main request handling)
    socket.rs           — Unix socket for daemon IPC (start/stop/status)
    worker_manager.rs   — Swarm worker lifecycle management
    morning_brief.rs    — Scheduled morning brief generation
    doctor.rs           — Workspace health checks
  buzz/
    mod.rs              — Auto-bot and auto-bot-runner dispatch
    auto_bot.rs         — AutoBot DB operations and runner
    auto_bot_runner.rs  — Runs auto-bots against providers
    config.rs           — Bot configuration parsing
    conversation.rs     — Conversation helpers for bot context
    review.rs           — Worker review generation
    schedule.rs         — Scheduled task runner
    swarm_reconciler.rs — Reconciles swarm state with DB workers
    worker.rs           — Worker DB operations
    worker_hooks.rs     — Worker lifecycle hooks
  shells/
    mod.rs              — Shell integration helpers
  ui/
    — ratatui TUI components (uses apiari-tui primitives)
```

## Key Files

- **`daemon/http.rs`** — The largest file (~366 KB). Contains all axum route handlers, system
  prompt construction, bot pipeline dispatch, WebSocket hub, streaming responses.
- **`buzz/swarm_reconciler.rs`** — Reconciles `.swarm/state.json` with the DB worker table
  on each poll cycle.
- **`config.rs`** — Defines `WorkspaceConfig`, `BotConfig`, all field types, and the
  `~/.config/hive/workspaces/*.toml` loading logic.

## Daemon

The daemon runs a single `tokio` runtime serving:
- HTTP API on a configured port (default: 4200)
- WebSocket at `/ws` (broadcast hub, 256-slot channel)
- Background tasks: config watcher (30s), signal watchers (60s), scheduled bots

Started via `apiari daemon start` (forks to background via a Unix socket handshake).

## Rules

- NEVER delete `~/.config/hive/hive.db` — it is the production database.
- `GH_TOKEN` is stripped from the environment on daemon startup (`git_safety.rs`) —
  Claude Code's sandbox token breaks git operations inside the daemon.
- `http.rs` is very large. When editing it, read only the relevant handler section.
- Run full checks before committing: `cargo fmt && cargo clippy -- -D warnings && cargo test`

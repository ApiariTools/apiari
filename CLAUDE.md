# Hive

Workspace chat hub — Rust daemon + React SPA.

## Rules
1. Only modify files within this repository.
2. Do NOT run `cargo install` or modify system state.
3. Run ALL checks before committing: `cargo fmt && cargo clippy -- -D warnings -A clippy::too_many_arguments && cargo test`
4. For frontend changes, also run: `cd web && npm run check && npx vitest run`
   (`npm run check` = typecheck + eslint + prettier format check)
5. A `pre-push` git hook runs all checks automatically. If push is rejected, fix the issues before retrying.

## Architecture

```
crates/
  apps/
    apiari/src/
      main.rs           — CLI + daemon startup, bot config loading, GH_TOKEN stripping
      routes.rs         — All HTTP/WS endpoints (axum), system prompt builder, provider dispatch
      db.rs             — SQLite (5 tables, WAL mode, reader/writer separation)
      bot.rs            — BotRunner trait, BotEvent enum, run_bot_pipeline, MockBotRunner
      events.rs         — WebSocket broadcast hub (tokio broadcast, 256-slot buffer)
      watcher.rs        — Signal watchers (GitHub polling) + proactive/scheduled bot runner
      publish.rs        — CLI subcommand for bots to post clean reports to DB
      config_watcher.rs — Polls every 30s for config/prompt changes, logs session-reset message
      lib.rs            — Re-exports (bot, db, events, publish, routes) for test visibility
    hive/               — Coordination brain: quests, coordinator, context
    swarm/              — Parallel agent orchestration: git worktrees + daemon IPC
  core/
    common/             — Shared types, JSONL IPC, utilities
    tui/                — Shared TUI design system: theme, scroll, widgets
  providers/
    claude-sdk/         — Rust SDK wrapping the Claude CLI (spawn, stream, session resume)
    codex-sdk/          — Rust SDK wrapping the Codex CLI
    gemini-sdk/         — Rust SDK wrapping the Gemini CLI

web/
  src/App.tsx           — Main app, routing, state management
  src/api.ts            — All API calls + WebSocket client
  src/types.ts          — TypeScript types (Workspace, Bot, Worker, Message, Repo)
  src/components/
    ChatPanel.tsx       — Chat messages, input, streaming, attachments
    BotNav.tsx          — Left sidebar: bot list + unread badges
    ReposPanel.tsx      — Right sidebar: repos + workers
    TopBar.tsx          — Workspace tabs + hamburger
    WorkerDetail.tsx    — Worker info + conversation
    WorkersPanel.tsx    — Workers list panel
    CommandPalette.tsx  — Cmd+K palette
```

## Bot System

Three-tier bot architecture:

1. **Passive bots** — Chat-only. Respond to user messages. Default behavior for all bots.
2. **Watch bots** — Poll signals (e.g. GitHub PRs with failing CI) every 60s. Respond autonomously when a signal fires. Configured via `watch = ["github"]`.
3. **Scheduled/proactive bots** — Run on interval. Generate reports written to file, published via `hive publish`. Configured via `schedule_hours` + `proactive_prompt`.

### Bot Lifecycle (chat)

1. User sends message → `POST /api/workspaces/{ws}/chat/{bot}`
2. User message stored in DB with optional attachments
3. System prompt built from: workspace config + bot role + context.md + soul.md + swarm instructions
4. Prompt hashed → compared against stored session hash
5. If hash matches → resume existing provider session. If changed → start fresh session.
6. Provider dispatched in background task (5-minute timeout):
   - `claude` → `apiari-claude-sdk` (streaming via `ClaudeClient`)
   - `codex` → `apiari-codex-sdk` (via `CodexClient`)
   - `gemini` → `apiari-gemini-sdk` (via `GeminiClient`)
7. Streaming text deltas written to `bot_status.streaming_content`
8. On completion: trimmed response stored in `conversations`, session ID saved, status set to idle
9. WebSocket events broadcast for `bot_status` and `message` updates
10. Frontend polls `bot_status` every 2s + listens on WebSocket for real-time updates

### Bot Lifecycle (proactive/scheduled)

1. Interval fires → system message logged → prompt built with bot role + proactive_prompt
2. Bot runs autonomously (max 10 turns for claude) with instructions to write report to file
3. Bot calls `hive publish --workspace {ws} --bot {name} --file /tmp/hive-report.md`
4. Report stored as assistant message in DB

## Config Schema

Workspace configs live at `~/.config/hive/workspaces/{name}.toml`:

```toml
[workspace]
root = "/path/to/repo"
name = "my-workspace"
description = "Optional description"

[[bots]]
name = "Main"
color = "#f5c542"           # hex color for UI
role = "General assistant"  # role description
provider = "claude"         # claude | codex | gemini
model = "claude-sonnet-4-20250514"    # optional model override
prompt_file = "custom.md"  # custom system prompt file (replaces default prompt)
watch = ["github"]          # signal sources to monitor
schedule_hours = 24         # proactive run interval in hours
proactive_prompt = "..."    # task description for scheduled runs
```

A default "Main" bot (gold, claude provider) is always injected even if not in the config.

### Workspace Context Files

- `.apiari/context.md` — Project context appended to all bot system prompts
- `.apiari/soul.md` — Communication style appended to all bot system prompts
- `.apiari/docs/` — Folder of reference docs. Contents are NOT injected — only filenames and first-line descriptions are indexed in the prompt. Bots read full contents on demand with `cat`.
- `.swarm/` directory — If present, swarm worker dispatch instructions are injected into bot prompt (bots become coordinators, not coders)

### Custom Prompt Files

When `prompt_file` is set, the file contents replace the entire default identity prompt. Workspace name/description and working directory are still appended.

## Database Schema

SQLite with WAL mode. Path: `~/.config/hive/hive.db`

Separate reader/writer connections — reads never block the writer.

- **`conversations`** — `id` (PK auto), `workspace`, `bot`, `role`, `content`, `attachments` (JSON nullable), `created_at`
- **`sessions`** — PK(`workspace`, `bot`), `session_id`, `prompt_hash`, `updated_at`
- **`bot_status`** — PK(`workspace`, `bot`), `status`, `streaming_content`, `tool_name` (nullable), `updated_at`
- **`signals`** — `id` (PK auto), `workspace`, `source`, `external_id`, `title`, `body`, `severity`, `status`, `url`, `metadata`, `created_at`, `updated_at`. UNIQUE(`workspace`, `source`, `external_id`)
- **`last_seen`** — PK(`workspace`, `bot`), `message_id` — tracks last-read message for unread counts

## API Endpoints

All routes defined in `routes.rs`:

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/workspaces` | List all workspaces (from config dir) |
| GET | `/api/workspaces/{ws}/bots` | List bots for workspace (always includes Main) |
| GET | `/api/workspaces/{ws}/conversations?limit=30` | All conversations across bots |
| GET | `/api/workspaces/{ws}/conversations/{bot}?limit=30` | Conversations for specific bot |
| GET | `/api/workspaces/{ws}/conversations/{bot}/search?q=&limit=20` | Search messages by content |
| POST | `/api/workspaces/{ws}/chat/{bot}` | Send message (JSON: `{message, attachments?}`) |
| GET | `/api/workspaces/{ws}/bots/{bot}/status` | Bot status (idle/thinking/streaming + content) |
| POST | `/api/workspaces/{ws}/bots/{bot}/cancel` | Cancel running bot response |
| GET | `/api/workspaces/{ws}/unread` | Unread counts per bot `{bot: count}` |
| POST | `/api/workspaces/{ws}/seen/{bot}` | Mark bot conversation as read |
| POST | `/api/transcribe` | Transcribe audio (multipart, requires ffmpeg + whisper-cli) |
| GET | `/api/workspaces/{ws}/repos` | List git repos in workspace root |
| GET | `/api/workspaces/{ws}/v2/workers` | List workers (v2, SQLite-backed — use this) |
| GET | `/api/workspaces/{ws}/v2/workers/{id}` | Worker detail |
| POST | `/api/workspaces/{ws}/v2/workers` | Create worker |
| POST | `/api/workspaces/{ws}/v2/workers/{id}/send` | Send message to worker |
| GET | `/api/workspaces/{ws}/workers` | **Deprecated** — legacy v1 worker list |
| GET | `/api/workspaces/{ws}/workers/{id}` | **Deprecated** — legacy v1 worker detail |
| GET | `/ws` | WebSocket for real-time events |

Body limit: 50MB (for image attachments).

## Provider Abstraction

`bot.rs` defines the `BotRunner` trait and `run_bot_pipeline()` shared pipeline:

- **claude** — `apiari-claude-sdk::ClaudeClient`. Streaming via `Event::Stream` with `TextDelta`. Supports images, session resume, max_turns.
- **codex** — `apiari-codex-sdk::CodexClient`. `exec`/`exec_resume` with full_auto mode. Supports images via temp files.
- **gemini** — `apiari-gemini-sdk::GeminiClient`. `exec`/`exec_resume`. No image support currently.
- **MockBotRunner** — Test-only (`#[cfg(test)]`). Variants: `simple`, `with_tool`, `streaming`, `error`. Emits timed `BotEvent`s.

## Session Management

- System prompt is hashed with `DefaultHasher` (non-cryptographic, 16-char hex)
- If hash matches stored session → resume conversation (no system prompt re-sent)
- If hash changed (config edit, context.md change) → start fresh session, system message logged
- `config_watcher.rs` polls every 30s: hashes TOML config + context.md + soul.md content
- On change detection: logs system message "Session reset — bot configuration was updated." (does not clear sessions table; the next chat message detects the hash mismatch and starts a fresh session)

## Swarm Integration

Hive reads swarm state from the workspace root:

- **`.swarm/state.json`** — Worker list. Each entry has `id`, `branch`, `phase`, `agent_kind`, `pr`, `repo_path`, `worktree_path`, `prompt`
- **`.swarm/agents/{worker_id}/events.jsonl`** — Agent conversation (event types: `assistant_text`, `tool_use`, `user_message`)
- **`.swarm/output.md`** — Worker output/results (in each worktree)

Workers are mapped to repos via `repo_path`. Messages are proxied to workers via:
```
swarm --dir {root} send {worker_id} "message"
```

When `.swarm/` exists in workspace root, bot prompts include swarm dispatch instructions — bots become coordinators that spawn workers instead of writing code directly.

## WebSocket Events

Events broadcast via `EventHub` (tokio broadcast channel, 256 capacity):

```json
{ "type": "message", "workspace": "...", "bot": "...", "role": "...", "content": "..." }
{ "type": "bot_status", "workspace": "...", "bot": "...", "status": "thinking|streaming|idle", "tool_name": "..." }
{ "type": "worker_update", "workspace": "...", "worker_id": "...", "status": "..." }
```

Frontend connects via `ws://host/ws`. Auto-reconnects after 3s on disconnect.

## Keyboard Shortcuts

- **Cmd+K** — Command palette (search workspaces, bots, workers)
- **Cmd+J** — Focus chat input

## Design System

Dark theme. CSS variables in `web/src/theme.css`:
- `--bg: #111` `--bg-card: #191919` `--border: #282828`
- `--text: #aaa` `--text-strong: #eee` `--text-faint: #555`
- `--accent: #f5c542` (gold) `--red: #e85555` `--green: #5cb85c`
- Font: system-ui, 15px base, 16px for inputs (prevents iOS zoom)
- Icons: `lucide-react` — DO NOT use emoji icons

## CSS Rules — READ CAREFULLY
- NEVER put `overflow: hidden` on `.msg` or `.messages` — it hides content
- `overflow-x: auto` goes on individual elements (`pre`, `table`) not containers
- Use CSS modules (`.module.css`), not global CSS
- Mobile breakpoint: `768px`
- Test on mobile — iOS Safari has quirks

## Testing
- Rust: `cargo test` — DB, API endpoints, streaming pipeline, bot runner
- Frontend: `cd web && npm run check && npx vitest run` — typecheck + lint + format + 241 tests
- CI runs on every push/PR: fmt, clippy, tests, tsc, vitest, vite build
- MockBotRunner in `crates/apps/apiari/src/bot.rs` for testing bot pipeline without live CLIs
- **Add tests for any new feature or bug fix**

## Key Patterns
- Frontend is dumb — all state lives in daemon/DB
- Bot sessions run in background tasks (fire-and-forget)
- Frontend polls bot_status every 2s, gets conversations on load + WebSocket events
- `useKeyboardHeight` was removed — don't re-add iOS keyboard hacks
- Uncontrolled textarea for chat input (no React state for input value)
- `onMouseDown preventDefault` on send button keeps iOS keyboard open
- `enterKeyHint="enter"` on textarea so mobile keyboard shows return/newline key; mobile users send via the send button
- On mobile (touch devices), Enter inserts a newline; on desktop, Enter sends the message

## Config
- Workspace configs: `~/.config/hive/workspaces/{name}.toml`
- DB: `~/.config/hive/hive.db` — NEVER delete this
- Bot personality: `.apiari/soul.md` in workspace root
- Project context: `.apiari/context.md` in workspace root
- Custom bot prompt: `prompt_file` field in workspace TOML

## Frontend Build
- `web/dist/` is NOT committed to the repo — it is gitignored
- `rust-embed` embeds `web/dist/` into the binary at compile time
- `build.rs` creates a placeholder `web/dist/index.html` if the directory is missing, so `cargo build` always works
- To build locally with the full frontend: `cd web && npm install && npx vite build && cd .. && cargo build --release`
- GitHub Actions release workflow builds the frontend and Rust binary together on version tags, and publishes to crates.io

## Common Pitfalls
- `overflow: hidden` on message containers HIDES ALL TEXT — we've hit this 3 times
- `.swarm/state.json` uses a `worktrees` array (not `workers`); each entry has `agent` (the CLI name) and an optional `agent_kind` field
- Bot responses start with `\n\n` — always trim before storing
- `GH_TOKEN` from Claude Code sandbox breaks git — daemon strips it on startup
- Canvas elements need explicit `width` — `left`/`right` doesn't stretch them
- Don't add `position: fixed` to `#root` — breaks iOS textarea focus
- The `node_modules/.vite` cache can cause phantom build errors — delete it

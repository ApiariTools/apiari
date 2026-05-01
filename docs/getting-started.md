# Getting Started with Hive

Hive is a workspace chat hub — a Rust daemon that serves a React UI where you interact with AI bots (Claude, Codex, Gemini). Bots can chat, monitor GitHub, run on schedules, and coordinate autonomous coding agents.

This guide walks you through installing Hive, creating your first workspace, and configuring bots.

## Prerequisites

- **Rust** (1.85+) — required to install from crates.io or build from source
- **At least one AI provider CLI** installed and authenticated:
  - [`claude`](https://docs.anthropic.com/en/docs/claude-code) — Anthropic's Claude Code CLI
  - [`codex`](https://github.com/openai/codex) — OpenAI Codex CLI
  - [`gemini`](https://github.com/google-gemini/gemini-cli) — Google Gemini CLI
- **Node 20+** — only needed if building the frontend from source
- **Optional:** `gh` CLI (for GitHub watch bots), `swarm` CLI (for worker dispatch)

## Installation

### From crates.io (recommended)

```bash
cargo install apiari-hive
```

This installs the `hive` binary. The frontend is pre-bundled — no Node required.

### From GitHub Releases

Download the latest binary for your platform from [Releases](https://github.com/ApiariTools/hive/releases):

```bash
# macOS (Apple Silicon)
chmod +x hive-aarch64-apple-darwin
mv hive-aarch64-apple-darwin /usr/local/bin/hive

# macOS (Intel)
chmod +x hive-x86_64-apple-darwin
mv hive-x86_64-apple-darwin /usr/local/bin/hive
```

### From source

```bash
git clone https://github.com/ApiariTools/hive.git
cd hive
cd web && npm install && npx vite build && cd ..
cargo install --path .
```

Building from source requires Node 20+ (for the frontend build step).

## Creating your first workspace

A workspace connects Hive to a project directory. Run `hive init` from your project root:

```bash
cd /path/to/my-project
hive init my-project
```

You can also specify the root explicitly:

```bash
hive init my-project --root /path/to/my-project
```

This creates:

| File | Purpose |
|------|---------|
| `~/.config/hive/workspaces/my-project.toml` | Workspace config (bots, providers, settings) |
| `.apiari/context.md` | Project context — included in all bot prompts |
| `.apiari/soul.md` | Communication style for bots |
| `.apiari/docs/` | Reference docs bots can access on demand |

## Starting the daemon

```bash
hive
```

By default Hive listens on port 4200. Open `http://localhost:4200` in your browser.

To use a different port:

```bash
hive --port 8080
```

Every workspace gets a default **Main** bot (gold, Claude provider) that's ready to chat immediately — no additional configuration needed.

## Configuring bots

Workspace configs live at `~/.config/hive/workspaces/{name}.toml`. Here's a complete example:

```toml
[workspace]
root = "/Users/you/projects/my-app"
name = "my-app"
description = "My web application"

# A basic chat bot
[[bots]]
name = "Assistant"
color = "#5cb85c"
role = "General coding assistant with expertise in TypeScript and React"
provider = "claude"
model = "claude-sonnet-4-20250514"

# A bot that watches GitHub for CI failures
[[bots]]
name = "CI Watch"
color = "#e85555"
role = "Monitor CI failures, investigate root causes, and suggest fixes"
provider = "claude"
watch = ["github"]

# A bot that runs weekly reports
[[bots]]
name = "Weekly Review"
color = "#7b68ee"
role = "Code quality reviewer"
provider = "claude"
schedule_hours = 168
proactive_prompt = "Review recent PRs and summarize code quality trends, recurring issues, and areas for improvement"

# A bot with a fully custom system prompt
[[bots]]
name = "Specialist"
color = "#ff8c00"
role = "Security auditor"
provider = "gemini"
prompt_file = "security-prompt.md"
```

### Bot fields reference

| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Display name in the UI |
| `color` | no | Hex color for the bot's avatar (defaults to a fallback in the UI) |
| `role` | no | Role description — injected into the bot's system prompt |
| `provider` | yes | Which AI to use: `claude`, `codex`, or `gemini` |
| `model` | no | Override the provider's default model |
| `prompt_file` | no | Path to a markdown file that replaces the default system prompt |
| `watch` | no | Signal sources to poll (currently: `["github"]`) |
| `schedule_hours` | no | How often (in hours) to run the bot proactively |
| `proactive_prompt` | no | Task description for scheduled runs (required with `schedule_hours`) |

### Bot types

**Passive bots** are the default. They respond when you send them a message. Every bot is passive unless you add `watch` or `schedule_hours`.

**Watch bots** poll external sources on a 60-second interval. When a signal fires (e.g., a GitHub PR with failing CI), the bot responds autonomously. Requires the `gh` CLI to be installed and authenticated.

```toml
[[bots]]
name = "CI Watch"
color = "#e85555"
role = "Monitor CI and investigate failures"
provider = "claude"
watch = ["github"]
```

**Scheduled bots** run on a fixed interval. They execute a task described in `proactive_prompt` and publish a report to the conversation. Both `schedule_hours` and `proactive_prompt` are required together.

```toml
[[bots]]
name = "Daily Standup"
color = "#7b68ee"
role = "Project status reporter"
provider = "claude"
schedule_hours = 24
proactive_prompt = "Summarize yesterday's git activity, open PRs, and any CI issues"
```

> **Note:** Adding or removing `watch` or `schedule_hours` from a bot config requires restarting the daemon for the change to take effect. Chat-related config changes (role, model, prompt_file) are picked up automatically.

### Providers

Each provider requires its respective CLI to be installed and authenticated:

| Provider | CLI | Notes |
|----------|-----|-------|
| `claude` | `claude` | Streaming responses, image support, session resume |
| `codex` | `codex` | Full-auto mode, image support via temp files |
| `gemini` | `gemini` | Session resume, no image support currently |

You can mix providers across bots in the same workspace.

### Custom prompts

By default, bots get a system prompt built from: workspace config + bot role + context.md + soul.md. If you need full control, set `prompt_file` to a markdown file path (relative to the workspace root):

```toml
[[bots]]
name = "Specialist"
color = "#ff8c00"
role = "Database migration expert"
provider = "claude"
prompt_file = "prompts/db-expert.md"
```

The file contents replace the entire default identity prompt. The workspace name, description, and working directory are still appended.

## Context and personality files

These files live in your project's `.apiari/` directory and are included in all bot prompts for that workspace.

### `.apiari/context.md` — Project context

Tell bots what your project is, how it's structured, and what conventions you follow. This is the single most impactful file for bot quality.

Example:

```markdown
# Project Context

## What is this?
A real-time collaboration platform built with Next.js and PostgreSQL.

## Tech Stack
- Next.js 14 (App Router)
- PostgreSQL 16 with Drizzle ORM
- Tailwind CSS
- Deployed on Vercel + Railway

## Architecture
- `src/app/` — Next.js routes and server components
- `src/lib/` — Shared utilities and database clients
- `src/components/` — React components (all client components in `components/client/`)

## Conventions
- All database queries go through Drizzle — no raw SQL
- Use server actions for mutations, not API routes
- Tests use Vitest with a test database
```

### `.apiari/soul.md` — Communication style

Control how bots communicate. This affects tone, verbosity, and formatting.

Example:

```markdown
# Communication Style

- Be concise and direct — lead with the answer, not the reasoning
- Use code blocks with language tags
- When suggesting changes, show the diff or the specific file and line
- Skip disclaimers and caveats unless the suggestion has real tradeoffs
- Assume senior engineer knowledge — don't explain basic concepts
```

### `.apiari/docs/` — Reference documents

Place markdown files in this directory for bots to reference on demand. Bots see a list of filenames and first-line descriptions in their prompt, but read full contents only when needed. This keeps prompts small while giving bots access to detailed docs.

Example structure:

```
.apiari/docs/
  api-schema.md       # OpenAPI schema for the public API
  deployment.md       # How to deploy to staging and production
  database-schema.md  # Current table definitions and migrations
```

You can manage docs with the `hive docs` subcommand:

```bash
hive docs list --workspace my-project
hive docs read --workspace my-project api-schema.md
hive docs write --workspace my-project overview.md --file /path/to/doc.md
hive docs delete --workspace my-project overview.md
```

The `write` and `delete` commands automatically git-commit the change in the workspace root.

## Swarm integration

When a `.swarm/` directory exists in your workspace root, bots automatically gain the ability to dispatch and coordinate **swarm workers** — autonomous coding agents that run in separate git worktrees.

In this mode, bots act as coordinators: instead of writing code directly, they spawn workers to handle implementation tasks, monitor their progress, and send follow-up instructions.

Workers appear in the UI's right sidebar where you can view their status, conversation history, and send them messages directly.

This feature requires the `swarm` CLI to be installed. See the [swarm documentation](https://github.com/ApiariTools/swarm) for setup instructions.

## Voice setup (optional)

Hive supports voice input (speech-to-text) and spoken bot responses (text-to-speech). To install the required dependencies:

```bash
hive setup
```

This runs three setup steps, each of which is skipped if already installed or if prerequisites are missing:

- **whisper-cpp** (STT) — installed via Homebrew. Skipped if `brew` is not available. Also downloads the `base.en` whisper model.
- **Kokoro TTS** — creates a Python venv in the `tts/` directory of the Hive installation, installs pip requirements, and downloads the Kokoro model. Skipped if the `tts/` directory is not found (only present when building from source or in release bundles that include it).

Voice servers start automatically when you launch the daemon if their dependencies are set up.

You can configure TTS per-workspace in the config TOML:

```toml
[workspace]
# ...
tts_voice = "af_nova"
tts_speed = 1.2
```

## Tips

### Session management

Hive tracks a hash of each bot's system prompt (workspace config + context.md + soul.md). When you edit any of these files, the bot's session resets automatically on the next message — no restart needed. Hive polls for config changes every 30 seconds and logs a "Session reset" message when it detects a change.

### Command palette

Press **Cmd+K** to open the command palette. Search across workspaces, bots, and workers. Press **Cmd+J** to focus the chat input.

### Multiple workspaces

You can create as many workspaces as you want — one per project. Each workspace has its own bots, conversations, and context. Switch between them using the tabs at the top of the UI or through the command palette.

### Image attachments

You can attach images to messages (up to 50MB). Claude and Codex providers support image analysis; Gemini does not currently.

### Custom config directory

By default, Hive stores its config and database in `~/.config/hive/`. To use a different location:

```bash
hive --config-dir /path/to/config
```

### Logs

Set the `RUST_LOG` environment variable for more detailed logging:

```bash
RUST_LOG=hive=debug hive
```

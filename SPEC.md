# Apiari — Product Spec v2

This document is the authoritative spec for the v2 rebuild. Workers should read this before touching any code.

---

## Product Vision

Three distinct entity types, each with a clear job:

- **Auto Bots** — always-on, autonomous. Triggered by signals (Sentry, GitHub, CI) or cron. They watch the project so you don't have to. They surface findings, triage alerts, and can dispatch workers.
- **Context Bots** — session-scoped assistants. They know what you're currently looking at (which worker, which signal, which page). Multiple can be open at once. They help you think, navigate, and dispatch workers with pre-packaged briefs.
- **Workers** — tasks in flight. Created by a human, a context bot, or an auto bot. Each gets a pre-digested brief, runs in an isolated git worktree via swarm, and produces a PR.

---

## UI Layout

### Desktop (>1024px)

```
┌──────────────────┬─────────────────────────────────────────────────────┐
│ Sidebar (240px)  │ Main Content                                         │
│                  │                                                       │
│  AUTO BOTS       │  [ Worker Detail | Auto Bot Feed | Dashboard ]        │
│  ● Triage        │                                                       │
│  ● Standup       │                                                       │
│  ─────────────   │                                                       │
│  WORKERS         │                                                       │
│  ◆ fix-auth      │                          ┌────────────────────────┐  │
│  ◆ rate-limit    │                          │ Context Bot            │  │
│  ◆ update-deps   │                          │ "Viewing: fix-auth"    │  │
│                  │                          └────────────────────────┘  │
└──────────────────┴─────────────────────────────────────────────────────┘
```

- Sidebar: fixed 240px, two sections (Auto Bots, Workers), each item is a row with status indicator + name
- Main content: fills remaining width, renders selected entity detail
- Context bots: float bottom-right, stack vertically, each minimizable

### iPad (768px–1024px)

- Sidebar collapses to a 60px icon rail by default
- Tap the rail or a hamburger to slide the full sidebar in as an overlay
- Context bots appear as a bottom sheet (slides up from bottom edge)

### Mobile (<768px)

- Sidebar replaced by a bottom tab bar: Auto Bots | Workers | (active context) | New
- Main content fills the full screen
- Context bots open as full-screen modal sheets
- All inputs: 16px min font size (prevents iOS zoom)
- Send buttons, not Enter key, for chat submission on mobile

---

## Design Language

Inspired by Linear, Raycast, and Sentry. Key principles:

- **Dark first.** Background `#111`, cards `#191919`, borders `#252525`
- **Color is status, not decoration.** Green = healthy, amber = attention, red = error, blue = info. No decorative color.
- **Typography hierarchy.** One strong weight for titles, normal for body, faint for metadata.
- **Subtle, not heavy.** 1px borders, not thick dividers. Shadows only for floating elements.
- **Keyboard-first.** Cmd+K palette, Cmd+J focus input, arrow key navigation in lists.
- **Icons only from lucide-react.** No emoji. No icon fonts.

### Tokens (extend existing theme.css, do not break existing vars)

```css
--sidebar-width: 240px;
--sidebar-rail-width: 60px;
--bg: #111;
--bg-card: #191919;
--bg-hover: #1e1e1e;
--border: #252525;
--border-strong: #333;
--text: #aaa;
--text-strong: #eee;
--text-faint: #555;
--accent: #f5c542;
--status-running: #5cb85c;
--status-waiting: #f5c542;
--status-stalled: #e8a020;
--status-failed: #e85555;
--status-merged: #7c5cbf;
--radius-sm: 4px;
--radius-md: 8px;
```

---

## Worker State Machine (LOCKED)

### States

| State | Description |
|-------|-------------|
| `created` | Record exists, no brief yet |
| `briefed` | Brief attached, not yet sent to swarm |
| `queued` | Swarm worktree creation in progress |
| `running` | Agent actively working |
| `waiting` | Agent explicitly paused, awaiting human input |
| `merged` | PR merged — terminal |
| `failed` | Agent errored or exited non-zero — terminal (re-queueable) |
| `abandoned` | Manually cancelled — terminal (re-queueable) |

### Properties (on the `workers` table)

| Column | Type | Description |
|--------|------|-------------|
| `tests_passing` | `BOOLEAN DEFAULT FALSE` | CI / local test status |
| `branch_ready` | `BOOLEAN DEFAULT FALSE` | Agent signaled it finished a pass |
| `pr_url` | `TEXT NULL` | GitHub PR URL once opened |
| `pr_approved` | `BOOLEAN DEFAULT FALSE` | PR has at least one human approval |
| `is_stalled` | `BOOLEAN DEFAULT FALSE` | No output for >10min while `running` |
| `revision_count` | `INTEGER DEFAULT 0` | Increments each time worker re-enters `running` from review feedback |
| `review_mode` | `TEXT DEFAULT 'local_first'` | `local_first` \| `pr_first` |
| `blocked_reason` | `TEXT NULL` | Why agent is waiting (set by agent or watcher) |
| `last_output_at` | `DATETIME NULL` | Timestamp of last agent output event |
| `state_entered_at` | `DATETIME NOT NULL` | When current state was entered (for stall detection + UI) |

### Derived UI Labels (computed, never stored)

| State | Properties | Display Label | Color |
|-------|-----------|--------------|-------|
| `running` | `revision_count == 0` | Working | green |
| `running` | `revision_count > 0` | Revising (pass N) | green |
| `running` | `is_stalled` | Stalled | amber |
| `waiting` | `branch_ready && !pr_url` | Ready for local review | amber |
| `waiting` | `pr_url && review_feedback pending` | Has feedback | amber |
| `waiting` | `pr_url && tests_passing && pr_approved` | Ready to merge | blue |
| `waiting` | `pr_url && !tests_passing` | Tests failing | red |
| `waiting` | `blocked_reason set` | Needs input | amber |
| `merged` | — | Merged | purple |
| `failed` | — | Failed | red |
| `abandoned` | — | Abandoned | faint |

### State Transitions

```
created   → briefed       [brief delivered]
briefed   → queued        [dispatch sent to swarm]
queued    → running       [swarm worktree ready, agent started]

running   → waiting       [agent explicitly pauses]
running   → failed        [agent exits non-zero]
running   → (properties)  [branch_ready=true if review_mode=local_first]
                          [pr_url set if review_mode=pr_first]

waiting   → running       [human sends message → revision_count++]
waiting   → abandoned     [manually cancelled]

(any)     → abandoned     [manual cancel]
failed    → queued        [manual re-queue — new attempt, revision_count reset]
abandoned → queued        [manual re-queue]
```

### Swarm Reconciliation Rules

Backend polls `.swarm/state.json` every 5s and applies these rules. Swarm state is a **forward-only input** — it can push state forward, never backward.

| Swarm signal | Current DB state | Action |
|-------------|-----------------|--------|
| agent running | `queued` | → `running`, set `state_entered_at` |
| agent waiting | `running` or `revising` | → `waiting`, set `blocked_reason` if available |
| agent exited 0 | `running` | → `waiting`, set `branch_ready=true` |
| agent exited non-0 | `running` | → `failed` |
| pr_url appeared | `running` or `waiting` | set `pr_url` property |
| pr merged | any | → `merged` |
| no output for 10min | `running` | set `is_stalled=true` |
| new output event | `running` | clear `is_stalled`, update `last_output_at` |

---

## Worker Brief Schema

This is the contract between context bot and worker. It is stored as JSON in the `workers.brief` column and passed to swarm as the worker's initial prompt.

```json
{
  "goal": "Short, precise description of what to build or fix",
  "context": {
    "relevant_files": ["src/routes.rs", "src/middleware/"],
    "recent_changes": "Prose summary of relevant recent changes",
    "known_issues": ["List of known gotchas or constraints"],
    "conventions": "Key patterns to follow in this area of the codebase"
  },
  "constraints": [
    "Do not touch the frontend",
    "Stay within crates/hive/",
    "Do not run cargo install"
  ],
  "repo": "apiari",
  "scope": ["crates/hive/src/"],
  "acceptance_criteria": [
    "Specific, testable criteria for done"
  ],
  "review_mode": "local_first"
}
```

The brief is rendered into a structured prompt handed to the worker via `swarm create --prompt-file`. Workers do not explore the codebase to orient themselves — the brief tells them where to look.

---

## Worker Hooks (reserved for future use — design for it now)

Workers have a hook system. Auto bots can subscribe to conditions and react. The schema supports this via a `worker_hooks` table (do not implement the execution logic yet, just the table).

```sql
CREATE TABLE worker_hooks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace TEXT NOT NULL,
  trigger_state TEXT,           -- state to watch (e.g. 'waiting')
  trigger_property TEXT,        -- property to watch (e.g. 'branch_ready')
  trigger_value TEXT,           -- value that triggers (e.g. 'true')
  duration_minutes INTEGER,     -- optional: must be in state for N minutes
  action TEXT NOT NULL,         -- 'nudge' | 'notify' | 'create_pr' | 'dispatch_auto_bot'
  auto_bot_id INTEGER,          -- which auto bot to dispatch (if action=dispatch_auto_bot)
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
```

---

## Auto Bot Entity

### DB Schema

```sql
CREATE TABLE auto_bots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace TEXT NOT NULL,
  name TEXT NOT NULL,
  color TEXT NOT NULL DEFAULT '#f5c542',
  trigger_type TEXT NOT NULL,     -- 'cron' | 'signal'
  cron_schedule TEXT,             -- cron expression (e.g. '0 9 * * 1-5')
  signal_source TEXT,             -- 'github' | 'sentry' | 'ci'
  signal_filter TEXT,             -- JSON filter conditions
  prompt TEXT NOT NULL,           -- what the bot does when triggered
  provider TEXT NOT NULL DEFAULT 'claude',
  model TEXT,
  enabled BOOLEAN DEFAULT TRUE,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE auto_bot_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  auto_bot_id INTEGER NOT NULL REFERENCES auto_bots(id),
  workspace TEXT NOT NULL,
  triggered_by TEXT NOT NULL,     -- 'cron' | 'signal:{source}:{id}'
  started_at DATETIME NOT NULL,
  finished_at DATETIME,
  outcome TEXT,                   -- 'dispatched_worker' | 'notified' | 'noise' | 'error'
  summary TEXT,                   -- what the bot decided / did
  worker_id INTEGER               -- if a worker was dispatched
);
```

### UI

- Sidebar: listed under "AUTO BOTS" section with a live status pulse (idle/running/error)
- Detail page: reverse-chronological feed of runs (Sentry issue activity style)
  - Each run shows: trigger, timestamp, outcome badge, summary
  - Expandable to see full bot output
- Config panel: edit trigger, schedule, signal filter, prompt

---

## Context Bot

Context bots are ephemeral Claude sessions scoped to a specific view context. They are not persisted between sessions.

### Context Payload

When a context bot is opened, the frontend captures and sends:

```json
{
  "view": "worker_detail",
  "entity_id": "cli-42",
  "entity_snapshot": {
    "state": "waiting",
    "branch_ready": true,
    "pr_url": null,
    "revision_count": 1,
    "goal": "Add rate limiting to /api/chat"
  }
}
```

This is injected into the bot's system prompt. The bot always knows what you're looking at.

### UI

- Float bottom-right on desktop
- Multiple can be open simultaneously, each identified by what they were opened on
- Each panel: title bar ("Viewing: fix-auth"), minimize button, close button, chat thread, input
- Stack vertically if multiple open; minimize to a tab strip along the bottom
- On iPad: bottom sheet. On mobile: full-screen modal.
- Context bots can call an action to dispatch a worker — triggers the brief flow.

---

## Database Changes from v1

### New tables
- `workers` — replaces ad-hoc swarm state reading
- `auto_bots` — new entity
- `auto_bot_runs` — auto bot run history
- `worker_hooks` — reserved, schema only

### Modified tables
- `conversations` — add `entity_type` (`bot` | `worker` | `auto_bot`) and `entity_id` columns for unified message history
- `bot_status` — rename concept: auto bots use this table too (keyed by auto_bot_id)

### Keep as-is
- `sessions` — unchanged
- `signals` — unchanged
- `last_seen` — unchanged

---

## API Changes

### New endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/workspaces/{ws}/workers` | List workers with state + properties |
| GET | `/api/workspaces/{ws}/workers/{id}` | Worker detail + brief + conversation |
| POST | `/api/workspaces/{ws}/workers` | Create worker (with brief JSON) |
| POST | `/api/workspaces/{ws}/workers/{id}/send` | Send message → swarm, transitions state |
| POST | `/api/workspaces/{ws}/workers/{id}/cancel` | Abandon worker |
| POST | `/api/workspaces/{ws}/workers/{id}/requeue` | Re-queue failed/abandoned worker |
| GET | `/api/workspaces/{ws}/auto-bots` | List auto bots |
| GET | `/api/workspaces/{ws}/auto-bots/{id}` | Auto bot detail + run history |
| POST | `/api/workspaces/{ws}/auto-bots` | Create auto bot |
| PATCH | `/api/workspaces/{ws}/auto-bots/{id}` | Update auto bot |
| GET | `/api/workspaces/{ws}/auto-bots/{id}/runs` | Paginated run history |
| POST | `/api/workspaces/{ws}/context-bot/chat` | Chat with context bot (stateless per call, session managed client-side) |

### Keep existing
All existing `/api/workspaces/{ws}/bots/*` and `/api/workspaces/{ws}/conversations/*` endpoints stay unchanged.

---

## WebSocket Events (additions)

```json
{ "type": "worker_state", "workspace": "...", "worker_id": "...", "state": "...", "properties": {...} }
{ "type": "worker_output", "workspace": "...", "worker_id": "...", "content": "..." }
{ "type": "auto_bot_run_started", "workspace": "...", "auto_bot_id": "..." }
{ "type": "auto_bot_run_finished", "workspace": "...", "auto_bot_id": "...", "outcome": "..." }
```

---

## Implementation Phases

### Phase 1 — Foundation (run in parallel)

**1A: Backend — Worker State Machine**
- New DB schema (`workers`, `worker_hooks` stub, update `conversations`)
- Swarm reconciliation loop: poll `.swarm/state.json` every 5s, apply transition rules
- Stall detection: `last_output_at` + 10min threshold → `is_stalled`
- Worker API endpoints (list, detail, create, send, cancel, requeue)
- Worker WebSocket events
- Keep all existing bot infrastructure untouched

**1B: Frontend — New UI Shell**
- Scrap current App.tsx layout and all panel components
- New design tokens in theme.css
- Sidebar component: Auto Bots section + Workers section, responsive (240px/60px rail/bottom tab)
- Main content router: renders selected entity or empty state
- No content yet — just the shell with placeholder views
- Responsive layout for desktop, iPad, mobile

### Phase 2 — Worker Detail Page

- Depends on Phase 1A + 1B both complete
- Linear-style worker detail: title, status badge, branch, PR link, revision count
- Property pills: tests passing, branch ready, stall indicator
- Full conversation thread (agent output events)
- Action bar: Send message, Approve (local review), Request PR, Cancel
- Wire to Phase 1A API

### Phase 3 — Auto Bots

**3A: Backend**
- `auto_bots` + `auto_bot_runs` tables
- Cron scheduler (tokio-cron or similar) — respects enabled flag
- Signal routing: existing watcher signals can trigger auto bots
- Auto bot runner: same BotRunner pipeline, result stored as run record
- Auto bot can dispatch a worker via the worker create API

**3B: Frontend**
- Auto bot sidebar entries with live status pulse
- Auto bot detail page: run feed (Sentry-style), config panel
- Toggle enabled/disabled

### Phase 4 — Context Bots

- Floating bottom-right chat panel component
- Context payload capture (current view + entity snapshot)
- Multiple simultaneous sessions (client-side session management)
- Minimize/stack behavior
- "Dispatch worker" action: opens brief form pre-filled by context bot suggestion
- iPad bottom sheet + mobile full-screen modal variants

### Phase 5 — Polish

- Cmd+K palette (workers + auto bots + bots, searchable)
- Worker hook execution (auto-nudge, auto-notify based on property conditions)
- Dashboard view (summary: N workers running, N waiting, recent auto bot activity)
- Empty states for all views

---

## Key Constraints for Workers

1. **Backend owns state.** Frontend never reads `.swarm/state.json` directly. All state flows through the backend API and WebSocket.
2. **Swarm state is forward-only.** Reconciliation rules never move state backward.
3. **Brief is the contract.** Workers receive a pre-digested brief. They do not explore the codebase to orient themselves.
4. **CSS rules.** Never `overflow: hidden` on message containers. Use CSS modules. 16px min font on inputs. Test mobile.
5. **Run all checks before committing.** `cargo fmt && cargo clippy -- -D warnings && cargo test`. Frontend: `cd web && npx tsc --noEmit && npx vitest run`.
6. **Do not run `cargo install`.**
7. **Stay in the apiari repo.** Do not modify swarm or other repos.
8. **`is_stalled` is a property, not a state.** Detect it via timeout, clear it when output arrives.
9. **`branch_ready` and `pr_url` are properties, not states.** The UI derives labels from these, the state machine does not branch on them.

# Bee Autonomy: Signals, Heartbeats, and Actions

## Problem

Bees are reactive and one-way. A signal comes in, the Bee comments on it via
Telegram/TUI, and that's it. The Bee's conclusion doesn't flow back into the
system as an action. There's no way for a Bee to dismiss a signal, escalate
to the user, dispatch a worker, or trigger another Bee.

## Design

### Two Trigger Modes

Every Bee operates in two modes simultaneously:

**Signal-driven (immediate, reactive)**
- Watcher detects something (Sentry error, CI failure, PR merged)
- Creates a signal record in the DB
- Orchestrator matches it to a Bee's signal hooks
- Bee wakes up immediately and processes it
- For urgent/critical items

**Heartbeat (periodic, proactive)**
- Bee wakes on a schedule (configurable per Bee: 30m, 1h, 4h, daily)
- Looks at ALL recent signals holistically, not just one at a time
- Synthesizes patterns ("5 DB timeouts in the last hour")
- Surfaces insights that individual signals miss
- For big-picture awareness and pattern detection

Config:
```toml
[[bees]]
name = "CustomerBee"
heartbeat = "30m"
heartbeat_prompt = "Review all Sentry errors from the last 30 minutes. Look for patterns, recurring issues, or anything that needs attention. If nothing notable, stay quiet."
```

### Bee Action Markers

When a Bee processes a signal (either via hook or heartbeat), its response
can include action markers that the system executes:

| Marker | What it does |
|--------|-------------|
| `[DISMISS: signal_id]` | Resolves the signal, removes from briefing |
| `[ESCALATE: message]` | Creates a high-priority briefing item for the user |
| `[FIX: description]` | Tells CodeBee to dispatch a swarm worker |
| `[RESEARCH: topic]` | Triggers ResearchBee's research workflow |
| `[TASK: title]` | Creates a task in the task store |
| `[SNOOZE: signal_id, hours]` | Snoozes a signal for N hours |

The Bee decides which action to take based on its judgment. The system
parses the response, extracts markers, and executes them.

Example flow:
1. Sentry error arrives: "DBConnection.ConnectionError: tcp timeout"
2. CustomerBee's signal hook fires
3. CustomerBee responds: "This is a Neon cold start timeout — transient, seen 3 times this week. No action needed. [DISMISS: 12345]"
4. System parses `[DISMISS: 12345]`, resolves the signal
5. Nothing reaches the user (noise filtered)

Example escalation:
1. Sentry error arrives: "MatchError: no match of right hand side"
2. CustomerBee responds: "New error type never seen before — affects the /api/events endpoint. This needs investigation. [ESCALATE: New MatchError in /api/events — likely a code regression] [FIX: Investigate and fix the MatchError in the events endpoint caused by a pattern match failure]"
3. System creates a briefing item AND tells CodeBee to dispatch a worker

### Bee-to-Bee Communication

`[FIX: ...]` is actually Bee-to-Bee communication — CustomerBee is telling
CodeBee to do something. This works through the signal/task system:

1. CustomerBee emits `[FIX: description]`
2. System creates a signal with source `bee/CustomerBee` and the description
3. CodeBee's signal hooks match `bee/*` signals
4. CodeBee wakes up and dispatches a swarm worker

No direct Bee-to-Bee messaging needed. They communicate through the same
signal system that external events use.

### Signal → Bee → Action Flow

```
External Event (Sentry, GitHub, etc.)
    ↓
Watcher creates Signal
    ↓
Orchestrator matches to Bee's signal_hooks
    ↓
Bee wakes up, processes signal
    ↓
Bee responds with text + action markers
    ↓
System parses markers:
    [DISMISS]   → resolve signal
    [ESCALATE]  → create briefing item
    [FIX]       → emit signal → CodeBee dispatches worker
    [RESEARCH]  → trigger research workflow
    [TASK]      → create task in store
    [SNOOZE]    → snooze signal
    ↓
Text response → Telegram/TUI/Web (for awareness)
```

### Heartbeat → Synthesis → Action Flow

```
Timer fires (every 30m/1h/4h)
    ↓
Bee wakes with heartbeat_prompt
    ↓
Bee queries recent signals (via Bash/sqlite3 or built-in context)
    ↓
Bee synthesizes: "5 DB timeouts, all Neon cold starts, no action needed"
    or: "New error pattern — 3 MatchErrors in /api/events since 2pm"
    ↓
Bee responds with text + action markers
    ↓
Same marker processing as signal-driven flow
```

### What the Briefing Shows

The Briefing becomes the output of Bee decisions, not raw signals:

- **Action items**: things a Bee escalated that need user decision
- **Bee activity**: what Bees did autonomously (dismissed 5 transient errors,
  dispatched a worker for a bug fix)
- **Quiet**: Bees checked in and found nothing notable

The user sees curated, Bee-processed information — not raw signal firehose.

## Implementation Plan

### Phase 1: Action Marker Parsing
- Parse Bee responses for `[DISMISS]`, `[ESCALATE]`, `[FIX]`, `[RESEARCH]`, `[TASK]`, `[SNOOZE]`
- Execute each action (resolve signal, create briefing item, emit signal, etc.)
- Works with existing signal hook responses

### Phase 2: Per-Bee Heartbeats
- Add `heartbeat` and `heartbeat_prompt` to BeeConfig
- Heartbeat timer per Bee (separate from the idle nudge)
- On fire: send heartbeat_prompt to the Bee's coordinator
- Parse response for action markers (same as Phase 1)

### Phase 3: Bee-to-Bee Signals
- `[FIX]` emits a signal with source `bee/CustomerBee`
- CodeBee's signal hooks match `bee/*`
- Enables autonomous cross-Bee collaboration

### Phase 4: Briefing as Bee Output
- Briefing items created by `[ESCALATE]` markers
- Activity log shows what Bees did autonomously
- User only sees what Bees decided needs attention

## Key Principles

1. **Bees decide, system executes.** The Bee's LLM judgment determines
   what to do. The system provides the mechanisms to act on that judgment.

2. **Same protocol, two triggers.** Whether a Bee wakes from a signal or
   a heartbeat, the response processing is identical. Action markers work
   the same way regardless of how the Bee was triggered.

3. **Signals are the universal bus.** External events, Bee actions, and
   Bee-to-Bee communication all flow through the signal system. No special
   channels needed.

4. **Noise reduction is the goal.** The whole point is that 50 Sentry
   errors become 1 briefing item (or 0 if the Bee handles it). The user's
   attention is the scarcest resource.

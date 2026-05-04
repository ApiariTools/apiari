# Architecture

`apiari` is a workspace-first agent runtime with multiple operator surfaces layered on top.

## Layer Ownership

### Runtime Core

Owns:

- conversation lifecycle
- session lifecycle
- followup scheduling
- worker orchestration hooks
- persistent state transitions

Primary code:

- `crates/apiari/src/daemon/`
- `crates/apiari/src/buzz/`
- `crates/apiari/src/config.rs`

### Provider Adapters

Own:

- subprocess execution against provider CLIs
- stream parsing
- event normalization
- provider-specific session resume handling

Primary code:

- `crates/claude-sdk/`
- `crates/codex-sdk/`
- `crates/gemini-sdk/`

### Persistent State

Owns:

- conversations
- unread/seen markers
- followups
- bot status
- signal and worker-related durable metadata

Primary code:

- `crates/apiari/src/buzz/signal/store.rs`
- related storage helpers in `crates/apiari/src/buzz/conversation.rs`

### Operator Surfaces

These surfaces should present the same underlying truth, not invent separate behavior.

#### Daemon HTTP + WebSocket

Owns:

- transport contract for web and external clients
- compatibility mapping across legacy/current workspace and bot naming

Primary code:

- `crates/apiari/src/daemon/http.rs`

#### Web App

Owns:

- workspace-first navigation
- local UI state reconciliation
- rendering of daemon contract surfaces

Primary code:

- `web/src/`

#### TUI

Owns:

- terminal rendering of the same runtime state
- keyboard-driven inspection and chat

Primary code:

- `crates/apiari/src/ui/`

## Design Rules

1. Runtime behavior should live below the UI layer.
2. Provider differences should be absorbed in adapters or normalized coordinator logic.
3. Path and workspace resolution should be centralized, not reimplemented in each surface.
4. Web, TUI, and future automation surfaces must consume the same state transitions.
5. Legacy compatibility should be explicit and tested.

## Common Failure Modes

The monorepo transition exposed the main risks:

- one path writes runtime state while another path reads a different default
- a UI assumes an API shape that the daemon no longer serves
- provider-specific quirks leak past normalization
- legacy names like `Bee` vs `Main` drift across surfaces

## Stabilization Priorities

When changing architecture-adjacent code, protect these first:

- chat send/receive/status lifecycle
- worker lifecycle
- workspace/repo path resolution
- followup persistence and resume
- provider parity at the normalized behavior layer

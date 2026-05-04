# Web Contract

The web app is an operator surface over the daemon. It should rely on explicit contract shapes, not inferred behavior.

## Core Principles

1. The daemon is the source of truth.
2. WebSocket events are real state signals, not hints with ad hoc meanings.
3. Legacy compatibility belongs in the daemon contract, not scattered through the UI.

## HTTP Surfaces

### Workspace Routes

- `GET /api/workspaces`
- `GET /api/workspaces/{workspace}/bots`
- `GET /api/workspaces/{workspace}/repos`
- `GET /api/workspaces/{workspace}/workers`
- `GET /api/workspaces/{workspace}/conversations/{bot}`
- `GET /api/workspaces/{workspace}/bots/{bot}/status`
- `POST /api/workspaces/{workspace}/chat/{bot}`
- `GET /api/workspaces/{workspace}/unread`
- `POST /api/workspaces/{workspace}/seen/{bot}`
- `GET /api/workspaces/{workspace}/followups`
- `DELETE /api/workspaces/{workspace}/followups/{id}`
- `GET /api/workspaces/{workspace}/research`
- `GET /api/workspaces/{workspace}/docs`
- `GET /api/workspaces/{workspace}/docs/{filename}`
- `PUT /api/workspaces/{workspace}/docs/{filename}`
- `DELETE /api/workspaces/{workspace}/docs/{filename}`

### Compatibility Expectations

- `Main` maps cleanly to the primary coordinator even when legacy state uses `Bee`.
- repo resolution must handle mismatched local dir names and remote slugs
- docs resolution must prefer `.apiari/docs` while still supporting legacy `docs/`
- swarm/worker state resolution must honor explicit config and workspace-root defaults

## WebSocket Event Types

The web app should treat these as stable event categories:

- `message`
- `bot_status`
- `followup_created`
- `followup_fired`
- `followup_cancelled`

Each event should be sufficient to reconcile the local UI without guessing hidden semantics.

## Status Semantics

The UI should assume:

- `thinking` means turn accepted and running
- `streaming` means partial assistant output may exist
- `idle` means the turn is complete or cancelled

The daemon should not report fake status transitions.

## UI Responsibilities

The web app may:

- optimistically show the local user message
- merge real WebSocket messages immediately
- reconcile with follow-up polling/fetches

The web app should not:

- invent provider-specific rules
- rely on duplicate hidden routes
- infer daemon state from missing fields when a stable endpoint exists

## Contract Change Rule

Any user-visible contract change should land with:

1. daemon-side contract tests
2. web-side reconciliation tests if the UI behavior depends on it
3. a short doc update here if the surface meaning changed

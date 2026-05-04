# Testing Doctrine

This repo is a long-running agent runtime, not a stateless CLI. The tests should reflect that.

## Rules

1. Every bug fix gets a regression test.
2. Every user entry point gets at least one happy-path test.
3. Prefer contract and integration tests over tiny isolated unit tests when behavior crosses layers.
4. Provider behavior is tested by normalized outcomes, not exact prose.
5. Runtime state transitions must be observable and asserted.

## Entry Points

The minimum happy-path coverage should include:

- daemon HTTP routes
- daemon WebSocket events
- web app primary views and navigation
- worker lifecycle surfaces
- provider adapter turn execution
- followup scheduling and resume behavior

## Preferred Test Shapes

### Contract tests

Use these when the exact API shape matters.

Examples:

- `GET /api/workspaces/{workspace}/bots`
- `POST /api/workspaces/{workspace}/chat/{bot}`
- WebSocket `message`, `bot_status`, and followup events

### Integration tests

Use these when a feature spans multiple layers.

Examples:

- user message persists, provider runs, assistant reply appears, bot returns to `idle`
- worker moves from `running` to `waiting` to PR-linked completion
- followup survives restart and fires later

### UI state tests

Use these when timing or reconciliation matters.

Examples:

- mobile mode switching
- WebSocket + polling reconciliation
- docs mobile list-to-document flow

### Real-provider smoke tests

Keep these opt-in and local-only when credentials are required.

They should verify:

- subprocess invocation still works
- streaming output is still parsed
- session resume still works
- provider-specific prompt shaping still preserves the same visible contract

## Provider Parity

Do not assert exact wording across Claude, Codex, and Gemini.

Do assert:

- status flow
- message persistence
- followup/task/research marker behavior
- cancellation behavior
- error handling shape

## When Adding Tests

- Add the narrowest test that would have caught the bug.
- If the bug crossed a surface boundary, add the test at that boundary.
- If the bug was caused by an implicit default or path assumption, write the test so the assumption is explicit.

## Scenario Evals

Not every important behavior fits cleanly into a single source-level test. The `evals/` directory exists for named runtime scenarios that should remain stable across refactors.

# apiari-codex-sdk

Rust SDK wrapping the OpenAI Codex CLI via JSONL stdout streaming.

## Rules
1. You are working in a git worktree on a `swarm/*` branch. Never commit directly to `main`.
2. Only modify files within this repository.
3. Do not run `cargo install` or modify system state.

## Quick Reference

```bash
cargo test -p apiari-codex-sdk
cargo test -p apiari-codex-sdk -- --ignored  # Integration tests (requires live `codex` CLI)
```

## Architecture

```
src/
  lib.rs          # Module declarations + re-exports
  client.rs       # CodexClient (factory) + Execution (read-only handle)
  options.rs      # ExecOptions, ResumeOptions, SandboxMode, ApprovalPolicy
  transport.rs    # ReadOnlyTransport (spawn, recv-only, interrupt, kill)
  types.rs        # Event, Item variants, Usage, supporting types
  error.rs        # SdkError enum + Result alias
tests/
  integration.rs  # Live CLI tests (#[ignore] by default)
```

## Protocol

Spawns: `codex exec --json [opts...] <prompt>`

**Unidirectional**: stdin is `/dev/null`. The SDK only reads JSONL events from stdout.
No `send_message()` or `send_tool_result()` — codex handles tool execution internally.

### Event Types (codex -> stdout)

- `thread.started` — thread ID assigned
- `turn.started` / `turn.completed` / `turn.failed` — turn lifecycle
- `item.started` / `item.updated` / `item.completed` — item lifecycle
- `token_count` — token usage statistics
- `error` — execution error

### Item Types

- `agent_message` — model text response
- `reasoning` — thinking/reasoning text
- `command_execution` — shell command with output
- `file_change` — file modifications
- `mcp_tool_call` — MCP tool invocation
- `web_search` — web search query
- `todo_list` — task list
- `error` — item-level error

## Execution Model — Key Difference from Claude SDK

**Claude SDK** is bidirectional: you send messages on stdin, receive events on stdout, and can interact mid-turn (send tool results, follow-up messages while the session is running).

**Codex SDK** is single-shot: one prompt in, event stream out, then it's done. There is NO way to send input during an execution.

### How "Chat" Works (Resume-as-Chat Pattern)

To build a conversational UI on top of codex, each user message is a **separate execution** that resumes the previous session:

```
User message 1  →  exec("do the thing", ExecOptions { .. })
                   ← stream events until EOF
                   ← save thread_id from ThreadStarted event

User message 2  →  exec_resume("now fix the tests", ResumeOptions { session_id, .. })
                   ← stream events until EOF

User message 3  →  exec_resume("looks good, commit it", ResumeOptions { session_id, .. })
                   ← stream events until EOF
```

### UI Implications

- **Input must be disabled while an execution is running.** You cannot queue or send messages mid-execution.
- **interrupt() (SIGINT) can cancel** a running execution, but you can't redirect it — only stop it.
- **Events stream in real-time** (`item.updated` gives incremental text) — the UI should render them live, not wait for completion.
- **Apiari integration** should abstract this behind `enum AgentBackend { Claude(..), Codex(..) }` at the coordinator level. The coordinator disables input for Codex backends while an execution is in flight, re-enables on EOF.

## Design Rules

- **Wrap CLI, not API.** This SDK spawns the `codex` binary. It does NOT call the OpenAI API directly.
- **Forward-compatible parsing.** Unknown event/item types deserialize as `Unknown` variant. Fields use `#[serde(default)]` liberally.
- **Async throughout.** All I/O uses tokio. Transport runs a background task to drain stderr.
- **No apiari-common dependency.** This crate is standalone.

## Error Handling

`SdkError` variants:
- `ProcessSpawn` — codex binary not found or failed to start
- `ProcessDied { exit_code, stderr }` — subprocess exited unexpectedly
- `InvalidJson` — NDJSON parse failure
- `ProtocolError` — unexpected protocol state
- `Timeout` — operation timed out
- `Io` — underlying I/O error
- `NotRunning` — execution already finished

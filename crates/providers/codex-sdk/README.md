# apiari-codex-sdk

**Rust SDK for the [OpenAI Codex CLI](https://github.com/openai/codex) — spawn `codex exec` and stream JSONL events.**

[![Crates.io](https://img.shields.io/crates/v/apiari-codex-sdk)](https://crates.io/crates/apiari-codex-sdk)
[![docs.rs](https://img.shields.io/docsrs/apiari-codex-sdk)](https://docs.rs/apiari-codex-sdk)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> Not a direct API client. This crate spawns the `codex` CLI as a subprocess —
> the CLI handles authentication, tool execution, file access, and sandboxing.

## Quick Start

```rust
use apiari_codex_sdk::{CodexClient, ExecOptions, Event, Item};

#[tokio::main]
async fn main() -> apiari_codex_sdk::Result<()> {
    let client = CodexClient::new();
    let mut execution = client.exec(
        "List files in the current directory",
        ExecOptions {
            model: Some("o4-mini".into()),
            full_auto: true,
            ..Default::default()
        },
    ).await?;

    while let Some(event) = execution.next_event().await? {
        match &event {
            Event::ItemCompleted { item: Item::AgentMessage { text, .. } } => {
                if let Some(text) = text {
                    println!("{text}");
                }
            }
            Event::TurnCompleted { usage } => {
                if let Some(u) = usage {
                    println!("Tokens: {} in, {} out", u.input_tokens, u.output_tokens);
                }
            }
            _ => {}
        }
    }
    Ok(())
}
```

Add to your `Cargo.toml`:

```toml
[dependencies]
apiari-codex-sdk = "0.1"
```

Requires the `codex` CLI on `$PATH` (or use `CodexClient::with_cli_path`).

## Execution Model

The SDK uses a **unidirectional, single-shot** protocol. Each execution is one prompt in, one event stream out. Stdin is `/dev/null`.

```
┌─────────┐    CLI args (prompt)        ┌───────────┐
│   SDK   │ ────── exec() ────────────► │  codex    │
│         │                             │  CLI      │
│         │ ◄──── next_event() ──────── │  (stdout) │
└─────────┘       JSONL stream          └───────────┘
                                        stdin = /dev/null
```

**Key properties:**

- **One prompt per execution** — the prompt is a CLI argument, not streamed.
- **Real-time event streaming** — `ItemUpdated` gives incremental text. Render it live.
- **No mid-execution input** — you cannot send messages during an execution.
- **Tool execution is internal** — codex runs commands, edits files, and searches the web on its own. The SDK observes these as `CommandExecution`, `FileChange`, `WebSearch` items.
- **`interrupt()` cancels** the current execution via SIGINT.
- **Resume for multi-turn** — save the `thread_id` from `ThreadStarted` and pass it to `exec_resume()`.

### Multi-Turn Chat

Each user message is a separate execution that resumes the previous session:

```rust
// First message
let mut exec1 = client.exec("Set up a new Rust project", opts).await?;
let mut thread_id = None;
while let Some(event) = exec1.next_event().await? {
    if let Event::ThreadStarted { thread_id: tid } = &event {
        thread_id = Some(tid.clone());
    }
    // ... handle events ...
}

// Follow-up — resumes the same session
let mut exec2 = client.exec_resume("Now add serde", ResumeOptions {
    session_id: thread_id,
    full_auto: true,
    ..Default::default()
}).await?;
```

### Codex SDK vs Claude SDK

| | Codex SDK | Claude SDK |
|---|---|---|
| **Protocol** | Unidirectional (stdout only) | Bidirectional (stdin + stdout) |
| **Session lifetime** | One subprocess per message | One subprocess for entire conversation |
| **Mid-turn input** | No (stdin is `/dev/null`) | Yes (`send_message`, `send_tool_result`) |
| **Tool execution** | CLI handles tools internally | SDK receives tool requests, sends results |
| **Multi-turn chat** | Resume with session ID | Send multiple messages on same session |

## Event Types

| Event | Description |
|-------|-------------|
| `ThreadStarted` | Thread ID assigned — save for resume |
| `TurnStarted` | A new turn has begun |
| `TurnCompleted` | Turn finished (includes usage stats) |
| `TurnFailed` | Turn failed (includes error details) |
| `ItemStarted` | An item began (message, command, etc.) |
| `ItemUpdated` | Incremental update to an in-flight item |
| `ItemCompleted` | An item finished |
| `TokenCount` | Token usage statistics |
| `Error` | Execution-level error |

## Item Types

| Item | Description |
|------|-------------|
| `AgentMessage` | Text response from the model |
| `Reasoning` | Chain-of-thought / thinking text |
| `CommandExecution` | Shell command with output and exit code |
| `FileChange` | File modifications with before/after content |
| `McpToolCall` | MCP tool invocation |
| `WebSearch` | Web search query |
| `TodoList` | Task list |
| `Error` | Item-level error |
| `Unknown` | Forward-compatibility catch-all |

## Architecture

```
src/
  lib.rs          # Re-exports
  client.rs       # CodexClient + Execution handle
  options.rs      # ExecOptions, ResumeOptions, SandboxMode, ApprovalPolicy
  transport.rs    # ReadOnlyTransport — spawn, recv, interrupt, kill
  types.rs        # Event, Item, Usage
  error.rs        # SdkError + Result alias
tests/
  integration.rs  # Live CLI tests (#[ignore] by default)
```

## Apiari Ecosystem

This crate is part of the [Apiari](https://github.com/ApiariTools) tooling ecosystem:

- **[apiari-codex-sdk](https://github.com/ApiariTools/apiari-codex-sdk)** — Codex CLI SDK (this crate)
- **[apiari-claude-sdk](https://github.com/ApiariTools/apiari-claude-sdk)** — Claude Code SDK

## License

MIT

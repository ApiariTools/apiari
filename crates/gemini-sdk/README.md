# apiari-gemini-sdk

Rust SDK for the [OpenAI Gemini CLI](https://github.com/openai/gemini). Wraps the `gemini` binary, reading JSONL events from stdout when invoked with `gemini exec --json`.

This is **not** a direct API client. It spawns the `gemini` CLI as a subprocess. The CLI handles authentication, tool execution, file access, and sandboxing.

## Quick Start

```rust
use apiari_gemini_sdk::{GeminiClient, ExecOptions, Event, Item};

#[tokio::main]
async fn main() -> apiari_gemini_sdk::Result<()> {
    let client = GeminiClient::new();
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

## Features

- Spawn and stream `gemini exec --json` sessions
- Typed event stream with real-time incremental updates
- Resume previous sessions for multi-turn chat
- Full `ExecOptions` covering model, sandbox, approval policy, and more
- Interrupt (SIGINT) and kill support
- Forward-compatible parsing — unknown event/item types deserialize as `Unknown`

## Requirements

- The `gemini` CLI must be installed and on `$PATH` (or provide a custom path via `GeminiClient::with_cli_path`)

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
apiari-gemini-sdk.workspace = true
```

## Execution Model — Unidirectional Single-Shot

The Gemini SDK uses a **unidirectional** protocol. Each execution is a single prompt in, event stream out. There is no stdin — it is set to `/dev/null`.

```
┌─────────┐    CLI args (prompt)        ┌───────────┐
│   SDK   │ ────── exec() ────────────► │  gemini    │
│         │                             │  CLI      │
│         │ ◄──── next_event() ──────── │  (stdout) │
└─────────┘       JSONL stream          └───────────┘
                                        stdin = /dev/null
```

### How a Single Execution Works

```rust
// 1. Start an execution — prompt goes as a CLI argument
let mut execution = client.exec("Fix the failing tests", opts).await?;

// 2. Read events as they stream in real-time
while let Some(event) = execution.next_event().await? {
    match &event {
        Event::ThreadStarted { thread_id } => {
            // Save this for resuming later
            println!("Thread: {thread_id}");
        }
        Event::ItemUpdated { item: Item::Reasoning { text, .. } } => {
            // Reasoning streams incrementally
            if let Some(text) = text { print!("{text}"); }
        }
        Event::ItemCompleted { item: Item::CommandExecution { command, exit_code, .. } } => {
            println!("Ran: {} (exit {})", command.as_deref().unwrap_or("?"), exit_code.unwrap_or(-1));
        }
        Event::ItemCompleted { item: Item::AgentMessage { text, .. } } => {
            if let Some(text) = text { println!("{text}"); }
        }
        _ => {}
    }
}
// 3. Execution is done — process has exited
```

### Multi-Turn Chat via Resume

To build a conversational UI, each user message is a **separate execution** that resumes the previous session using the thread ID:

```rust
// First message — new execution
let mut exec1 = client.exec("Set up a new Rust project", opts).await?;
let mut thread_id = None;
while let Some(event) = exec1.next_event().await? {
    if let Event::ThreadStarted { thread_id: tid } = &event {
        thread_id = Some(tid.clone());
    }
    // ... handle events ...
}

// Second message — resumes the same session
let mut exec2 = client.exec_resume("Now add serde as a dependency", ResumeOptions {
    session_id: thread_id,
    full_auto: true,
    ..Default::default()
}).await?;
while let Some(event) = exec2.next_event().await? {
    // ... handle events ...
}
```

Each `exec_resume()` creates a new subprocess that loads the previous session state. The conversation continues where it left off.

### Key Properties

- **One prompt per execution** — the prompt is a CLI argument, not streamed via stdin.
- **Events stream in real-time** — `item.updated` gives incremental text for reasoning and messages. Render them live.
- **No mid-execution input** — you cannot send messages or tool results during an execution. Input must be disabled in the UI while gemini is working.
- **Tool execution is internal** — gemini runs commands, edits files, and searches the web on its own. The SDK observes these as `CommandExecution`, `FileChange`, `WebSearch` items.
- **`interrupt()` cancels** the current execution (SIGINT), but you cannot redirect it — only stop it.
- **Resume for follow-ups** — save the `thread_id` from `ThreadStarted` and pass it to `exec_resume()` for the next message.

### UI Implications

- **Disable input while an execution is running.** There is no way to send messages mid-execution.
- **Re-enable input when `next_event()` returns `None`** (execution finished).
- **Render events incrementally** — `item.updated` events carry partial text that grows over time.
- **Show tool activity** — `CommandExecution`, `FileChange`, `McpToolCall` items let you show what gemini is doing.

### Comparison with Claude SDK

| | Gemini SDK | Claude SDK |
|---|---|---|
| **Protocol** | Unidirectional (stdout only) | Bidirectional (stdin + stdout) |
| **Session lifetime** | One subprocess per message | One subprocess for entire conversation |
| **Mid-turn input** | No (stdin is `/dev/null`) | Yes (`send_message`, `send_tool_result`) |
| **Tool execution** | CLI handles tools internally | SDK receives tool requests, sends results |
| **Multi-turn chat** | Resume with session ID for each message | Send multiple messages on same session |
| **Input availability** | Disabled during execution | Always enabled |

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
  lib.rs          # Module declarations + re-exports
  client.rs       # GeminiClient (factory) + Execution (read-only handle)
  options.rs      # ExecOptions, ResumeOptions, SandboxMode, ApprovalPolicy
  transport.rs    # ReadOnlyTransport (spawn, recv-only, interrupt, kill)
  types.rs        # Event, Item variants, Usage, supporting types
  error.rs        # SdkError enum + Result alias
tests/
  integration.rs  # Live CLI tests (#[ignore] by default)
```

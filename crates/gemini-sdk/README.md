# apiari-gemini-sdk

Rust SDK for the [Gemini CLI](https://github.com/google-gemini/gemini-cli). It wraps the `gemini` binary and reads headless JSON events from stdout.

This is **not** a direct API client. It spawns the Gemini CLI as a subprocess; the CLI handles auth, tool execution, file access, and sandboxing.

## Current CLI Contract

The SDK targets the current headless Gemini CLI shape:

```bash
gemini --output-format stream-json --skip-trust -p "your prompt"
```

Key points:

- No `exec` subcommand.
- Output is line-delimited JSON in `stream-json` mode.
- Each user turn is a separate subprocess.
- Multi-turn chat is implemented by saving the Gemini session ID and using `--resume` on the next turn.

## Quick Start

```rust
use apiari_gemini_sdk::{Event, GeminiClient, GeminiOptions};

#[tokio::main]
async fn main() -> apiari_gemini_sdk::Result<()> {
    let client = GeminiClient::new();
    let mut execution = client
        .exec(
            "Reply with the single word OK.",
            GeminiOptions {
                model: Some("gemini-2.5-flash".into()),
                yolo: true,
                ..Default::default()
            },
        )
        .await?;

    while let Some(event) = execution.next_event().await? {
        if let Some(text) = event.text() {
            println!("{text}");
        }
    }

    Ok(())
}
```

## Features

- Spawn Gemini headless sessions from Rust
- Stream typed `stream-json` events as they arrive
- Resume previous Gemini sessions for multi-turn chat
- Track usage, result, and error events
- Interrupt or kill running executions
- Tolerate forward-compatible event additions with `Unknown`

## Requirements

- The `gemini` CLI must be installed and on `$PATH`
- Or provide a custom binary path via `GeminiClient::with_cli_path`

## Usage

```toml
[dependencies]
apiari-gemini-sdk.workspace = true
```

## Execution Model

Gemini is a **unidirectional** provider in this SDK:

- prompt goes in as CLI args
- stdout emits JSON events
- stdin is `/dev/null`
- there is no mid-turn user input

```
┌─────────┐    CLI args (prompt)        ┌───────────┐
│   SDK   │ ────── exec() ────────────► │  gemini   │
│         │                             │   CLI     │
│         │ ◄──── next_event() ──────── │  stdout   │
└─────────┘       JSON event stream     └───────────┘
                                        stdin = /dev/null
```

### Multi-Turn Chat

Each message is a fresh subprocess. To continue a conversation:

1. read the `session_id` from `init` / thread-start events
2. save it
3. call `exec_resume()` with that ID on the next turn

That means Gemini behaves differently from Claude internally, even if the product-level chat UX looks the same.

## Event Model

The SDK supports both:

- current `stream-json` events such as `init`, `message`, `usage`, `result`, `error`
- older typed events such as `thread.started`, `item.completed`, `turn.completed`

`Event::text()` normalizes assistant text across both schemas, including:

- final JSON output text
- `message` string content
- structured content parts
- legacy item-completed message text

## UI Notes

- Disable input while a Gemini execution is running
- Re-enable it when `next_event()` returns `None`
- Render assistant deltas as they arrive from `message` events
- Use saved session IDs to preserve conversation continuity across turns

## Architecture

```text
src/
  lib.rs
  client.rs       # GeminiClient + Execution
  options.rs      # GeminiOptions + SessionOptions
  transport.rs    # subprocess spawn + stream-json line reader
  types.rs        # typed event schema + helpers like Event::text()
  error.rs        # SdkError + Result
tests/
  integration.rs
```

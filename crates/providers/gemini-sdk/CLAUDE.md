# apiari-gemini-sdk

Rust SDK for spawning and streaming [Gemini CLI](https://github.com/google-gemini/gemini-cli)
execution sessions. Wraps the `gemini` subprocess and parses its JSONL event stream.

## Usage

```rust
use apiari_gemini_sdk::{GeminiClient, GeminiOptions, Event, Item};

let client = GeminiClient { cli_path: "gemini".to_string() };
let opts = GeminiOptions { yolo: true, working_dir: Some(dir), ..Default::default() };
let mut exec = client.exec("do the task", opts).await?;
while let Some(event) = exec.next().await {
    match event? {
        Event::Item(Item::AssistantText(text)) => print!("{text}"),
        Event::Finished { usage, .. } => println!("done: {usage:?}"),
        _ => {}
    }
}
```

## Public API

| Item | Description |
|------|-------------|
| `GeminiClient` | Main entry point — holds `cli_path` |
| `GeminiClient::exec` | Start a new execution, returns `Execution` |
| `GeminiClient::exec_resume` | Resume an existing session by ID |
| `Execution` | Async stream of `Event` values |
| `GeminiOptions` | Options for new execution (model, working_dir, yolo, ephemeral) |
| `SessionOptions` | Options for resumed execution (session_id, model, working_dir, yolo) |
| `Event` | Enum: `Item(Item)`, `Finished { usage, session_id }`, `Error(ThreadError)` |
| `Item` | Enum: `AssistantText`, `ToolUse`, `ToolResult`, `FileUpdate`, `Todo`, etc. |
| `Usage` | Token counts: `input_tokens`, `output_tokens` |
| `SdkError` | Error type (spawn failure, parse error, process error) |

## Architecture

```
client.rs     — GeminiClient, Execution (async event stream)
transport.rs  — ReadOnlyTransport: reads stdout line-by-line, parses JSONL
types.rs      — Event, Item, Usage, TodoItem, FileUpdateChange, ThreadError
options.rs    — GeminiOptions, SessionOptions (CLI arg builders)
error.rs      — SdkError, Result alias
```

## Rules

- The `--yolo` flag is required for non-interactive (headless) use — always set it in production.
- No image support currently (unlike claude-sdk and codex-sdk).
- Session IDs are returned in `Event::Finished` — store them to enable `exec_resume`.
- No `tokio` in the public API surface — `Execution::next()` is the only async boundary.

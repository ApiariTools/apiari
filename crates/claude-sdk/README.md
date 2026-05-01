# apiari-claude-sdk

[![Crates.io](https://img.shields.io/crates/v/apiari-claude-sdk)](https://crates.io/crates/apiari-claude-sdk)
[![docs.rs](https://img.shields.io/docsrs/apiari-claude-sdk)](https://docs.rs/apiari-claude-sdk)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Rust SDK for spawning and controlling [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions programmatically.

The SDK launches the `claude` CLI as a subprocess and communicates via newline-delimited JSON (NDJSON) over stdin/stdout. It is **not** a direct API client — the CLI handles authentication, model routing, tool execution, and permissions. The SDK gives you typed Rust bindings over the streaming protocol so you can build agents, orchestrators, and toolchains on top of Claude Code.

## When to Use This vs the Anthropic API

| | **apiari-claude-sdk** | **Anthropic API directly** |
|---|---|---|
| **Auth** | Handled by the CLI (OAuth, API key, etc.) | You manage API keys |
| **Tool execution** | CLI runs tools (Bash, Read, Edit, …) in a sandbox | You implement every tool yourself |
| **System prompt** | Claude Code's full agent system prompt included | You write your own |
| **Session persistence** | Built-in resume, continue, and fork | You manage conversation state |
| **Streaming** | NDJSON over subprocess pipes | SSE over HTTP |
| **Best for** | Orchestrating Claude Code agents, building dev tools, CI pipelines | Custom chatbots, fine-grained API control, non-CLI environments |

Use this SDK when you want Claude Code's agent capabilities (tool use, permissions, session management) without reimplementing them. Use the API directly when you need full control over the request/response cycle or are running in an environment where you can't spawn the CLI.

## Quick Start

```rust
use apiari_claude_sdk::{ClaudeClient, Event, ContentBlock, SessionOptions};

#[tokio::main]
async fn main() -> apiari_claude_sdk::Result<()> {
    let client = ClaudeClient::new();
    let mut session = client.spawn(SessionOptions {
        model: Some("sonnet".into()),
        allowed_tools: vec!["Bash".into(), "Read".into()],
        ..Default::default()
    }).await?;

    session.send_message("List files in the current directory").await?;

    while let Some(event) = session.next_event().await? {
        match event {
            Event::Assistant { message, tool_uses } => {
                for block in &message.message.content {
                    if let ContentBlock::Text { text } = block {
                        print!("{text}");
                    }
                }
                for tool in &tool_uses {
                    println!("\nTool call: {} ({})", tool.name, tool.id);
                }
            }
            Event::Result(_) => break,
            _ => {}
        }
    }
    Ok(())
}
```

## Features

- Spawn and manage multi-turn Claude Code sessions
- Typed event stream: `System`, `Assistant`, `Result`, `Stream`, `RateLimit`
- Tool use/result protocol for custom tool handling
- Session resume, continue, and fork
- Stream assembler for partial/incremental message events
- Interrupt (`SIGINT`) and graceful shutdown
- Full `SessionOptions` covering all CLI flags (model, budget, permissions, MCP, etc.)

## Protocol

The SDK spawns `claude` with these base flags:

```
claude --print --output-format stream-json --input-format stream-json --verbose
```

| Flag | Purpose |
|---|---|
| `--print` | Run non-interactively (no TUI) |
| `--output-format stream-json` | Emit NDJSON events on stdout |
| `--input-format stream-json` | Accept NDJSON messages on stdin |
| `--verbose` | Include system messages and full content blocks |

Additional flags from `SessionOptions` (model, tools, permissions, etc.) are appended automatically.

### Environment variable hygiene

The SDK removes two environment variables before spawning the subprocess:

- **`CLAUDECODE`** — The CLI sets this in child processes to detect nesting. If left in place, the spawned `claude` process would refuse to start, thinking it's already inside a Claude Code session. Removing it allows the SDK to launch Claude from within a Claude Code agent (e.g., a daemon or orchestrator that is itself a Claude Code tool).
- **`CLAUDE_CODE_ENTRYPOINT`** — Controls startup behavior. If set, the CLI may wait for an IPC handshake instead of streaming NDJSON. Removing it ensures the subprocess uses the expected streaming protocol.

## Requirements

The `claude` CLI must be installed and on `$PATH`. Alternatively, provide a custom path:

```rust
let client = ClaudeClient::with_cli_path("/usr/local/bin/claude");
```

## Ecosystem

`apiari-claude-sdk` is part of the [Apiari](https://github.com/ApiariTools/apiari) ecosystem — tools for building on top of Claude Code.

## License

[MIT](LICENSE)

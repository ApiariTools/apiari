# apiari-tui

Shared ratatui rendering primitives used by both `swarm` (live agent TUI) and `apiari` (worker
detail chat view). No binary — library crate only.

## Modules

| Module | Description |
|--------|-------------|
| `conversation` | Renders a `Vec<ConversationEntry>` into ratatui `Line`/`Span` lists with role-based styling |
| `events_parser` | Parses `.swarm/agents/{id}/events.jsonl` into `ConversationEntry` values |
| `markdown` | Converts Markdown text to styled ratatui spans (bold, italic, code, headings) |
| `scroll` | `ScrollState` helper — tracks offset, viewport height, and content height |
| `theme` | Color constants and `Style` helpers matching the apiari dark theme |

## Key Types

```rust
pub enum ConversationEntry {
    User { text: String, timestamp: String },
    // assistant, tool_use, tool_result variants
}
```

`render_conversation(&entries, width) -> Vec<Line>` — the main rendering function consumed by TUI widgets.

## Rules

- Pure rendering library — no async, no I/O, no DB.
- All public items re-exported from `lib.rs`.
- Depends only on `ratatui` and `serde_json` (for events parsing).

## Git Hooks

Run `git config core.hooksPath .githooks` to activate pre-commit checks (fmt + clippy).

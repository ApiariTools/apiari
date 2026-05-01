# apiari-tui

Shared TUI design system library for the [Apiari](https://github.com/ApiariTools) toolchain, built on [ratatui](https://ratatui.rs).

## Why

Apiari's tools — `swarm` (agent orchestrator) and `apiari` (CLI dashboard) — share terminal UIs that display agent conversations, tool calls, and status. `apiari-tui` extracts the common theme, rendering, and scroll logic so both tools look and behave consistently.

## Modules

| Module | Purpose |
|---|---|
| `theme` | Honey/amber color palette and pre-built ratatui `Style` helpers |
| `scroll` | Visual-line-aware scrollable text rendering with auto-scroll |
| `markdown` | Markdown-to-ratatui renderer (headings, lists, code blocks, tables, links) |
| `conversation` | Shared conversation types (`ConversationEntry`) and turn rendering |
| `events_parser` | Parse agent `events.jsonl` logs into `ConversationEntry` items |

## Theme

The color palette is built around warm honey and amber tones designed to pop against dark terminal backgrounds:

| Color | Name | Hex | Usage |
|---|---|---|---|
| `HONEY` | Warm amber | `#FFB74D` | Titles, accents, active borders |
| `GOLD` | Bright gold | `#FFD700` | Selections, input cursor |
| `NECTAR` | Deep orange | `#FF8A3D` | Warnings |
| `POLLEN` | Soft yellow | `#FAE68C` | Inline code, completed status |
| `WAX` | Dark warm gray | `#3C3830` | Borders, dividers |
| `COMB` | Darker bg | `#282520` | Highlights background |
| `SMOKE` | Muted text | `#8C877D` | Subtitles, timestamps |
| `ROYAL` | Purple accent | `#A078FF` | Agent labels, merged PRs |
| `MINT` | Green | `#64E6B4` | Success, running status |
| `EMBER` | Red | `#FF5A5A` | Errors, critical severity |
| `FROST` | Bright text | `#DCDCE1` | Primary body text |

Pre-built style functions like `theme::title()`, `theme::error()`, `theme::border_active()`, etc. compose these colors with appropriate modifiers (bold, italic) for common UI elements.

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
apiari-tui = "0.1"
```

### Example: applying the theme

```rust
use apiari_tui::theme;
use ratatui::widgets::{Block, Borders, Paragraph};

let block = Block::default()
    .title("Agents")
    .title_style(theme::title())
    .borders(Borders::ALL)
    .border_style(theme::border_active());

let paragraph = Paragraph::new("Hello from the hive")
    .style(theme::text())
    .block(block);
```

### Example: scrollable content

```rust
use apiari_tui::scroll::{ScrollState, render_scrollable};
use ratatui::text::Line;
use ratatui::widgets::Block;

let mut scroll = ScrollState::new(); // auto-scrolls to bottom
let lines: Vec<Line> = vec![/* ... */];

// In your render function:
render_scrollable(frame, area, lines, &scroll, Block::default());

// On key events:
scroll.scroll_up(3);   // disables auto-scroll
scroll.scroll_down(3); // re-enables auto-scroll at offset 0
```

## License

MIT

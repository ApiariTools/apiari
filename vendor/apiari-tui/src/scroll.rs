//! Visual-line-aware scrollable text rendering.
//!
//! Ratatui's `Paragraph::scroll()` counts **visual** lines (post-wrap), but most
//! callers only know **logical** line counts.  When `Wrap` is enabled, long lines
//! expand into multiple visual rows, so a naïve `total_logical - viewport` scroll
//! offset falls short of the true bottom.
//!
//! `ScrollState` + `render_scrollable()` solve this by computing visual line
//! counts the same way ratatui does (`ceil(line_width / viewport_width)`), keeping
//! auto-scroll pinned to the real bottom.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};

/// Scroll state for a scrollable text region.
///
/// `offset == 0` means "pinned to the bottom" (auto-scroll).
/// Scrolling up increases `offset`; scrolling back to 0 re-enables auto-scroll.
#[derive(Debug, Clone)]
pub struct ScrollState {
    /// Lines scrolled up from the bottom (0 = follow latest content).
    pub offset: u32,
    /// Whether we're auto-following new content.
    pub auto_scroll: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset: 0,
            auto_scroll: true,
        }
    }
}

impl ScrollState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scroll up (away from bottom). Disables auto-scroll.
    pub fn scroll_up(&mut self, amount: u32) {
        self.offset = self.offset.saturating_add(amount);
        self.auto_scroll = false;
    }

    /// Scroll down (toward bottom). Re-enables auto-scroll when reaching offset 0.
    pub fn scroll_down(&mut self, amount: u32) {
        self.offset = self.offset.saturating_sub(amount);
        if self.offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Jump to bottom and re-enable auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.offset = 0;
        self.auto_scroll = true;
    }
}

/// Count total visual lines for `lines` at viewport width `w`.
fn visual_line_count(lines: &[Line<'_>], w: usize) -> u32 {
    let mut total: u32 = 0;
    for line in lines {
        let lw = line.width();
        total += (lw.max(1).div_ceil(w)) as u32;
    }
    total
}

/// Render `lines` into `area` with visual-line-aware scrolling.
///
/// Handles both auto-scroll (pinned to bottom) and manual scroll modes.
/// Uses `Wrap { trim: false }` so long lines wrap naturally.
///
/// The `block` is rendered first; scrollable content fills its inner area.
/// Over-scroll is clamped — you can't scroll past the first line.
pub fn render_scrollable<'a>(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'a>>,
    scroll: &ScrollState,
    block: Block<'a>,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let w = inner.width.max(1) as usize;
    let visible_height = inner.height as u32;

    if scroll.auto_scroll {
        // Trim to a small tail so our visual-line estimate stays accurate
        // (drift accumulates over many lines with ratatui's word-wrapping).
        let keep_lines = (visible_height as usize) * 4 + 50;
        let display_lines = if lines.len() > keep_lines {
            &lines[lines.len() - keep_lines..]
        } else {
            &lines[..]
        };

        let tail_visual = visual_line_count(display_lines, w);
        let scroll_rows = tail_visual.saturating_sub(visible_height);

        let paragraph = Paragraph::new(Text::from(display_lines.to_vec()))
            .scroll((scroll_rows.min(u16::MAX as u32) as u16, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, inner);
    } else {
        // Manual scroll: use visual line count, clamp offset.
        let total_visual = visual_line_count(&lines, w);
        let max_offset = total_visual.saturating_sub(visible_height);
        let clamped_offset = scroll.offset.min(max_offset);

        let target_scroll = max_offset.saturating_sub(clamped_offset);

        // For large offsets, drop earlier lines to avoid perf issues.
        let (display_lines, effective_scroll) = if target_scroll > 500 {
            let buffer = visible_height.max(100);
            let drop_target = target_scroll.saturating_sub(buffer);
            let mut drop_count = 0usize;
            let mut dropped = 0u32;
            for line in lines.iter() {
                let lw = line.width();
                let vl = (lw.max(1).div_ceil(w)) as u32;
                if dropped + vl > drop_target {
                    break;
                }
                dropped += vl;
                drop_count += 1;
            }
            let adj = target_scroll - dropped;
            (
                Text::from(lines[drop_count..].to_vec()),
                adj.min(u16::MAX as u32) as u16,
            )
        } else {
            (Text::from(lines), target_scroll as u16)
        };

        let paragraph = Paragraph::new(display_lines)
            .scroll((effective_scroll, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, inner);
    }
}

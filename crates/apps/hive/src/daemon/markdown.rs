//! Sanitize Markdown for Telegram's MarkdownV2 renderer.
//!
//! Telegram chokes on many standard Markdown constructs (tables, `##` headers,
//! horizontal rules, GitHub-flavoured bold `**`). This module converts those
//! into plain text or Telegram-compatible equivalents while leaving code blocks
//! intact.

/// Sanitize a Markdown string for Telegram consumption.
///
/// Transformations (applied outside of fenced code blocks):
/// 1. Table separator lines (e.g. `|---|---|`) are stripped.
/// 2. Table rows have leading/trailing `|` removed; cells are joined with spaces.
/// 3. `## Header` lines become just `Header`.
/// 4. Horizontal rules (`---`, `***`, `___`) are removed.
/// 5. `**bold**` is converted to `*bold*` (Telegram bold).
/// 6. Fenced code blocks (triple backticks) are preserved as-is.
pub fn sanitize_for_telegram(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Toggle code-block state on triple-backtick fences.
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Inside a code block — pass through verbatim.
        if in_code_block {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Horizontal rules: lines that are only dashes, asterisks, or underscores
        // (at least 3), optionally with spaces.
        if is_horizontal_rule(trimmed) {
            // Drop the line entirely.
            continue;
        }

        // Table separator lines: composed solely of `|`, `-`, `:`, and spaces.
        if is_table_separator(trimmed) {
            continue;
        }

        // Table rows: contain `|` — strip outer pipes and collapse cells.
        if trimmed.contains('|') {
            let cleaned = clean_table_row(line);
            out.push_str(&cleaned);
            out.push('\n');
            continue;
        }

        // Strip leading `#` header markers.
        if trimmed.starts_with('#') {
            let stripped = strip_header(trimmed);
            out.push_str(stripped);
            out.push('\n');
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    // Convert **bold** → *bold* (Telegram MarkdownV2 bold).
    let out = convert_bold(&out);

    // Remove the trailing newline we always append.
    if out.ends_with('\n') && !text.ends_with('\n') {
        out[..out.len() - 1].to_owned()
    } else {
        out
    }
}

/// Returns `true` if the line is a Markdown horizontal rule.
fn is_horizontal_rule(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    // Must be made entirely of one repeated char (-, *, _) optionally with spaces.
    let base = trimmed.replace(' ', "");
    base.chars().all(|c| c == '-')
        || base.chars().all(|c| c == '*')
        || base.chars().all(|c| c == '_')
}

/// Returns `true` if the line is a Markdown table separator (e.g. `|---|:---:|`).
fn is_table_separator(trimmed: &str) -> bool {
    if !trimmed.contains('-') || !trimmed.contains('|') {
        return false;
    }
    trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Strip leading/trailing pipes from a table row, collapse cells with spaces.
fn clean_table_row(line: &str) -> String {
    let stripped = line.trim();
    // Remove leading and trailing pipes.
    let inner = stripped
        .strip_prefix('|')
        .unwrap_or(stripped)
        .strip_suffix('|')
        .unwrap_or(stripped);
    // Split on pipes and join with spaces.
    inner
        .split('|')
        .map(|cell| cell.trim())
        .collect::<Vec<_>>()
        .join("  ")
}

/// Strip leading `#` characters and the following space from a header line.
fn strip_header(trimmed: &str) -> &str {
    let without_hashes = trimmed.trim_start_matches('#');
    without_hashes.strip_prefix(' ').unwrap_or(without_hashes)
}

/// Convert `**bold**` to `*bold*` for Telegram.
fn convert_bold(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;
    let mut in_inline_code = false;

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Track code block state.
        if i + 2 < len && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            in_code_block = !in_code_block;
            result.push('`');
            result.push('`');
            result.push('`');
            i += 3;
            continue;
        }

        // Track inline code state (single backtick, only outside code blocks).
        if !in_code_block && chars[i] == '`' {
            in_inline_code = !in_inline_code;
            result.push('`');
            i += 1;
            continue;
        }

        // Only convert ** outside code.
        if !in_code_block
            && !in_inline_code
            && i + 1 < len
            && chars[i] == '*'
            && chars[i + 1] == '*'
        {
            result.push('*');
            i += 2;
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_table_separator() {
        let input = "| Name | Value |\n|------|-------|\n| foo  | bar   |";
        let result = sanitize_for_telegram(input);
        assert!(!result.contains("------"));
        assert!(result.contains("foo"));
    }

    #[test]
    fn converts_table_rows() {
        let input = "| Name | Value |\n|------|-------|\n| foo  | bar   |";
        let result = sanitize_for_telegram(input);
        // Table rows should have pipes removed.
        assert!(!result.contains('|'));
        assert!(result.contains("Name"));
        assert!(result.contains("Value"));
        assert!(result.contains("foo"));
        assert!(result.contains("bar"));
    }

    #[test]
    fn strips_headers() {
        assert_eq!(sanitize_for_telegram("## Summary"), "Summary");
        assert_eq!(sanitize_for_telegram("### Details"), "Details");
        assert_eq!(sanitize_for_telegram("# Title"), "Title");
    }

    #[test]
    fn removes_horizontal_rules() {
        let input = "above\n---\nbelow";
        let result = sanitize_for_telegram(input);
        assert_eq!(result, "above\nbelow");

        let input2 = "above\n***\nbelow";
        let result2 = sanitize_for_telegram(input2);
        assert_eq!(result2, "above\nbelow");

        let input3 = "above\n___\nbelow";
        let result3 = sanitize_for_telegram(input3);
        assert_eq!(result3, "above\nbelow");

        // With spaces.
        let input4 = "above\n- - -\nbelow";
        let result4 = sanitize_for_telegram(input4);
        assert_eq!(result4, "above\nbelow");
    }

    #[test]
    fn converts_bold() {
        let input = "This is **bold** text and **more bold**";
        let result = sanitize_for_telegram(input);
        assert_eq!(result, "This is *bold* text and *more bold*");
    }

    #[test]
    fn preserves_code_blocks() {
        let input = "```\n**not bold**\n| table | row |\n## header\n---\n```";
        let result = sanitize_for_telegram(input);
        // Everything inside the code block should be preserved.
        assert!(result.contains("**not bold**"));
        assert!(result.contains("| table | row |"));
        assert!(result.contains("## header"));
        assert!(result.contains("---"));
    }

    #[test]
    fn preserves_code_blocks_with_language() {
        let input = "before\n```rust\nfn main() {\n    println!(\"**hello**\");\n}\n```\nafter";
        let result = sanitize_for_telegram(input);
        assert!(result.contains("```rust"));
        assert!(result.contains("**hello**"));
        assert!(result.contains("fn main()"));
    }

    #[test]
    fn bold_inside_inline_code_untouched() {
        let input = "use `**bold**` syntax";
        let result = sanitize_for_telegram(input);
        assert!(result.contains("`**bold**`"));
    }

    #[test]
    fn full_example() {
        let input = "\
## Status Report

| Agent | Status | PR |
|-------|--------|----|
| hive-1 | done | #42 |
| hive-2 | running | — |

---

**Summary**: all good";

        let result = sanitize_for_telegram(input);

        // Header stripped.
        assert!(result.contains("Status Report"));
        assert!(!result.contains("## "));

        // Table separator gone.
        assert!(!result.contains("|----"));

        // Table rows cleaned.
        assert!(!result.contains('|'));
        assert!(result.contains("hive-1"));
        assert!(result.contains("done"));

        // HR removed.
        assert!(!result.contains("---"));

        // Bold converted.
        assert!(result.contains("*Summary*"));
        assert!(!result.contains("**Summary**"));
    }

    #[test]
    fn table_separator_with_colons() {
        let input = "| Left | Center | Right |\n|:-----|:------:|------:|\n| a | b | c |";
        let result = sanitize_for_telegram(input);
        assert!(!result.contains(":------:"));
        assert!(result.contains("a"));
    }

    #[test]
    fn empty_input() {
        assert_eq!(sanitize_for_telegram(""), "");
    }

    #[test]
    fn plain_text_unchanged() {
        let input = "Just some normal text\nwith multiple lines";
        assert_eq!(sanitize_for_telegram(input), input);
    }

    #[test]
    fn preserves_single_asterisks() {
        let input = "This is *italic* text";
        let result = sanitize_for_telegram(input);
        assert_eq!(result, "This is *italic* text");
    }

    #[test]
    fn header_without_space() {
        // Edge case: `##NoSpace` — still strip hashes.
        let result = sanitize_for_telegram("##NoSpace");
        assert_eq!(result, "NoSpace");
    }
}

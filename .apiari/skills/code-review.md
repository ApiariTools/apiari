# Code Review

Review the branch diff thoroughly and output a structured verdict.

## How to Review
Run `git diff main...<branch-name>` to see the changes.
Also read the changed files (not just the diff) to understand full context.

## Review Focus
- **Correctness**: Logic errors, off-by-one errors, incorrect assumptions?
- **Error handling**: Panics, unwraps, silent failures? Are Result types propagated?
- **Edge cases**: Empty strings, None values, missing files, concurrent access, malformed input?
- **Shell injection / security**: Are user strings safely escaped before shell commands or queries?
- **Data consistency**: New fields have `#[serde(default)]`? Deserialization breaks? Race conditions?
- **API consistency**: Follows existing patterns and conventions?
- **Test coverage**: Tests for new behavior? Edge cases and error paths covered, not just happy path?
- **Resource cleanup**: File handles, connections, spawned tasks properly cleaned up?

## Verdict Rules
- Correctness issues, shell injection risks, or data consistency problems → CHANGES_REQUESTED
- Tests only cover happy path, miss obvious edge cases → CHANGES_REQUESTED
- Code works but minor style issues → APPROVED (mention as comments)
- When in doubt, request changes. A false positive is better than a missed bug.

## Verdict Format
Output EXACTLY one of these as your final message:

If the branch looks good:
```
REVIEW_VERDICT: APPROVED
```

If changes are needed:
```
REVIEW_VERDICT: CHANGES_REQUESTED
- [file:line] description of issue
- [file:line] description of issue
```

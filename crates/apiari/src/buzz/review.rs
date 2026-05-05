//! Worker code review — one-shot ephemeral review sessions powered by the claude CLI.
//!
//! `run_review()` is the main entry point. It:
//! 1. Gets the diff for the worker's branch
//! 2. Reads optional repo context
//! 3. Runs the claude CLI with a structured review prompt
//! 4. Parses the JSON response
//! 5. Stores the review in the DB
//! 6. Optionally sends the worker_message via `swarm send`
//! 7. Emits a WebSocket event

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use color_eyre::eyre::{Result, WrapErr, eyre};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::buzz::worker::Worker;

// ── Domain types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    pub severity: String,
    pub file: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewVerdict::Approve => "approve",
            ReviewVerdict::RequestChanges => "request_changes",
            ReviewVerdict::Comment => "comment",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(ReviewVerdict::Approve),
            "request_changes" => Some(ReviewVerdict::RequestChanges),
            "comment" => Some(ReviewVerdict::Comment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerReview {
    pub id: i64,
    pub workspace: String,
    pub worker_id: String,
    pub reviewer: String,
    pub verdict: String,
    pub summary: String,
    pub issues: Vec<ReviewIssue>,
    pub worker_message: Option<String>,
    pub created_at: String,
}

// ── DB schema ──────────────────────────────────────────────────────────

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS worker_reviews (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          workspace TEXT NOT NULL,
          worker_id TEXT NOT NULL,
          reviewer TEXT NOT NULL DEFAULT 'General',
          verdict TEXT NOT NULL,
          summary TEXT NOT NULL,
          issues TEXT NOT NULL DEFAULT '[]',
          worker_message TEXT,
          created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .wrap_err("failed to create worker_reviews table")?;
    Ok(())
}

// ── Store ──────────────────────────────────────────────────────────────

pub struct ReviewStore {
    conn: Arc<Mutex<Connection>>,
}

impl ReviewStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        {
            let c = conn.lock().unwrap();
            ensure_schema(&c)?;
        }
        Ok(Self { conn })
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let conn = Arc::new(Mutex::new(conn));
        Self::new(conn)
    }

    pub fn insert(&self, review: &WorkerReview) -> Result<WorkerReview> {
        let issues_json = serde_json::to_string(&review.issues).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO worker_reviews
             (workspace, worker_id, reviewer, verdict, summary, issues, worker_message)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                review.workspace,
                review.worker_id,
                review.reviewer,
                review.verdict,
                review.summary,
                issues_json,
                review.worker_message,
            ],
        )
        .wrap_err("insert worker_review")?;
        let id = conn.last_insert_rowid();
        drop(conn);

        // Re-fetch the inserted row so we get the DB-generated created_at.
        let conn = self.conn.lock().unwrap();
        let row = conn.query_row(
            "SELECT id,workspace,worker_id,reviewer,verdict,summary,issues,worker_message,created_at
             FROM worker_reviews WHERE id=?1",
            params![id],
            row_to_review,
        )
        .wrap_err("fetch inserted review")?;
        Ok(row)
    }

    pub fn list_for_worker(&self, workspace: &str, worker_id: &str) -> Result<Vec<WorkerReview>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,workspace,worker_id,reviewer,verdict,summary,issues,worker_message,created_at
             FROM worker_reviews
             WHERE workspace=?1 AND worker_id=?2
             ORDER BY created_at DESC",
        )?;
        let reviews = stmt
            .query_map(params![workspace, worker_id], row_to_review)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("list worker reviews")?;
        Ok(reviews)
    }

    pub fn conn_arc(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
}

fn row_to_review(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkerReview> {
    let issues_json: String = row.get(6)?;
    let issues: Vec<ReviewIssue> = serde_json::from_str(&issues_json).unwrap_or_default();
    Ok(WorkerReview {
        id: row.get(0)?,
        workspace: row.get(1)?,
        worker_id: row.get(2)?,
        reviewer: row.get(3)?,
        verdict: row.get(4)?,
        summary: row.get(5)?,
        issues,
        worker_message: row.get(7)?,
        created_at: row.get(8)?,
    })
}

// ── Prompt ────────────────────────────────────────────────────────────

fn build_review_prompt(goal: &str, constraints: &str, context: &str, diff: &str) -> String {
    format!(
        r#"You are a code reviewer. Review the following changes made by an AI worker.

## Worker Goal
{goal}

## Constraints
{constraints}

## Repo Context
{context}

## Diff
```diff
{diff}
```

## Your Task

Review the diff against the goal. You have tool access — use Read and Bash to explore the repo if needed (e.g. to check tests, understand context, verify the change is complete).

When done, output your review as a JSON block (and nothing after it):

```json
{{
  "verdict": "approve" | "request_changes" | "comment",
  "summary": "One paragraph summary of the review.",
  "issues": [
    {{
      "severity": "blocking" | "suggestion" | "nitpick",
      "file": "path/to/file",
      "description": "What the issue is and why it matters."
    }}
  ],
  "worker_message": "The full message to send to the worker. Be specific and actionable. null if verdict is approve."
}}
```

Rules:
- verdict "approve": changes are good, no action needed
- verdict "request_changes": blocking issues exist, worker must fix before merge
- verdict "comment": non-blocking feedback, worker may address or ignore
- worker_message must be null if verdict is "approve"
- Be concise. Don't repeat the diff back. Focus on what matters."#
    )
}

// ── JSON parsing ──────────────────────────────────────────────────────

/// Parsed response from claude's review output.
#[derive(Debug, Deserialize)]
struct RawReviewResponse {
    verdict: String,
    summary: String,
    #[serde(default)]
    issues: Vec<ReviewIssue>,
    worker_message: Option<String>,
}

/// Extract and parse the last ```json ... ``` block (or bare `{...}` blob) from claude's output.
///
/// We use brace-balancing (not ``` delimiter scanning) to find the end of the JSON object,
/// because issue descriptions often contain triple-backtick code examples inside the JSON
/// strings which would cause naive ``` scanning to terminate early.
pub fn parse_review_json(
    output: &str,
) -> Result<(String, String, Vec<ReviewIssue>, Option<String>)> {
    // Find the last ```json opening, then brace-balance from the first `{` after it.
    // Fall back to brace-balancing the entire output if no code fence found.
    let search_from = if let Some(last) = output.rfind("```json") {
        last + 7
    } else {
        0
    };

    let slice = &output[search_from..];
    let json_str = if let Some(rel) = slice.find('{') {
        let from = &slice[rel..];
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escape = false;
        let mut end_idx = None;

        for (i, ch) in from.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' && in_string {
                escape = true;
                continue;
            }
            if ch == '"' {
                in_string = !in_string;
                continue;
            }
            if in_string {
                continue;
            }
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = Some(i + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }

        match end_idx {
            Some(end) => from[..end].trim().to_string(),
            None => return Err(eyre!("no valid JSON block found in review output")),
        }
    } else {
        return Err(eyre!("no JSON block found in review output"));
    };

    let parsed: RawReviewResponse =
        serde_json::from_str(&json_str).wrap_err("failed to parse review JSON")?;

    // Validate verdict.
    if ReviewVerdict::from_str(&parsed.verdict).is_none() {
        return Err(eyre!("unknown verdict: {}", parsed.verdict));
    }

    Ok((
        parsed.verdict,
        parsed.summary,
        parsed.issues,
        parsed.worker_message,
    ))
}

// ── Main entry point ──────────────────────────────────────────────────

/// Result of a completed review, including whether the worker message was delivered.
pub struct ReviewOutcome {
    pub review: WorkerReview,
    /// True when `swarm send` succeeded or the verdict was "approve" (no send needed).
    pub send_succeeded: bool,
}

/// Run a code review for the given worker. Returns a `ReviewOutcome`.
///
/// Steps:
/// 1. Get `git diff main...HEAD` in the worker's worktree.
/// 2. Read `.apiari/context.md` if it exists.
/// 3. Build and run the claude CLI review prompt.
/// 4. Parse the JSON block from the output.
/// 5. Store the review in the DB.
/// 6. If verdict != approve AND worker_message is set: run `swarm send`.
/// 7. Emit a WebSocket event.
pub async fn run_review(
    workspace: &str,
    worker: &Worker,
    workspace_root: &Path,
    conn: Arc<Mutex<Connection>>,
    event_tx: Option<broadcast::Sender<Value>>,
) -> Result<ReviewOutcome> {
    // ── 1. Determine worktree path ────────────────────────────────────

    let worktree_path = workspace_root.join(".swarm").join("wt").join(&worker.id);

    let worktree_path = if worktree_path.exists() {
        worktree_path
    } else {
        // Fall back to workspace root if worktree doesn't exist (e.g. testing).
        workspace_root.to_path_buf()
    };

    info!(
        "[review/{workspace}] running review for worker {} in {}",
        worker.id,
        worktree_path.display()
    );

    // ── 2. Get git diff ────────────────────────────────────────────────

    let diff_output = tokio::process::Command::new("git")
        .arg("diff")
        .arg("main...HEAD")
        .current_dir(&worktree_path)
        .output()
        .await;

    let diff = match diff_output {
        Ok(out) if out.status.success() || out.stderr.is_empty() => {
            let d = String::from_utf8_lossy(&out.stdout).to_string();
            if d.trim().is_empty() {
                "(no diff — branch may be up to date with main)".to_string()
            } else {
                d
            }
        }
        Ok(out) => {
            warn!(
                "[review/{workspace}] git diff failed for {}: {}",
                worker.id,
                String::from_utf8_lossy(&out.stderr)
            );
            "(git diff unavailable)".to_string()
        }
        Err(e) => {
            warn!("[review/{workspace}] failed to run git diff: {e}");
            "(git diff unavailable)".to_string()
        }
    };

    // ── 3. Read context.md if it exists ────────────────────────────────

    let context_path = workspace_root.join(".apiari").join("context.md");
    let context = if context_path.exists() {
        tokio::fs::read_to_string(&context_path)
            .await
            .unwrap_or_else(|_| "No context file found.".to_string())
    } else {
        "No context file found.".to_string()
    };

    // ── 4. Build prompt ────────────────────────────────────────────────

    let goal = worker.goal.as_deref().unwrap_or("(no goal specified)");

    // Extract constraints from brief if available.
    let constraints = worker
        .brief
        .as_ref()
        .and_then(|b| b.get("constraints"))
        .and_then(|c| {
            if let Some(arr) = c.as_array() {
                let items: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                if items.is_empty() {
                    None
                } else {
                    Some(items.join("\n- "))
                }
            } else {
                c.as_str().map(String::from)
            }
        })
        .map(|s| format!("- {s}"))
        .unwrap_or_else(|| "(none specified)".to_string());

    let prompt = build_review_prompt(goal, &constraints, &context, &diff);

    // ── 5. Run claude CLI ──────────────────────────────────────────────
    // Pipe prompt via stdin — the diff can be large and shell arg limits vary.

    use tokio::io::AsyncWriteExt as _;

    let mut child = tokio::process::Command::new("claude")
        .arg("--print")
        .arg("--max-turns")
        .arg("10")
        .arg("--allowedTools")
        .arg("Read,Bash")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(&worktree_path)
        .spawn()
        .map_err(|e| eyre!("failed to spawn claude: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| eyre!("failed to write prompt to claude stdin: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| eyre!("failed to wait for claude: {e}"))?;

    let raw_output = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        warn!(
            "[review/{workspace}] claude exited non-zero for worker {}: {stderr}",
            worker.id
        );
        return Err(eyre!("claude exited non-zero: {stderr}"));
    };

    // ── 6. Parse JSON ──────────────────────────────────────────────────

    info!(
        "[review/{workspace}] claude output ({} chars): {}",
        raw_output.len(),
        raw_output.chars().take(500).collect::<String>()
    );

    let (verdict, summary, issues, worker_message) = parse_review_json(&raw_output)
        .map_err(|e| {
            warn!(
                "[review/{workspace}] parse failed for {}: {e}\nFull output:\n{raw_output}",
                worker.id
            );
            e
        })
        .wrap_err("failed to parse review output")?;

    // ── 7. Store in DB ─────────────────────────────────────────────────

    let store = ReviewStore::new(Arc::clone(&conn))?;
    let review = store.insert(&WorkerReview {
        id: 0, // will be set by DB
        workspace: workspace.to_string(),
        worker_id: worker.id.clone(),
        reviewer: "General".to_string(),
        verdict: verdict.clone(),
        summary,
        issues,
        worker_message: worker_message.clone(),
        created_at: String::new(), // will be set by DB
    })?;

    info!(
        "[review/{workspace}] stored review {} for worker {} verdict={verdict}",
        review.id, worker.id
    );

    // ── 8. Send worker_message if needed ───────────────────────────────

    let mut send_succeeded = true; // assume success; only relevant when send is attempted

    if verdict != "approve"
        && let Some(ref msg) = worker_message
    {
        let send_output = tokio::process::Command::new("swarm")
            .arg("--dir")
            .arg(workspace_root)
            .arg("send")
            .arg(&worker.id)
            .arg(msg)
            .output()
            .await;

        match send_output {
            Ok(out) if out.status.success() => {
                info!("[review/{workspace}] sent worker message to {}", worker.id);
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!(
                    "[review/{workspace}] swarm send failed for {}: {stderr}",
                    worker.id
                );
                send_succeeded = false;
            }
            Err(e) => {
                warn!("[review/{workspace}] failed to run swarm send: {e}");
                send_succeeded = false;
            }
        }
    }

    // ── 9. Emit WebSocket event ────────────────────────────────────────

    if let Some(tx) = event_tx {
        let event = serde_json::json!({
            "type": "worker_review",
            "workspace": workspace,
            "worker_id": worker.id,
            "verdict": review.verdict,
            "reviewer": review.reviewer,
        });
        let _ = tx.send(event);
    }

    Ok(ReviewOutcome {
        review,
        send_succeeded,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_worker(id: &str, workspace: &str) -> Worker {
        let now = Utc::now().to_rfc3339();
        Worker {
            id: id.to_string(),
            workspace: workspace.to_string(),
            state: crate::buzz::worker::WorkerState::Waiting,
            brief: None,
            repo: None,
            branch: Some("swarm/fix-auth".to_string()),
            goal: Some("Add rate limiting".to_string()),
            tests_passing: true,
            branch_ready: true,
            pr_url: None,
            pr_approved: false,
            is_stalled: false,
            revision_count: 0,
            review_mode: "local_first".to_string(),
            blocked_reason: None,
            last_output_at: None,
            state_entered_at: now.clone(),
            created_at: now.clone(),
            updated_at: now,
            label: String::new(),
        }
    }

    #[test]
    fn parse_review_json_valid() {
        let output = r#"
Here is my review:

```json
{
  "verdict": "request_changes",
  "summary": "The implementation is missing error handling.",
  "issues": [
    {
      "severity": "blocking",
      "file": "src/main.rs",
      "description": "Unwrap on line 42 will panic in production."
    }
  ],
  "worker_message": "Please add proper error handling to src/main.rs line 42."
}
```
"#;
        let (verdict, summary, issues, msg) = parse_review_json(output).unwrap();
        assert_eq!(verdict, "request_changes");
        assert!(summary.contains("error handling"));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, "blocking");
        assert_eq!(issues[0].file, "src/main.rs");
        assert!(msg.is_some());
    }

    #[test]
    fn parse_review_json_approve_no_message() {
        let output = r#"
```json
{
  "verdict": "approve",
  "summary": "LGTM. Changes look correct and complete.",
  "issues": [],
  "worker_message": null
}
```
"#;
        let (verdict, summary, issues, msg) = parse_review_json(output).unwrap();
        assert_eq!(verdict, "approve");
        assert!(!summary.is_empty());
        assert!(issues.is_empty());
        assert!(msg.is_none());
    }

    #[test]
    fn parse_review_json_missing_block() {
        let output = "The changes look fine to me. No issues found.";
        let result = parse_review_json(output);
        assert!(result.is_err());
    }

    #[test]
    fn parse_review_json_code_in_description() {
        // The issue description contains a triple-backtick code block inside the JSON string.
        // Naive ``` scanning would terminate the block early; brace-balancing must handle it.
        let output = r#"
Analysis: JSX comments are stripped at compile time.

```json
{
  "verdict": "request_changes",
  "summary": "Wrong comment syntax used.",
  "issues": [
    {
      "severity": "blocking",
      "file": "src/Foo.tsx",
      "description": "Replace with:\n```jsx\ndangerouslySetInnerHTML={{ __html: '<!-- test -->' }}\n```\nThis inserts real HTML."
    }
  ],
  "worker_message": "Fix the comment."
}
```
"#;
        let (verdict, _summary, issues, msg) = parse_review_json(output).unwrap();
        assert_eq!(verdict, "request_changes");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].description.contains("dangerouslySetInnerHTML"));
        assert!(msg.is_some());
    }

    #[test]
    fn parse_review_json_bare_json() {
        let output = r#"{"verdict": "comment", "summary": "Minor nit.", "issues": [], "worker_message": null}"#;
        let (verdict, _summary, _issues, _msg) = parse_review_json(output).unwrap();
        assert_eq!(verdict, "comment");
    }

    #[test]
    fn review_store_insert_and_list() {
        let store = ReviewStore::open_memory().unwrap();
        let w = make_worker("w1", "acme");

        let review = WorkerReview {
            id: 0,
            workspace: "acme".to_string(),
            worker_id: w.id.clone(),
            reviewer: "General".to_string(),
            verdict: "approve".to_string(),
            summary: "Looks good.".to_string(),
            issues: vec![],
            worker_message: None,
            created_at: String::new(),
        };

        let inserted = store.insert(&review).unwrap();
        assert!(inserted.id > 0);
        assert_eq!(inserted.verdict, "approve");
        assert!(!inserted.created_at.is_empty());

        let list = store.list_for_worker("acme", "w1").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, inserted.id);
    }

    #[test]
    fn review_store_list_empty() {
        let store = ReviewStore::open_memory().unwrap();
        let list = store.list_for_worker("acme", "w1").unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn review_store_insert_with_issues() {
        let store = ReviewStore::open_memory().unwrap();
        let review = WorkerReview {
            id: 0,
            workspace: "acme".to_string(),
            worker_id: "w2".to_string(),
            reviewer: "General".to_string(),
            verdict: "request_changes".to_string(),
            summary: "Blocking issue found.".to_string(),
            issues: vec![ReviewIssue {
                severity: "blocking".to_string(),
                file: "src/lib.rs".to_string(),
                description: "Missing error handling.".to_string(),
            }],
            worker_message: Some("Fix the error handling.".to_string()),
            created_at: String::new(),
        };

        let inserted = store.insert(&review).unwrap();
        assert_eq!(inserted.issues.len(), 1);
        assert_eq!(inserted.issues[0].severity, "blocking");

        let list = store.list_for_worker("acme", "w2").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].worker_message.as_deref(),
            Some("Fix the error handling.")
        );
    }
}

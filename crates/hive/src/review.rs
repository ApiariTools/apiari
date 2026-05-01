//! Local review system — state machine, comments storage, config parsing, and API endpoints.

use crate::db::Db;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, patch, post},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};

// ── Config types (parsed from workspace TOML) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_pr: bool,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub reviewers: Vec<ReviewerConfig>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_pr: true,
            sequential: false,
            reviewers: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerConfig {
    pub name: String,
    #[serde(rename = "type", default = "default_reviewer_type")]
    pub reviewer_type: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_reviewer_type() -> String {
    "bot".to_string()
}

// ── DB schema ──

const REVIEW_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS review_state (
        workspace TEXT NOT NULL,
        worker_id TEXT NOT NULL,
        state TEXT NOT NULL DEFAULT 'review_pending',
        round INTEGER DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        PRIMARY KEY (workspace, worker_id)
    );

    CREATE TABLE IF NOT EXISTS review_comments (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        workspace TEXT NOT NULL,
        worker_id TEXT NOT NULL,
        reviewer TEXT NOT NULL,
        reviewer_type TEXT NOT NULL,
        file TEXT NOT NULL,
        line INTEGER,
        body TEXT NOT NULL,
        status TEXT DEFAULT 'open',
        round INTEGER DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        resolved_at TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_review_comments_worker
        ON review_comments(workspace, worker_id, round);

    CREATE TABLE IF NOT EXISTS review_verdicts (
        workspace TEXT NOT NULL,
        worker_id TEXT NOT NULL,
        reviewer TEXT NOT NULL,
        round INTEGER NOT NULL,
        verdict TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        PRIMARY KEY (workspace, worker_id, reviewer, round)
    );
";

pub fn ensure_schema(db: &Db) {
    db.execute_batch(REVIEW_SCHEMA).ok();
}

// ── Response types ──

#[derive(Debug, Serialize)]
pub struct ReviewStatus {
    pub state: String,
    pub round: i32,
    pub reviewers: Vec<ReviewerStatus>,
    pub config: Option<ReviewConfig>,
}

#[derive(Debug, Serialize)]
pub struct ReviewerStatus {
    pub name: String,
    pub reviewer_type: String,
    pub required: bool,
    pub verdict: String,
    pub comment_count: i32,
}

#[derive(Debug, Serialize, Clone)]
pub struct ReviewComment {
    pub id: i64,
    pub reviewer: String,
    pub reviewer_type: String,
    pub file: String,
    pub line: Option<i32>,
    pub body: String,
    pub status: String,
    pub round: i32,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ReviewState {
    pub workspace: String,
    pub worker_id: String,
    pub state: String,
    pub round: i32,
    pub created_at: String,
    pub updated_at: String,
}

// ── DB helpers ──

pub fn get_review_state(db: &Db, workspace: &str, worker_id: &str) -> Option<ReviewState> {
    let conn = db.reader().ok()?;
    conn.query_row(
        "SELECT workspace, worker_id, state, round, created_at, updated_at
         FROM review_state WHERE workspace = ?1 AND worker_id = ?2",
        params![workspace, worker_id],
        |row| {
            Ok(ReviewState {
                workspace: row.get(0)?,
                worker_id: row.get(1)?,
                state: row.get(2)?,
                round: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .ok()
}

pub fn set_review_state(db: &Db, workspace: &str, worker_id: &str, state: &str, round: i32) {
    let round_str = round.to_string();
    let _ = db.execute_sql(
        "INSERT INTO review_state (workspace, worker_id, state, round, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         ON CONFLICT(workspace, worker_id) DO UPDATE SET
           state = ?3, round = ?4, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        &[workspace, worker_id, state, &round_str],
    );
}

pub fn add_review_comment(
    db: &Db,
    workspace: &str,
    worker_id: &str,
    reviewer: &str,
    reviewer_type: &str,
    file: &str,
    line: Option<i32>,
    body: &str,
    round: i32,
) -> Option<i64> {
    let line_val: Box<dyn rusqlite::types::ToSql> = match line {
        Some(l) => Box::new(l),
        None => Box::new(rusqlite::types::Null),
    };
    db.insert_returning_id(
        "INSERT INTO review_comments (workspace, worker_id, reviewer, reviewer_type, file, line, body, round)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        &[
            &workspace as &dyn rusqlite::types::ToSql,
            &worker_id,
            &reviewer,
            &reviewer_type,
            &file,
            &*line_val,
            &body,
            &round,
        ],
    )
    .ok()
}

pub fn list_review_comments(
    db: &Db,
    workspace: &str,
    worker_id: &str,
    round: Option<i32>,
    status: Option<&str>,
) -> Vec<ReviewComment> {
    let conn = match db.reader() {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut sql = String::from(
        "SELECT id, reviewer, reviewer_type, file, line, body, status, round, created_at, resolved_at
         FROM review_comments WHERE workspace = ?1 AND worker_id = ?2",
    );
    let mut param_values: Vec<String> = vec![workspace.to_string(), worker_id.to_string()];

    if let Some(r) = round {
        param_values.push(r.to_string());
        sql.push_str(&format!(" AND round = ?{}", param_values.len()));
    }
    if let Some(s) = status {
        param_values.push(s.to_string());
        sql.push_str(&format!(" AND status = ?{}", param_values.len()));
    }
    sql.push_str(" ORDER BY id ASC");

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(ReviewComment {
                id: row.get(0)?,
                reviewer: row.get(1)?,
                reviewer_type: row.get(2)?,
                file: row.get(3)?,
                line: row.get(4)?,
                body: row.get(5)?,
                status: row.get(6)?,
                round: row.get(7)?,
                created_at: row.get(8)?,
                resolved_at: row.get(9)?,
            })
        })
        .ok();

    match rows {
        Some(r) => r.filter_map(|r| r.ok()).collect(),
        None => vec![],
    }
}

pub fn update_comment_status(db: &Db, comment_id: i64, status: &str) {
    if status == "resolved" {
        let id_str = comment_id.to_string();
        let _ = db.execute_sql(
            "UPDATE review_comments SET status = ?1, resolved_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?2",
            &[status, &id_str],
        );
    } else {
        let id_str = comment_id.to_string();
        let _ = db.execute_sql(
            "UPDATE review_comments SET status = ?1, resolved_at = NULL WHERE id = ?2",
            &[status, &id_str],
        );
    }
}

#[allow(dead_code)]
pub fn delete_review_state(db: &Db, workspace: &str, worker_id: &str) {
    let _ = db.execute_sql(
        "DELETE FROM review_state WHERE workspace = ?1 AND worker_id = ?2",
        &[workspace, worker_id],
    );
}

pub fn set_verdict(
    db: &Db,
    workspace: &str,
    worker_id: &str,
    reviewer: &str,
    round: i32,
    verdict: &str,
) {
    let round_str = round.to_string();
    let _ = db.execute_sql(
        "INSERT INTO review_verdicts (workspace, worker_id, reviewer, round, verdict)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(workspace, worker_id, reviewer, round) DO UPDATE SET
           verdict = ?5, created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        &[workspace, worker_id, reviewer, &round_str, verdict],
    );
}

pub fn get_verdicts(
    db: &Db,
    workspace: &str,
    worker_id: &str,
    round: i32,
) -> Vec<(String, String)> {
    let conn = match db.reader() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT reviewer, verdict FROM review_verdicts
         WHERE workspace = ?1 AND worker_id = ?2 AND round = ?3",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(params![workspace, worker_id, round], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
    .ok()
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

fn get_comment_count_for_reviewer(
    db: &Db,
    workspace: &str,
    worker_id: &str,
    reviewer: &str,
    round: i32,
) -> i32 {
    let conn = match db.reader() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    conn.query_row(
        "SELECT COUNT(*) FROM review_comments
         WHERE workspace = ?1 AND worker_id = ?2 AND reviewer = ?3 AND round = ?4",
        params![workspace, worker_id, reviewer, round],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

// ── Config loading helper ──

/// Parse [review] section from workspace TOML. Returns default (disabled) if absent.
pub fn parse_review_config(config_path: &std::path::Path) -> ReviewConfig {
    #[derive(Deserialize, Default)]
    struct FullConfig {
        review: Option<ReviewConfig>,
    }

    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|c| toml::from_str::<FullConfig>(&c).ok())
        .and_then(|c| c.review)
        .unwrap_or_default()
}

// ── Helpers ──

fn validate_workspace_name(name: &str) -> Result<(), StatusCode> {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn get_comment_by_id(db: &Db, comment_id: i64) -> Option<ReviewComment> {
    let conn = db.reader().ok()?;
    conn.query_row(
        "SELECT id, reviewer, reviewer_type, file, line, body, status, round, created_at, resolved_at
         FROM review_comments WHERE id = ?1",
        params![comment_id],
        |row| {
            Ok(ReviewComment {
                id: row.get(0)?,
                reviewer: row.get(1)?,
                reviewer_type: row.get(2)?,
                file: row.get(3)?,
                line: row.get(4)?,
                body: row.get(5)?,
                status: row.get(6)?,
                round: row.get(7)?,
                created_at: row.get(8)?,
                resolved_at: row.get(9)?,
            })
        },
    )
    .ok()
}

fn comment_belongs_to_worker(db: &Db, comment_id: i64, workspace: &str, worker_id: &str) -> bool {
    let conn = match db.reader() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT 1 FROM review_comments WHERE id = ?1 AND workspace = ?2 AND worker_id = ?3",
        params![comment_id, workspace, worker_id],
        |_| Ok(()),
    )
    .is_ok()
}

// ── API route handlers ──

use crate::routes::AppState;

pub fn review_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/review",
            get(get_review_status),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/review/comments",
            get(get_review_comments).post(post_review_comment),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/review/comments/{comment_id}",
            patch(patch_comment),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/review/approve",
            post(approve_review),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/review/request-changes",
            post(request_changes),
        )
}

async fn get_review_status(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Result<Json<ReviewStatus>, StatusCode> {
    validate_workspace_name(&workspace)?;
    let review_state = get_review_state(&state.db, &workspace, &worker_id);

    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let config = parse_review_config(&config_path);

    let (current_state, round) = match &review_state {
        Some(rs) => (rs.state.clone(), rs.round),
        None => ("none".to_string(), 1),
    };

    let verdicts = get_verdicts(&state.db, &workspace, &worker_id, round);

    let reviewers: Vec<ReviewerStatus> = config
        .reviewers
        .iter()
        .map(|rc| {
            let verdict = verdicts
                .iter()
                .find(|(name, _)| name == &rc.name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "pending".to_string());
            let comment_count =
                get_comment_count_for_reviewer(&state.db, &workspace, &worker_id, &rc.name, round);
            ReviewerStatus {
                name: rc.name.clone(),
                reviewer_type: rc.reviewer_type.clone(),
                required: rc.required,
                verdict,
                comment_count,
            }
        })
        .collect();

    Ok(Json(ReviewStatus {
        state: current_state,
        round,
        reviewers,
        config: Some(config),
    }))
}

#[derive(Deserialize)]
struct CommentsQuery {
    round: Option<i32>,
    status: Option<String>,
}

async fn get_review_comments(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
    Query(params): Query<CommentsQuery>,
) -> Json<Vec<ReviewComment>> {
    let comments = list_review_comments(
        &state.db,
        &workspace,
        &worker_id,
        params.round,
        params.status.as_deref(),
    );
    Json(comments)
}

#[derive(Deserialize)]
struct PostCommentRequest {
    file: String,
    line: Option<i32>,
    body: String,
}

async fn post_review_comment(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
    Json(body): Json<PostCommentRequest>,
) -> Result<Json<ReviewComment>, StatusCode> {
    let review_state = get_review_state(&state.db, &workspace, &worker_id);
    let round = review_state.map(|rs| rs.round).unwrap_or(1);

    let id = add_review_comment(
        &state.db, &workspace, &worker_id, "human", "human", &body.file, body.line, &body.body,
        round,
    )
    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let comment = get_comment_by_id(&state.db, id).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(comment))
}

#[derive(Deserialize)]
struct PatchCommentRequest {
    status: String,
}

async fn patch_comment(
    State(state): State<AppState>,
    Path((workspace, worker_id, comment_id)): Path<(String, String, i64)>,
    Json(body): Json<PatchCommentRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let valid_statuses = ["resolved", "open", "wont_fix"];
    if !valid_statuses.contains(&body.status.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !comment_belongs_to_worker(&state.db, comment_id, &workspace, &worker_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    update_comment_status(&state.db, comment_id, &body.status);
    Ok(Json(
        serde_json::json!({ "ok": true, "status": body.status }),
    ))
}

async fn approve_review(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    validate_workspace_name(&workspace)?;
    let review_state = get_review_state(&state.db, &workspace, &worker_id);
    let round = review_state.as_ref().map(|rs| rs.round).unwrap_or(1);

    // Record human approval
    set_verdict(
        &state.db, &workspace, &worker_id, "human", round, "approved",
    );

    // Check if all required reviewers have approved
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let config = parse_review_config(&config_path);

    // Re-read verdicts after recording the human approval
    let verdicts = get_verdicts(&state.db, &workspace, &worker_id, round);
    let all_required_approved = config.reviewers.iter().filter(|r| r.required).all(|r| {
        verdicts
            .iter()
            .any(|(name, v)| name == &r.name && v == "approved")
    });

    let new_state = if config.reviewers.is_empty() || all_required_approved {
        "approved"
    } else {
        // Not all required reviewers approved yet; keep current state
        review_state
            .as_ref()
            .map(|rs| rs.state.as_str())
            .unwrap_or("review_pending")
    };

    set_review_state(&state.db, &workspace, &worker_id, new_state, round);

    Ok(Json(serde_json::json!({ "ok": true, "state": new_state })))
}

async fn request_changes(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let review_state = get_review_state(&state.db, &workspace, &worker_id);
    let round = review_state.map(|rs| rs.round).unwrap_or(1);

    set_verdict(
        &state.db,
        &workspace,
        &worker_id,
        "human",
        round,
        "changes_requested",
    );
    set_review_state(
        &state.db,
        &workspace,
        &worker_id,
        "changes_requested",
        round,
    );

    Json(serde_json::json!({ "ok": true, "state": "changes_requested" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_db() -> (Db, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);
        (db, dir)
    }

    #[test]
    fn test_review_state_crud() {
        let (db, _dir) = test_db();

        // Initially no state
        assert!(get_review_state(&db, "ws", "w1").is_none());

        // Set state
        set_review_state(&db, "ws", "w1", "review_pending", 1);
        let state = get_review_state(&db, "ws", "w1").unwrap();
        assert_eq!(state.state, "review_pending");
        assert_eq!(state.round, 1);

        // Update state
        set_review_state(&db, "ws", "w1", "approved", 2);
        let state = get_review_state(&db, "ws", "w1").unwrap();
        assert_eq!(state.state, "approved");
        assert_eq!(state.round, 2);

        // Delete
        delete_review_state(&db, "ws", "w1");
        assert!(get_review_state(&db, "ws", "w1").is_none());
    }

    #[test]
    fn test_review_comments() {
        let (db, _dir) = test_db();

        let id = add_review_comment(
            &db,
            "ws",
            "w1",
            "alice",
            "human",
            "src/main.rs",
            Some(42),
            "fix this",
            1,
        );
        assert!(id.is_some());

        let comments = list_review_comments(&db, "ws", "w1", Some(1), None);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].file, "src/main.rs");
        assert_eq!(comments[0].line, Some(42));
        assert_eq!(comments[0].body, "fix this");
        assert_eq!(comments[0].status, "open");

        // Filter by status
        let open = list_review_comments(&db, "ws", "w1", None, Some("open"));
        assert_eq!(open.len(), 1);
        let resolved = list_review_comments(&db, "ws", "w1", None, Some("resolved"));
        assert_eq!(resolved.len(), 0);

        // Update status
        update_comment_status(&db, comments[0].id, "resolved");
        let resolved = list_review_comments(&db, "ws", "w1", None, Some("resolved"));
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn test_verdicts() {
        let (db, _dir) = test_db();

        set_verdict(&db, "ws", "w1", "alice", 1, "approved");
        set_verdict(&db, "ws", "w1", "bob", 1, "changes_requested");

        let verdicts = get_verdicts(&db, "ws", "w1", 1);
        assert_eq!(verdicts.len(), 2);

        // Update verdict
        set_verdict(&db, "ws", "w1", "bob", 1, "approved");
        let verdicts = get_verdicts(&db, "ws", "w1", 1);
        let bob_verdict = verdicts.iter().find(|(n, _)| n == "bob").unwrap();
        assert_eq!(bob_verdict.1, "approved");
    }

    #[test]
    fn test_parse_review_config() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test.toml");

        // No review section — defaults to disabled
        std::fs::write(&config_path, "[workspace]\nname = \"test\"\n").unwrap();
        let config = parse_review_config(&config_path);
        assert!(!config.enabled);
        assert!(config.auto_pr);
        assert!(config.reviewers.is_empty());

        // With review section
        std::fs::write(
            &config_path,
            r#"
[workspace]
name = "test"

[review]
enabled = true
auto_pr = false
sequential = true

[[review.reviewers]]
name = "security-bot"
type = "bot"
prompt = "Review for security issues"
required = true

[[review.reviewers]]
name = "josh"
type = "human"
required = false
"#,
        )
        .unwrap();
        let config = parse_review_config(&config_path);
        assert!(config.enabled);
        assert!(!config.auto_pr);
        assert!(config.sequential);
        assert_eq!(config.reviewers.len(), 2);
        assert_eq!(config.reviewers[0].name, "security-bot");
        assert_eq!(config.reviewers[0].reviewer_type, "bot");
        assert_eq!(
            config.reviewers[0].prompt.as_deref(),
            Some("Review for security issues")
        );
        assert!(config.reviewers[0].required);
        assert_eq!(config.reviewers[1].name, "josh");
        assert_eq!(config.reviewers[1].reviewer_type, "human");
        assert!(!config.reviewers[1].required);
    }

    #[test]
    fn test_comment_without_line() {
        let (db, _dir) = test_db();

        let id = add_review_comment(
            &db,
            "ws",
            "w1",
            "bob",
            "bot",
            "README.md",
            None,
            "general comment",
            1,
        );
        assert!(id.is_some());

        let comments = list_review_comments(&db, "ws", "w1", None, None);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, None);
    }
}

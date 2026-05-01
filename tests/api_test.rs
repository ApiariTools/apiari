use apiari_hive::db::Db;
use apiari_hive::events::EventHub;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tempfile::tempdir;
use tower::ServiceExt;

fn test_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();

    let ws_root = dir.path().join("workspace");
    std::fs::create_dir_all(&ws_root).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        format!("[workspace]\nname = \"test\"\nroot = \"{}\"\n\n[[bots]]\nname = \"Customer\"\ncolor = \"#e85555\"\nrole = \"Test bot\"\n", ws_root.display()),
    )
    .unwrap();

    let db = Db::open(&config_dir.join("hive.db")).unwrap();
    let events = EventHub::new();
    let app = apiari_hive::routes::router(
        db,
        &config_dir,
        events,
        Default::default(),
        Default::default(),
        apiari_hive::remote::new_registry(),
    );
    (app, dir)
}

fn test_app_with_tts_url(tts_base_url: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();

    let ws_root = dir.path().join("workspace");
    std::fs::create_dir_all(&ws_root).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        format!("[workspace]\nname = \"test\"\nroot = \"{}\"\n\n[[bots]]\nname = \"Customer\"\ncolor = \"#e85555\"\nrole = \"Test bot\"\n", ws_root.display()),
    )
    .unwrap();

    let db = Db::open(&config_dir.join("hive.db")).unwrap();
    let events = EventHub::new();
    let app = apiari_hive::routes::router_with_http_client(
        db,
        &config_dir,
        events,
        Default::default(),
        Default::default(),
        reqwest::Client::new(),
        tts_base_url.to_string(),
        "http://127.0.0.1:4202".to_string(),
        apiari_hive::remote::new_registry(),
    );
    (app, dir)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String) {
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn post_json(app: &axum::Router, path: &str, json: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(json.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn test_list_workspaces() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["name"], "test");
    // tts_voice not set in default test config — should be omitted
    assert!(parsed[0].get("tts_voice").is_none());
}

#[tokio::test]
async fn test_list_workspaces_tts_voice() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();

    let ws_root = dir.path().join("workspace");
    std::fs::create_dir_all(&ws_root).unwrap();
    std::fs::write(
        config_dir.join("workspaces/voice.toml"),
        format!(
            "[workspace]\nname = \"voice\"\nroot = \"{}\"\ntts_voice = \"am_echo\"\n",
            ws_root.display()
        ),
    )
    .unwrap();

    let db = apiari_hive::db::Db::open(&config_dir.join("hive.db")).unwrap();
    let events = apiari_hive::events::EventHub::new();
    let app = apiari_hive::routes::router(
        db,
        &config_dir,
        events,
        Default::default(),
        Default::default(),
        apiari_hive::remote::new_registry(),
    );

    let (status, body) = get(&app, "/api/workspaces").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["name"], "voice");
    assert_eq!(parsed[0]["tts_voice"], "am_echo");
}

#[tokio::test]
async fn test_list_bots() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces/test/bots").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0]["name"], "Main");
    assert_eq!(parsed[1]["name"], "Customer");
}

#[tokio::test]
async fn test_list_bots_unknown_workspace() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/nonexistent/bots").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 1); // Just Main default
}

#[tokio::test]
async fn test_conversations_empty() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(parsed.is_empty());
}

#[tokio::test]
async fn test_bot_status_default_idle() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/bots/Main/status").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "idle");
}

#[tokio::test]
async fn test_unread_empty() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/unread").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(parsed.as_object().unwrap().is_empty());
}

#[tokio::test]
async fn test_mark_seen() {
    let (app, _dir) = test_app();
    let (status, body) = post_json(&app, "/api/workspaces/test/seen/Main", "").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[tokio::test]
async fn test_cancel_bot() {
    let (app, _dir) = test_app();
    let (status, body) = post_json(&app, "/api/workspaces/test/bots/Main/cancel", "").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[tokio::test]
async fn test_workers_empty() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/workers").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(parsed.is_empty());
}

#[tokio::test]
async fn test_repos() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces/test/repos").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(parsed.is_empty()); // temp dir has no git repos
}

#[tokio::test]
async fn test_worker_detail_not_found() {
    let (app, _dir) = test_app();
    let (status, _) = get(&app, "/api/workspaces/test/workers/nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_worker_diff_no_swarm() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces/test/workers/nonexistent/diff").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(parsed["diff"].is_null());
}

#[tokio::test]
async fn test_worker_diff_rejects_invalid_name() {
    let (app, _dir) = test_app();
    let (status, _) = get(&app, "/api/workspaces/bad.name/workers/x/diff").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_send_message_stores_user_msg() {
    let (app, _dir) = test_app();
    let (status, _) = post_json(
        &app,
        "/api/workspaces/test/chat/Main",
        r#"{"message":"hello"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Give the background task a moment
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(
        parsed
            .iter()
            .any(|m| m["content"] == "hello" && m["role"] == "user")
    );
}

#[tokio::test]
async fn test_tts_missing_text() {
    let (app, _dir) = test_app();
    let (status, body) = post_json(&app, "/api/tts", r#"{"text":""}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Missing text"));
}

#[tokio::test]
async fn test_tts_no_text_field() {
    let (app, _dir) = test_app();
    let (status, body) = post_json(&app, "/api/tts", r#"{}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Missing text"));
}

#[tokio::test]
async fn test_tts_text_too_long() {
    let (app, _dir) = test_app();
    let long_text = "a".repeat(5001);
    let payload = format!(r#"{{"text":"{}"}}"#, long_text);
    let (status, body) = post_json(&app, "/api/tts", &payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("too long"));
}

#[tokio::test]
async fn test_tts_server_unavailable() {
    let (app, _dir) = test_app_with_tts_url("http://127.0.0.1:9");
    let (status, body) = post_json(&app, "/api/tts", r#"{"text":"hello"}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("TTS server not running"));
}

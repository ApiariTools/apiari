use apiari_hive::db::Db;
use apiari_hive::events::EventHub;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tempfile::tempdir;
use tower::ServiceExt;

fn test_app_with_workspace(workspace_toml: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();
    std::fs::write(config_dir.join("workspaces/test.toml"), workspace_toml).unwrap();

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
async fn test_multiple_workspaces() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();
    std::fs::write(
        config_dir.join("workspaces/ws1.toml"),
        "[workspace]\nname = \"ws1\"\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.join("workspaces/ws2.toml"),
        "[workspace]\nname = \"ws2\"\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.join("workspaces/ws3.toml"),
        "[workspace]\nname = \"ws3\"\n",
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

    let (_, body) = get(&app, "/api/workspaces").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 3);
}

#[tokio::test]
async fn test_bot_config_with_provider() {
    let (app, _dir) = test_app_with_workspace(
        "[workspace]\nname = \"test\"\n\n[[bots]]\nname = \"CodexBot\"\nprovider = \"codex\"\nrole = \"Codex\"\n",
    );
    let (_, body) = get(&app, "/api/workspaces/test/bots").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 2); // Main + CodexBot
    assert_eq!(parsed[1]["name"], "CodexBot");
    assert_eq!(parsed[1]["provider"], "codex");
}

#[tokio::test]
async fn test_conversations_per_bot_isolation() {
    let (app, _dir) = test_app_with_workspace("[workspace]\nname = \"test\"\n");

    // Send to Main
    post_json(
        &app,
        "/api/workspaces/test/chat/Main",
        r#"{"message":"hello main"}"#,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Send to Customer (default bot)
    post_json(
        &app,
        "/api/workspaces/test/chat/Customer",
        r#"{"message":"hello customer"}"#,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (_, main_body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let main_msgs: Vec<serde_json::Value> = serde_json::from_str(&main_body).unwrap();

    let (_, cust_body) = get(&app, "/api/workspaces/test/conversations/Customer").await;
    let cust_msgs: Vec<serde_json::Value> = serde_json::from_str(&cust_body).unwrap();

    // Messages should not leak between bots
    assert!(main_msgs.iter().any(|m| m["content"] == "hello main"));
    assert!(!main_msgs.iter().any(|m| m["content"] == "hello customer"));
    assert!(cust_msgs.iter().any(|m| m["content"] == "hello customer"));
}

#[tokio::test]
async fn test_unread_per_bot() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        "[workspace]\nname = \"test\"\n",
    )
    .unwrap();
    let db = Db::open(&config_dir.join("hive.db")).unwrap();

    // Add assistant messages for two bots
    db.add_message("test", "Main", "assistant", "hi from main", None)
        .unwrap();
    db.add_message("test", "Customer", "assistant", "hi from customer", None)
        .unwrap();
    db.add_message(
        "test",
        "Customer",
        "assistant",
        "another from customer",
        None,
    )
    .unwrap();

    let events = EventHub::new();
    let app = apiari_hive::routes::router(
        db,
        &config_dir,
        events,
        Default::default(),
        Default::default(),
        apiari_hive::remote::new_registry(),
    );

    let (_, body) = get(&app, "/api/workspaces/test/unread").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(parsed["Main"], 1);
    assert_eq!(parsed["Customer"], 2);

    // Mark Main as seen
    post_json(&app, "/api/workspaces/test/seen/Main", "").await;
    let (_, body) = get(&app, "/api/workspaces/test/unread").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert!(parsed.get("Main").is_none() || parsed["Main"] == 0);
    assert_eq!(parsed["Customer"], 2);
}

#[tokio::test]
async fn test_cancel_resets_status() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        "[workspace]\nname = \"test\"\n",
    )
    .unwrap();
    let db = Db::open(&config_dir.join("hive.db")).unwrap();

    // Simulate a bot that's streaming
    db.set_bot_status(
        "test",
        "Main",
        "streaming",
        "partial response...",
        Some("Read"),
    )
    .unwrap();

    let events = EventHub::new();
    let app = apiari_hive::routes::router(
        db,
        &config_dir,
        events,
        Default::default(),
        Default::default(),
        apiari_hive::remote::new_registry(),
    );

    // Cancel
    post_json(&app, "/api/workspaces/test/bots/Main/cancel", "").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Status should be idle
    let (_, body) = get(&app, "/api/workspaces/test/bots/Main/status").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "idle");

    // Should have a "Response cancelled" system message
    let (_, body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let msgs: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(
        msgs.iter()
            .any(|m| m["content"].as_str().unwrap_or("").contains("cancelled"))
    );
}

#[tokio::test]
async fn test_send_message_with_attachments() {
    let (app, _dir) = test_app_with_workspace("[workspace]\nname = \"test\"\n");

    let (status, _) = post_json(
        &app,
        "/api/workspaces/test/chat/Main",
        r#"{"message":"look at this","attachments":[{"name":"test.txt","type":"text/plain","dataUrl":"data:text/plain;base64,SGVsbG8="}]}"#,
    ).await;
    assert_eq!(status, StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (_, body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let msgs: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    let user_msg = msgs.iter().find(|m| m["role"] == "user").unwrap();
    assert!(user_msg["attachments"].is_string());
}

#[tokio::test]
async fn test_search_endpoint() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        "[workspace]\nname = \"test\"\n",
    )
    .unwrap();
    let db = Db::open(&config_dir.join("hive.db")).unwrap();
    db.add_message("test", "Main", "user", "fix the login page", None)
        .unwrap();
    db.add_message("test", "Main", "assistant", "dispatching a worker", None)
        .unwrap();
    db.add_message("test", "Main", "user", "check the dashboard", None)
        .unwrap();

    let events = EventHub::new();
    let app = apiari_hive::routes::router(
        db,
        &config_dir,
        events,
        Default::default(),
        Default::default(),
        apiari_hive::remote::new_registry(),
    );

    let (_, body) = get(
        &app,
        "/api/workspaces/test/conversations/Main/search?q=login",
    )
    .await;
    let results: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0]["content"].as_str().unwrap().contains("login"));
}

#[tokio::test]
async fn test_non_toml_files_ignored() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        "[workspace]\nname = \"test\"\n",
    )
    .unwrap();
    std::fs::write(config_dir.join("workspaces/readme.md"), "# Not a workspace").unwrap();
    std::fs::write(config_dir.join("workspaces/.hidden"), "nope").unwrap();

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

    let (_, body) = get(&app, "/api/workspaces").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["name"], "test");
}

#[tokio::test]
async fn test_usage_endpoint_returns_not_installed_when_cache_empty() {
    let (app, _dir) = test_app_with_workspace("[workspace]\nname = \"test\"\n");
    let (status, body) = get(&app, "/api/usage").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["installed"], false);
    assert!(parsed["providers"].as_array().unwrap().is_empty());
    assert!(parsed["updated_at"].is_null());
}

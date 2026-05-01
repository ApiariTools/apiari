use apiari_hive::db::Db;
use tempfile::tempdir;

#[test]
fn test_open_creates_tables() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();
    // Should not panic — tables created
    let msgs = db.get_conversations("test", "Main", 10).unwrap();
    assert!(msgs.is_empty());
}

#[test]
fn test_add_and_get_messages() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws1", "Main", "user", "hello", None)
        .unwrap();
    db.add_message("ws1", "Main", "assistant", "hi there", None)
        .unwrap();
    db.add_message("ws1", "Other", "user", "different bot", None)
        .unwrap();

    let msgs = db.get_conversations("ws1", "Main", 10).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content, "hello");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].content, "hi there");

    // Other bot's messages don't leak
    let other = db.get_conversations("ws1", "Other", 10).unwrap();
    assert_eq!(other.len(), 1);
}

#[test]
fn test_get_all_conversations() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws1", "Main", "user", "a", None).unwrap();
    db.add_message("ws1", "Bot2", "user", "b", None).unwrap();
    db.add_message("ws2", "Main", "user", "c", None).unwrap();

    let all = db.get_all_conversations("ws1", 10).unwrap();
    assert_eq!(all.len(), 2);

    let ws2 = db.get_all_conversations("ws2", 10).unwrap();
    assert_eq!(ws2.len(), 1);
}

#[test]
fn test_message_limit() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    for i in 0..20 {
        db.add_message("ws", "Main", "user", &format!("msg {i}"), None)
            .unwrap();
    }

    let msgs = db.get_conversations("ws", "Main", 5).unwrap();
    assert_eq!(msgs.len(), 5);
    // Should be the last 5 in chronological order
    assert_eq!(msgs[0].content, "msg 15");
    assert_eq!(msgs[4].content, "msg 19");
}

#[test]
fn test_attachments_stored() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message(
        "ws",
        "Main",
        "user",
        "see this",
        Some(r#"[{"name":"photo.jpg"}]"#),
    )
    .unwrap();

    let msgs = db.get_conversations("ws", "Main", 10).unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].attachments.as_ref().unwrap().contains("photo.jpg"));
}

#[test]
fn test_session_set_and_get() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // No session yet
    let id = db.get_session_id("ws", "Main", "hash1").unwrap();
    assert!(id.is_none());

    // Set session
    db.set_session("ws", "Main", "sess_123", "hash1").unwrap();
    let id = db.get_session_id("ws", "Main", "hash1").unwrap();
    assert_eq!(id, Some("sess_123".to_string()));

    // Same hash — returns session
    let id = db.get_session_id("ws", "Main", "hash1").unwrap();
    assert_eq!(id, Some("sess_123".to_string()));

    // Different hash — returns None (prompt changed)
    let id = db.get_session_id("ws", "Main", "hash2").unwrap();
    assert!(id.is_none());
}

#[test]
fn test_bot_status() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // No status yet
    let s = db.get_bot_status("ws", "Main").unwrap();
    assert!(s.is_none());

    // Set thinking
    db.set_bot_status("ws", "Main", "thinking", "", None)
        .unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.status, "thinking");
    assert_eq!(s.streaming_content, "");

    // Set streaming with content
    db.set_bot_status("ws", "Main", "streaming", "hello", Some("Read"))
        .unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.status, "streaming");
    assert_eq!(s.streaming_content, "hello");
    assert_eq!(s.tool_name, Some("Read".to_string()));

    // Append streaming
    db.append_streaming("ws", "Main", " world").unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.streaming_content, "hello world");

    // Set idle
    db.set_bot_status("ws", "Main", "idle", "", None).unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.status, "idle");
}

#[test]
fn test_unread_tracking() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // Add messages
    db.add_message("ws", "Main", "user", "hello", None).unwrap();
    db.add_message("ws", "Main", "assistant", "hi", None)
        .unwrap();
    db.add_message("ws", "Main", "assistant", "how can I help?", None)
        .unwrap();

    // 2 unread assistant messages
    let counts = db.get_unread_counts("ws").unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0], ("Main".to_string(), 2));

    // Mark as seen
    db.mark_seen("ws", "Main").unwrap();
    let counts = db.get_unread_counts("ws").unwrap();
    assert!(counts.is_empty());

    // New message after marking seen
    db.add_message("ws", "Main", "assistant", "anything else?", None)
        .unwrap();
    let counts = db.get_unread_counts("ws").unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0], ("Main".to_string(), 1));
}

#[test]
fn test_search_conversations() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws", "Main", "user", "fix the login bug", None)
        .unwrap();
    db.add_message("ws", "Main", "assistant", "I'll dispatch a worker", None)
        .unwrap();
    db.add_message("ws", "Main", "user", "check the dashboard", None)
        .unwrap();

    let results = db.search_conversations("ws", "Main", "login", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("login"));

    let results = db.search_conversations("ws", "Main", "worker", 10).unwrap();
    assert_eq!(results.len(), 1);

    let results = db
        .search_conversations("ws", "Main", "nonexistent", 10)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_concurrent_read_write() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // Write a message
    db.add_message("ws", "Main", "user", "hello", None).unwrap();

    // Simultaneous read and write should not deadlock
    let db2 = db.clone();
    let handle = std::thread::spawn(move || {
        for i in 0..10 {
            db2.add_message("ws", "Main", "assistant", &format!("reply {i}"), None)
                .unwrap();
        }
    });

    for _ in 0..10 {
        let _ = db.get_conversations("ws", "Main", 100);
        let _ = db.get_bot_status("ws", "Main");
    }

    handle.join().unwrap();
    let msgs = db.get_conversations("ws", "Main", 100).unwrap();
    assert_eq!(msgs.len(), 11); // 1 user + 10 assistant
}

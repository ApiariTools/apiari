use apiari_hive::db::Db;
use tempfile::tempdir;

#[test]
fn test_streaming_lifecycle() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // Start thinking
    db.set_bot_status("ws", "Main", "thinking", "", None)
        .unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.status, "thinking");
    assert_eq!(s.streaming_content, "");
    assert!(s.tool_name.is_none());

    // Start streaming
    db.set_bot_status("ws", "Main", "streaming", "", None)
        .unwrap();
    db.append_streaming("ws", "Main", "Hello").unwrap();
    db.append_streaming("ws", "Main", " world").unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.status, "streaming");
    assert_eq!(s.streaming_content, "Hello world");

    // Tool use during streaming
    db.set_bot_status("ws", "Main", "streaming", "Hello world", Some("Bash"))
        .unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.tool_name, Some("Bash".to_string()));

    // Back to idle
    db.set_bot_status("ws", "Main", "idle", "", None).unwrap();
    let s = db.get_bot_status("ws", "Main").unwrap().unwrap();
    assert_eq!(s.status, "idle");
    assert_eq!(s.streaming_content, "");
    assert!(s.tool_name.is_none());
}

#[test]
fn test_streaming_content_trimmed_on_store() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // Simulate what happens when bot response starts with newlines
    let content = "\n\nHere is my response";
    db.add_message("ws", "Main", "assistant", content.trim(), None)
        .unwrap();

    let msgs = db.get_conversations("ws", "Main", 10).unwrap();
    assert_eq!(msgs[0].content, "Here is my response");
}

#[test]
fn test_multiple_bots_independent_status() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.set_bot_status("ws", "Main", "streaming", "working...", None)
        .unwrap();
    db.set_bot_status("ws", "Customer", "idle", "", None)
        .unwrap();

    let main = db.get_bot_status("ws", "Main").unwrap().unwrap();
    let customer = db.get_bot_status("ws", "Customer").unwrap().unwrap();

    assert_eq!(main.status, "streaming");
    assert_eq!(customer.status, "idle");
}

#[test]
fn test_multiple_workspaces_independent() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws1", "Main", "user", "hello from ws1", None)
        .unwrap();
    db.add_message("ws2", "Main", "user", "hello from ws2", None)
        .unwrap();
    db.set_bot_status("ws1", "Main", "streaming", "...", None)
        .unwrap();

    let ws1 = db.get_conversations("ws1", "Main", 10).unwrap();
    let ws2 = db.get_conversations("ws2", "Main", 10).unwrap();
    assert_eq!(ws1.len(), 1);
    assert_eq!(ws2.len(), 1);
    assert_eq!(ws1[0].content, "hello from ws1");
    assert_eq!(ws2[0].content, "hello from ws2");

    // Status is per workspace
    let s1 = db.get_bot_status("ws1", "Main").unwrap().unwrap();
    let s2 = db.get_bot_status("ws2", "Main").unwrap();
    assert_eq!(s1.status, "streaming");
    assert!(s2.is_none());
}

#[test]
fn test_session_hash_reset_inserts_message() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    // Set initial session
    db.set_session("ws", "Main", "sess_1", "hash_a").unwrap();

    // Same hash — returns session
    let id = db.get_session_id("ws", "Main", "hash_a").unwrap();
    assert_eq!(id, Some("sess_1".to_string()));

    // Different hash — returns None and inserts system message
    let id = db.get_session_id("ws", "Main", "hash_b").unwrap();
    assert!(id.is_none());

    // Check system message was inserted
    let msgs = db.get_conversations("ws", "Main", 10).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "system");
    assert!(msgs[0].content.contains("Session reset"));
}

#[test]
fn test_unread_only_counts_assistant() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws", "Main", "user", "hello", None).unwrap();
    db.add_message("ws", "Main", "system", "session reset", None)
        .unwrap();
    db.add_message("ws", "Main", "assistant", "hi", None)
        .unwrap();

    let counts = db.get_unread_counts("ws").unwrap();
    // Only assistant messages count as unread
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0].1, 1);
}

#[test]
fn test_mark_seen_then_new_messages() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws", "Main", "assistant", "msg 1", None)
        .unwrap();
    db.add_message("ws", "Main", "assistant", "msg 2", None)
        .unwrap();
    db.mark_seen("ws", "Main").unwrap();

    let counts = db.get_unread_counts("ws").unwrap();
    assert!(counts.is_empty());

    db.add_message("ws", "Main", "assistant", "msg 3", None)
        .unwrap();
    let counts = db.get_unread_counts("ws").unwrap();
    assert_eq!(counts[0].1, 1);

    db.add_message("ws", "Main", "assistant", "msg 4", None)
        .unwrap();
    let counts = db.get_unread_counts("ws").unwrap();
    assert_eq!(counts[0].1, 2);
}

#[test]
fn test_search_case_insensitive() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.add_message("ws", "Main", "user", "Fix the LOGIN bug", None)
        .unwrap();

    let results = db.search_conversations("ws", "Main", "login", 10).unwrap();
    assert_eq!(results.len(), 1);

    let results = db.search_conversations("ws", "Main", "LOGIN", 10).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_concurrent_streaming_and_reads() {
    let dir = tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();

    db.set_bot_status("ws", "Main", "streaming", "", None)
        .unwrap();

    let db_writer = db.clone();
    let db_reader = db.clone();

    let writer = std::thread::spawn(move || {
        for i in 0..50 {
            db_writer
                .append_streaming("ws", "Main", &format!("chunk{i} "))
                .unwrap();
        }
    });

    let reader = std::thread::spawn(move || {
        let mut last_len = 0;
        for _ in 0..50 {
            if let Ok(Some(s)) = db_reader.get_bot_status("ws", "Main") {
                // Content should only grow, never shrink
                assert!(s.streaming_content.len() >= last_len);
                last_len = s.streaming_content.len();
            }
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();

    let final_status = db.get_bot_status("ws", "Main").unwrap().unwrap();
    // All 50 chunks should be there
    assert!(final_status.streaming_content.contains("chunk49"));
}

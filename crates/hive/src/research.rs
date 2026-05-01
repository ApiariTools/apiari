//! Research worker — background Claude sessions that produce docs.

use crate::db::Db;
use crate::events::{EventHub, HiveEvent};
use apiari_claude_sdk::{
    ClaudeClient, Event, SessionOptions, streaming::AssembledEvent, types::ContentBlock,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchTask {
    pub id: String,
    pub workspace: String,
    pub topic: String,
    pub status: String, // "running", "complete", "failed"
    pub error: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub output_file: Option<String>,
}

const RESEARCH_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS research_tasks (
        id TEXT PRIMARY KEY,
        workspace TEXT NOT NULL,
        topic TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'running',
        error TEXT,
        started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        completed_at TEXT,
        output_file TEXT
    );
";

/// Ensure the research_tasks table exists.
pub fn ensure_schema(db: &Db) {
    db.execute_batch(RESEARCH_SCHEMA).ok();
}

/// Insert a new research task.
pub fn insert_task(db: &Db, id: &str, workspace: &str, topic: &str) -> color_eyre::Result<()> {
    db.execute_sql(
        "INSERT INTO research_tasks (id, workspace, topic, status) VALUES (?1, ?2, ?3, 'running')",
        &[id, workspace, topic],
    )
}

/// Update task status to complete.
fn complete_task(db: &Db, id: &str, output_file: &str) {
    db.execute_sql(
        "UPDATE research_tasks SET status = 'complete', output_file = ?2, completed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
        &[id, output_file],
    ).ok();
}

/// Update task status to failed.
fn fail_task(db: &Db, id: &str, error: &str) {
    db.execute_sql(
        "UPDATE research_tasks SET status = 'failed', error = ?2, completed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
        &[id, error],
    )
    .ok();
}

/// List research tasks for a workspace (recent first).
pub fn list_tasks(db: &Db, workspace: &str) -> Vec<ResearchTask> {
    db.query_research_tasks(workspace).unwrap_or_default()
}

/// Get a single research task by ID.
pub fn get_task(db: &Db, id: &str) -> Option<ResearchTask> {
    db.query_research_task(id)
}

/// Slugify a topic string for use as a filename.
fn slugify(topic: &str) -> String {
    let slug: String = topic
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse repeated dashes and trim
    let mut result = String::new();
    let mut last_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !last_dash && !result.is_empty() {
                result.push('-');
            }
            last_dash = true;
        } else {
            result.push(c);
            last_dash = false;
        }
    }
    result.trim_end_matches('-').to_string()
}

/// Spawn a research task in the background.
pub fn spawn_research(
    db: Db,
    events: EventHub,
    task_id: String,
    workspace: String,
    topic: String,
    working_dir: PathBuf,
) {
    tokio::spawn(async move {
        let result = run_research(&db, &events, &task_id, &workspace, &topic, &working_dir).await;
        if let Err(e) = result {
            tracing::error!("[research] task {} failed: {}", task_id, e);
            fail_task(&db, &task_id, &e);
            events.send(HiveEvent::ResearchUpdate {
                workspace: workspace.clone(),
                task_id: task_id.clone(),
                status: "failed".to_string(),
                topic: topic.clone(),
                output_file: None,
            });
        }
    });
}

async fn run_research(
    db: &Db,
    events: &EventHub,
    task_id: &str,
    workspace: &str,
    topic: &str,
    working_dir: &std::path::Path,
) -> Result<(), String> {
    // Load context if available
    let context_path = working_dir.join(".apiari/context.md");
    let context = std::fs::read_to_string(&context_path).unwrap_or_default();

    let system_prompt = format!(
        "You are a research assistant. Investigate the topic thoroughly using available tools. \
         You have access to Bash, file reading, web search, and web fetch tools through your normal Claude capabilities.\n\
         \n\
         Working directory: {}\n\
         {}\n\
         When done, write your findings as a well-structured markdown document. \
         Output ONLY the final markdown document content at the end, prefixed with a line containing exactly `---RESEARCH OUTPUT---`",
        working_dir.display(),
        if context.is_empty() {
            String::new()
        } else {
            format!("\nProject context:\n{context}\n")
        }
    );

    let opts = SessionOptions {
        dangerously_skip_permissions: true,
        include_partial_messages: true,
        working_dir: Some(working_dir.to_path_buf()),
        max_turns: Some(20),
        system_prompt: Some(system_prompt),
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client.spawn(opts).await.map_err(|e| e.to_string())?;

    let prompt = format!("Research the following topic thoroughly: {topic}");
    session
        .send_message(&prompt)
        .await
        .map_err(|e| e.to_string())?;

    let mut full_text = String::new();
    loop {
        match session.next_event().await {
            Ok(Some(event)) => match event {
                Event::Stream { assembled, .. } => {
                    for asm in assembled {
                        if let AssembledEvent::TextDelta { text, .. } = asm {
                            full_text.push_str(&text);
                        }
                    }
                }
                Event::Assistant { message: msg, .. } => {
                    for block in &msg.message.content {
                        if let ContentBlock::Text { text } = block
                            && !text.is_empty()
                            && full_text.is_empty()
                        {
                            full_text.push_str(text);
                        }
                    }
                }
                Event::Result(_) => break,
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }

    // Parse the output for research findings
    let findings = if let Some(idx) = full_text.find("---RESEARCH OUTPUT---") {
        full_text[idx + "---RESEARCH OUTPUT---".len()..].trim()
    } else {
        // If no marker found, use the entire output
        full_text.trim()
    };

    if findings.is_empty() {
        return Err("Research produced no output".to_string());
    }

    // Write to .apiari/docs/{slug}.md
    let slug = slugify(topic);
    if slug.is_empty() {
        return Err("Topic produced an empty slug".to_string());
    }
    let filename = format!("{slug}.md");
    let docs_dir = working_dir.join(".apiari/docs");
    std::fs::create_dir_all(&docs_dir).map_err(|e| e.to_string())?;
    let output_path = docs_dir.join(&filename);
    std::fs::write(&output_path, findings).map_err(|e| e.to_string())?;

    complete_task(db, task_id, &filename);
    events.send(HiveEvent::ResearchUpdate {
        workspace: workspace.to_string(),
        task_id: task_id.to_string(),
        status: "complete".to_string(),
        topic: topic.to_string(),
        output_file: Some(filename.clone()),
    });

    tracing::info!("[research] task {task_id} complete → docs/{filename}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(
            slugify("Rust async/await patterns"),
            "rust-async-await-patterns"
        );
        assert_eq!(slugify("  spaces  "), "spaces");
        assert_eq!(slugify("CamelCase"), "camelcase");
        assert_eq!(slugify("a--b"), "a-b");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn test_research_schema_and_crud() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        insert_task(&db, "task-1", "ws", "test topic").unwrap();
        let tasks = list_tasks(&db, "ws");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task-1");
        assert_eq!(tasks[0].status, "running");

        complete_task(&db, "task-1", "test-topic.md");
        let task = get_task(&db, "task-1").unwrap();
        assert_eq!(task.status, "complete");
        assert_eq!(task.output_file.as_deref(), Some("test-topic.md"));
    }

    #[test]
    fn test_research_fail_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        insert_task(&db, "task-2", "ws", "failing topic").unwrap();
        fail_task(&db, "task-2", "something went wrong");
        let task = get_task(&db, "task-2").unwrap();
        assert_eq!(task.status, "failed");
        assert_eq!(task.error.as_deref(), Some("something went wrong"));
    }
}

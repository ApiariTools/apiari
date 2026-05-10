// Cycle detection: GoTo revision counter aborts infinite loops.

use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, WorkflowDef};

#[tokio::test]
async fn infinite_loop_is_detected_and_fails() {
    // A step that always GoTos itself — should hit max_revisions.
    let (loop_step, _) = GotoStep::new("loop", "loop", u32::MAX, json!({}));

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("inf").max_revisions(5).step(loop_step));

    let id = engine.start("inf", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    let err = run.error.unwrap();
    assert!(err.contains("cycle"), "expected cycle error, got: {err}");
    // Revision should be at or past the limit
    assert!(run.revision > 5);
}

#[tokio::test]
async fn exactly_at_max_revisions_is_allowed() {
    // With max_revisions=3, three GoTos should succeed.
    let (loop_step, _) = GotoStep::new("loop", "loop", 3, json!({"done": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("exact")
            .max_revisions(3)
            .step(loop_step)
            .step(ConstStep::new("end", json!({}))),
    );

    let id = engine.start("exact", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.revision, 3);
}

#[tokio::test]
async fn one_over_max_revisions_fails() {
    // With max_revisions=3, four GoTos should fail.
    let (loop_step, _) = GotoStep::new("loop", "loop", 4, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("one_over")
            .max_revisions(3)
            .step(loop_step)
            .step(ConstStep::new("end", json!({}))),
    );

    let id = engine.start("one_over", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
}

#[tokio::test]
async fn high_max_revisions_allows_deep_loops() {
    let (loop_step, _) = GotoStep::new("l", "l", 50, json!({"deep": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("deep")
            .max_revisions(100)
            .step(loop_step)
            .step(ConstStep::new("end", json!({}))),
    );

    let id = engine.start("deep", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.revision, 50);
}

#[tokio::test]
async fn cycle_detection_error_persisted_in_db() {
    let (loop_step, _) = GotoStep::new("loop", "loop", u32::MAX, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("persist_cycle")
            .max_revisions(2)
            .step(loop_step),
    );

    let id = engine.start("persist_cycle", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    // Must be persisted as failed (not just a return error)
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.is_some());
}

// Edge cases and corner scenarios not covered by other test modules.

use async_trait::async_trait;
use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, StepContext, StepDef, StepResult, WorkflowDef};

#[tokio::test]
#[should_panic(expected = "workflow kind already registered")]
async fn workflow_kind_registered_twice_panics() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("dup_kind").step(ConstStep::new("s", json!({}))));
    // Registering the same kind again must panic.
    engine.register(WorkflowDef::new("dup_kind").step(ConstStep::new("s2", json!({}))));
}

#[tokio::test]
async fn start_creates_unique_run_per_call() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("unique_runs").step(ConstStep::new("s", json!({}))));

    let n = 5usize;
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        ids.push(engine.start("unique_runs", json!({})).await.unwrap());
    }

    // All IDs must be distinct
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), n);

    // All runs should exist and be completed
    for id in &ids {
        let run = engine.get_run(id).await.unwrap();
        assert_eq!(run.status, RunStatus::Completed);
    }
}

#[tokio::test]
async fn list_runs_empty_when_no_runs() {
    let engine = mem_engine();
    // No workflows registered, no runs started — every status list should be empty.
    for status in [
        RunStatus::Running,
        RunStatus::Parked,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Compensating,
    ] {
        let runs = engine.list_runs(&status).await.unwrap();
        assert!(
            runs.is_empty(),
            "expected empty list for status {:?}",
            status
        );
    }
}

#[tokio::test]
async fn fail_run_with_compensation_that_does_goto_is_handled() {
    // A compensation step that returns GoTo should not crash the engine.
    // The engine logs a warning and swallows non-Complete comp results.
    struct GotoCompStep;

    #[async_trait]
    impl crate::step::Step for GotoCompStep {
        fn name(&self) -> &'static str {
            "goto_comp"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::GoTo {
                step: "nowhere",
                output: json!({}),
            }
        }
    }

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("comp_goto")
            .step(AlwaysFailStep {
                name: "fail",
                message: "trigger comp",
            })
            .on_failure(GotoCompStep),
    );

    let id = engine.start("comp_goto", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    // Must not panic; final status is Failed
    assert_eq!(run.status, RunStatus::Failed);
}

#[tokio::test]
async fn large_context_roundtrip() {
    // A 100-key JSON object must survive a complete workflow run (insert + update + retrieve).
    let mut large = serde_json::Map::new();
    for i in 0..100 {
        large.insert(format!("key_{i}"), json!(i));
    }
    let initial = serde_json::Value::Object(large);

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("large_ctx")
            .step(ConstStep::new("passthrough", json!({"extra": "value"}))),
    );

    let id = engine.start("large_ctx", initial.clone()).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);

    // All 100 original keys must be preserved
    for i in 0..100i64 {
        let key = format!("key_{i}");
        assert_eq!(
            run.context[&key], i,
            "context key '{key}' missing or wrong after roundtrip"
        );
    }
    // The step output must also be present
    assert_eq!(run.context["extra"], "value");
}

#[tokio::test]
async fn step_with_waiting_for_wont_be_resumed_by_tick_alone() {
    // A WaitForSignal with a future timeout should not be resumed by tick
    // (the deadline hasn't passed yet).
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("no_early_tick")
            .step_with(
                StepDef::new(SignalWaitStep {
                    name: "wait",
                    pattern: "delayed",
                    timeout: Some(std::time::Duration::from_secs(3600)), // 1 hour
                })
                .on_timeout(crate::TimeoutAction::Continue),
            )
            .step(ConstStep::new("after", json!({}))),
    );

    let id = engine.start("no_early_tick", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    // tick should not resume this — deadline not passed
    let resumed = engine.tick().await.unwrap();
    assert!(
        resumed.is_empty(),
        "tick should not resume a run whose deadline hasn't passed"
    );
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);
}

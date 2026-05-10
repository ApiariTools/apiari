// Targeted tests to close specific coverage gaps identified by llvm-cov.
// Each test is annotated with which line(s) it exercises.

use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

use super::helpers::*;
use crate::{ErrorAction, RetryPolicy, RunStatus, StepContext, StepDef, StepResult, WorkflowDef};

// ── engine.rs:119-120 ────────────────────────────────────────────────────────
// on_signal() → advance_step_name() returns Completed (wait step was last step)

#[tokio::test]
async fn signal_at_last_step_completes_without_advance() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("sig_last")
            .step(ConstStep::new("setup", json!({"setup": true})))
            .step(SignalWaitStep {
                name: "wait",
                pattern: "done",
                timeout: None,
            }),
    );

    let id = engine.start("sig_last", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine
        .on_signal(&crate::IncomingSignal {
            pattern: "done".into(),
            payload: json!({"sig": 1}),
        })
        .await
        .unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    // Context from both setup and signal payload present
    assert_eq!(run.context["setup"], true);
    assert_eq!(run.context["sig"], 1);
}

// ── engine.rs:214-215 ────────────────────────────────────────────────────────
// tick() → advance_step_name() returns Completed (WaitUntil was last step)

#[tokio::test]
async fn wait_until_at_last_step_completes_via_tick() {
    use chrono::{Duration as CDuration, Utc};

    struct LastStepTimer;
    #[async_trait]
    impl crate::step::Step for LastStepTimer {
        fn name(&self) -> &'static str {
            "sleep"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::WaitUntil(Utc::now() - CDuration::seconds(1))
        }
    }

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timer_last")
            .step(ConstStep::new("first", json!({"pre": true})))
            .step(LastStepTimer),
    );

    let id = engine.start("timer_last", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    let resumed = engine.tick().await.unwrap();
    assert!(resumed.contains(&id));

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["pre"], true);
}

// ── engine.rs:406 ─────────────────────────────────────────────────────────────
// Retry with non-zero backoff actually sleeps (use 1ms to keep test fast)

#[tokio::test]
async fn retry_with_nonzero_backoff_succeeds_eventually() {
    let (step, count) = CountingStep::new("flaky", 1, json!({"done": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("backoff_retry").step_with(StepDef::new(step).with_retry(
            RetryPolicy::with_retries(2, Duration::from_millis(1), ErrorAction::FailWorkflow),
        )),
    );

    let id = engine.start("backoff_retry", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(*count.lock().unwrap(), 2);
}

// ── engine.rs:421-423 ─────────────────────────────────────────────────────────
// Retry exhaustion with GoTo to a nonexistent target → FailWorkflow

#[tokio::test]
async fn retry_exhausted_goto_nonexistent_target_fails_workflow() {
    let (step, _) = CountingStep::new("bad", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("bad_goto_retry").step_with(StepDef::new(step).with_retry(
            RetryPolicy::with_retries(1, Duration::ZERO, ErrorAction::GoTo("nonexistent_target")),
        )),
    );

    let id = engine.start("bad_goto_retry", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.as_ref().unwrap().contains("nonexistent_target"));
}

// ── engine.rs:532 ─────────────────────────────────────────────────────────────
// merge_context where patch is not an Object (scalar replaces base)

#[tokio::test]
async fn step_output_scalar_replaces_context() {
    // Step outputs a plain number (not an Object) — this exercises the
    // `(base, patch) => *base = patch.clone()` arm of merge_context.
    struct NumberStep;
    #[async_trait]
    impl crate::step::Step for NumberStep {
        fn name(&self) -> &'static str {
            "num"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::Complete(json!(42))
        }
    }

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("scalar_out").step(NumberStep));

    let id = engine
        .start("scalar_out", json!({"original": "data"}))
        .await
        .unwrap();
    let run = engine.get_run(&id).await.unwrap();
    // context was replaced by the scalar 42
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context, json!(42));
}

// ── step.rs:86-89 ─────────────────────────────────────────────────────────────
// StepDef::retry() shorthand builder method

#[tokio::test]
async fn stepdef_retry_shorthand_works() {
    let (step, count) = CountingStep::new("short", 1, json!({"ok": true}));

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("shorthand").step_with(
        // Use the shorthand .retry() builder instead of .with_retry()
        StepDef::new(step).retry(2, Duration::ZERO),
    ));

    let id = engine.start("shorthand", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(*count.lock().unwrap(), 2);
}

// ── types.rs:120-121 ──────────────────────────────────────────────────────────
// RunStatus::parse("compensating") and parse(unknown)

#[test]
fn runstatus_parse_compensating() {
    assert_eq!(
        crate::RunStatus::parse("compensating"),
        Some(crate::RunStatus::Compensating)
    );
}

#[test]
fn runstatus_parse_unknown_returns_none() {
    assert_eq!(crate::RunStatus::parse("garbage"), None);
    assert_eq!(crate::RunStatus::parse(""), None);
}

// ── workflow.rs:47-49 ─────────────────────────────────────────────────────────
// WorkflowDef::kind() accessor

#[test]
fn workflowdef_kind_returns_kind_str() {
    let def = WorkflowDef::new("my_workflow");
    assert_eq!(def.kind(), "my_workflow");
}

// ── db.rs:111 ─────────────────────────────────────────────────────────────────
// WorkflowError::RunNotFound is returned for missing rows (not Db variant)

#[tokio::test]
async fn get_nonexistent_run_returns_run_not_found_error() {
    let engine = mem_engine();
    let err = engine.get_run("does-not-exist").await.unwrap_err();
    assert!(
        matches!(err, crate::WorkflowError::RunNotFound(_)),
        "expected RunNotFound, got: {err:?}"
    );
}

// ── engine.rs:124 ─────────────────────────────────────────────────────────────
// on_signal() → advance() returns error (cycle in the resumed workflow)
// The warn! branch fires when the step after the wait step itself hits a cycle.

#[tokio::test]
async fn signal_resume_advances_into_cycle_marks_run_failed() {
    // Workflow: wait → loop (GoTo itself forever)
    let (loop_step, _) = GotoStep::new("loop", "loop", u32::MAX, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("sig_then_cycle")
            .max_revisions(3)
            .step(SignalWaitStep {
                name: "wait",
                pattern: "go",
                timeout: None,
            })
            .step(loop_step),
    );

    let id = engine.start("sig_then_cycle", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    // Signal resumes → advances to "loop" → loop hits cycle detection → run fails
    engine
        .on_signal(&crate::IncomingSignal {
            pattern: "go".into(),
            payload: json!({}),
        })
        .await
        .unwrap(); // on_signal itself should not error (it swallows advance errors)

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.as_ref().unwrap().contains("cycle"));
}

// ── engine.rs:219 ─────────────────────────────────────────────────────────────
// tick() → advance() returns error (cycle in resumed workflow after WaitUntil)

#[tokio::test]
async fn tick_resume_advances_into_cycle_marks_run_failed() {
    use chrono::{Duration as CDuration, Utc};

    struct ExpiredWait;
    #[async_trait]
    impl crate::step::Step for ExpiredWait {
        fn name(&self) -> &'static str {
            "wait"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::WaitUntil(Utc::now() - CDuration::seconds(1))
        }
    }

    let (loop_step, _) = GotoStep::new("loop", "loop", u32::MAX, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("tick_then_cycle")
            .max_revisions(3)
            .step(ExpiredWait)
            .step(loop_step),
    );

    let id = engine.start("tick_then_cycle", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine.tick().await.unwrap(); // swallows advance error

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.as_ref().unwrap().contains("cycle"));
}

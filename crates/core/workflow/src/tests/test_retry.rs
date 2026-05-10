// Retry policies: max_attempts, on_exhausted (FailWorkflow / GoTo / Skip).

use serde_json::json;
use std::time::Duration;

use super::helpers::*;
use crate::{ErrorAction, RetryPolicy, RunStatus, StepDef, WorkflowDef};

// ── FailWorkflow on exhaustion ────────────────────────────────────────────────

#[tokio::test]
async fn single_failure_with_no_retry_fails_workflow() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("no_retry").step_with(
            StepDef::new(AlwaysFailStep {
                name: "boom",
                message: "oops",
            })
            .with_retry(RetryPolicy::default()), // 1 attempt
        ),
    );

    let id = engine.start("no_retry", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.as_ref().unwrap().contains("oops"));
}

#[tokio::test]
async fn step_succeeds_on_second_attempt() {
    let (step, count) = CountingStep::new("flaky", 1, json!({"ok": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("retry_ok").step_with(StepDef::new(step).with_retry(
            RetryPolicy::with_retries(2, Duration::ZERO, ErrorAction::FailWorkflow),
        )),
    );

    let id = engine.start("retry_ok", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["ok"], true);
    assert_eq!(*count.lock().unwrap(), 2); // failed once, succeeded on second
}

#[tokio::test]
async fn exhausted_retries_fail_workflow() {
    let (step, count) = CountingStep::new("always_fail", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("exhausted").step_with(StepDef::new(step).with_retry(
            RetryPolicy::with_retries(3, Duration::ZERO, ErrorAction::FailWorkflow),
        )),
    );

    let id = engine.start("exhausted", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    // step was attempted exactly 3 times
    assert_eq!(*count.lock().unwrap(), 3);
}

// ── GoTo on exhaustion ────────────────────────────────────────────────────────

#[tokio::test]
async fn exhausted_retries_goto_fallback() {
    let (step, _) = CountingStep::new("failing", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("goto_fallback")
            .step_with(StepDef::new(step).with_retry(RetryPolicy::with_retries(
                2,
                Duration::ZERO,
                ErrorAction::GoTo("fallback"),
            )))
            .step(ConstStep::new("fallback", json!({"used_fallback": true}))),
    );

    let id = engine.start("goto_fallback", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    // After the exhausted GoTo the fallback step completes the workflow.
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["used_fallback"], true);
}

#[tokio::test]
async fn goto_fallback_then_continues_from_there() {
    // failing → (retry exhausted, goto fallback) → fallback → final
    let (step, _) = CountingStep::new("failing", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("goto_chain")
            .step_with(StepDef::new(step).with_retry(RetryPolicy::with_retries(
                1,
                Duration::ZERO,
                ErrorAction::GoTo("fallback"),
            )))
            .step(ConstStep::new("fallback", json!({"fb": true})))
            .step(ConstStep::new("final", json!({"fin": true}))),
    );

    let id = engine.start("goto_chain", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["fb"], true);
    assert_eq!(run.context["fin"], true);
}

// ── Skip on exhaustion ────────────────────────────────────────────────────────

#[tokio::test]
async fn skip_on_exhausted_continues_to_next_step() {
    let (step, _) = CountingStep::new("skipme", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("skip")
            .step_with(StepDef::new(step).with_retry(RetryPolicy::with_retries(
                2,
                Duration::ZERO,
                ErrorAction::Skip,
            )))
            .step(ConstStep::new("after", json!({"continued": true}))),
    );

    let id = engine.start("skip", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["continued"], true);
}

#[tokio::test]
async fn skip_last_step_completes_workflow() {
    let (step, _) = CountingStep::new("last", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("skip_last")
            .step(ConstStep::new("first", json!({"a": 1})))
            .step_with(StepDef::new(step).with_retry(RetryPolicy::with_retries(
                1,
                Duration::ZERO,
                ErrorAction::Skip,
            ))),
    );

    let id = engine.start("skip_last", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["a"], 1);
}

// ── Error message persisted ────────────────────────────────────────────────────

#[tokio::test]
async fn error_message_persisted_on_failure() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("err_msg").step(AlwaysFailStep {
        name: "boom",
        message: "specific error here",
    }));

    let id = engine.start("err_msg", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    let err = run.error.unwrap();
    assert!(err.contains("specific error here"), "got: {err}");
}

#[tokio::test]
async fn many_retries_all_fail_counts_correctly() {
    let (step, count) = CountingStep::new("many", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("many_retry").step_with(StepDef::new(step).with_retry(
            RetryPolicy::with_retries(5, Duration::ZERO, ErrorAction::FailWorkflow),
        )),
    );

    let id = engine.start("many_retry", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(*count.lock().unwrap(), 5);
}

#[tokio::test]
async fn step_succeeds_on_last_allowed_attempt() {
    // With max_attempts=4, fails 3 times then succeeds on attempt 4.
    let (step, count) = CountingStep::new("last_chance", 3, json!({"made_it": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("last_attempt").step_with(StepDef::new(step).with_retry(
            RetryPolicy::with_retries(4, Duration::ZERO, ErrorAction::FailWorkflow),
        )),
    );

    let id = engine.start("last_attempt", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["made_it"], true);
    assert_eq!(*count.lock().unwrap(), 4); // failed 3 times, succeeded on 4th
}

#[tokio::test]
async fn attempt_counter_resets_between_steps() {
    // step1 fails twice then succeeds; step2 should start at attempt=0.
    use std::sync::{Arc, Mutex};

    let (step1, count1) = CountingStep::new("step1", 2, json!({"s1": true}));

    // step2 captures its attempt value
    struct AttemptCapture {
        captured: Arc<Mutex<Option<u32>>>,
    }
    #[async_trait::async_trait]
    impl crate::step::Step for AttemptCapture {
        fn name(&self) -> &'static str {
            "step2"
        }
        async fn run(&self, ctx: &crate::StepContext) -> crate::StepResult {
            *self.captured.lock().unwrap() = Some(ctx.attempt);
            crate::StepResult::Complete(serde_json::json!({"s2": true}))
        }
    }

    let captured_attempt = Arc::new(Mutex::new(None));
    let step2 = AttemptCapture {
        captured: captured_attempt.clone(),
    };

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("reset_attempt")
            .step_with(StepDef::new(step1).with_retry(RetryPolicy::with_retries(
                5,
                Duration::ZERO,
                ErrorAction::FailWorkflow,
            )))
            .step(step2),
    );

    let id = engine.start("reset_attempt", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(*count1.lock().unwrap(), 3); // failed 2 times, succeeded on 3rd
    // step2 should see attempt=0 (reset after step1 completed)
    assert_eq!(*captured_attempt.lock().unwrap(), Some(0));
}

#[tokio::test]
async fn goto_fallback_that_also_fails_then_fails_workflow() {
    // Primary step always fails → GoTo fallback → fallback always fails → workflow fails
    let (primary, _) = CountingStep::new("primary", 999, json!({}));
    let (fallback, _) = CountingStep::new("fallback", 999, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("double_fail")
            .step_with(StepDef::new(primary).with_retry(RetryPolicy::with_retries(
                2,
                Duration::ZERO,
                ErrorAction::GoTo("fallback"),
            )))
            .step_with(StepDef::new(fallback).with_retry(RetryPolicy::with_retries(
                2,
                Duration::ZERO,
                ErrorAction::FailWorkflow,
            ))),
    );

    let id = engine.start("double_fail", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
}

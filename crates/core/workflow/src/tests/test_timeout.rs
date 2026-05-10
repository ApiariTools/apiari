// Signal timeout tests: WaitForSignal with a timeout that fires via tick().

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::time::Duration;

use super::helpers::*;
use crate::{
    IncomingSignal, RunStatus, StepContext, StepDef, StepResult, TimeoutAction, WorkflowDef,
};

// ── Helper: a step that parks waiting for signal "x" with Duration::ZERO timeout ──────

/// A step that immediately times out (timeout=Duration::ZERO, already in the past).
struct TimedSignalStep {
    name: &'static str,
}

#[async_trait]
impl crate::step::Step for TimedSignalStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        StepResult::WaitForSignal {
            pattern: "x".to_string(),
            timeout: Some(Duration::ZERO),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn signal_timeout_fail_workflow() {
    // on_timeout = FailWorkflow (default): tick fires → status is Failed.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timeout_fail")
            .step_with(
                StepDef::new(TimedSignalStep { name: "wait" })
                    .on_timeout(TimeoutAction::FailWorkflow),
            )
            .step(ConstStep::new("after", json!({"after": true}))),
    );

    let id = engine.start("timeout_fail", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine.tick().await.unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    // "after" step should NOT have run
    assert!(run.context.get("after").is_none());
}

#[tokio::test]
async fn signal_timeout_goto() {
    // on_timeout = GoTo("fallback"): tick fires → jumps to fallback → completes.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timeout_goto")
            .step_with(
                StepDef::new(TimedSignalStep { name: "wait" })
                    .on_timeout(TimeoutAction::GoTo("fallback")),
            )
            .step(ConstStep::new("after_normal", json!({"normal": true})))
            .step(ConstStep::new("fallback", json!({"fell_back": true}))),
    );

    let id = engine.start("timeout_goto", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine.tick().await.unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["fell_back"], true);
    // The normal "after" step was skipped
    assert!(run.context.get("normal").is_none());
}

#[tokio::test]
async fn signal_timeout_continue() {
    // on_timeout = Continue: tick fires → advances past wait step → next step runs.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timeout_continue")
            .step_with(
                StepDef::new(TimedSignalStep { name: "wait" }).on_timeout(TimeoutAction::Continue),
            )
            .step(ConstStep::new("after", json!({"continued": true}))),
    );

    let id = engine.start("timeout_continue", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine.tick().await.unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["continued"], true);
}

#[tokio::test]
async fn signal_timeout_goto_nonexistent_step_fails() {
    // on_timeout = GoTo("bad"): the step doesn't exist → workflow fails.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timeout_goto_bad")
            .step_with(
                StepDef::new(TimedSignalStep { name: "wait" })
                    .on_timeout(TimeoutAction::GoTo("nonexistent_step")),
            )
            .step(ConstStep::new("after", json!({}))),
    );

    let id = engine.start("timeout_goto_bad", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine.tick().await.unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert!(
        run.error
            .as_ref()
            .map_or(false, |e| e.contains("nonexistent_step")),
        "expected error about nonexistent_step, got: {:?}",
        run.error
    );
}

#[tokio::test]
async fn signal_arrives_before_timeout_resumes_normally() {
    // Use a far-future timeout (1 hour); signal delivered before tick → completes normally.
    struct FutureTimeoutStep;

    #[async_trait]
    impl crate::step::Step for FutureTimeoutStep {
        fn name(&self) -> &'static str {
            "wait_future"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::WaitForSignal {
                pattern: "arrive".to_string(),
                timeout: Some(Duration::from_secs(3600)), // 1 hour in the future
            }
        }
    }

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("signal_before_timeout")
            .step_with(StepDef::new(FutureTimeoutStep).on_timeout(TimeoutAction::FailWorkflow))
            .step(ConstStep::new("done", json!({"completed": true}))),
    );

    let id = engine
        .start("signal_before_timeout", json!({}))
        .await
        .unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    // Deliver signal before tick
    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "arrive".into(),
            payload: json!({"signal_data": "present"}),
        })
        .await
        .unwrap();

    assert!(resumed.contains(&id));
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["completed"], true);
    assert_eq!(run.context["signal_data"], "present");

    // Now tick — should find nothing to resume
    let tick_resumed = engine.tick().await.unwrap();
    assert!(
        tick_resumed.is_empty(),
        "tick should have nothing to resume after signal delivery"
    );
}

#[tokio::test]
async fn pure_wait_until_is_not_a_signal_timeout() {
    // A pure WaitUntil (no waiting_for) should use the advance path
    // even if the step has an on_timeout set — the on_timeout only applies
    // to WaitForSignal timeouts.
    struct PastWaitStep;

    #[async_trait]
    impl crate::step::Step for PastWaitStep {
        fn name(&self) -> &'static str {
            "wait_until"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            // Park until 1 second in the past (already expired)
            StepResult::WaitUntil(Utc::now() - chrono::Duration::seconds(1))
        }
    }

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("pure_wait_until")
            .step_with(
                // on_timeout=FailWorkflow should NOT be triggered for a pure WaitUntil
                StepDef::new(PastWaitStep).on_timeout(TimeoutAction::FailWorkflow),
            )
            .step(ConstStep::new("after", json!({"woke_up": true}))),
    );

    let id = engine.start("pure_wait_until", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    // Verify it's a pure WaitUntil (no waiting_for)
    let parked = engine.get_run(&id).await.unwrap();
    assert!(parked.waiting_for.is_none());
    assert!(parked.park_until.is_some());

    engine.tick().await.unwrap();

    // Should complete normally, not fail
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["woke_up"], true);
}

#[tokio::test]
async fn signal_timeout_context_is_preserved() {
    // Context accumulated before the wait step must still be present after timeout fires.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timeout_ctx_preserve")
            .step(ConstStep::new(
                "setup",
                json!({"before_wait": "hello", "count": 42}),
            ))
            .step_with(
                StepDef::new(TimedSignalStep { name: "wait" }).on_timeout(TimeoutAction::Continue),
            )
            .step(ConstStep::new("after", json!({"after_wait": true}))),
    );

    let id = engine
        .start("timeout_ctx_preserve", json!({}))
        .await
        .unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    engine.tick().await.unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    // Context from before the wait must be preserved
    assert_eq!(run.context["before_wait"], "hello");
    assert_eq!(run.context["count"], 42);
    assert_eq!(run.context["after_wait"], true);
}

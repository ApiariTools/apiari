// Timer parking: WaitUntil + tick() resumption.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, StepContext, StepResult, WorkflowDef};

struct SleepUntilStep {
    offset: Duration,
}

#[async_trait]
impl crate::step::Step for SleepUntilStep {
    fn name(&self) -> &'static str {
        "sleep"
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        StepResult::WaitUntil(Utc::now() + self.offset)
    }
}

#[tokio::test]
async fn wait_until_parks_run() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timer")
            .step(SleepUntilStep {
                offset: Duration::hours(1),
            })
            .step(ConstStep::new("done", json!({}))),
    );

    let id = engine.start("timer", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Parked);
    assert!(run.park_until.is_some());
    assert!(run.waiting_for.is_none());
}

#[tokio::test]
async fn tick_resumes_expired_timer() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("timer_tick")
            // Park until 1 second in the past (already expired)
            .step(SleepUntilStep {
                offset: Duration::seconds(-1),
            })
            .step(ConstStep::new("done", json!({"woke": true}))),
    );

    let id = engine.start("timer_tick", json!({})).await.unwrap();
    // Run is parked immediately (deadline is in the past but engine just parks first)
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    let resumed = engine.tick().await.unwrap();
    assert!(resumed.contains(&id));

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["woke"], true);
}

#[tokio::test]
async fn tick_does_not_resume_future_timer() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("future_timer")
            .step(SleepUntilStep {
                offset: Duration::hours(10),
            })
            .step(ConstStep::new("done", json!({}))),
    );

    let id = engine.start("future_timer", json!({})).await.unwrap();
    let resumed = engine.tick().await.unwrap();

    assert!(resumed.is_empty());
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);
}

#[tokio::test]
async fn tick_does_not_resume_signal_parked_runs() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("sig_only").step(SignalWaitStep {
        name: "wait",
        pattern: "go",
        timeout: None,
    }));

    let id = engine.start("sig_only", json!({})).await.unwrap();
    let resumed = engine.tick().await.unwrap();

    // Signal-parked run has no park_until so tick should not touch it
    assert!(resumed.is_empty());
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);
}

#[tokio::test]
async fn multiple_expired_timers_all_resume() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("multi_timer")
            .step(SleepUntilStep {
                offset: Duration::seconds(-1),
            })
            .step(ConstStep::new("done", json!({}))),
    );

    let id1 = engine.start("multi_timer", json!({})).await.unwrap();
    let id2 = engine.start("multi_timer", json!({})).await.unwrap();

    let resumed = engine.tick().await.unwrap();
    assert_eq!(resumed.len(), 2);
    assert_eq!(
        engine.get_run(&id1).await.unwrap().status,
        RunStatus::Completed
    );
    assert_eq!(
        engine.get_run(&id2).await.unwrap().status,
        RunStatus::Completed
    );
}

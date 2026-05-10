// Signal parking and resumption.

use serde_json::json;

use super::helpers::*;
use crate::{IncomingSignal, RunStatus, WorkflowDef};

#[tokio::test]
async fn run_parks_waiting_for_signal() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("wait")
            .step(ConstStep::new("before", json!({"before": true})))
            .step(SignalWaitStep {
                name: "wait",
                pattern: "approval",
                timeout: None,
            })
            .step(ConstStep::new("after", json!({"after": true}))),
    );

    let id = engine.start("wait", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.waiting_for.as_deref(), Some("approval"));
    assert_eq!(run.step_name, "wait");
    assert_eq!(run.context["before"], true);
    // "after" not yet set
    assert!(run.context.get("after").is_none());
}

#[tokio::test]
async fn signal_resumes_parked_run() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("resume")
            .step(SignalWaitStep {
                name: "wait",
                pattern: "go",
                timeout: None,
            })
            .step(ConstStep::new("after", json!({"after": true}))),
    );

    let id = engine.start("resume", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "go".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    assert!(resumed.contains(&id));
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["after"], true);
}

#[tokio::test]
async fn signal_payload_merged_into_context() {
    let (capture, cap) = CapturingStep::new("capture");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("payload")
            .step(SignalWaitStep {
                name: "wait",
                pattern: "data",
                timeout: None,
            })
            .step(capture),
    );

    let _id = engine.start("payload", json!({})).await.unwrap();

    engine
        .on_signal(&IncomingSignal {
            pattern: "data".into(),
            payload: json!({"signal_value": 99}),
        })
        .await
        .unwrap();

    let ctx = cap.lock().unwrap().clone().unwrap();
    assert_eq!(ctx["signal_value"], 99);
}

#[tokio::test]
async fn wrong_pattern_does_not_resume() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("wrong_pat")
            .step(SignalWaitStep {
                name: "wait",
                pattern: "approval",
                timeout: None,
            })
            .step(ConstStep::new("after", json!({}))),
    );

    let id = engine.start("wrong_pat", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "rejection".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    assert!(resumed.is_empty());
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);
}

#[tokio::test]
async fn signal_resumes_only_matching_runs() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("multi_park")
            .step(SignalWaitStep {
                name: "wait",
                pattern: "go",
                timeout: None,
            })
            .step(ConstStep::new("done", json!({}))),
    );

    let id1 = engine.start("multi_park", json!({})).await.unwrap();
    let id2 = engine.start("multi_park", json!({})).await.unwrap();

    // Both parked, deliver signal
    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "go".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

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

#[tokio::test]
async fn multiple_wait_steps_sequential() {
    // A workflow that waits for two signals in sequence.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("two_signals")
            .step(SignalWaitStep {
                name: "wait1",
                pattern: "first",
                timeout: None,
            })
            .step(ConstStep::new("middle", json!({"m": 1})))
            .step(SignalWaitStep {
                name: "wait2",
                pattern: "second",
                timeout: None,
            })
            .step(ConstStep::new("end", json!({"end": true}))),
    );

    let id = engine.start("two_signals", json!({})).await.unwrap();
    assert_eq!(
        engine.get_run(&id).await.unwrap().waiting_for.as_deref(),
        Some("first")
    );

    engine
        .on_signal(&IncomingSignal {
            pattern: "first".into(),
            payload: json!({"f": 1}),
        })
        .await
        .unwrap();

    let mid = engine.get_run(&id).await.unwrap();
    assert_eq!(mid.status, RunStatus::Parked);
    assert_eq!(mid.waiting_for.as_deref(), Some("second"));

    engine
        .on_signal(&IncomingSignal {
            pattern: "second".into(),
            payload: json!({"s": 2}),
        })
        .await
        .unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["f"], 1);
    assert_eq!(run.context["s"], 2);
    assert_eq!(run.context["end"], true);
}

#[tokio::test]
async fn parked_run_is_in_parked_list() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("list_park").step(SignalWaitStep {
        name: "wait",
        pattern: "x",
        timeout: None,
    }));

    let id = engine.start("list_park", json!({})).await.unwrap();

    let parked = engine.list_runs(&RunStatus::Parked).await.unwrap();
    assert!(parked.iter().any(|r| r.id == id));
}

#[tokio::test]
async fn signal_to_already_completed_run_is_noop() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("completed_sig").step(ConstStep::new("only", json!({"done": true}))),
    );

    // Start and complete the workflow (no signal wait)
    let id = engine.start("completed_sig", json!({})).await.unwrap();
    assert_eq!(
        engine.get_run(&id).await.unwrap().status,
        RunStatus::Completed
    );

    // Deliver a signal — should not resume anything
    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "irrelevant".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    assert!(resumed.is_empty());
    // Run still completed
    assert_eq!(
        engine.get_run(&id).await.unwrap().status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn signal_with_null_payload_is_ok() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("null_payload")
            .step(SignalWaitStep {
                name: "wait",
                pattern: "go",
                timeout: None,
            })
            .step(ConstStep::new("after", json!({"after": true}))),
    );

    let id = engine
        .start("null_payload", json!({"seed": 1}))
        .await
        .unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    // Deliver signal with null payload — should not crash
    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "go".into(),
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();

    assert!(resumed.contains(&id));
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    // Original context keys still present
    assert_eq!(run.context["seed"], 1);
    assert_eq!(run.context["after"], true);
}

#[tokio::test]
async fn wrong_signal_leaves_run_parked_with_original_pattern() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("stay_parked")
            .step(SignalWaitStep {
                name: "wait",
                pattern: "correct_signal",
                timeout: None,
            })
            .step(ConstStep::new("after", json!({}))),
    );

    let id = engine.start("stay_parked", json!({})).await.unwrap();
    assert_eq!(engine.get_run(&id).await.unwrap().status, RunStatus::Parked);

    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "wrong_signal".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    assert!(resumed.is_empty());
    let run = engine.get_run(&id).await.unwrap();
    // Still parked and still waiting for the original pattern
    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.waiting_for.as_deref(), Some("correct_signal"));
}

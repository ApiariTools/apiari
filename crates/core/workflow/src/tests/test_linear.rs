// Linear workflow: steps execute in order, context accumulates, run completes.

use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, WorkflowDef};

#[tokio::test]
async fn single_step_completes() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("single").step(ConstStep::new("only", json!({"result": "done"}))),
    );

    let id = engine.start("single", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["result"], "done");
}

#[tokio::test]
async fn three_steps_complete_in_order() {
    let (capture_b, cap_b) = CapturingStep::new("b");
    let (capture_c, cap_c) = CapturingStep::new("c");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("linear3")
            .step(ConstStep::new("a", json!({"a": 1})))
            .step(capture_b)
            .step(capture_c),
    );

    let id = engine.start("linear3", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);

    // Step b saw the output of step a
    let ctx_b = cap_b.lock().unwrap().clone().unwrap();
    assert_eq!(ctx_b["a"], 1);

    // Step c saw the output of both a and b (b produced null but merge still works)
    let ctx_c = cap_c.lock().unwrap().clone().unwrap();
    assert_eq!(ctx_c["a"], 1);
}

#[tokio::test]
async fn initial_context_visible_to_first_step() {
    let (capture, cap) = CapturingStep::new("step");
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("ctx").step(capture));

    engine.start("ctx", json!({"seed": "hello"})).await.unwrap();
    let ctx = cap.lock().unwrap().clone().unwrap();
    assert_eq!(ctx["seed"], "hello");
}

#[tokio::test]
async fn context_accumulates_across_steps() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("accum")
            .step(ConstStep::new("s1", json!({"x": 1})))
            .step(ConstStep::new("s2", json!({"y": 2})))
            .step(ConstStep::new("s3", json!({"z": 3}))),
    );

    let id = engine.start("accum", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.context["x"], 1);
    assert_eq!(run.context["y"], 2);
    assert_eq!(run.context["z"], 3);
}

#[tokio::test]
async fn later_step_can_overwrite_earlier_key() {
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("overwrite")
            .step(ConstStep::new("s1", json!({"val": "first"})))
            .step(ConstStep::new("s2", json!({"val": "second"}))),
    );

    let id = engine.start("overwrite", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.context["val"], "second");
}

#[tokio::test]
async fn run_step_name_tracks_progress() {
    // We can't observe intermediate states without hooking the DB; but we can
    // verify that the final state shows the last step name.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("track")
            .step(ConstStep::new("alpha", json!({})))
            .step(ConstStep::new("beta", json!({}))),
    );
    let id = engine.start("track", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    // Final step_name is the last step that ran
    assert_eq!(run.step_name, "beta");
}

#[tokio::test]
async fn multiple_independent_runs_dont_interfere() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("indep").step(ConstStep::new("s", json!({"v": 42}))));

    let id1 = engine.start("indep", json!({"run": 1})).await.unwrap();
    let id2 = engine.start("indep", json!({"run": 2})).await.unwrap();

    let r1 = engine.get_run(&id1).await.unwrap();
    let r2 = engine.get_run(&id2).await.unwrap();

    assert_eq!(r1.status, RunStatus::Completed);
    assert_eq!(r2.status, RunStatus::Completed);
    assert_eq!(r1.context["run"], 1);
    assert_eq!(r2.context["run"], 2);
    assert_ne!(id1, id2);
}

#[tokio::test]
async fn empty_workflow_errors_on_start() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("empty_wf")); // no steps added

    let result = engine.start("empty_wf", json!({})).await;
    assert!(
        result.is_err(),
        "starting an empty workflow should return Err"
    );
}

#[tokio::test]
async fn workflow_with_null_output_still_completes() {
    // A step returning Complete(Value::Null) should be a no-op merge but still complete.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("null_output")
            .step(ConstStep::new("set", json!({"key": "val"})))
            .step(ConstStep::new("null_step", serde_json::Value::Null))
            .step(ConstStep::new("after", json!({"after": true}))),
    );

    let id = engine.start("null_output", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    // Key from before the null step should still be present
    assert_eq!(run.context["key"], "val");
    assert_eq!(run.context["after"], true);
}

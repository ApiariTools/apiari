// GoTo branching: named jumps, revision counter, loop termination.

use async_trait::async_trait;
use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, StepContext, StepResult, WorkflowDef};

#[tokio::test]
async fn goto_jumps_to_named_step() {
    // Workflow: start → decide → end
    // decide GoTos to end after first visit.
    use std::sync::{Arc, Mutex};

    struct DecideStep {
        calls: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl crate::step::Step for DecideStep {
        fn name(&self) -> &'static str {
            "decide"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                StepResult::GoTo {
                    step: "end",
                    output: json!({"jumped": true}),
                }
            } else {
                StepResult::Complete(json!({}))
            }
        }
    }

    let calls = Arc::new(Mutex::new(0u32));
    let decide = DecideStep {
        calls: calls.clone(),
    };

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("jump")
            .step(ConstStep::new("start", json!({"started": true})))
            .step(decide)
            .step(ConstStep::new("end", json!({"ended": true}))),
    );

    let id = engine.start("jump", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["jumped"], true);
    assert_eq!(run.context["ended"], true);
    // decide was only called once (it GoTo'd end, which is after decide, so engine never revisited decide)
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn goto_can_loop_backwards() {
    // Workflow: loop_step → done
    // loop_step GoTos itself twice, then completes.
    let (loop_step, call_count) = GotoStep::new("loop", "loop", 2, json!({"finished": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("looping")
            .max_revisions(10)
            .step(loop_step)
            .step(ConstStep::new("done", json!({"done": true}))),
    );

    let id = engine.start("looping", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["finished"], true);
    assert_eq!(*call_count.lock().unwrap(), 3); // 2 gotos + 1 final complete
    assert_eq!(run.revision, 2); // two GoTo calls
}

#[tokio::test]
async fn goto_output_merges_into_context() {
    let (goto_step, _) = GotoStep::new("g", "done", 1, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("merge_goto")
            .step(goto_step)
            .step(ConstStep::new("done", json!({"final": true}))),
    );

    let id = engine.start("merge_goto", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    // goto_visits was set during the GoTo
    assert_eq!(run.context["goto_visits"], 1);
    assert_eq!(run.context["final"], true);
}

#[tokio::test]
async fn goto_increments_revision() {
    let (loop_step, _) = GotoStep::new("loop", "loop", 3, json!({}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("rev_check")
            .max_revisions(10)
            .step(loop_step)
            .step(ConstStep::new("done", json!({}))),
    );

    let id = engine.start("rev_check", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.revision, 3);
}

#[tokio::test]
async fn goto_to_nonexistent_step_fails_workflow() {
    struct BadGoto;
    #[async_trait]
    impl crate::step::Step for BadGoto {
        fn name(&self) -> &'static str {
            "bad"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::GoTo {
                step: "nonexistent",
                output: json!({}),
            }
        }
    }

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("bad_goto").step(BadGoto));

    let id = engine.start("bad_goto", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.unwrap().contains("nonexistent"));
}

#[tokio::test]
async fn goto_backward_to_earlier_step() {
    // Workflow: start → middle → end
    // middle GoTos "start" once (backward jump), then completes on second visit.
    use std::sync::{Arc, Mutex};

    let (middle, _calls) = GotoStep::new("middle", "start", 1, json!({"middle_done": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("backward_goto")
            .max_revisions(10)
            .step(ConstStep::new("start", json!({"started": true})))
            .step(middle)
            .step(ConstStep::new("end", json!({"ended": true}))),
    );

    let id = engine.start("backward_goto", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["started"], true);
    assert_eq!(run.context["middle_done"], true);
    assert_eq!(run.context["ended"], true);
    // One GoTo backward happened
    assert_eq!(run.revision, 1);
}

#[tokio::test]
async fn goto_to_same_step_loops_correctly() {
    // A step that GoTos itself N times then completes — self-loop.
    let (looper, calls) = GotoStep::new("looper", "looper", 3, json!({"loop_done": true}));

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("self_loop")
            .max_revisions(10)
            .step(looper)
            .step(ConstStep::new("after", json!({"after": true}))),
    );

    let id = engine.start("self_loop", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["loop_done"], true);
    assert_eq!(run.context["after"], true);
    // looper ran 4 times: 3 GoTos + 1 Complete
    assert_eq!(*calls.lock().unwrap(), 4);
    assert_eq!(run.revision, 3);
}

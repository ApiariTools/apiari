// Saga compensation: on_failure steps run when a workflow fails.

use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

use super::helpers::*;
use crate::{RunStatus, StepContext, StepResult, WorkflowDef};

#[tokio::test]
async fn compensation_runs_on_failure() {
    let (comp, ran) = CompensationRecorder::new("rollback");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("saga")
            .step(ConstStep::new("ok", json!({})))
            .step(AlwaysFailStep {
                name: "boom",
                message: "it broke",
            })
            .on_failure(comp),
    );

    let id = engine.start("saga", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert!(*ran.lock().unwrap(), "compensation step should have run");
}

#[tokio::test]
async fn compensation_does_not_run_on_success() {
    let (comp, ran) = CompensationRecorder::new("rollback");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("no_comp")
            .step(ConstStep::new("ok", json!({})))
            .on_failure(comp),
    );

    let id = engine.start("no_comp", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert!(
        !*ran.lock().unwrap(),
        "compensation should NOT run on success"
    );
}

#[tokio::test]
async fn multiple_compensation_steps_all_run() {
    let (comp1, ran1) = CompensationRecorder::new("rollback1");
    let (comp2, ran2) = CompensationRecorder::new("rollback2");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("multi_comp")
            .step(AlwaysFailStep {
                name: "boom",
                message: "fail",
            })
            .on_failure(comp1)
            .on_failure(comp2),
    );

    engine.start("multi_comp", json!({})).await.unwrap();

    assert!(*ran1.lock().unwrap());
    assert!(*ran2.lock().unwrap());
}

#[tokio::test]
async fn compensation_sees_accumulated_context() {
    let (capture, cap) = CapturingStep::new("capture_comp");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("comp_ctx")
            .step(ConstStep::new("setup", json!({"setup_val": 42})))
            .step(AlwaysFailStep {
                name: "fail",
                message: "oops",
            })
            .on_failure(capture),
    );

    engine.start("comp_ctx", json!({})).await.unwrap();

    let ctx = cap.lock().unwrap().clone().unwrap();
    // Compensation step should see context accumulated before the failure
    assert_eq!(ctx["setup_val"], 42);
}

#[tokio::test]
async fn workflow_status_is_failed_after_compensation() {
    let (comp, _) = CompensationRecorder::new("rollback");

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("final_status")
            .step(AlwaysFailStep {
                name: "fail",
                message: "err",
            })
            .on_failure(comp),
    );

    let id = engine.start("final_status", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    // End state must be Failed (not Compensating)
    assert_eq!(run.status, RunStatus::Failed);
}

#[tokio::test]
async fn no_compensation_workflow_fails_cleanly() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("no_comp2").step(AlwaysFailStep {
        name: "fail",
        message: "bare fail",
    }));

    let id = engine.start("no_comp2", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.as_ref().unwrap().contains("bare fail"));
}

#[tokio::test]
async fn compensation_step_failure_is_swallowed() {
    // A compensation step that returns Failed should not panic;
    // final status must still be Failed (not Compensating or anything else).
    struct FailingCompStep;

    #[async_trait]
    impl crate::step::Step for FailingCompStep {
        fn name(&self) -> &'static str {
            "failing_comp"
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            StepResult::Failed(color_eyre::eyre::eyre!("comp step failed"))
        }
    }

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("swallow_comp")
            .step(AlwaysFailStep {
                name: "main_fail",
                message: "main failure",
            })
            .on_failure(FailingCompStep),
    );

    let id = engine.start("swallow_comp", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    // Engine must not panic; workflow must end as Failed
    assert_eq!(run.status, RunStatus::Failed);
}

#[tokio::test]
async fn compensation_steps_run_in_registration_order() {
    // Register two compensation recorders; verify both ran and in order.
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    struct OrderedComp {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl crate::step::Step for OrderedComp {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn run(&self, _ctx: &StepContext) -> StepResult {
            self.order.lock().unwrap().push(self.name);
            StepResult::Complete(json!({}))
        }
    }

    let comp_a = OrderedComp {
        name: "comp_a",
        order: order.clone(),
    };
    let comp_b = OrderedComp {
        name: "comp_b",
        order: order.clone(),
    };

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("ordered_comp")
            .step(AlwaysFailStep {
                name: "fail",
                message: "trigger comp",
            })
            .on_failure(comp_a)
            .on_failure(comp_b),
    );

    engine.start("ordered_comp", json!({})).await.unwrap();

    let ran_order = order.lock().unwrap().clone();
    assert_eq!(ran_order, vec!["comp_a", "comp_b"]);
}

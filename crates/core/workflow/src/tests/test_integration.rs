// End-to-end integration tests: realistic multi-step workflows with signals,
// retries, and concurrency.

use serde_json::json;

use super::helpers::*;
use crate::{IncomingSignal, RunStatus, WorkflowDef};

// ── Workflow definition helper ─────────────────────────────────────────────────

/// Build the "code_review" workflow:
///   1. refine            — ConstStep, outputs {"refined": true}
///   2. dispatch          — ConstStep, outputs {"worker_id": "w-1"}
///   3. wait_branch_ready — SignalWaitStep, pattern "branch_ready"
///   4. review            — CountingStep (fails `fail_first` times)
///   5. wait_approval     — SignalWaitStep, pattern "pr_approved"
///   6. merge             — ConstStep, outputs {"merged": true}
fn code_review_def(
    kind: &'static str,
    review_fail_first: u32,
) -> (WorkflowDef, std::sync::Arc<std::sync::Mutex<u32>>) {
    use crate::{ErrorAction, RetryPolicy, StepDef};
    use std::time::Duration;

    let (review, count) =
        CountingStep::new("review", review_fail_first, json!({"review_passed": true}));

    let def = WorkflowDef::new(kind)
        .step(ConstStep::new("refine", json!({"refined": true})))
        .step(ConstStep::new("dispatch", json!({"worker_id": "w-1"})))
        .step(SignalWaitStep {
            name: "wait_branch_ready",
            pattern: "branch_ready",
            timeout: None,
        })
        .step_with(StepDef::new(review).with_retry(RetryPolicy::with_retries(
            5,
            Duration::ZERO,
            ErrorAction::FailWorkflow,
        )))
        .step(SignalWaitStep {
            name: "wait_approval",
            pattern: "pr_approved",
            timeout: None,
        })
        .step(ConstStep::new("merge", json!({"merged": true})));

    (def, count)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn code_review_happy_path() {
    let (def, _count) = code_review_def("cr_happy", 0); // review succeeds immediately

    let mut engine = mem_engine();
    engine.register(def);

    let id = engine.start("cr_happy", json!({})).await.unwrap();

    // After start: should be parked at "wait_branch_ready"
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.waiting_for.as_deref(), Some("branch_ready"));
    assert_eq!(run.context["refined"], true);
    assert_eq!(run.context["worker_id"], "w-1");

    // Deliver "branch_ready"
    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "branch_ready".into(),
            payload: json!({"branch": "feat/my-feature"}),
        })
        .await
        .unwrap();
    assert!(resumed.contains(&id));

    // Should now be parked at "wait_approval"
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.waiting_for.as_deref(), Some("pr_approved"));
    assert_eq!(run.context["review_passed"], true);
    assert_eq!(run.context["branch"], "feat/my-feature");

    // Deliver "pr_approved"
    let resumed = engine
        .on_signal(&IncomingSignal {
            pattern: "pr_approved".into(),
            payload: json!({"approver": "alice"}),
        })
        .await
        .unwrap();
    assert!(resumed.contains(&id));

    // Final state: completed with all keys present
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["refined"], true);
    assert_eq!(run.context["worker_id"], "w-1");
    assert_eq!(run.context["branch"], "feat/my-feature");
    assert_eq!(run.context["review_passed"], true);
    assert_eq!(run.context["approver"], "alice");
    assert_eq!(run.context["merged"], true);
}

#[tokio::test]
async fn code_review_review_fails_then_succeeds() {
    // review step fails 2 times then succeeds on attempt 3.
    let (def, count) = code_review_def("cr_retry", 2);

    let mut engine = mem_engine();
    engine.register(def);

    let id = engine.start("cr_retry", json!({})).await.unwrap();

    // Deliver "branch_ready" to unpark and run the review step
    engine
        .on_signal(&IncomingSignal {
            pattern: "branch_ready".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    // review failed 2× then succeeded; now parked at "wait_approval"
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.waiting_for.as_deref(), Some("pr_approved"));
    assert_eq!(*count.lock().unwrap(), 3); // 2 failures + 1 success

    // Complete the rest of the workflow
    engine
        .on_signal(&IncomingSignal {
            pattern: "pr_approved".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.context["merged"], true);
}

#[tokio::test]
async fn code_review_waits_at_each_signal() {
    let (def, _) = code_review_def("cr_waits", 0);

    let mut engine = mem_engine();
    engine.register(def);

    let id = engine.start("cr_waits", json!({})).await.unwrap();

    // First park: at wait_branch_ready
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.step_name, "wait_branch_ready");

    engine
        .on_signal(&IncomingSignal {
            pattern: "branch_ready".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    // Second park: at wait_approval
    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Parked);
    assert_eq!(run.step_name, "wait_approval");

    engine
        .on_signal(&IncomingSignal {
            pattern: "pr_approved".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    let run = engine.get_run(&id).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn two_concurrent_workflows_independent() {
    // Two "code_review" runs — signals delivered to each independently;
    // verify they don't interfere.
    let (def_a, _) = code_review_def("cr_concurrent_a", 0);
    let (def_b, _) = code_review_def("cr_concurrent_b", 0);

    let mut engine = mem_engine();
    engine.register(def_a);
    engine.register(def_b);

    let id_a = engine
        .start("cr_concurrent_a", json!({"run": "a"}))
        .await
        .unwrap();
    let id_b = engine
        .start("cr_concurrent_b", json!({"run": "b"}))
        .await
        .unwrap();

    // Both parked at their first signal
    assert_eq!(
        engine.get_run(&id_a).await.unwrap().status,
        RunStatus::Parked
    );
    assert_eq!(
        engine.get_run(&id_b).await.unwrap().status,
        RunStatus::Parked
    );

    // Deliver branch_ready to run A only
    // Since pattern matches by exact string and these are different workflow kinds,
    // both are using the same pattern "branch_ready" — deliver once, both may resume.
    // We need to verify the context keys are correct per run.
    engine
        .on_signal(&IncomingSignal {
            pattern: "branch_ready".into(),
            payload: json!({"who": "both"}),
        })
        .await
        .unwrap();

    // Both should now be parked at wait_approval
    let run_a = engine.get_run(&id_a).await.unwrap();
    let run_b = engine.get_run(&id_b).await.unwrap();
    assert_eq!(run_a.status, RunStatus::Parked);
    assert_eq!(run_b.status, RunStatus::Parked);
    assert_eq!(run_a.waiting_for.as_deref(), Some("pr_approved"));
    assert_eq!(run_b.waiting_for.as_deref(), Some("pr_approved"));

    // Each run preserved its own seed context
    assert_eq!(run_a.context["run"], "a");
    assert_eq!(run_b.context["run"], "b");

    // Deliver pr_approved to both
    engine
        .on_signal(&IncomingSignal {
            pattern: "pr_approved".into(),
            payload: json!({}),
        })
        .await
        .unwrap();

    let run_a = engine.get_run(&id_a).await.unwrap();
    let run_b = engine.get_run(&id_b).await.unwrap();
    assert_eq!(run_a.status, RunStatus::Completed);
    assert_eq!(run_b.status, RunStatus::Completed);
    assert_eq!(run_a.context["merged"], true);
    assert_eq!(run_b.context["merged"], true);

    // Contexts remain independent
    assert_eq!(run_a.context["run"], "a");
    assert_eq!(run_b.context["run"], "b");
}

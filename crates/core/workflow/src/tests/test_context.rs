// Context: typed get(), key accumulation, step-to-step data flow.

use async_trait::async_trait;
use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, StepContext, StepResult, WorkflowDef};

#[tokio::test]
async fn step_can_read_typed_value_from_context() {
    struct ReadStep {
        found: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
    }

    #[async_trait]
    impl crate::step::Step for ReadStep {
        fn name(&self) -> &'static str {
            "read"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            *self.found.lock().unwrap() = ctx.get::<i64>("count");
            StepResult::Complete(json!({}))
        }
    }

    let found = std::sync::Arc::new(std::sync::Mutex::new(None));
    let read = ReadStep {
        found: found.clone(),
    };

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("typed_read")
            .step(ConstStep::new("set", json!({"count": 7})))
            .step(read),
    );

    engine.start("typed_read", json!({})).await.unwrap();
    assert_eq!(*found.lock().unwrap(), Some(7));
}

#[tokio::test]
async fn get_returns_none_for_missing_key() {
    struct CheckStep;
    #[async_trait]
    impl crate::step::Step for CheckStep {
        fn name(&self) -> &'static str {
            "check"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            assert!(ctx.get::<String>("nonexistent").is_none());
            StepResult::Complete(json!({}))
        }
    }

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("missing_key").step(CheckStep));
    let id = engine.start("missing_key", json!({})).await.unwrap();
    assert_eq!(
        engine.get_run(&id).await.unwrap().status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn initial_context_typed_read() {
    struct ReadStep {
        found: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }
    #[async_trait]
    impl crate::step::Step for ReadStep {
        fn name(&self) -> &'static str {
            "read"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            *self.found.lock().unwrap() = ctx.get::<String>("name");
            StepResult::Complete(json!({}))
        }
    }

    let found = std::sync::Arc::new(std::sync::Mutex::new(None));
    let read = ReadStep {
        found: found.clone(),
    };

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("init_ctx").step(read));

    engine
        .start("init_ctx", json!({"name": "alice"}))
        .await
        .unwrap();
    assert_eq!(*found.lock().unwrap(), Some("alice".to_string()));
}

#[tokio::test]
async fn context_not_shared_between_different_runs() {
    let (cap1, captured1) = CapturingStep::new("cap");
    let (cap2, captured2) = CapturingStep::new("cap");

    // Two separate workflow kinds so we can register independently
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("iso_a")
            .step(ConstStep::new("set", json!({"key": "from_a"})))
            .step(cap1),
    );
    engine.register(
        WorkflowDef::new("iso_b")
            .step(ConstStep::new("set", json!({"key": "from_b"})))
            .step(cap2),
    );

    engine.start("iso_a", json!({})).await.unwrap();
    engine.start("iso_b", json!({})).await.unwrap();

    assert_eq!(captured1.lock().unwrap().as_ref().unwrap()["key"], "from_a");
    assert_eq!(captured2.lock().unwrap().as_ref().unwrap()["key"], "from_b");
}

#[tokio::test]
async fn run_id_and_kind_available_in_step_context() {
    use std::sync::{Arc, Mutex};

    struct MetaCapture {
        data: Arc<Mutex<Option<(String, String)>>>,
    }
    #[async_trait]
    impl crate::step::Step for MetaCapture {
        fn name(&self) -> &'static str {
            "meta"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            *self.data.lock().unwrap() = Some((ctx.run_id.clone(), ctx.workflow_kind.clone()));
            StepResult::Complete(json!({}))
        }
    }

    let data = Arc::new(Mutex::new(None));
    let step = MetaCapture { data: data.clone() };

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("meta_wf").step(step));

    let id = engine.start("meta_wf", json!({})).await.unwrap();

    let (run_id, kind) = data.lock().unwrap().clone().unwrap();
    assert_eq!(run_id, id);
    assert_eq!(kind, "meta_wf");
}

#[tokio::test]
async fn context_get_with_wrong_type_returns_none() {
    // Store a string, try to read as i64 — should return None.
    struct CheckStep;
    #[async_trait]
    impl crate::step::Step for CheckStep {
        fn name(&self) -> &'static str {
            "check_type"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            // "hello" is a string, not an i64
            let val = ctx.get::<i64>("greeting");
            assert!(val.is_none(), "wrong type should return None");
            StepResult::Complete(json!({}))
        }
    }

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("wrong_type")
            .step(ConstStep::new("set", json!({"greeting": "hello"})))
            .step(CheckStep),
    );

    let id = engine.start("wrong_type", json!({})).await.unwrap();
    assert_eq!(
        engine.get_run(&id).await.unwrap().status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn context_merge_with_array_value() {
    // A step outputs {"items": [1,2,3]}; the next step can read the array.
    struct ReadArray {
        found: std::sync::Arc<std::sync::Mutex<Option<Vec<i64>>>>,
    }
    #[async_trait]
    impl crate::step::Step for ReadArray {
        fn name(&self) -> &'static str {
            "read_arr"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            *self.found.lock().unwrap() = ctx.get::<Vec<i64>>("items");
            StepResult::Complete(json!({}))
        }
    }

    let found = std::sync::Arc::new(std::sync::Mutex::new(None));
    let reader = ReadArray {
        found: found.clone(),
    };

    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("arr_ctx")
            .step(ConstStep::new("produce", json!({"items": [1, 2, 3]})))
            .step(reader),
    );

    engine.start("arr_ctx", json!({})).await.unwrap();
    assert_eq!(*found.lock().unwrap(), Some(vec![1i64, 2, 3]));
}

#[tokio::test]
async fn context_shallow_merge_replaces_nested_objects() {
    // Base has {"a": {"x": 1}}, patch has {"a": {"y": 2}}.
    // Shallow merge replaces the nested object entirely, result is {"a": {"y": 2}}.
    let mut engine = mem_engine();
    engine.register(
        WorkflowDef::new("shallow_merge")
            .step(ConstStep::new("base", json!({"a": {"x": 1}})))
            .step(ConstStep::new("patch", json!({"a": {"y": 2}}))),
    );

    let id = engine.start("shallow_merge", json!({})).await.unwrap();
    let run = engine.get_run(&id).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    // Shallow merge: "a" key is replaced entirely
    assert_eq!(run.context["a"], json!({"y": 2}));
    // "x" should not be present under "a"
    assert!(run.context["a"].get("x").is_none());
}

#[tokio::test]
async fn step_name_in_context() {
    // ctx.step_name matches the step's name field.
    struct NameCapture {
        captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }
    #[async_trait]
    impl crate::step::Step for NameCapture {
        fn name(&self) -> &'static str {
            "named_step"
        }
        async fn run(&self, ctx: &StepContext) -> StepResult {
            *self.captured.lock().unwrap() = Some(ctx.step_name.clone());
            StepResult::Complete(json!({}))
        }
    }

    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let step = NameCapture {
        captured: captured.clone(),
    };

    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("name_ctx_wf").step(step));

    engine.start("name_ctx_wf", json!({})).await.unwrap();
    assert_eq!(captured.lock().unwrap().as_deref(), Some("named_step"));
}

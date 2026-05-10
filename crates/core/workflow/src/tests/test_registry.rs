// Registry: unknown kinds, multiple registered workflows, listing runs.

use serde_json::json;

use super::helpers::*;
use crate::{RunStatus, WorkflowDef, WorkflowError};

#[tokio::test]
async fn unknown_kind_returns_error() {
    let engine = mem_engine();
    let err = engine.start("nonexistent", json!({})).await.unwrap_err();
    assert!(matches!(err, WorkflowError::UnknownKind(_)));
}

#[tokio::test]
async fn multiple_kinds_coexist() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("alpha").step(ConstStep::new("a", json!({"kind": "alpha"}))));
    engine.register(WorkflowDef::new("beta").step(ConstStep::new("b", json!({"kind": "beta"}))));

    let id_a = engine.start("alpha", json!({})).await.unwrap();
    let id_b = engine.start("beta", json!({})).await.unwrap();

    assert_eq!(
        engine.get_run(&id_a).await.unwrap().context["kind"],
        "alpha"
    );
    assert_eq!(engine.get_run(&id_b).await.unwrap().context["kind"], "beta");
}

#[tokio::test]
async fn list_runs_by_status_completed() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("listable").step(ConstStep::new("done", json!({}))));

    let id = engine.start("listable", json!({})).await.unwrap();
    let completed = engine.list_runs(&RunStatus::Completed).await.unwrap();
    assert!(completed.iter().any(|r| r.id == id));
}

#[tokio::test]
async fn list_runs_by_status_failed() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("faillist").step(AlwaysFailStep {
        name: "f",
        message: "err",
    }));

    let id = engine.start("faillist", json!({})).await.unwrap();
    let failed = engine.list_runs(&RunStatus::Failed).await.unwrap();
    assert!(failed.iter().any(|r| r.id == id));
}

#[tokio::test]
async fn get_run_not_found_returns_error() {
    let engine = mem_engine();
    let err = engine.get_run("nonexistent-id").await.unwrap_err();
    assert!(matches!(
        err,
        WorkflowError::RunNotFound(_) | WorkflowError::Db(_)
    ));
}

#[tokio::test]
async fn completed_runs_not_in_parked_list() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("not_parked").step(ConstStep::new("done", json!({}))));

    engine.start("not_parked", json!({})).await.unwrap();
    let parked = engine.list_runs(&RunStatus::Parked).await.unwrap();
    assert!(parked.is_empty());
}

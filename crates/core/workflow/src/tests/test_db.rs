// DB layer: persistence, crash recovery simulation, WAL mode.

use chrono::Utc;
use serde_json::json;

use super::helpers::*;
use crate::db::WorkflowDb;
use crate::types::WorkflowRun;
use crate::{RunStatus, WorkflowDef};

#[test]
fn db_open_in_memory_creates_schema() {
    let db = WorkflowDb::open(":memory:").unwrap();
    // Insert and retrieve a run to verify schema exists
    let now = Utc::now();
    let run = WorkflowRun {
        id: "test-1".into(),
        kind: "wf".into(),
        step_name: "s1".into(),
        status: RunStatus::Running,
        context: json!({"k": "v"}),
        waiting_for: None,
        park_until: None,
        attempt: 0,
        revision: 0,
        error: None,
        created_at: now,
        updated_at: now,
    };
    db.insert(&run).unwrap();
    let retrieved = db.get("test-1").unwrap();
    assert_eq!(retrieved.id, "test-1");
    assert_eq!(retrieved.context["k"], "v");
}

#[test]
fn db_update_persists_changes() {
    let db = WorkflowDb::open(":memory:").unwrap();
    let now = Utc::now();
    let mut run = WorkflowRun {
        id: "upd-1".into(),
        kind: "wf".into(),
        step_name: "s1".into(),
        status: RunStatus::Running,
        context: json!({}),
        waiting_for: None,
        park_until: None,
        attempt: 0,
        revision: 0,
        error: None,
        created_at: now,
        updated_at: now,
    };
    db.insert(&run).unwrap();

    run.status = RunStatus::Completed;
    run.context = json!({"result": "ok"});
    run.step_name = "s2".into();
    db.update(&run).unwrap();

    let retrieved = db.get("upd-1").unwrap();
    assert_eq!(retrieved.status, RunStatus::Completed);
    assert_eq!(retrieved.context["result"], "ok");
    assert_eq!(retrieved.step_name, "s2");
}

#[test]
fn db_find_parked_by_signal_matches_correctly() {
    let db = WorkflowDb::open(":memory:").unwrap();
    let now = Utc::now();

    let parked = WorkflowRun {
        id: "park-1".into(),
        kind: "wf".into(),
        step_name: "wait".into(),
        status: RunStatus::Parked,
        context: json!({}),
        waiting_for: Some("approval".into()),
        park_until: None,
        attempt: 0,
        revision: 0,
        error: None,
        created_at: now,
        updated_at: now,
    };
    let not_parked = WorkflowRun {
        id: "run-2".into(),
        status: RunStatus::Running,
        waiting_for: None,
        ..parked.clone()
    };
    let wrong_signal = WorkflowRun {
        id: "park-3".into(),
        status: RunStatus::Parked,
        waiting_for: Some("rejection".into()),
        ..parked.clone()
    };

    db.insert(&parked).unwrap();
    db.insert(&not_parked).unwrap();
    db.insert(&wrong_signal).unwrap();

    let results = db.find_parked_by_signal("approval").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "park-1");
}

#[test]
fn db_find_timed_out_matches_expired() {
    use chrono::Duration;

    let db = WorkflowDb::open(":memory:").unwrap();
    let now = Utc::now();

    let expired = WorkflowRun {
        id: "exp-1".into(),
        kind: "wf".into(),
        step_name: "wait".into(),
        status: RunStatus::Parked,
        context: json!({}),
        waiting_for: None,
        park_until: Some(now - Duration::hours(1)), // in the past
        attempt: 0,
        revision: 0,
        error: None,
        created_at: now,
        updated_at: now,
    };
    let future = WorkflowRun {
        id: "fut-1".into(),
        park_until: Some(now + Duration::hours(1)), // in the future
        ..expired.clone()
    };
    let no_timer = WorkflowRun {
        id: "notimer".into(),
        park_until: None,
        ..expired.clone()
    };

    db.insert(&expired).unwrap();
    db.insert(&future).unwrap();
    db.insert(&no_timer).unwrap();

    let results = db.find_timed_out(&now).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "exp-1");
}

/// Simulate crash recovery: start a run, "crash" (drop engine), recreate engine,
/// and verify the DB state is still correct.
#[tokio::test]
async fn crash_recovery_run_persisted_in_db() {
    use tempfile::NamedTempFile;

    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();

    // Phase 1: start a workflow that parks
    {
        let mut engine = crate::WorkflowEngine::new(&path).unwrap();
        engine.register(
            WorkflowDef::new("crash_wf")
                .step(SignalWaitStep {
                    name: "wait",
                    pattern: "resume",
                    timeout: None,
                })
                .step(ConstStep::new("done", json!({}))),
        );
        engine.start("crash_wf", json!({"seed": 1})).await.unwrap();
    } // engine dropped — simulates crash

    // Phase 2: new engine, verify state
    {
        let db = WorkflowDb::open(&path).unwrap();
        let parked = db.find_parked_by_signal("resume").unwrap();
        assert_eq!(parked.len(), 1);
        assert_eq!(parked[0].context["seed"], 1);
        assert_eq!(parked[0].status, RunStatus::Parked);
    }
}

#[tokio::test]
async fn run_ids_are_unique_uuids() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("uuid_check").step(ConstStep::new("s", json!({}))));

    let ids: Vec<_> =
        futures::future::join_all((0..10).map(|_| engine.start("uuid_check", json!({}))))
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 10, "all IDs must be unique");
}

#[test]
fn db_context_roundtrip_complex_json() {
    let db = WorkflowDb::open(":memory:").unwrap();
    let now = Utc::now();
    let complex_ctx = json!({
        "nested": {"a": [1, 2, 3]},
        "flag": true,
        "count": 42,
        "label": "hello"
    });

    let run = WorkflowRun {
        id: "complex-1".into(),
        kind: "wf".into(),
        step_name: "s1".into(),
        status: RunStatus::Running,
        context: complex_ctx.clone(),
        waiting_for: None,
        park_until: None,
        attempt: 0,
        revision: 0,
        error: None,
        created_at: now,
        updated_at: now,
    };
    db.insert(&run).unwrap();
    let retrieved = db.get("complex-1").unwrap();

    assert_eq!(retrieved.context["nested"]["a"], json!([1, 2, 3]));
    assert_eq!(retrieved.context["flag"], true);
    assert_eq!(retrieved.context["count"], 42);
    assert_eq!(retrieved.context["label"], "hello");
}

#[tokio::test]
async fn db_list_by_status_returns_correct_subset() {
    let mut engine = mem_engine();
    engine.register(WorkflowDef::new("listable_wf").step(ConstStep::new("done", json!({}))));
    engine.register(WorkflowDef::new("parked_wf").step(SignalWaitStep {
        name: "wait",
        pattern: "x",
        timeout: None,
    }));
    engine.register(WorkflowDef::new("failed_wf").step(AlwaysFailStep {
        name: "fail",
        message: "fail",
    }));

    let completed_id = engine.start("listable_wf", json!({})).await.unwrap();
    let parked_id = engine.start("parked_wf", json!({})).await.unwrap();
    let failed_id = engine.start("failed_wf", json!({})).await.unwrap();

    let completed = engine.list_runs(&RunStatus::Completed).await.unwrap();
    let parked = engine.list_runs(&RunStatus::Parked).await.unwrap();
    let failed = engine.list_runs(&RunStatus::Failed).await.unwrap();

    assert!(completed.iter().any(|r| r.id == completed_id));
    assert!(!completed.iter().any(|r| r.id == parked_id));
    assert!(!completed.iter().any(|r| r.id == failed_id));

    assert!(parked.iter().any(|r| r.id == parked_id));
    assert!(!parked.iter().any(|r| r.id == completed_id));
    assert!(!parked.iter().any(|r| r.id == failed_id));

    assert!(failed.iter().any(|r| r.id == failed_id));
    assert!(!failed.iter().any(|r| r.id == completed_id));
    assert!(!failed.iter().any(|r| r.id == parked_id));
}

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::Utc;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    db::WorkflowDb,
    step::StepContext,
    types::{
        ErrorAction, IncomingSignal, Result, RunStatus, StepResult, TimeoutAction, WorkflowError,
        WorkflowRun,
    },
    workflow::WorkflowDef,
};

/// Durable workflow engine backed by SQLite.
///
/// - `start()` creates a new run and immediately advances it.
/// - `on_signal()` resumes any parked run whose `waiting_for` matches.
/// - `tick()` resumes time-parked runs whose deadline has passed.
/// - All state is persisted before/after every step transition.
pub struct WorkflowEngine {
    db: Arc<Mutex<WorkflowDb>>,
    registry: HashMap<&'static str, WorkflowDef>,
}

impl WorkflowEngine {
    /// Create an engine backed by a DB at `path` (use `":memory:"` for tests).
    pub fn new(path: &str) -> Result<Self> {
        let db = WorkflowDb::open(path)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            registry: HashMap::new(),
        })
    }

    /// Register a workflow definition. Panics if a workflow with the same kind is already registered.
    pub fn register(&mut self, def: WorkflowDef) {
        assert!(
            !self.registry.contains_key(def.kind),
            "workflow kind already registered: {}",
            def.kind
        );
        self.registry.insert(def.kind, def);
    }

    /// Start a new workflow run and immediately advance it.
    ///
    /// Returns the run ID.
    pub async fn start(&self, kind: &str, initial: Value) -> Result<String> {
        let def = self
            .registry
            .get(kind)
            .ok_or_else(|| WorkflowError::UnknownKind(kind.into()))?;
        let first_step = def
            .steps
            .first()
            .ok_or_else(|| WorkflowError::StepNotFound("(first)".into()))?;

        let now = Utc::now();
        let run = WorkflowRun {
            id: Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            step_name: first_step.name().to_string(),
            status: RunStatus::Running,
            context: initial,
            waiting_for: None,
            park_until: None,
            attempt: 0,
            revision: 0,
            error: None,
            created_at: now,
            updated_at: now,
        };

        {
            let db = self.db.lock().await;
            db.insert(&run)?;
        }

        info!(run_id = %run.id, kind, "workflow started");
        let run_id = run.id.clone();
        if let Err(e) = self.advance(run).await {
            warn!(run_id = %run_id, "advance error (run persisted as failed): {e}");
        }
        Ok(run_id)
    }

    /// Deliver a signal and resume any parked run whose `waiting_for` matches.
    ///
    /// Returns the IDs of runs that were resumed.
    pub async fn on_signal(&self, signal: &IncomingSignal) -> Result<Vec<String>> {
        let parked = {
            let db = self.db.lock().await;
            db.find_parked_by_signal(&signal.pattern)?
        };

        let mut resumed = Vec::new();
        for mut run in parked {
            info!(run_id = %run.id, pattern = %signal.pattern, "signal resume");
            merge_context(&mut run.context, &signal.payload);
            run.waiting_for = None;
            run.park_until = None;
            run.status = RunStatus::Running;
            run.updated_at = Utc::now();

            // Advance past the step that was waiting for this signal.
            run = self.advance_step_name(run);

            {
                let db = self.db.lock().await;
                db.update(&run)?;
            }
            resumed.push(run.id.clone());
            if run.status == RunStatus::Completed {
                info!(run_id = %run.id, "workflow completed at last wait step");
                continue;
            }
            let id = run.id.clone();
            if let Err(e) = self.advance(run).await {
                warn!(run_id = %id, "advance error after signal resume: {e}");
            }
        }
        Ok(resumed)
    }

    /// Resume time-parked runs whose deadline has passed.
    ///
    /// Call this periodically (e.g. every 30s) to fire WaitUntil timeouts.
    /// Returns the IDs of runs that were resumed.
    pub async fn tick(&self) -> Result<Vec<String>> {
        let now = Utc::now();
        let timed_out = {
            let db = self.db.lock().await;
            db.find_timed_out(&now)?
        };

        let mut resumed = Vec::new();
        for mut run in timed_out {
            info!(run_id = %run.id, "time-park expired, resuming");

            // Capture before clearing — distinguishes signal-timeout from pure WaitUntil.
            let was_waiting_for = run.waiting_for.take();
            run.park_until = None;
            run.status = RunStatus::Running;
            run.updated_at = Utc::now();

            if was_waiting_for.is_some() {
                // This was a WaitForSignal with a timeout — the signal never arrived.
                // Apply the step's on_timeout action instead of just advancing.
                let action = self.step_on_timeout(&run);
                match action {
                    TimeoutAction::FailWorkflow => {
                        let msg = format!(
                            "signal timeout: waiting_for pattern expired for step '{}'",
                            run.step_name
                        );
                        {
                            let db = self.db.lock().await;
                            db.update(&run)?;
                        }
                        self.fail_run(&mut run, &msg).await?;
                        resumed.push(run.id.clone());
                        continue;
                    }
                    TimeoutAction::GoTo(target) => {
                        if self
                            .registry
                            .get(run.kind.as_str())
                            .and_then(|d| d.step_index(target))
                            .is_none()
                        {
                            let msg = format!("on_timeout GoTo target not found: {target}");
                            {
                                let db = self.db.lock().await;
                                db.update(&run)?;
                            }
                            self.fail_run(&mut run, &msg).await?;
                            resumed.push(run.id.clone());
                            continue;
                        }
                        run.step_name = target.to_string();
                        run.attempt = 0;
                        {
                            let db = self.db.lock().await;
                            db.update(&run)?;
                        }
                        resumed.push(run.id.clone());
                        let id = run.id.clone();
                        if let Err(e) = self.advance(run).await {
                            warn!(run_id = %id, "advance error after signal timeout GoTo: {e}");
                        }
                        continue;
                    }
                    TimeoutAction::Continue => {
                        // Fall through to the normal advance-past-step path below.
                    }
                }
            }

            // Pure WaitUntil (no waiting_for) OR TimeoutAction::Continue:
            // advance past the wait step and continue execution.
            run = self.advance_step_name(run);

            {
                let db = self.db.lock().await;
                db.update(&run)?;
            }
            resumed.push(run.id.clone());
            if run.status == RunStatus::Completed {
                info!(run_id = %run.id, "workflow completed at last wait-until step");
                continue;
            }
            let id = run.id.clone();
            if let Err(e) = self.advance(run).await {
                warn!(run_id = %id, "advance error after tick resume: {e}");
            }
        }
        Ok(resumed)
    }

    /// Move `run.step_name` forward by one step (used when resuming from a park).
    ///
    /// If already on the last step, marks the run as Completed and returns it as-is
    /// (the caller should persist and skip `advance()`).
    fn advance_step_name(&self, mut run: WorkflowRun) -> WorkflowRun {
        let Some(def) = self.registry.get(run.kind.as_str()) else {
            return run;
        };
        let Some(idx) = def.step_index(&run.step_name) else {
            return run;
        };
        if let Some(next) = def.steps.get(idx + 1) {
            run.step_name = next.name().to_string();
        } else {
            run.status = RunStatus::Completed;
        }
        run
    }

    /// Load a run from the DB by ID.
    pub async fn get_run(&self, run_id: &str) -> Result<WorkflowRun> {
        let db = self.db.lock().await;
        db.get(run_id)
    }

    /// List runs by status.
    pub async fn list_runs(&self, status: &RunStatus) -> Result<Vec<WorkflowRun>> {
        let db = self.db.lock().await;
        db.list_by_status(status)
    }

    // ── Internal ────────────────────────────────────────────────────────────

    /// Look up the `on_timeout` action for the current step of `run`.
    ///
    /// Returns `TimeoutAction::FailWorkflow` if the def or step cannot be found.
    fn step_on_timeout(&self, run: &WorkflowRun) -> TimeoutAction {
        let Some(def) = self.registry.get(run.kind.as_str()) else {
            return TimeoutAction::FailWorkflow;
        };
        let Some(idx) = def.step_index(&run.step_name) else {
            return TimeoutAction::FailWorkflow;
        };
        def.steps[idx].on_timeout.clone()
    }

    /// Drive `run` forward until it parks, completes, or fails.
    async fn advance(&self, mut run: WorkflowRun) -> Result<()> {
        loop {
            let def = match self.registry.get(run.kind.as_str()) {
                Some(d) => d,
                None => {
                    let msg = format!("unknown kind: {}", run.kind);
                    self.fail_run(&mut run, &msg).await?;
                    return Err(WorkflowError::UnknownKind(run.kind.clone()));
                }
            };

            let step_idx = match def.step_index(&run.step_name) {
                Some(i) => i,
                None => {
                    let msg = format!("step not found: {}", run.step_name);
                    self.fail_run(&mut run, &msg).await?;
                    return Err(WorkflowError::StepNotFound(run.step_name.clone()));
                }
            };

            let step_def = &def.steps[step_idx];
            let ctx = StepContext::new(
                run.id.clone(),
                run.kind.clone(),
                run.step_name.clone(),
                run.attempt,
                run.context.clone(),
            );

            debug!(run_id = %run.id, step = %run.step_name, attempt = run.attempt, "executing step");
            let result = step_def.step.run(&ctx).await;

            match result {
                StepResult::Complete(output) => {
                    merge_context(&mut run.context, &output);
                    run.attempt = 0;

                    if let Some(next) = def.steps.get(step_idx + 1) {
                        run.step_name = next.name().to_string();
                        run.updated_at = Utc::now();
                        {
                            let db = self.db.lock().await;
                            db.update(&run)?;
                        }
                        // Continue loop — advance to next step immediately.
                    } else {
                        // All steps done.
                        run.status = RunStatus::Completed;
                        run.updated_at = Utc::now();
                        {
                            let db = self.db.lock().await;
                            db.update(&run)?;
                        }
                        info!(run_id = %run.id, "workflow completed");
                        return Ok(());
                    }
                }

                StepResult::GoTo { step, output } => {
                    run.revision += 1;
                    if run.revision > def.max_revisions {
                        let err = format!(
                            "cycle detected: GoTo '{}' at revision {}",
                            step, run.revision
                        );
                        self.fail_run(&mut run, &err).await?;
                        return Err(WorkflowError::CycleDetected {
                            run_id: run.id,
                            step: step.to_string(),
                            revision: run.revision,
                        });
                    }

                    if def.step_index(step).is_none() {
                        let err = format!("GoTo target not found: {step}");
                        self.fail_run(&mut run, &err).await?;
                        return Err(WorkflowError::StepNotFound(step.to_string()));
                    }

                    merge_context(&mut run.context, &output);
                    run.step_name = step.to_string();
                    run.attempt = 0;
                    run.updated_at = Utc::now();
                    {
                        let db = self.db.lock().await;
                        db.update(&run)?;
                    }
                    debug!(run_id = %run.id, step, revision = run.revision, "GoTo");
                    // Continue loop.
                }

                StepResult::WaitForSignal { pattern, timeout } => {
                    run.status = RunStatus::Parked;
                    run.waiting_for = Some(pattern.clone());
                    run.park_until = timeout.map(|d| {
                        Utc::now()
                            + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::seconds(0))
                    });
                    run.updated_at = Utc::now();
                    {
                        let db = self.db.lock().await;
                        db.update(&run)?;
                    }
                    info!(run_id = %run.id, %pattern, "workflow parked on signal");
                    return Ok(());
                }

                StepResult::WaitUntil(when) => {
                    run.status = RunStatus::Parked;
                    run.waiting_for = None;
                    run.park_until = Some(when);
                    run.updated_at = Utc::now();
                    {
                        let db = self.db.lock().await;
                        db.update(&run)?;
                    }
                    info!(run_id = %run.id, until = %when, "workflow parked until time");
                    return Ok(());
                }

                StepResult::Failed(err) => {
                    run.attempt += 1;
                    let policy = &step_def.retry;

                    if run.attempt < policy.max_attempts {
                        // Retry after backoff.
                        let backoff = policy.backoff;
                        run.error = Some(err.to_string());
                        run.updated_at = Utc::now();
                        {
                            let db = self.db.lock().await;
                            db.update(&run)?;
                        }
                        if backoff > Duration::ZERO {
                            tokio::time::sleep(backoff).await;
                        }
                        warn!(run_id = %run.id, step = %run.step_name, attempt = run.attempt, "step failed, retrying");
                        // Continue loop (retry same step).
                    } else {
                        // Exhausted.
                        match &policy.on_exhausted {
                            ErrorAction::FailWorkflow => {
                                let msg = err.to_string();
                                self.fail_run(&mut run, &msg).await?;
                                return Ok(());
                            }
                            ErrorAction::GoTo(target) => {
                                let target = *target;
                                if def.step_index(target).is_none() {
                                    let msg = format!("GoTo target not found: {target}");
                                    self.fail_run(&mut run, &msg).await?;
                                    return Err(WorkflowError::StepNotFound(target.to_string()));
                                }
                                warn!(run_id = %run.id, step = %run.step_name, goto = target, "retries exhausted, going to fallback step");
                                run.step_name = target.to_string();
                                run.attempt = 0;
                                run.error = Some(err.to_string());
                                run.updated_at = Utc::now();
                                {
                                    let db = self.db.lock().await;
                                    db.update(&run)?;
                                }
                                // Continue loop.
                            }
                            ErrorAction::Skip => {
                                warn!(run_id = %run.id, step = %run.step_name, "retries exhausted, skipping step");
                                run.error = Some(err.to_string());
                                run.attempt = 0;
                                if let Some(next) = def.steps.get(step_idx + 1) {
                                    run.step_name = next.name().to_string();
                                    run.updated_at = Utc::now();
                                    {
                                        let db = self.db.lock().await;
                                        db.update(&run)?;
                                    }
                                    // Continue loop.
                                } else {
                                    // Skipped last step — complete.
                                    run.status = RunStatus::Completed;
                                    run.updated_at = Utc::now();
                                    {
                                        let db = self.db.lock().await;
                                        db.update(&run)?;
                                    }
                                    info!(run_id = %run.id, "workflow completed (last step skipped)");
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Mark a run as failed and run compensation steps.
    async fn fail_run(&self, run: &mut WorkflowRun, reason: &str) -> Result<()> {
        warn!(run_id = %run.id, %reason, "workflow failed");
        run.status = RunStatus::Failed;
        run.error = Some(reason.to_string());
        run.updated_at = Utc::now();
        {
            let db = self.db.lock().await;
            db.update(run)?;
        }

        let def = match self.registry.get(run.kind.as_str()) {
            Some(d) => d,
            None => return Ok(()),
        };

        if def.on_failure.is_empty() {
            return Ok(());
        }

        run.status = RunStatus::Compensating;
        run.updated_at = Utc::now();
        {
            let db = self.db.lock().await;
            db.update(run)?;
        }

        for comp_step in &def.on_failure {
            let ctx = StepContext::new(
                run.id.clone(),
                run.kind.clone(),
                comp_step.name().to_string(),
                0,
                run.context.clone(),
            );
            debug!(run_id = %run.id, step = comp_step.name(), "compensation step");
            match comp_step.run(&ctx).await {
                StepResult::Complete(output) => merge_context(&mut run.context, &output),
                other => {
                    warn!(run_id = %run.id, step = comp_step.name(), "compensation step returned non-complete: {:?}", std::mem::discriminant(&other));
                }
            }
        }

        run.status = RunStatus::Failed;
        run.updated_at = Utc::now();
        {
            let db = self.db.lock().await;
            db.update(run)?;
        }
        Ok(())
    }
}

/// Merge `patch` into `base` (shallow key-merge at the top level).
fn merge_context(base: &mut Value, patch: &Value) {
    if patch.is_null() {
        return;
    }
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                b.insert(k.clone(), v.clone());
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_adds_keys() {
        let mut base = json!({"a": 1});
        merge_context(&mut base, &json!({"b": 2}));
        assert_eq!(base, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn merge_overwrites_existing() {
        let mut base = json!({"a": 1});
        merge_context(&mut base, &json!({"a": 99}));
        assert_eq!(base["a"], 99);
    }

    #[test]
    fn merge_null_is_noop() {
        let mut base = json!({"a": 1});
        merge_context(&mut base, &Value::Null);
        assert_eq!(base, json!({"a": 1}));
    }
}

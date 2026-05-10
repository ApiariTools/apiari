use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::types::{ErrorAction, RetryPolicy, StepResult, TimeoutAction};

/// Context passed to each step during execution.
///
/// Holds the accumulated JSON context from all previous steps. Steps read
/// their inputs here and return new values via `StepResult::Complete`.
#[derive(Debug, Clone)]
pub struct StepContext {
    pub run_id: String,
    pub workflow_kind: String,
    pub step_name: String,
    pub attempt: u32,
    pub(crate) ctx: Value,
}

impl StepContext {
    pub(crate) fn new(
        run_id: String,
        workflow_kind: String,
        step_name: String,
        attempt: u32,
        ctx: Value,
    ) -> Self {
        Self {
            run_id,
            workflow_kind,
            step_name,
            attempt,
            ctx,
        }
    }

    /// Get a typed value from the context by key.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.ctx
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Get the raw context JSON.
    pub fn raw(&self) -> &Value {
        &self.ctx
    }
}

/// A single workflow step.
#[async_trait]
pub trait Step: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, ctx: &StepContext) -> StepResult;
}

/// A step with its associated retry and timeout configuration.
pub struct StepDef {
    pub(crate) step: Box<dyn Step>,
    pub(crate) retry: RetryPolicy,
    pub(crate) on_timeout: TimeoutAction,
}

impl StepDef {
    pub fn new(step: impl Step + 'static) -> Self {
        Self {
            step: Box::new(step),
            retry: RetryPolicy::default(),
            on_timeout: TimeoutAction::default(),
        }
    }

    /// Set the retry policy for this step.
    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Set what happens when a signal-wait timeout fires.
    pub fn on_timeout(mut self, action: TimeoutAction) -> Self {
        self.on_timeout = action;
        self
    }

    /// Shorthand: retry N times before failing the workflow.
    pub fn retry(mut self, max_attempts: u32, backoff: std::time::Duration) -> Self {
        self.retry = RetryPolicy::with_retries(max_attempts, backoff, ErrorAction::FailWorkflow);
        self
    }

    pub fn name(&self) -> &'static str {
        self.step.name()
    }
}

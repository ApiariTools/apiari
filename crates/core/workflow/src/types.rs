use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use thiserror::Error;

/// Outcome a step returns to the engine.
#[derive(Debug)]
pub enum StepResult {
    /// Step succeeded. Merge `output` into context and advance to next step.
    Complete(Value),

    /// Jump to a named step (for loops / conditional branching). Increments revision counter.
    GoTo { step: &'static str, output: Value },

    /// Park this run until an incoming signal matches `pattern`.
    WaitForSignal {
        pattern: String,
        timeout: Option<Duration>,
    },

    /// Park this run until the given UTC timestamp.
    WaitUntil(DateTime<Utc>),

    /// Step failed with a recoverable or permanent error.
    Failed(color_eyre::Report),
}

/// A signal delivered to the engine to resume a parked workflow run.
#[derive(Debug, Clone)]
pub struct IncomingSignal {
    /// Pattern token — matched against `waiting_for` in parked runs.
    pub pattern: String,
    /// Payload merged into the run's context on resume.
    pub payload: Value,
}

/// What to do when retries are exhausted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ErrorAction {
    /// Fail the entire workflow (default).
    #[default]
    FailWorkflow,
    /// Jump to a named step (e.g. a cleanup or fallback step).
    GoTo(&'static str),
    /// Skip this step and continue to the next.
    Skip,
}

/// What to do when a `WaitForSignal` timeout fires.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TimeoutAction {
    /// Fail the workflow.
    #[default]
    FailWorkflow,
    /// Jump to a named step.
    GoTo(&'static str),
    /// Continue to the next step with a null signal payload.
    Continue,
}

/// Per-step retry policy.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// How many attempts before `on_exhausted` fires. Default: 1 (no retry).
    pub max_attempts: u32,
    /// Initial backoff between attempts.
    pub backoff: Duration,
    /// What to do when attempts are exhausted.
    pub on_exhausted: ErrorAction,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff: Duration::from_secs(0),
            on_exhausted: ErrorAction::FailWorkflow,
        }
    }
}

impl RetryPolicy {
    pub fn with_retries(attempts: u32, backoff: Duration, on_exhausted: ErrorAction) -> Self {
        Self {
            max_attempts: attempts,
            backoff,
            on_exhausted,
        }
    }
}

/// Status of a workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Parked,
    Completed,
    Failed,
    Compensating,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Parked => "parked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Compensating => "compensating",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "parked" => Some(Self::Parked),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "compensating" => Some(Self::Compensating),
            _ => None,
        }
    }
}

/// A persisted workflow run record.
#[derive(Debug, Clone)]
pub struct WorkflowRun {
    pub id: String,
    pub kind: String,
    pub step_name: String,
    pub status: RunStatus,
    /// Accumulated context JSON.
    pub context: Value,
    /// Signal pattern this run is waiting for (if parked).
    pub waiting_for: Option<String>,
    /// UTC timestamp this run is parked until (if time-parked).
    pub park_until: Option<DateTime<Utc>>,
    /// Current attempt count for the active step.
    pub attempt: u32,
    /// GoTo revision counter for cycle detection.
    pub revision: u32,
    /// Error message from last failure.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors from the workflow engine.
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("workflow kind not registered: {0}")]
    UnknownKind(String),

    #[error("step not found: {0}")]
    StepNotFound(String),

    #[error("cycle detected in workflow {run_id}: GoTo '{step}' at revision {revision}")]
    CycleDetected {
        run_id: String,
        step: String,
        revision: u32,
    },

    #[error("run not found: {0}")]
    RunNotFound(String),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error(transparent)]
    Other(#[from] color_eyre::Report),
}

pub type Result<T> = std::result::Result<T, WorkflowError>;

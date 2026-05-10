// Shared test helpers — simple Step implementations backed by closures.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Mutex};

use crate::step::Step;
use crate::{StepContext, StepResult};

// ── ConstStep ────────────────────────────────────────────────────────────────

/// A step that always returns `Complete(output)`.
pub struct ConstStep {
    pub name: &'static str,
    pub output: Value,
}

impl ConstStep {
    pub fn new(name: &'static str, output: Value) -> Self {
        Self { name, output }
    }
}

#[async_trait]
impl Step for ConstStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        StepResult::Complete(self.output.clone())
    }
}

// ── CountingStep ─────────────────────────────────────────────────────────────

/// A step that records how many times it ran and can fail for the first N calls.
pub struct CountingStep {
    pub name: &'static str,
    pub call_count: Arc<Mutex<u32>>,
    pub fail_first: u32,
    pub output: Value,
}

impl CountingStep {
    pub fn new(name: &'static str, fail_first: u32, output: Value) -> (Self, Arc<Mutex<u32>>) {
        let count = Arc::new(Mutex::new(0u32));
        let step = Self {
            name,
            call_count: count.clone(),
            fail_first,
            output,
        };
        (step, count)
    }
}

#[async_trait]
impl Step for CountingStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        let mut n = self.call_count.lock().unwrap();
        *n += 1;
        if *n <= self.fail_first {
            StepResult::Failed(color_eyre::eyre::eyre!("forced failure #{}", *n))
        } else {
            StepResult::Complete(self.output.clone())
        }
    }
}

// ── CapturingStep ─────────────────────────────────────────────────────────────

/// A step that captures the context it receives (for assertions).
pub struct CapturingStep {
    pub name: &'static str,
    pub captured: Arc<Mutex<Option<Value>>>,
}

impl CapturingStep {
    pub fn new(name: &'static str) -> (Self, Arc<Mutex<Option<Value>>>) {
        let cap = Arc::new(Mutex::new(None));
        (
            Self {
                name,
                captured: cap.clone(),
            },
            cap,
        )
    }
}

#[async_trait]
impl Step for CapturingStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, ctx: &StepContext) -> StepResult {
        *self.captured.lock().unwrap() = Some(ctx.raw().clone());
        StepResult::Complete(Value::Null)
    }
}

// ── SignalWaitStep ─────────────────────────────────────────────────────────────

/// A step that parks waiting for a signal then completes.
pub struct SignalWaitStep {
    pub name: &'static str,
    pub pattern: &'static str,
    pub timeout: Option<std::time::Duration>,
}

#[async_trait]
impl Step for SignalWaitStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        StepResult::WaitForSignal {
            pattern: self.pattern.to_string(),
            timeout: self.timeout,
        }
    }
}

// ── GotoStep ─────────────────────────────────────────────────────────────────

/// A step that GoTo's a target after N visits, then completes.
pub struct GotoStep {
    pub name: &'static str,
    pub target: &'static str,
    pub goto_count: u32,
    pub calls: Arc<Mutex<u32>>,
    pub output: Value,
}

impl GotoStep {
    pub fn new(
        name: &'static str,
        target: &'static str,
        goto_count: u32,
        output: Value,
    ) -> (Self, Arc<Mutex<u32>>) {
        let calls = Arc::new(Mutex::new(0u32));
        let step = Self {
            name,
            target,
            goto_count,
            calls: calls.clone(),
            output,
        };
        (step, calls)
    }
}

#[async_trait]
impl Step for GotoStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        if *n <= self.goto_count {
            StepResult::GoTo {
                step: self.target,
                output: serde_json::json!({ "goto_visits": *n }),
            }
        } else {
            StepResult::Complete(self.output.clone())
        }
    }
}

// ── CompensationRecorder ──────────────────────────────────────────────────────

/// A compensation step that records when it ran.
pub struct CompensationRecorder {
    pub name: &'static str,
    pub ran: Arc<Mutex<bool>>,
}

impl CompensationRecorder {
    pub fn new(name: &'static str) -> (Self, Arc<Mutex<bool>>) {
        let ran = Arc::new(Mutex::new(false));
        (
            Self {
                name,
                ran: ran.clone(),
            },
            ran,
        )
    }
}

#[async_trait]
impl Step for CompensationRecorder {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        *self.ran.lock().unwrap() = true;
        StepResult::Complete(Value::Null)
    }
}

// ── AlwaysFailStep ────────────────────────────────────────────────────────────

pub struct AlwaysFailStep {
    pub name: &'static str,
    pub message: &'static str,
}

#[async_trait]
impl Step for AlwaysFailStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn run(&self, _ctx: &StepContext) -> StepResult {
        StepResult::Failed(color_eyre::eyre::eyre!("{}", self.message))
    }
}

// ── Engine builder ────────────────────────────────────────────────────────────

/// Convenience: build a fresh in-memory engine.
pub fn mem_engine() -> crate::WorkflowEngine {
    crate::WorkflowEngine::new(":memory:").expect("mem engine")
}

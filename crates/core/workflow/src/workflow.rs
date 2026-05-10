use crate::step::{Step, StepDef};

/// A complete workflow definition: ordered steps + failure compensation.
pub struct WorkflowDef {
    pub(crate) kind: &'static str,
    pub(crate) steps: Vec<StepDef>,
    /// Maximum GoTo revisions before cycle detection aborts the run.
    pub(crate) max_revisions: u32,
    /// Compensation steps run in order when the workflow fails (saga pattern).
    pub(crate) on_failure: Vec<Box<dyn Step>>,
}

impl WorkflowDef {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            steps: Vec::new(),
            max_revisions: 20,
            on_failure: Vec::new(),
        }
    }

    /// Append a step (with default retry/timeout config).
    pub fn step(mut self, s: impl Step + 'static) -> Self {
        self.steps.push(StepDef::new(s));
        self
    }

    /// Append a step with custom retry/timeout config.
    pub fn step_with(mut self, def: StepDef) -> Self {
        self.steps.push(def);
        self
    }

    /// Set the maximum GoTo revision count before aborting with CycleDetected.
    pub fn max_revisions(mut self, n: u32) -> Self {
        self.max_revisions = n;
        self
    }

    /// Register a compensation step run when the workflow fails.
    pub fn on_failure(mut self, s: impl Step + 'static) -> Self {
        self.on_failure.push(Box::new(s));
        self
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub(crate) fn step_index(&self, name: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.name() == name)
    }
}

//! Built-in workflow graph for the default task lifecycle.

use std::path::Path;

use super::WorkflowGraph;

/// The default workflow graph used when no custom graph is configured.
///
/// Flow: triage → write_code → coding → ai_review → await_verdict
///       → create_pr → human_review → merged/dismissed
///
/// With rework loop: await_verdict -(CHANGES_REQUESTED)→ write_code → coding
/// Bounded at 3 cycles, then forces to create_pr.
pub fn builtin_workflow() -> WorkflowGraph {
    WorkflowGraph::from_yaml(BUILTIN_YAML).expect("builtin workflow YAML must be valid")
}

/// Load a workflow graph from a YAML file, falling back to the builtin.
pub fn load_workflow(path: Option<&Path>) -> color_eyre::Result<WorkflowGraph> {
    if let Some(p) = path
        && p.exists()
    {
        let yaml = std::fs::read_to_string(p)?;
        return WorkflowGraph::from_yaml(&yaml);
    }
    Ok(builtin_workflow())
}

const BUILTIN_YAML: &str = r#"
name: builtin
nodes:
  triage:
    type: entry
    label: Triage
    stage: Triage
  write_code:
    type: action
    label: Write Code
    action:
      kind: dispatch_worker
  coding:
    type: wait
    label: Coding
    stage: InProgress
  ai_review:
    type: action
    label: AI Review
    action:
      kind: dispatch_worker
      role: reviewer
  await_verdict:
    type: wait
    label: Awaiting Verdict
    stage: InAiReview
  create_pr:
    type: action
    label: Create PR
    stage: InAiReview
    action:
      kind: create_pr
  human_review:
    type: wait
    label: Human Review
    stage: HumanReview
  merged:
    type: terminal
    label: Merged
    stage: Merged
  dismissed:
    type: terminal
    label: Dismissed
    stage: Dismissed
edges:
  # triage → write_code (unconditional, auto-chains)
  - from: triage
    to: write_code
  # write_code → coding (unconditional, auto-chains from action)
  - from: write_code
    to: coding
  # coding → ai_review when worker signals waiting
  - from: coding
    to: ai_review
    condition:
      type: signal
      source: swarm_worker_waiting
  # ai_review → await_verdict (unconditional, auto-chains from action)
  - from: ai_review
    to: await_verdict
  # await_verdict → write_code on CHANGES_REQUESTED (bounded loop)
  - from: await_verdict
    to: write_code
    condition:
      type: agent_output
      source: swarm_review_verdict
      match:
        verdict: CHANGES_REQUESTED
    guard:
      counter: review_cycles
      max: 3
    priority: 0
  # await_verdict → create_pr on CHANGES_REQUESTED when max exceeded (force PR)
  - from: await_verdict
    to: create_pr
    condition:
      type: agent_output
      source: swarm_review_verdict
      match:
        verdict: CHANGES_REQUESTED
    guard:
      counter: review_cycles
      max: 3
      exceeded: true
    priority: 1
  # await_verdict → create_pr on APPROVED
  - from: await_verdict
    to: create_pr
    condition:
      type: agent_output
      source: swarm_review_verdict
      match:
        verdict: APPROVED
    priority: 2
  # create_pr → human_review (unconditional, auto-chains from action)
  - from: create_pr
    to: human_review
  # human_review → merged
  - from: human_review
    to: merged
    condition:
      type: signal
      source: github_merged_pr
  # human_review → dismissed
  - from: human_review
    to: dismissed
    condition:
      type: signal
      source: github_pr_closed
"#;

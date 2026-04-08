//! Graph walker — advances a cursor through the workflow graph based on signals.
//!
//! The walker is pure: it evaluates conditions and guards, mutates the cursor,
//! and returns actions for the caller to execute. No I/O.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::{ActionKind, Condition, Guard, NodeType, WorkflowGraph};
use crate::buzz::{orchestrator::workflow::WorkflowAction, signal::SignalRecord, task::Task};

// ── Cursor ─────────────────────────────────────────────────────────────

/// Tracks where a task is in the workflow graph.
///
/// Stored as JSON in `Task.metadata["graph_cursor"]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCursor {
    /// Current node id in the graph.
    pub current_node: String,
    /// Named counters (e.g. "review_cycles" for bounded loops).
    #[serde(default)]
    pub counters: HashMap<String, u32>,
    /// Collected artifacts (e.g. "branch_name", "worker_id").
    #[serde(default)]
    pub artifacts: HashMap<String, String>,
    /// Audit trail of steps taken.
    #[serde(default)]
    pub history: Vec<CursorStep>,
}

/// A single step in the cursor's history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorStep {
    pub from_node: String,
    pub to_node: String,
    pub timestamp: DateTime<Utc>,
    pub trigger: String,
}

impl GraphCursor {
    /// Create a new cursor at the graph's entry node.
    pub fn new(entry_node: &str) -> Self {
        Self {
            current_node: entry_node.to_string(),
            counters: HashMap::new(),
            artifacts: HashMap::new(),
            history: Vec::new(),
        }
    }

    /// Load cursor from task metadata, or create a new one at the entry node.
    pub fn from_task(task: &Task, graph: &WorkflowGraph) -> Self {
        task.metadata
            .get("graph_cursor")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_else(|| {
                let entry = graph.entry_node().unwrap_or("triage");
                Self::new(entry)
            })
    }

    /// Serialize cursor back into a JSON value for task metadata storage.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

// ── Walker output ──────────────────────────────────────────────────────

/// Result of a single walker step.
#[derive(Debug, Clone)]
pub struct WalkerOutput {
    /// The workflow action to execute (if the target node is an action node).
    pub action: Option<WorkflowAction>,
    /// The node we moved to.
    pub new_node: String,
    /// The node's label (for notifications).
    pub label: String,
    /// Optional stage name for kanban mapping.
    pub stage: Option<String>,
}

// ── Walker ──────────────────────────────────────────────────────────────

/// The graph walker evaluates edges and advances the cursor.
pub struct GraphWalker<'g> {
    graph: &'g WorkflowGraph,
}

impl<'g> GraphWalker<'g> {
    pub fn new(graph: &'g WorkflowGraph) -> Self {
        Self { graph }
    }

    /// Advance the cursor one step based on the incoming signal.
    ///
    /// Evaluates all outgoing edges from the cursor's current node (sorted by priority).
    /// The first edge whose condition and guard both pass is traversed.
    ///
    /// Returns `None` if no edge matches (cursor stays put).
    /// Returns `Some(outputs)` with one or more steps if edges matched.
    /// The walker follows unconditional edges automatically (chain stepping).
    pub fn step(
        &self,
        cursor: &mut GraphCursor,
        signal: &SignalRecord,
        task: &Task,
    ) -> Vec<WalkerOutput> {
        let mut outputs = Vec::new();
        // Cap chain steps to prevent infinite loops in misconfigured graphs
        let max_chain = 10;

        for _ in 0..max_chain {
            let edges = self.graph.outgoing_edges(&cursor.current_node);
            if edges.is_empty() {
                debug!(
                    "[graph] no outgoing edges from '{}' for signal '{}'",
                    cursor.current_node, signal.source
                );
                break;
            }

            debug!(
                "[graph] evaluating {} edge(s) from '{}' for signal '{}'",
                edges.len(),
                cursor.current_node,
                signal.source
            );

            let matched = edges.into_iter().find(|edge| {
                let cond_ok = self.evaluate_condition(edge.condition.as_ref(), signal, task);
                let guard_ok = self.evaluate_guard(edge.guard.as_ref(), cursor);
                debug!(
                    "[graph]   edge '{}' → '{}': condition={cond_ok}, guard={guard_ok}",
                    edge.from, edge.to
                );
                cond_ok && guard_ok
            });

            let edge = match matched {
                Some(e) => e,
                None => break,
            };

            let from = cursor.current_node.clone();
            let to = edge.to.clone();

            // Increment counter if guard references one
            if let Some(ref guard) = edge.guard {
                let counter = cursor.counters.entry(guard.counter.clone()).or_insert(0);
                *counter += 1;
            }

            // Record step
            cursor.history.push(CursorStep {
                from_node: from.clone(),
                to_node: to.clone(),
                timestamp: Utc::now(),
                trigger: signal.source.clone(),
            });
            cursor.current_node = to.clone();

            info!(
                "[graph] stepped {} → {} (trigger: {})",
                from, to, signal.source
            );

            // Collect artifacts from signal metadata
            self.collect_artifacts(cursor, signal);

            // Build output for this step
            let node = self.graph.nodes.get(&to);
            let label = node.map(|n| n.label.clone()).unwrap_or_default();
            let stage = node.and_then(|n| n.stage.clone());
            let action = self.node_action(&to, cursor, task);

            outputs.push(WalkerOutput {
                action,
                new_node: to,
                label,
                stage,
            });

            // If we landed on a node with unconditional outgoing edges, keep going.
            // But only auto-advance from action nodes (not wait/terminal).
            let landed_type = node.map(|n| &n.node_type);
            if landed_type != Some(&NodeType::Action) {
                break;
            }
            // Check if there's an unconditional edge from here
            let next_edges = self.graph.outgoing_edges(&cursor.current_node);
            let has_unconditional = next_edges.iter().any(|e| e.condition.is_none());
            if !has_unconditional {
                break;
            }
            // Continue the loop — the unconditional edge will be picked up next iteration
        }

        outputs
    }

    /// Evaluate whether a condition matches the current signal.
    fn evaluate_condition(
        &self,
        condition: Option<&Condition>,
        signal: &SignalRecord,
        task: &Task,
    ) -> bool {
        let condition = match condition {
            Some(c) => c,
            None => return true, // unconditional edge
        };

        match condition {
            Condition::Signal {
                source,
                metadata_match,
            } => {
                if signal.source != *source {
                    return false;
                }
                // Check optional metadata field matching
                if !metadata_match.is_empty() {
                    return self.check_metadata_match(signal, metadata_match);
                }
                true
            }
            Condition::AgentOutput {
                source,
                match_fields,
            } => {
                if signal.source != *source {
                    return false;
                }
                self.check_metadata_match(signal, match_fields)
            }
            Condition::StateCheck { check, expect } => {
                let actual = self.evaluate_state_check(check, task);
                actual == *expect
            }
        }
    }

    /// Check if signal metadata contains all required field values.
    fn check_metadata_match(
        &self,
        signal: &SignalRecord,
        fields: &HashMap<String, String>,
    ) -> bool {
        let meta = match signal
            .metadata
            .as_ref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        {
            Some(v) => v,
            None => return fields.is_empty(),
        };

        fields.iter().all(|(key, expected)| {
            meta.get(key)
                .and_then(|v| v.as_str())
                .is_some_and(|actual| actual == expected)
        })
    }

    /// Evaluate a named state check against the current task.
    fn evaluate_state_check(&self, check: &str, task: &Task) -> serde_json::Value {
        match check {
            "has_worker" => serde_json::Value::Bool(task.worker_id.is_some()),
            "has_pr" => serde_json::Value::Bool(task.pr_url.is_some()),
            "is_terminal" => serde_json::Value::Bool(task.stage.is_terminal()),
            "stage" => serde_json::Value::String(task.stage.as_str().to_string()),
            _ => {
                debug!("[graph] unknown state check: {check}");
                serde_json::Value::Null
            }
        }
    }

    /// Evaluate whether a guard allows traversal.
    fn evaluate_guard(&self, guard: Option<&Guard>, cursor: &GraphCursor) -> bool {
        let guard = match guard {
            Some(g) => g,
            None => return true,
        };

        let count = cursor.counters.get(&guard.counter).copied().unwrap_or(0);

        if let Some(max) = guard.max {
            if guard.exceeded == Some(true) {
                // Only traverse if counter has reached/exceeded max
                return count >= max;
            }
            // Only traverse if counter is below max
            return count < max;
        }

        if guard.exceeded == Some(true) {
            // exceeded without max makes no sense, but treat as always-false
            return false;
        }

        true
    }

    /// Determine the workflow action for an action node by reading its config.
    fn node_action(
        &self,
        node_id: &str,
        cursor: &GraphCursor,
        task: &Task,
    ) -> Option<WorkflowAction> {
        let node = self.graph.nodes.get(node_id)?;
        if node.node_type != NodeType::Action {
            return None;
        }

        let action_config = node.action.as_ref()?;

        let branch_name = cursor
            .artifacts
            .get("branch_name")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let worker_id = task
            .worker_id
            .clone()
            .or_else(|| cursor.artifacts.get("worker_id").cloned())
            .unwrap_or_else(|| "unknown".to_string());

        match action_config.kind {
            ActionKind::DispatchWorker => {
                // If there's review feedback in artifacts, this is a rework dispatch
                let feedback = cursor
                    .artifacts
                    .get("review_feedback")
                    .cloned()
                    .unwrap_or_default();

                if !feedback.is_empty() && action_config.role.as_deref() != Some("reviewer") {
                    Some(WorkflowAction::DispatchRework {
                        task_id: task.id.clone(),
                        feedback,
                    })
                } else {
                    Some(WorkflowAction::DispatchReviewer {
                        task_id: task.id.clone(),
                        branch_name,
                        worker_id,
                    })
                }
            }
            ActionKind::CreatePr => {
                let cycle_count = cursor.counters.get("review_cycles").copied().unwrap_or(0);
                if cycle_count > 0
                    && cursor
                        .artifacts
                        .get("force_pr")
                        .is_some_and(|v| v == "true")
                {
                    Some(WorkflowAction::ForceCreatePr {
                        task_id: task.id.clone(),
                        branch_name,
                        cycle_count,
                    })
                } else {
                    Some(WorkflowAction::CreatePr {
                        task_id: task.id.clone(),
                        branch_name,
                    })
                }
            }
            ActionKind::Notify | ActionKind::Custom => {
                // These don't produce WorkflowActions (yet) — they're handled
                // by the notification/action system outside the graph walker.
                None
            }
        }
    }

    /// Extract useful artifacts from signal metadata into the cursor.
    fn collect_artifacts(&self, cursor: &mut GraphCursor, signal: &SignalRecord) {
        let meta = match signal
            .metadata
            .as_ref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        {
            Some(v) => v,
            None => return,
        };

        for key in &["branch_name", "worker_id", "pr_url", "verdict", "comments"] {
            if let Some(val) = meta.get(*key).and_then(|v| v.as_str()) {
                cursor.artifacts.insert((*key).to_string(), val.to_string());
            }
        }

        // Store review feedback from verdict signals
        if let Some(comments) = meta.get("comments").and_then(|v| v.as_str()) {
            cursor
                .artifacts
                .insert("review_feedback".to_string(), comments.to_string());
        }

        // Mark force_pr if the guard was an exceeded edge
        if let Some(verdict) = meta.get("verdict").and_then(|v| v.as_str())
            && verdict == "CHANGES_REQUESTED"
            && let Some(max) = cursor.counters.get("review_cycles")
            && *max >= 3
        {
            cursor
                .artifacts
                .insert("force_pr".to_string(), "true".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::buzz::{
        signal::{Severity, SignalRecord, SignalStatus},
        task::{Task, TaskStage},
    };

    fn make_signal(source: &str) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.to_string(),
            external_id: "test".to_string(),
            title: "test".to_string(),
            body: None,
            severity: Severity::Info,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
            snoozed_until: None,
        }
    }

    fn make_signal_with_meta(source: &str, meta: serde_json::Value) -> SignalRecord {
        let mut sig = make_signal(source);
        sig.metadata = Some(meta.to_string());
        sig
    }

    fn make_task() -> Task {
        Task {
            id: "task-1".to_string(),
            workspace: "test".to_string(),
            title: "Test task".to_string(),
            stage: TaskStage::Triage,
            source: None,
            source_url: None,
            worker_id: Some("worker-1".to_string()),
            pr_url: None,
            pr_number: None,
            repo: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: serde_json::json!({}),
        }
    }

    fn simple_graph() -> WorkflowGraph {
        WorkflowGraph::from_yaml(
            r#"
name: test
nodes:
  start:
    type: entry
    label: Start
    stage: Triage
  work:
    type: action
    label: Work
    stage: InProgress
  wait_done:
    type: wait
    label: Waiting
  done:
    type: terminal
    label: Done
    stage: Merged
edges:
  - from: start
    to: work
  - from: work
    to: wait_done
  - from: wait_done
    to: done
    condition:
      type: signal
      source: task_complete
"#,
        )
        .unwrap()
    }

    #[test]
    fn test_step_unconditional_chain() {
        let graph = simple_graph();
        let task = make_task();
        let signal = make_signal("some_signal");
        let walker = GraphWalker::new(&graph);
        let mut cursor = GraphCursor::new("start");

        // From start, the unconditional edge to work should fire,
        // then work→wait_done is also unconditional, so we chain.
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].new_node, "work");
        assert_eq!(outputs[1].new_node, "wait_done");
        assert_eq!(cursor.current_node, "wait_done");
    }

    #[test]
    fn test_step_signal_condition() {
        let graph = simple_graph();
        let task = make_task();
        let walker = GraphWalker::new(&graph);
        let mut cursor = GraphCursor::new("wait_done");

        // Wrong signal — no match
        let signal = make_signal("wrong_signal");
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert!(outputs.is_empty());
        assert_eq!(cursor.current_node, "wait_done");

        // Right signal — should advance to done
        let signal = make_signal("task_complete");
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].new_node, "done");
        assert_eq!(cursor.current_node, "done");
    }

    #[test]
    fn test_state_check_condition() {
        let yaml = r#"
name: test
nodes:
  start:
    type: entry
    label: Start
  has_worker:
    type: wait
    label: Has Worker
  no_worker:
    type: terminal
    label: No Worker
edges:
  - from: start
    to: has_worker
    condition:
      type: state_check
      check: has_worker
      expect: true
    priority: 0
  - from: start
    to: no_worker
    condition:
      type: state_check
      check: has_worker
      expect: false
    priority: 1
"#;
        let graph = WorkflowGraph::from_yaml(yaml).unwrap();
        let walker = GraphWalker::new(&graph);
        let signal = make_signal("any");

        // Task WITH worker
        let task = make_task();
        let mut cursor = GraphCursor::new("start");
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert_eq!(outputs[0].new_node, "has_worker");

        // Task WITHOUT worker
        let mut task_no_worker = make_task();
        task_no_worker.worker_id = None;
        let mut cursor = GraphCursor::new("start");
        let outputs = walker.step(&mut cursor, &signal, &task_no_worker);
        assert_eq!(outputs[0].new_node, "no_worker");
    }

    #[test]
    fn test_agent_output_condition() {
        let yaml = r#"
name: test
nodes:
  start:
    type: entry
    label: Start
  wait_verdict:
    type: wait
    label: Await Verdict
  approved:
    type: terminal
    label: Approved
  rejected:
    type: terminal
    label: Rejected
edges:
  - from: start
    to: wait_verdict
  - from: wait_verdict
    to: approved
    condition:
      type: agent_output
      source: swarm_review_verdict
      match:
        verdict: APPROVED
  - from: wait_verdict
    to: rejected
    condition:
      type: agent_output
      source: swarm_review_verdict
      match:
        verdict: CHANGES_REQUESTED
"#;
        let graph = WorkflowGraph::from_yaml(yaml).unwrap();
        let walker = GraphWalker::new(&graph);
        let task = make_task();

        // Approved
        let signal = make_signal_with_meta(
            "swarm_review_verdict",
            serde_json::json!({"verdict": "APPROVED"}),
        );
        let mut cursor = GraphCursor::new("wait_verdict");
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert_eq!(outputs[0].new_node, "approved");

        // Rejected
        let signal = make_signal_with_meta(
            "swarm_review_verdict",
            serde_json::json!({"verdict": "CHANGES_REQUESTED"}),
        );
        let mut cursor = GraphCursor::new("wait_verdict");
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert_eq!(outputs[0].new_node, "rejected");
    }

    #[test]
    fn test_guard_counter_max() {
        let yaml = r#"
name: test
nodes:
  start:
    type: entry
    label: Start
  loop_node:
    type: wait
    label: Loop
  rework:
    type: action
    label: Rework
  done:
    type: terminal
    label: Done
edges:
  - from: start
    to: loop_node
  - from: loop_node
    to: rework
    condition:
      type: signal
      source: needs_rework
    guard:
      counter: cycles
      max: 2
    priority: 0
  - from: loop_node
    to: done
    condition:
      type: signal
      source: needs_rework
    guard:
      counter: cycles
      max: 2
      exceeded: true
    priority: 1
  - from: rework
    to: loop_node
"#;
        let graph = WorkflowGraph::from_yaml(yaml).unwrap();
        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let signal = make_signal("needs_rework");

        let mut cursor = GraphCursor::new("loop_node");

        // First rework — counter 0 < 2, should go to rework then chain to loop_node
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert!(outputs.iter().any(|o| o.new_node == "rework"));
        assert_eq!(cursor.current_node, "loop_node");
        assert_eq!(cursor.counters["cycles"], 1);

        // Second rework — counter 1 < 2
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert!(outputs.iter().any(|o| o.new_node == "rework"));
        assert_eq!(cursor.current_node, "loop_node");
        assert_eq!(cursor.counters["cycles"], 2);

        // Third attempt — counter 2 >= 2, guard blocks rework, exceeded edge fires
        let outputs = walker.step(&mut cursor, &signal, &task);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].new_node, "done");
    }

    #[test]
    fn test_cursor_serialization() {
        let mut cursor = GraphCursor::new("start");
        cursor.counters.insert("review_cycles".to_string(), 2);
        cursor
            .artifacts
            .insert("branch_name".to_string(), "feat/foo".to_string());

        let json = cursor.to_json();
        let restored: GraphCursor = serde_json::from_value(json).unwrap();
        assert_eq!(restored.current_node, "start");
        assert_eq!(restored.counters["review_cycles"], 2);
        assert_eq!(restored.artifacts["branch_name"], "feat/foo");
    }

    #[test]
    fn test_history_recorded() {
        let graph = simple_graph();
        let task = make_task();
        let signal = make_signal("any");
        let walker = GraphWalker::new(&graph);
        let mut cursor = GraphCursor::new("start");

        walker.step(&mut cursor, &signal, &task);
        assert_eq!(cursor.history.len(), 2); // start→work, work→wait_done
        assert_eq!(cursor.history[0].from_node, "start");
        assert_eq!(cursor.history[0].to_node, "work");
        assert_eq!(cursor.history[1].from_node, "work");
        assert_eq!(cursor.history[1].to_node, "wait_done");
    }

    // ── Builtin workflow tests ─────────────────────────────────────────

    fn builtin_graph() -> WorkflowGraph {
        crate::buzz::orchestrator::graph::builtin::builtin_workflow()
    }

    #[test]
    fn test_builtin_full_happy_path() {
        // Walk the entire builtin graph: triage → coding → ai_review → await_verdict → create_pr → human_review → merged
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();

        // Step 1: Entry → auto-chains through write_code to coding
        let mut cursor = GraphCursor::new("triage");
        let outputs = walker.step(&mut cursor, &make_signal("swarm_worker_spawned"), &task);
        assert!(!outputs.is_empty());
        assert_eq!(cursor.current_node, "coding");

        // Step 2: Worker waiting → coding→ai_review→await_verdict
        let outputs = walker.step(&mut cursor, &make_signal("swarm_worker_waiting"), &task);
        assert!(!outputs.is_empty());
        assert_eq!(cursor.current_node, "await_verdict");

        // Step 3: Review approved → await_verdict→create_pr→human_review
        let sig = make_signal_with_meta(
            "swarm_review_verdict",
            serde_json::json!({"verdict": "APPROVED"}),
        );
        let outputs = walker.step(&mut cursor, &sig, &task);
        assert!(!outputs.is_empty());
        assert_eq!(cursor.current_node, "human_review");

        // Step 4: PR merged → human_review→merged
        let outputs = walker.step(&mut cursor, &make_signal("github_merged_pr"), &task);
        assert_eq!(outputs.len(), 1);
        assert_eq!(cursor.current_node, "merged");

        // Step 5: Terminal — no more steps
        let outputs = walker.step(&mut cursor, &make_signal("anything"), &task);
        assert!(outputs.is_empty());
    }

    #[test]
    fn test_builtin_dismissed_path() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();

        // Advance to human_review
        let mut cursor = GraphCursor::new("human_review");

        // PR closed → dismissed
        let outputs = walker.step(&mut cursor, &make_signal("github_pr_closed"), &task);
        assert_eq!(outputs.len(), 1);
        assert_eq!(cursor.current_node, "dismissed");
    }

    #[test]
    fn test_builtin_rework_loop_three_cycles() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();

        let mut cursor = GraphCursor::new("await_verdict");
        let changes = make_signal_with_meta(
            "swarm_review_verdict",
            serde_json::json!({"verdict": "CHANGES_REQUESTED", "comments": "fix tests"}),
        );

        // Cycle 1: counter 0 < 3, changes requested → loops back to write_code→coding
        let outputs = walker.step(&mut cursor, &changes, &task);
        assert!(!outputs.is_empty());
        assert_eq!(cursor.current_node, "coding");
        assert_eq!(cursor.counters["review_cycles"], 1);

        // Simulate worker finishing rework and getting back to await_verdict
        cursor.current_node = "await_verdict".to_string();

        // Cycle 2: counter 1 < 3
        walker.step(&mut cursor, &changes, &task);
        assert_eq!(cursor.current_node, "coding");
        assert_eq!(cursor.counters["review_cycles"], 2);

        cursor.current_node = "await_verdict".to_string();

        // Cycle 3: counter 2 < 3, still under max
        walker.step(&mut cursor, &changes, &task);
        assert_eq!(cursor.current_node, "coding");
        assert_eq!(cursor.counters["review_cycles"], 3);

        cursor.current_node = "await_verdict".to_string();

        // Cycle 4: counter 3 >= 3, exceeded → force to create_pr→human_review
        let outputs = walker.step(&mut cursor, &changes, &task);
        assert!(!outputs.is_empty());
        assert_eq!(
            cursor.current_node, "human_review",
            "should force PR after 3 review cycles exceeded"
        );
    }

    #[test]
    fn test_builtin_wrong_signal_at_coding_does_nothing() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("coding");

        // These signals should NOT advance from coding
        for source in &[
            "swarm_worker_spawned",
            "swarm_branch_ready",
            "github_ci_pass",
            "github_merged_pr",
            "swarm_review_verdict",
        ] {
            let outputs = walker.step(&mut cursor, &make_signal(source), &task);
            assert!(
                outputs.is_empty(),
                "signal '{source}' should not advance from coding"
            );
            assert_eq!(cursor.current_node, "coding");
        }

        // Only swarm_worker_waiting advances
        let outputs = walker.step(&mut cursor, &make_signal("swarm_worker_waiting"), &task);
        assert!(!outputs.is_empty());
        assert_eq!(cursor.current_node, "await_verdict");
    }

    #[test]
    fn test_builtin_wrong_signal_at_await_verdict_does_nothing() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("await_verdict");

        // These should NOT advance from await_verdict
        for source in &[
            "swarm_worker_waiting",
            "swarm_worker_spawned",
            "github_merged_pr",
            "github_ci_failure",
        ] {
            let outputs = walker.step(&mut cursor, &make_signal(source), &task);
            assert!(
                outputs.is_empty(),
                "signal '{source}' should not advance from await_verdict"
            );
        }
    }

    #[test]
    fn test_builtin_action_nodes_produce_workflow_actions() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let mut task = make_task();
        task.worker_id = Some("w1".to_string());
        let mut cursor = GraphCursor::new("triage");
        cursor
            .artifacts
            .insert("branch_name".to_string(), "feat/foo".to_string());

        // Step through triage → write_code (action) → coding
        let outputs = walker.step(&mut cursor, &make_signal("any"), &task);

        // write_code should produce a DispatchReviewer action (dispatch_worker kind)
        let actions: Vec<_> = outputs.iter().filter_map(|o| o.action.as_ref()).collect();
        assert!(
            !actions.is_empty(),
            "action nodes should produce workflow actions"
        );
    }

    #[test]
    fn test_builtin_create_pr_action_produced() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("await_verdict");
        cursor
            .artifacts
            .insert("branch_name".to_string(), "feat/foo".to_string());

        let sig = make_signal_with_meta(
            "swarm_review_verdict",
            serde_json::json!({"verdict": "APPROVED"}),
        );
        let outputs = walker.step(&mut cursor, &sig, &task);

        // Should produce a CreatePr action when passing through create_pr node
        let has_create_pr = outputs
            .iter()
            .any(|o| matches!(o.action, Some(WorkflowAction::CreatePr { .. })));
        assert!(
            has_create_pr,
            "APPROVED verdict should produce CreatePr action"
        );
    }

    #[test]
    fn test_artifacts_collected_from_signals() {
        let graph = builtin_graph();
        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("coding");

        let sig = make_signal_with_meta(
            "swarm_worker_waiting",
            serde_json::json!({
                "worker_id": "w1",
                "branch_name": "feat/bar",
            }),
        );
        walker.step(&mut cursor, &sig, &task);

        assert_eq!(
            cursor.artifacts.get("worker_id").map(|s| s.as_str()),
            Some("w1")
        );
        assert_eq!(
            cursor.artifacts.get("branch_name").map(|s| s.as_str()),
            Some("feat/bar")
        );
    }

    // ── Custom graph config tests ──────────────────────────────────────

    #[test]
    fn test_custom_graph_linear_three_step() {
        // A simple custom workflow: intake → process → done
        let graph = WorkflowGraph::from_yaml(
            r#"
name: simple
nodes:
  intake:
    type: entry
    label: Intake
  process:
    type: wait
    label: Processing
  done:
    type: terminal
    label: Done
edges:
  - from: intake
    to: process
  - from: process
    to: done
    condition:
      type: signal
      source: completed
"#,
        )
        .unwrap();

        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("intake");

        // Auto-advance from intake to process
        let outputs = walker.step(&mut cursor, &make_signal("any"), &task);
        assert_eq!(cursor.current_node, "process");

        // Wrong signal — stays
        let outputs = walker.step(&mut cursor, &make_signal("wrong"), &task);
        assert!(outputs.is_empty());
        assert_eq!(cursor.current_node, "process");

        // Right signal — advances to done
        let outputs = walker.step(&mut cursor, &make_signal("completed"), &task);
        assert_eq!(cursor.current_node, "done");
    }

    #[test]
    fn test_custom_graph_with_metadata_matching() {
        let graph = WorkflowGraph::from_yaml(
            r#"
name: approval
nodes:
  start:
    type: entry
    label: Start
  review:
    type: wait
    label: Review
  approved:
    type: terminal
    label: Approved
  rejected:
    type: terminal
    label: Rejected
edges:
  - from: start
    to: review
  - from: review
    to: approved
    condition:
      type: agent_output
      source: review_result
      match:
        decision: approve
    priority: 0
  - from: review
    to: rejected
    condition:
      type: agent_output
      source: review_result
      match:
        decision: reject
    priority: 1
"#,
        )
        .unwrap();

        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("review");

        // Approve
        let sig =
            make_signal_with_meta("review_result", serde_json::json!({"decision": "approve"}));
        let outputs = walker.step(&mut cursor, &sig, &task);
        assert_eq!(cursor.current_node, "approved");

        // Reset and reject
        let mut cursor = GraphCursor::new("review");
        let sig = make_signal_with_meta("review_result", serde_json::json!({"decision": "reject"}));
        walker.step(&mut cursor, &sig, &task);
        assert_eq!(cursor.current_node, "rejected");
    }

    #[test]
    fn test_custom_graph_state_check_with_pr() {
        let graph = WorkflowGraph::from_yaml(
            r#"
name: pr-check
nodes:
  start:
    type: entry
    label: Start
  has_pr:
    type: terminal
    label: Has PR
  no_pr:
    type: terminal
    label: No PR
edges:
  - from: start
    to: has_pr
    condition:
      type: state_check
      check: has_pr
      expect: true
    priority: 0
  - from: start
    to: no_pr
    condition:
      type: state_check
      check: has_pr
      expect: false
    priority: 1
"#,
        )
        .unwrap();

        let walker = GraphWalker::new(&graph);

        // Task WITH pr_url
        let mut task_with_pr = make_task();
        task_with_pr.pr_url = Some("https://github.com/org/repo/pull/1".to_string());
        let mut cursor = GraphCursor::new("start");
        walker.step(&mut cursor, &make_signal("any"), &task_with_pr);
        assert_eq!(cursor.current_node, "has_pr");

        // Task WITHOUT pr_url
        let mut task_no_pr = make_task();
        task_no_pr.pr_url = None;
        let mut cursor = GraphCursor::new("start");
        walker.step(&mut cursor, &make_signal("any"), &task_no_pr);
        assert_eq!(cursor.current_node, "no_pr");
    }

    #[test]
    fn test_custom_graph_multiple_guards_on_same_counter() {
        // Test that the exceeded guard works at exactly the max boundary
        let graph = WorkflowGraph::from_yaml(
            r#"
name: bounded
nodes:
  start:
    type: entry
    label: Start
  loop:
    type: wait
    label: Loop
  retry:
    type: action
    label: Retry
  fallback:
    type: terminal
    label: Fallback
edges:
  - from: start
    to: loop
  - from: loop
    to: retry
    condition:
      type: signal
      source: failed
    guard:
      counter: attempts
      max: 2
    priority: 0
  - from: loop
    to: fallback
    condition:
      type: signal
      source: failed
    guard:
      counter: attempts
      max: 2
      exceeded: true
    priority: 1
  - from: retry
    to: loop
"#,
        )
        .unwrap();

        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let failed = make_signal("failed");

        let mut cursor = GraphCursor::new("loop");

        // Attempt 1: counter 0 < 2, retry
        walker.step(&mut cursor, &failed, &task);
        assert_eq!(cursor.current_node, "loop");
        assert_eq!(cursor.counters["attempts"], 1);

        // Attempt 2: counter 1 < 2, retry
        walker.step(&mut cursor, &failed, &task);
        assert_eq!(cursor.current_node, "loop");
        assert_eq!(cursor.counters["attempts"], 2);

        // Attempt 3: counter 2 >= 2, fallback
        walker.step(&mut cursor, &failed, &task);
        assert_eq!(cursor.current_node, "fallback");
    }

    #[test]
    fn test_chain_stops_at_wait_nodes() {
        // Auto-chaining should stop at wait nodes, not blast through them
        let graph = WorkflowGraph::from_yaml(
            r#"
name: chain-test
nodes:
  start:
    type: entry
    label: Start
  action1:
    type: action
    label: Action 1
  wait1:
    type: wait
    label: Wait 1
  action2:
    type: action
    label: Action 2
  end:
    type: terminal
    label: End
edges:
  - from: start
    to: action1
  - from: action1
    to: wait1
  - from: wait1
    to: action2
    condition:
      type: signal
      source: go
  - from: action2
    to: end
"#,
        )
        .unwrap();

        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("start");

        // Should chain: start→action1→wait1, then STOP (wait node)
        let outputs = walker.step(&mut cursor, &make_signal("any"), &task);
        assert_eq!(cursor.current_node, "wait1");
        assert_eq!(outputs.len(), 2); // action1 + wait1

        // Now signal "go" should chain: wait1→action2→end
        let outputs = walker.step(&mut cursor, &make_signal("go"), &task);
        assert_eq!(cursor.current_node, "end");
    }

    #[test]
    fn test_infinite_loop_protection() {
        // A graph with only unconditional edges forming a cycle should not infinite loop
        let graph = WorkflowGraph::from_yaml(
            r#"
name: loop
nodes:
  a:
    type: entry
    label: A
  b:
    type: action
    label: B
  c:
    type: action
    label: C
  end:
    type: terminal
    label: End
edges:
  - from: a
    to: b
  - from: b
    to: c
  - from: c
    to: b
  - from: b
    to: end
    condition:
      type: signal
      source: stop
"#,
        )
        .unwrap();

        let walker = GraphWalker::new(&graph);
        let task = make_task();
        let mut cursor = GraphCursor::new("a");

        // Should not hang — max_chain caps at 10 steps
        let outputs = walker.step(&mut cursor, &make_signal("any"), &task);
        assert!(outputs.len() <= 10, "chain should be capped at 10 steps");
    }
}

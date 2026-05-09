//! Workflow graph — a declarative DAG of nodes and edges that drives task lifecycle.
//!
//! Graphs are defined in YAML and loaded at startup. The walker evaluates edges
//! and advances a cursor stored on each task.

pub mod walker;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod builtin;

// ── Core types ────────────────────────────────────────────────────────

/// The type of a graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    Entry,
    Action,
    Wait,
    Terminal,
}

/// What an action node does when the cursor lands on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    DispatchWorker,
    CreatePr,
    Notify,
    Custom,
}

/// Configuration for an action node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionConfig {
    pub kind: ActionKind,
    #[serde(default)]
    pub role: Option<String>,
}

/// A condition that must be true for an edge to fire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Condition {
    Signal {
        source: String,
        #[serde(default, rename = "metadata")]
        metadata_match: HashMap<String, String>,
    },
    AgentOutput {
        source: String,
        #[serde(default, rename = "match")]
        match_fields: HashMap<String, String>,
    },
    StateCheck {
        check: String,
        expect: serde_json::Value,
    },
}

/// A guard restricts traversal based on a named counter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guard {
    pub counter: String,
    #[serde(default)]
    pub max: Option<u32>,
    #[serde(default)]
    pub exceeded: Option<bool>,
}

/// A node in the workflow graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub label: String,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub action: Option<ActionConfig>,
}

/// An edge connecting two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: Option<Condition>,
    #[serde(default)]
    pub guard: Option<Guard>,
    #[serde(default)]
    pub priority: i32,
}

// ── Graph ─────────────────────────────────────────────────────────────

/// A workflow graph: nodes + edges.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowGraph {
    pub name: String,
    pub nodes: HashMap<String, Node>,
    pub edges: Vec<Edge>,
}

impl WorkflowGraph {
    /// Parse a workflow graph from YAML.
    pub fn from_yaml(yaml: &str) -> color_eyre::Result<Self> {
        let raw: RawGraph = serde_yaml::from_str(yaml)?;
        Ok(Self {
            name: raw.name,
            nodes: raw.nodes,
            edges: raw.edges,
        })
    }

    /// Find the entry node (first node with type=entry).
    pub fn entry_node(&self) -> Option<&str> {
        self.nodes
            .iter()
            .find(|(_, n)| n.node_type == NodeType::Entry)
            .map(|(id, _)| id.as_str())
    }

    /// Get all outgoing edges from a node, sorted by priority (lowest first).
    pub fn outgoing_edges(&self, node_id: &str) -> Vec<&Edge> {
        let mut edges: Vec<&Edge> = self.edges.iter().filter(|e| e.from == node_id).collect();
        edges.sort_by_key(|e| e.priority);
        edges
    }
}

/// Raw YAML structure for deserialization.
#[derive(Debug, Deserialize)]
struct RawGraph {
    #[allow(dead_code)]
    name: String,
    nodes: HashMap<String, Node>,
    edges: Vec<Edge>,
}

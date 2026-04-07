//! Graph representation for workflow edges.

/// A directed graph of workflow nodes and edges.
#[derive(Debug, Default)]
pub struct WorkflowGraph {
    edges: Vec<(usize, usize)>,
}

impl WorkflowGraph {
    /// Create an empty workflow graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a directed edge from `src` to `dst`.
    pub fn add_edge(&mut self, src: usize, dst: usize) {
        self.edges.push((src, dst));
    }

    /// Return the number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_count_empty() {
        let graph = WorkflowGraph::new();
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_edge_count_after_adds() {
        let mut graph = WorkflowGraph::new();
        graph.add_edge(0, 1);
        graph.add_edge(1, 2);
        graph.add_edge(2, 3);
        assert_eq!(graph.edge_count(), 3);
    }
}

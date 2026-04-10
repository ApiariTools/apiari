import { useMemo } from 'react';
import type { GraphView, NodeView, TaskView } from '../types';
import './WorkflowGraph.css';

// ── Lane detection ──────────────────────────────────────────────────────
//
// Detect "lanes" by finding the entry node's outgoing edges. Each conditional
// edge from the entry creates a lane. Nodes reachable from that first hop
// (before they merge back into another lane) belong to that lane.
// Shared nodes (entry, terminals) span all lanes.

interface Lane {
  id: string;
  label: string;       // derived from the first node in the lane
  color: string;
  nodeIds: string[];   // ordered nodes in this lane
}

interface LaneLayout {
  entryNode: NodeView;
  lanes: Lane[];
  terminalNodes: NodeView[];
}

const LANE_COLORS = ['#3b82f6', '#f59e0b', '#8b5cf6', '#10b981', '#ef4444', '#ec4899'];

function detectLanes(graph: GraphView): LaneLayout | null {
  const nodeMap = new Map(graph.nodes.map((n) => [n.id, n]));
  const outgoing = new Map<string, typeof graph.edges>();
  for (const edge of graph.edges) {
    if (!outgoing.has(edge.from)) outgoing.set(edge.from, []);
    outgoing.get(edge.from)!.push(edge);
  }

  const entryNode = graph.nodes.find((n) => n.node_type === 'entry');
  if (!entryNode) return null;

  const terminalNodes = graph.nodes.filter((n) => n.node_type === 'terminal');
  const terminalIds = new Set(terminalNodes.map((n) => n.id));

  // Get entry's outgoing edges sorted by priority
  const entryEdges = [...(outgoing.get(entryNode.id) ?? [])].sort(
    (a, b) => (a.priority ?? 0) - (b.priority ?? 0)
  );

  if (entryEdges.length <= 1) {
    // Single-lane workflow — just walk the spine
    const nodes: string[] = [];
    const visited = new Set<string>();
    let current: string | null = entryEdges[0]?.to ?? null;
    while (current && !visited.has(current) && !terminalIds.has(current)) {
      visited.add(current);
      nodes.push(current);
      const edges = outgoing.get(current) ?? [];
      const next = [...edges].sort((a, b) => (a.priority ?? 0) - (b.priority ?? 0));
      // Follow first edge to unvisited non-terminal
      current = next.find((e) => !visited.has(e.to) && !terminalIds.has(e.to))?.to ?? null;
    }
    return {
      entryNode,
      lanes: [{
        id: 'main',
        label: nodeMap.get(nodes[0] ?? '')?.label ?? 'Main',
        color: LANE_COLORS[0],
        nodeIds: nodes,
      }],
      terminalNodes,
    };
  }

  // Multi-lane: each entry edge starts a lane
  const lanes: Lane[] = [];
  const globalVisited = new Set<string>();

  for (let i = 0; i < entryEdges.length; i++) {
    const edge = entryEdges[i];
    const firstNode = nodeMap.get(edge.to);
    if (!firstNode || terminalIds.has(edge.to)) continue;

    const laneNodes: string[] = [];
    const queue = [edge.to];
    const laneVisited = new Set<string>();

    while (queue.length > 0) {
      const nodeId = queue.shift()!;
      if (laneVisited.has(nodeId) || terminalIds.has(nodeId) || globalVisited.has(nodeId)) continue;
      laneVisited.add(nodeId);
      laneNodes.push(nodeId);

      const edges = outgoing.get(nodeId) ?? [];
      for (const e of edges) {
        if (!laneVisited.has(e.to) && !terminalIds.has(e.to) && !globalVisited.has(e.to)) {
          queue.push(e.to);
        }
      }
    }

    for (const id of laneNodes) globalVisited.add(id);

    // Derive lane label from the condition or first node
    let label = edge.label?.replace(/^signal:\s*/, '').replace(/^check:\s*/, '') ?? firstNode.label;
    // Shorten known sources
    if (label.includes('swarm_worker')) label = 'Code';
    else if (label.includes('sentry')) label = 'Customer';
    else if (label.includes('github')) label = 'Product';

    lanes.push({
      id: `lane-${i}`,
      label,
      color: LANE_COLORS[i % LANE_COLORS.length],
      nodeIds: laneNodes,
    });
  }

  return { entryNode, lanes, terminalNodes };
}

// ── Component ──────────────────────────────────────────────────────────

interface Props {
  graph: GraphView;
  tasks: TaskView[];
  selectedNodeId?: string | null;
  onSelectNode?: (id: string) => void;
}

export default function WorkflowGraph({ graph, tasks, selectedNodeId, onSelectNode }: Props) {
  const layout = useMemo(() => detectLanes(graph), [graph]);
  const nodeMap = useMemo(() => new Map(graph.nodes.map((n) => [n.id, n])), [graph]);

  const nodeTaskCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const task of tasks) {
      if (task.cursor?.current_node) {
        counts.set(task.cursor.current_node, (counts.get(task.cursor.current_node) ?? 0) + 1);
      }
    }
    return counts;
  }, [tasks]);

  const activeNodes = useMemo(() => new Set(nodeTaskCounts.keys()), [nodeTaskCounts]);

  const visitedNodes = useMemo(() => {
    const set = new Set<string>();
    for (const task of tasks) {
      if (task.cursor?.history) {
        for (const step of task.cursor.history) {
          set.add(step.from_node);
          set.add(step.to_node);
        }
      }
    }
    return set;
  }, [tasks]);

  if (!layout) {
    return <div className="pipeline" style={{ color: '#94a3b8', padding: 40 }}>No workflow loaded</div>;
  }

  const isMultiLane = layout.lanes.length > 1;

  function renderNode(nodeId: string, laneColor?: string) {
    const node = nodeMap.get(nodeId);
    if (!node) return null;
    const isActive = activeNodes.has(nodeId);
    const isVisited = visitedNodes.has(nodeId);
    const isSelected = nodeId === selectedNodeId;
    const count = nodeTaskCounts.get(nodeId) ?? 0;

    // Determine execution mode from action.kind
    const action = node.action as { kind?: string; role?: string } | undefined;
    const execMode = action?.kind;
    const execBadge = execMode === 'dispatch_worker' ? { icon: '🔀', label: 'A2A', color: '#7c3aed' }
      : execMode === 'custom' ? { icon: '🐝', label: 'inline', color: '#d97706' }
      : execMode === 'create_pr' ? { icon: '⚙️', label: 'system', color: '#64748b' }
      : execMode === 'notify' ? { icon: '📢', label: 'notify', color: '#64748b' }
      : null;

    return (
      <div
        key={nodeId}
        className={[
          'node-card',
          `node-${node.node_type}`,
          isActive && 'node-active',
          isVisited && !isActive && 'node-visited',
          isSelected && 'node-selected',
          onSelectNode && 'node-clickable',
        ].filter(Boolean).join(' ')}
        style={laneColor && !isActive && !isVisited ? { borderLeftColor: laneColor, borderLeftWidth: 3 } : undefined}
        onClick={onSelectNode ? () => onSelectNode(nodeId) : undefined}
      >
        <span className="node-type-dot" />
        <div className="node-content">
          <span className="node-label">{node.label}</span>
          {execBadge && (
            <span style={{
              fontSize: 10, color: execBadge.color, fontWeight: 600,
              display: 'flex', alignItems: 'center', gap: 3, marginTop: 1,
            }}>
              <span>{execBadge.icon}</span> {execBadge.label}
              {action?.role && <span style={{ color: '#94a3b8', fontWeight: 400 }}> · {action.role}</span>}
            </span>
          )}
        </div>
        {count > 0 && <span className="node-badge">{count}</span>}
      </div>
    );
  }

  if (!isMultiLane) {
    // Single-lane: vertical pipeline (original behavior)
    return (
      <div className="pipeline">
        <div className="pipeline-step">{renderNode(layout.entryNode.id)}</div>
        <div className="connector"><div className="connector-line" /></div>
        {layout.lanes[0].nodeIds.map((id, i) => (
          <div key={id} className="pipeline-step">
            {renderNode(id)}
            {i < layout.lanes[0].nodeIds.length - 1 && (
              <div className="connector"><div className="connector-line" /></div>
            )}
          </div>
        ))}
        {layout.terminalNodes.length > 0 && (
          <>
            <div className="connector"><div className="connector-line" /></div>
            <div className="terminal-row">
              {layout.terminalNodes.map((n) => renderNode(n.id))}
            </div>
          </>
        )}
      </div>
    );
  }

  // Multi-lane: swimlane layout
  return (
    <div className="swimlane-container">
      {/* Entry node — spans all lanes */}
      <div className="swimlane-entry">
        {renderNode(layout.entryNode.id)}
      </div>

      {/* Lane headers + connector */}
      <div className="swimlane-connector">
        <svg width="100%" height="40" className="swimlane-svg">
          <line x1="50%" y1="0" x2="50%" y2="20" stroke="#e2e8f0" strokeWidth="2" />
          {layout.lanes.map((_, i) => {
            const pct = ((i + 0.5) / layout.lanes.length) * 100;
            return (
              <g key={i}>
                <line x1="50%" y1="20" x2={`${pct}%`} y2="40" stroke="#e2e8f0" strokeWidth="2" />
              </g>
            );
          })}
        </svg>
      </div>

      {/* Lanes */}
      <div className="swimlane-lanes">
        {layout.lanes.map((lane) => (
          <div key={lane.id} className="swimlane-lane">
            <div className="swimlane-lane-header" style={{ borderBottomColor: lane.color }}>
              <span className="swimlane-lane-dot" style={{ background: lane.color }} />
              <span className="swimlane-lane-title">{lane.label}</span>
            </div>
            <div className="swimlane-lane-nodes">
              {lane.nodeIds.map((id, i) => (
                <div key={id} className="swimlane-node-wrapper">
                  {renderNode(id, lane.color)}
                  {i < lane.nodeIds.length - 1 && (
                    <div className="connector"><div className="connector-line" /></div>
                  )}
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>

      {/* Terminal nodes — span all lanes */}
      {layout.terminalNodes.length > 0 && (
        <>
          <div className="swimlane-connector">
            <svg width="100%" height="40" className="swimlane-svg">
              {layout.lanes.map((_, i) => {
                const pct = ((i + 0.5) / layout.lanes.length) * 100;
                return (
                  <line key={i} x1={`${pct}%`} y1="0" x2="50%" y2="40" stroke="#e2e8f0" strokeWidth="2" />
                );
              })}
            </svg>
          </div>
          <div className="terminal-row">
            {layout.terminalNodes.map((n) => renderNode(n.id))}
          </div>
        </>
      )}
    </div>
  );
}

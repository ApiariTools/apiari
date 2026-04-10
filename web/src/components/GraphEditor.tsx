import { useState } from 'react';
import { saveGraph } from '../api';
import type { GraphView, NodeView, EdgeView, NodeType } from '../types';
import { NODE_TYPES } from '../types';
import './GraphEditor.css';

const ACTION_KINDS = [
  { value: 'dispatch_worker', label: 'Dispatch Worker (A2A)' },
  { value: 'create_pr', label: 'Create PR' },
  { value: 'notify', label: 'Send Notification' },
  { value: 'custom', label: 'Custom' },
] as const;

const COMMON_SIGNALS = [
  'swarm_worker_spawned',
  'swarm_worker_running',
  'swarm_branch_ready',
  'swarm_review_verdict',
  'swarm_pr_opened',
  'github_ci_pass',
  'github_ci_failure',
  'github_merged_pr',
  'github_pr_closed',
];

// ── Helper to read/write nested JSON fields ────────────────────────────

function getActionField(node: NodeView, field: string): string {
  const action = node.action as Record<string, unknown> | undefined;
  return (action?.[field] as string) ?? '';
}

function setActionField(node: NodeView, field: string, value: string | undefined): Partial<NodeView> {
  const action = (node.action as Record<string, unknown> | undefined) ?? {};
  const updated = { ...action, [field]: value || undefined };
  // Clean up undefined fields
  for (const key of Object.keys(updated)) {
    if (updated[key] === undefined || updated[key] === '') delete updated[key];
  }
  // Ensure kind always exists
  if (!updated.kind) updated.kind = 'dispatch_worker';
  return { action: Object.keys(updated).length > 0 ? updated : undefined };
}

function getWaitForField(node: NodeView, field: string): string {
  const wf = node.wait_for as Record<string, unknown> | undefined;
  return (wf?.[field] as string) ?? '';
}

function setWaitForField(node: NodeView, field: string, value: string | undefined): Partial<NodeView> {
  const wf = (node.wait_for as Record<string, unknown> | undefined) ?? {};
  const updated = { ...wf, [field]: value || undefined };
  for (const key of Object.keys(updated)) {
    if (updated[key] === undefined || updated[key] === '') delete updated[key];
  }
  return { wait_for: Object.keys(updated).length > 0 ? updated : undefined };
}

// ── Action editor sub-component ────────────────────────────────────────

function ActionEditor({ node, updateNode }: { node: NodeView; updateNode: (id: string, u: Partial<NodeView>) => void }) {
  const kind = getActionField(node, 'kind') || 'dispatch_worker';

  return (
    <div className="config-section">
      <span className="config-section-title">Action Config</span>

      <div className="editor-field">
        <span className="field-label">Kind</span>
        <select
          className="editor-select"
          value={kind}
          onChange={(e) => updateNode(node.id, setActionField(node, 'kind', e.target.value))}
        >
          {ACTION_KINDS.map((ak) => (
            <option key={ak.value} value={ak.value}>{ak.label}</option>
          ))}
        </select>
      </div>

      {(kind === 'dispatch_worker') && (
        <>
          <div className="editor-field">
            <span className="field-label">Prompt</span>
            <textarea
              className="editor-textarea"
              value={getActionField(node, 'prompt')}
              placeholder="Instructions for the agent..."
              rows={3}
              onChange={(e) => updateNode(node.id, setActionField(node, 'prompt', e.target.value))}
            />
          </div>
          <div className="editor-field">
            <span className="field-label">Role</span>
            <input
              className="editor-input"
              value={getActionField(node, 'role')}
              placeholder="e.g. coder, reviewer, classifier"
              onChange={(e) => updateNode(node.id, setActionField(node, 'role', e.target.value))}
            />
          </div>
        </>
      )}

      {kind === 'notify' && (
        <>
          <div className="editor-field">
            <span className="field-label">Channel</span>
            <input
              className="editor-input"
              value={getActionField(node, 'channel')}
              placeholder="e.g. slack, pagerduty, telegram"
              onChange={(e) => updateNode(node.id, setActionField(node, 'channel', e.target.value))}
            />
          </div>
          <div className="editor-field">
            <span className="field-label">Template</span>
            <textarea
              className="editor-textarea"
              value={getActionField(node, 'template')}
              placeholder="Message template..."
              rows={2}
              onChange={(e) => updateNode(node.id, setActionField(node, 'template', e.target.value))}
            />
          </div>
        </>
      )}
    </div>
  );
}

// ── Wait-for editor sub-component ──────────────────────────────────────

function WaitForEditor({ node, updateNode }: { node: NodeView; updateNode: (id: string, u: Partial<NodeView>) => void }) {
  const source = getWaitForField(node, 'source');
  const isCustom = source !== '' && !COMMON_SIGNALS.includes(source);

  return (
    <div className="config-section">
      <span className="config-section-title">Waiting For</span>

      <div className="editor-field">
        <span className="field-label">Signal source</span>
        <select
          className="editor-select"
          value={isCustom ? '__custom__' : source}
          onChange={(e) => {
            if (e.target.value === '__custom__') return;
            updateNode(node.id, setWaitForField(node, 'source', e.target.value));
          }}
        >
          <option value="">— select signal —</option>
          {COMMON_SIGNALS.map((s) => (
            <option key={s} value={s}>{s}</option>
          ))}
          {isCustom && <option value="__custom__">custom: {source}</option>}
          <option value="__custom__">+ custom...</option>
        </select>
        {(isCustom || source === '__custom__') && (
          <input
            className="editor-input"
            style={{ marginTop: 4 }}
            value={source === '__custom__' ? '' : source}
            placeholder="Custom signal source"
            onChange={(e) => updateNode(node.id, setWaitForField(node, 'source', e.target.value))}
          />
        )}
      </div>

      <div className="editor-field">
        <span className="field-label">Description</span>
        <input
          className="editor-input"
          value={getWaitForField(node, 'description')}
          placeholder="What are we waiting for?"
          onChange={(e) => updateNode(node.id, setWaitForField(node, 'description', e.target.value))}
        />
      </div>
    </div>
  );
}

interface Props {
  graph: GraphView;
  selectedNodeId: string | null;
  onSelectNode: (id: string | null) => void;
  onGraphChange: (graph: GraphView) => void;
}

export default function GraphEditor({ graph, selectedNodeId, onSelectNode, onGraphChange }: Props) {
  const [saving, setSaving] = useState(false);
  const [saveStatus, setSaveStatus] = useState<'idle' | 'saved' | 'error'>('idle');
  const [errorMsg, setErrorMsg] = useState('');
  const [addingNode, setAddingNode] = useState(false);
  const [newNodeId, setNewNodeId] = useState('');
  const [newNodeLabel, setNewNodeLabel] = useState('');
  const [newNodeType, setNewNodeType] = useState<NodeType>('action');

  const selectedNode = graph.nodes.find((n) => n.id === selectedNodeId);
  const outgoingEdges = graph.edges.filter((e) => e.from === selectedNodeId);
  const incomingEdges = graph.edges.filter((e) => e.to === selectedNodeId);

  // ── Node editing ───────────────────────────────────────────────────

  const updateNode = (id: string, updates: Partial<NodeView>) => {
    const newNodes = graph.nodes.map((n) =>
      n.id === id ? { ...n, ...updates } : n,
    );
    onGraphChange({ ...graph, nodes: newNodes });
  };

  const deleteNode = (id: string) => {
    const newNodes = graph.nodes.filter((n) => n.id !== id);
    const newEdges = graph.edges.filter((e) => e.from !== id && e.to !== id);
    onGraphChange({ ...graph, nodes: newNodes, edges: newEdges });
    onSelectNode(null);
  };

  const addNode = () => {
    if (!newNodeId.trim() || !newNodeLabel.trim()) return;
    const id = newNodeId.trim().toLowerCase().replace(/\s+/g, '_');
    if (graph.nodes.some((n) => n.id === id)) return;

    const node: NodeView = {
      id,
      label: newNodeLabel.trim(),
      node_type: newNodeType,
      stage: null,
    };
    onGraphChange({ ...graph, nodes: [...graph.nodes, node] });
    setAddingNode(false);
    setNewNodeId('');
    setNewNodeLabel('');
    onSelectNode(id);
  };

  // ── Edge editing ───────────────────────────────────────────────────

  const addEdge = (from: string) => {
    const otherNodes = graph.nodes.filter((n) => n.id !== from);
    if (otherNodes.length === 0) return;
    const to = otherNodes[0].id;
    const edge: EdgeView = {
      from,
      to,
      label: null,
      has_condition: false,
      priority: 0,
    };
    onGraphChange({ ...graph, edges: [...graph.edges, edge] });
  };

  const updateEdge = (index: number, updates: Partial<EdgeView>) => {
    const newEdges = graph.edges.map((e, i) =>
      i === index ? { ...e, ...updates } : e,
    );
    onGraphChange({ ...graph, edges: newEdges });
  };

  const deleteEdge = (index: number) => {
    const newEdges = graph.edges.filter((_, i) => i !== index);
    onGraphChange({ ...graph, edges: newEdges });
  };

  // ── Save ───────────────────────────────────────────────────────────

  const handleSave = async () => {
    setSaving(true);
    setSaveStatus('idle');
    const result = await saveGraph(graph);
    setSaving(false);
    if (result.ok) {
      setSaveStatus('saved');
      setTimeout(() => setSaveStatus('idle'), 2000);
    } else {
      setSaveStatus('error');
      setErrorMsg(result.error ?? 'Unknown error');
    }
  };

  // ── Render ─────────────────────────────────────────────────────────

  return (
    <div className="editor">
      {/* Header */}
      <div className="editor-header">
        <span className="editor-title">Editor</span>
        <button
          className={`save-btn ${saveStatus}`}
          onClick={handleSave}
          disabled={saving}
        >
          {saving ? 'Saving...' : saveStatus === 'saved' ? 'Saved ✓' : 'Save'}
        </button>
      </div>

      {saveStatus === 'error' && (
        <div className="editor-error">{errorMsg}</div>
      )}

      {/* Graph name */}
      <div className="editor-section">
        <label className="editor-label">Graph Name</label>
        <input
          className="editor-input"
          value={graph.name}
          onChange={(e) => onGraphChange({ ...graph, name: e.target.value })}
        />
      </div>

      {/* Node list */}
      <div className="editor-section">
        <div className="editor-section-header">
          <label className="editor-label">Nodes ({graph.nodes.length})</label>
          <button className="editor-add-btn" onClick={() => setAddingNode(true)}>+ Add</button>
        </div>

        {addingNode && (
          <div className="add-node-form">
            <input
              className="editor-input"
              placeholder="Node ID (e.g. my_step)"
              value={newNodeId}
              onChange={(e) => setNewNodeId(e.target.value)}
            />
            <input
              className="editor-input"
              placeholder="Label (e.g. My Step)"
              value={newNodeLabel}
              onChange={(e) => setNewNodeLabel(e.target.value)}
            />
            <select
              className="editor-select"
              value={newNodeType}
              onChange={(e) => setNewNodeType(e.target.value as NodeType)}
            >
              {NODE_TYPES.map((t) => (
                <option key={t} value={t}>{t}</option>
              ))}
            </select>
            <div className="add-node-actions">
              <button className="editor-btn-primary" onClick={addNode}>Add</button>
              <button className="editor-btn-secondary" onClick={() => setAddingNode(false)}>Cancel</button>
            </div>
          </div>
        )}

        <div className="node-list">
          {graph.nodes.map((node) => (
            <div
              key={node.id}
              className={`node-list-item ${node.id === selectedNodeId ? 'selected' : ''}`}
              onClick={() => onSelectNode(node.id === selectedNodeId ? null : node.id)}
            >
              <span className={`node-dot node-dot-${node.node_type}`} />
              <span className="node-list-label">{node.label}</span>
              <span className="node-list-id">{node.id}</span>
            </div>
          ))}
        </div>
      </div>

      {/* Selected node editor */}
      {selectedNode && (
        <div className="editor-section">
          <label className="editor-label">Edit: {selectedNode.id}</label>

          <div className="editor-field">
            <span className="field-label">Label</span>
            <input
              className="editor-input"
              value={selectedNode.label}
              onChange={(e) => updateNode(selectedNode.id, { label: e.target.value })}
            />
          </div>

          <div className="editor-field">
            <span className="field-label">Type</span>
            <select
              className="editor-select"
              value={selectedNode.node_type}
              onChange={(e) => updateNode(selectedNode.id, { node_type: e.target.value as NodeType })}
            >
              {NODE_TYPES.map((t) => (
                <option key={t} value={t}>{t}</option>
              ))}
            </select>
          </div>

          <div className="editor-field">
            <span className="field-label">Stage</span>
            <input
              className="editor-input"
              value={selectedNode.stage ?? ''}
              placeholder="e.g. InProgress"
              onChange={(e) => updateNode(selectedNode.id, { stage: e.target.value || null })}
            />
          </div>

          {/* Action config — shown for action nodes */}
          {selectedNode.node_type === 'action' && (
            <ActionEditor node={selectedNode} updateNode={updateNode} />
          )}

          {/* Wait-for config — shown for wait nodes */}
          {selectedNode.node_type === 'wait' && (
            <WaitForEditor node={selectedNode} updateNode={updateNode} />
          )}

          {/* Notify tier — shown for all nodes */}
          <div className="editor-field">
            <span className="field-label">Notify</span>
            <div className="radio-group">
              {['silent', 'badge', 'chat'].map((tier) => (
                <label key={tier} className="radio-option">
                  <input
                    type="radio"
                    name={`notify-${selectedNode.id}`}
                    checked={(selectedNode.notify ?? 'silent') === tier}
                    onChange={() => updateNode(selectedNode.id, { notify: tier })}
                  />
                  <span>{tier}</span>
                </label>
              ))}
              <label className="radio-option">
                <input
                  type="radio"
                  name={`notify-${selectedNode.id}`}
                  checked={!selectedNode.notify}
                  onChange={() => updateNode(selectedNode.id, { notify: undefined })}
                />
                <span>none</span>
              </label>
            </div>
          </div>

          {/* Outgoing edges */}
          <div className="edge-section">
            <div className="editor-section-header">
              <span className="field-label">Outgoing edges ({outgoingEdges.length})</span>
              <button className="editor-add-btn" onClick={() => addEdge(selectedNode.id)}>+</button>
            </div>
            {outgoingEdges.map((edge) => {
              const edgeIndex = graph.edges.indexOf(edge);
              return (
                <div key={edgeIndex} className="edge-item">
                  <span className="edge-arrow">→</span>
                  <select
                    className="editor-select edge-target"
                    value={edge.to}
                    onChange={(e) => updateEdge(edgeIndex, { to: e.target.value })}
                  >
                    {graph.nodes.map((n) => (
                      <option key={n.id} value={n.id}>{n.label}</option>
                    ))}
                  </select>
                  <button
                    className="edge-delete"
                    onClick={() => deleteEdge(edgeIndex)}
                    title="Remove edge"
                  >
                    ×
                  </button>
                </div>
              );
            })}
          </div>

          {/* Incoming edges */}
          {incomingEdges.length > 0 && (
            <div className="edge-section">
              <span className="field-label">Incoming from</span>
              <div className="incoming-list">
                {incomingEdges.map((edge, i) => (
                  <span key={i} className="incoming-badge">
                    {graph.nodes.find((n) => n.id === edge.from)?.label ?? edge.from}
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* Delete node */}
          <button
            className="delete-node-btn"
            onClick={() => deleteNode(selectedNode.id)}
          >
            Delete Node
          </button>
        </div>
      )}
    </div>
  );
}

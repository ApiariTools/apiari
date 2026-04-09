// Types matching the Rust daemon HTTP API

export interface NodeView {
  id: string;
  label: string;
  node_type: 'entry' | 'action' | 'wait' | 'terminal';
  stage: string | null;
  action?: unknown;       // NodeAction config (JSON)
  wait_for?: unknown;     // WaitFor config (JSON)
  notify?: string | null; // "silent" | "badge" | "chat"
  description?: string | null; // human-readable summary of what the node does
}

export interface EdgeView {
  from: string;
  to: string;
  label: string | null;
  has_condition: boolean;
  condition?: unknown;  // full JSON condition for round-trip editing
  guard?: unknown;      // full JSON guard for round-trip editing
  priority: number;
}

export interface GraphView {
  name: string;
  nodes: NodeView[];
  edges: EdgeView[];
}

export interface StepView {
  from_node: string;
  to_node: string;
  trigger: string;
  timestamp: string;
}

export interface CursorView {
  current_node: string;
  counters: Record<string, number>;
  history: StepView[];
}

export interface TaskView {
  id: string;
  title: string;
  stage: string;
  worker_id: string | null;
  pr_url: string | null;
  created_at: string;
  updated_at: string;
  cursor: CursorView | null;
}

export interface SignalView {
  id: number;
  workspace: string;
  source: string;
  title: string;
  severity: string;
  status?: string;
  url?: string;
  created_at: string;
}

export type WsMessage =
  | { type: 'snapshot'; tasks: TaskView[]; graph: GraphView }
  | { type: 'task_updated'; task: TaskView }
  | { type: 'signal_processed'; source: string; title: string }
  | { type: 'graph_updated'; graph: GraphView }
  | { type: 'signal'; id: number; workspace: string; source: string; title: string; severity: string; url?: string; created_at: string };

export const NODE_TYPES = ['entry', 'action', 'wait', 'terminal'] as const;
export type NodeType = typeof NODE_TYPES[number];

// ── Bee (coordinator) config ──────────────────────────────────────────

export interface SignalHookView {
  source: string;
  prompt: string;
  action?: string;
  ttl_secs: number;
}

export interface BeeConfigView {
  name: string;
  provider: string;
  model: string;
  max_turns: number;
  prompt?: string;
  max_session_turns: number;
  signal_hooks: SignalHookView[];
  topic_id?: number;
}

export interface BeesConfigResponse {
  workspace: string;
  bees: BeeConfigView[];
}

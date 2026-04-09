import type { BeeConfigView, BeesConfigResponse, GraphView, TaskView, WsMessage } from './types';

const API_BASE = '/api';

export async function fetchGraph(workspace?: string): Promise<GraphView> {
  const qs = workspace ? `?workspace=${encodeURIComponent(workspace)}` : '';
  const res = await fetch(`${API_BASE}/graph${qs}`);
  return res.json();
}

export async function saveGraph(graph: GraphView): Promise<{ ok: boolean; error?: string }> {
  const res = await fetch(`${API_BASE}/graph`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(graph),
  });
  if (!res.ok) {
    const text = await res.text();
    return { ok: false, error: text };
  }
  return { ok: true };
}

export async function fetchTasks(): Promise<TaskView[]> {
  const res = await fetch(`${API_BASE}/tasks`);
  return res.json();
}

export async function clearTasks(): Promise<void> {
  await fetch(`${API_BASE}/tasks`, { method: 'DELETE' });
}

export async function injectSignal(
  source: string,
  title: string,
  metadata?: Record<string, unknown>,
): Promise<void> {
  await fetch(`${API_BASE}/signal`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ source, title, metadata }),
  });
}

export async function fetchWorkspaces(): Promise<string[]> {
  const res = await fetch(`${API_BASE}/workspaces`);
  return res.json();
}

export async function fetchBees(workspace?: string): Promise<BeesConfigResponse> {
  const qs = workspace ? `?workspace=${encodeURIComponent(workspace)}` : '';
  const res = await fetch(`${API_BASE}/bees${qs}`);
  return res.json();
}

export async function saveBees(bees: BeeConfigView[], workspace?: string): Promise<{ ok: boolean; error?: string }> {
  const qs = workspace ? `?workspace=${encodeURIComponent(workspace)}` : '';
  const res = await fetch(`${API_BASE}/bees${qs}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(bees),
  });
  if (!res.ok) {
    const text = await res.text();
    return { ok: false, error: text };
  }
  return { ok: true };
}

export async function fetchConversations(workspace?: string): Promise<Array<{
  role: string;
  content: string;
  source: string | null;
  bee: string;
  workspace: string;
  created_at: string;
}>> {
  const qs = workspace ? `?workspace=${encodeURIComponent(workspace)}` : '';
  const res = await fetch(`${API_BASE}/conversations${qs}`);
  return res.json();
}

export async function sendChat(
  workspace: string,
  text: string,
  bee?: string,
): Promise<{ type: string; text: string }> {
  const res = await fetch(`${API_BASE}/chat`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspace, text, bee }),
  });
  return res.json();
}

export function connectWs(onMessage: (msg: WsMessage) => void): WebSocket {
  const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(`${proto}//${window.location.host}${API_BASE}/ws`);

  ws.onmessage = (event) => {
    try {
      const msg = JSON.parse(event.data) as WsMessage;
      onMessage(msg);
    } catch {
      console.warn('Failed to parse WS message:', event.data);
    }
  };

  ws.onclose = () => {
    console.log('WebSocket closed, reconnecting in 2s...');
    setTimeout(() => connectWs(onMessage), 2000);
  };

  return ws;
}

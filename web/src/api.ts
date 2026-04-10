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

export async function fetchBriefing(): Promise<Array<{
  id: string;
  priority: string;
  icon: string;
  title: string;
  body: string | null;
  workspace: string;
  source: string;
  url: string | null;
  actions: Array<{ label: string; style: string }>;
  timestamp: string;
}>> {
  const res = await fetch(`${API_BASE}/briefing`);
  return res.json();
}

export async function fetchCanvas(workspace: string, bee: string): Promise<{
  workspace: string;
  bee: string;
  content: string;
}> {
  const res = await fetch(`${API_BASE}/canvas?workspace=${encodeURIComponent(workspace)}&bee=${encodeURIComponent(bee)}`);
  return res.json();
}

export async function fetchBeeActivity(): Promise<Array<{
  id: string;
  priority: string;
  icon: string;
  title: string;
  body: string | null;
  workspace: string;
  source: string;
  timestamp: string;
}>> {
  const res = await fetch(`${API_BASE}/bee-activity`);
  return res.json();
}

export async function fetchSignals(workspace?: string): Promise<Array<{
  id: number;
  workspace: string;
  source: string;
  title: string;
  severity: string;
  status: string;
  url: string | null;
  created_at: string;
}>> {
  const qs = workspace ? `?workspace=${encodeURIComponent(workspace)}` : '';
  const res = await fetch(`${API_BASE}/signals${qs}`);
  return res.json();
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

export async function dismissBriefingItem(signalId: number, workspace: string): Promise<void> {
  await fetch(`${API_BASE}/briefing/dismiss`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ signal_id: signalId, workspace }),
  });
}

export async function snoozeBriefingItem(signalId: number, workspace: string, hours: number = 1): Promise<void> {
  await fetch(`${API_BASE}/briefing/snooze`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ signal_id: signalId, workspace, hours }),
  });
}

export async function sendChat(
  workspace: string,
  text: string,
  bee?: string,
  onToken?: (text: string) => void,
): Promise<string> {
  const res = await fetch(`${API_BASE}/chat`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspace, text, bee }),
  });

  const reader = res.body?.getReader();
  const decoder = new TextDecoder();
  let fullText = '';

  if (!reader) return '(no response)';

  let buffer = '';
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;

    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split('\n');
    buffer = lines.pop() ?? '';

    for (const line of lines) {
      if (line.startsWith('data: ')) {
        try {
          const event = JSON.parse(line.slice(6));
          if (event.type === 'token') {
            fullText += event.text;
            onToken?.(fullText);
          } else if (event.type === 'error') {
            return event.text;
          }
        } catch { /* ignore parse errors */ }
      }
    }
  }

  return fullText || '(no response)';
}

export async function runWorkflow(
  workspace: string,
  topic: string,
  bee?: string,
  lane?: string,
  onStepStart?: (step: string, label: string) => void,
  onToken?: (text: string) => void,
  onStepDone?: (step: string) => void,
): Promise<string> {
  const res = await fetch(`${API_BASE}/workflow/run`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspace, topic, bee, lane }),
  });

  const reader = res.body?.getReader();
  const decoder = new TextDecoder();
  let fullText = '';

  if (!reader) return '(no response)';

  let buffer = '';
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;

    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split('\n');
    buffer = lines.pop() ?? '';

    for (const line of lines) {
      if (line.startsWith('data: ')) {
        try {
          const event = JSON.parse(line.slice(6));
          if (event.type === 'token') {
            fullText += event.text;
            onToken?.(fullText);
          } else if (event.type === 'step_start') {
            onStepStart?.(event.step, event.label);
          } else if (event.type === 'step_done') {
            onStepDone?.(event.step);
          } else if (event.type === 'error') {
            return event.text;
          }
        } catch { /* ignore */ }
      }
    }
  }

  return fullText || '(no response)';
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

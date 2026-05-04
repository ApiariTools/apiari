import type { Workspace, Bot, Worker, WorkerDetail, Message, Repo, Doc, ResearchTask, Followup, Signal } from "./types";

const BASE = "/api";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`);
  return res.json();
}

/** Build the workspace path prefix, routing through the proxy for remote workspaces */
function wsPath(workspace: string, remote?: string): string {
  if (remote) return `/remotes/${remote}/workspaces/${workspace}`;
  return `/workspaces/${workspace}`;
}

export function getWorkspaces(): Promise<Workspace[]> {
  return get("/workspaces");
}

export function getBots(workspace: string, remote?: string): Promise<Bot[]> {
  return get(`${wsPath(workspace, remote)}/bots`);
}

export function getWorkers(workspace: string, remote?: string): Promise<Worker[]> {
  return get(`${wsPath(workspace, remote)}/workers`);
}

export function getRepos(workspace: string, remote?: string): Promise<Repo[]> {
  return get(`${wsPath(workspace, remote)}/repos`);
}

export function getConversations(
  workspace: string,
  bot: string,
  limit?: number,
  remote?: string,
): Promise<Message[]> {
  const params = limit ? `?limit=${limit}` : "";
  return get(`${wsPath(workspace, remote)}/conversations/${bot}${params}`);
}

export function getWorkerDetail(
  workspace: string,
  workerId: string,
  remote?: string,
): Promise<WorkerDetail> {
  return get(`${wsPath(workspace, remote)}/workers/${workerId}`);
}

export async function sendWorkerMessage(
  workspace: string,
  workerId: string,
  message: string,
  remote?: string,
): Promise<{ ok: boolean; error?: string }> {
  const res = await fetch(
    `${BASE}${wsPath(workspace, remote)}/workers/${workerId}/send`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message }),
    },
  );
  return res.json();
}

export interface BotStatus {
  status: string;
  streaming_content: string;
  tool_name: string | null;
}

export function getBotStatus(
  workspace: string,
  bot: string,
  remote?: string,
): Promise<BotStatus> {
  return get(`${wsPath(workspace, remote)}/bots/${bot}/status`);
}

export async function cancelBot(
  workspace: string,
  bot: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/bots/${bot}/cancel`, {
    method: "POST",
  });
  return res.json();
}

export function getUnread(workspace: string, remote?: string): Promise<Record<string, number>> {
  return get(`${wsPath(workspace, remote)}/unread`);
}

export async function markSeen(workspace: string, bot: string, remote?: string): Promise<void> {
  await fetch(`${BASE}${wsPath(workspace, remote)}/seen/${bot}`, { method: "POST" });
}

export interface ManagedWebSocket {
  close(): void;
}

export function connectWebSocket(
  onEvent: (event: { type: string; workspace: string; bot: string; [key: string]: unknown }) => void,
): ManagedWebSocket {
  let intentionalClose = false;
  let reconnectTimer: number | null = null;
  let ws: WebSocket | null = null;
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const wsUrl = `${protocol}//${window.location.host}/ws`;

  function connect() {
    if (intentionalClose) return;
    ws = new WebSocket(wsUrl);
    ws.onmessage = (e) => {
      try {
        const event = JSON.parse(e.data);
        onEvent(event);
      } catch {
        return;
      }
    };
    ws.onclose = () => {
      ws = null;
      if (!intentionalClose) {
        reconnectTimer = window.setTimeout(() => {
          reconnectTimer = null;
          connect();
        }, 3000);
      }
    };
  }

  connect();

  return {
    close() {
      intentionalClose = true;
      if (reconnectTimer !== null) {
        window.clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      ws?.close();
    },
  };
}

export async function textToSpeech(text: string, voice?: string): Promise<ArrayBuffer | null> {
  try {
    const res = await fetch(`${BASE}/tts`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text, ...(voice ? { voice } : {}) }),
    });
    if (!res.ok) return null;
    return await res.arrayBuffer();
  } catch {
    return null;
  }
}

export interface ProviderUsage {
  name: string;
  status: string;
  usage_percent: number | null;
  remaining: string | null;
  limit: string | null;
  resets_at: string | null;
}

export interface UsageData {
  installed: boolean;
  providers: ProviderUsage[];
  updated_at: string | null;
}

export function getUsage(): Promise<UsageData> {
  return get("/usage");
}

export function getDocs(workspace: string, remote?: string): Promise<Doc[]> {
  return get(`${wsPath(workspace, remote)}/docs`);
}

export function getDoc(workspace: string, filename: string, remote?: string): Promise<Doc> {
  return get(`${wsPath(workspace, remote)}/docs/${encodeURIComponent(filename)}`);
}

export async function saveDoc(
  workspace: string,
  filename: string,
  content: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/docs/${encodeURIComponent(filename)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  if (!res.ok) throw new Error(`PUT docs/${filename}: ${res.status}`);
  return res.json();
}

export async function deleteDoc(
  workspace: string,
  filename: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/docs/${encodeURIComponent(filename)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`DELETE docs/${filename}: ${res.status}`);
  return res.json();
}

export function getFollowups(workspace: string, remote?: string): Promise<Followup[]> {
  return get(`${wsPath(workspace, remote)}/followups`);
}

export async function cancelFollowup(
  workspace: string,
  followupId: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/followups/${followupId}`, {
    method: "DELETE",
  });
  return res.json();
}

export function getResearchTasks(workspace: string, remote?: string): Promise<ResearchTask[]> {
  return get(`${wsPath(workspace, remote)}/research`);
}

export function getSignals(
  workspace: string,
  options?: { history?: boolean; limit?: number },
): Promise<Signal[]> {
  const params = new URLSearchParams({ workspace });
  if (options?.history) params.set("history", "true");
  if (options?.limit) params.set("limit", String(options.limit));
  return get(`/signals?${params.toString()}`);
}

export async function startResearch(
  workspace: string,
  topic: string,
  remote?: string,
): Promise<{ id: string; topic: string; status: string }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/research`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ topic }),
  });
  if (!res.ok) throw new Error(`POST research: ${res.status}`);
  return res.json();
}

export async function getWorkerDiff(workspace: string, workerId: string, remote?: string): Promise<string | null> {
  const data = await get<{ diff: string | null }>(`${wsPath(workspace, remote)}/workers/${workerId}/diff`);
  return data.diff;
}

export async function sendMessage(
  workspace: string,
  bot: string,
  message: string,
  attachments?: Array<{ name: string; type: string; dataUrl: string }>,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/chat/${bot}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ message, attachments }),
  });
  if (!res.ok) throw new Error(`POST chat/${bot}: ${res.status}`);
  const data = await res.json();
  if (!data.ok) {
    throw new Error(data.error ?? `POST chat/${bot} failed`);
  }
  return data;
}

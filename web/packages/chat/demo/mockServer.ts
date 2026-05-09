/**
 * Intercepts fetch + WebSocket so the demo runs without a real apiari daemon.
 * Install this before rendering by calling installMockServer().
 */

// ── Mock data ──────────────────────────────────────────────────────────────

const BOTS = [
  { name: "Main", color: "#f5c542", watch: [] },
  { name: "Code", color: "#4a9eff", watch: [] },
  { name: "Research", color: "#b56ef0", watch: [] },
];

const SEED_MESSAGES: Record<string, Array<{ role: string; content: string }>> = {
  Main: [
    { role: "user", content: "Hey, what can you help me with?" },
    {
      role: "assistant",
      content:
        "I'm your general-purpose workspace assistant. I can help with questions, drafting documents, summarizing context, coordinating tasks, or just thinking through problems. What's on your mind?",
    },
  ],
  Code: [
    { role: "user", content: "Can you review this TypeScript?" },
    {
      role: "assistant",
      content:
        "Sure — paste it in and I'll take a look. I can check types, suggest improvements, spot bugs, or explain what a block does.",
    },
  ],
  Research: [],
};

const CANNED_RESPONSES: Record<string, string[]> = {
  Main: [
    "Got it. Let me think through that for you.\n\nThe short answer is: it depends on your constraints, but the most common approach is to start simple and iterate.",
    "That's a good question. Here are a few things worth considering:\n\n1. **Context matters** — what worked in one situation may not translate directly.\n2. **Start with first principles** — break the problem down before reaching for a solution.\n3. **Validate early** — don't over-engineer before you know if the approach works.",
    "Happy to help with that. Can you give me a bit more context about what you're trying to achieve?",
    "Makes sense. Here's how I'd approach it:\n\n- Define the outcome you actually want\n- Identify the biggest unknown\n- Run the cheapest possible test to resolve it\n\nWant me to help work through any of these steps?",
  ],
  Code: [
    "Here's a clean way to do that in TypeScript:\n\n```ts\nfunction debounce<T extends (...args: unknown[]) => void>(\n  fn: T,\n  delay: number\n): (...args: Parameters<T>) => void {\n  let timer: ReturnType<typeof setTimeout>;\n  return (...args) => {\n    clearTimeout(timer);\n    timer = setTimeout(() => fn(...args), delay);\n  };\n}\n```\n\nThis is fully generic and type-safe — `Parameters<T>` infers the argument types from the original function.",
    "A few things I'd flag in that code:\n\n1. **Missing null check** — `element.querySelector()` can return `null`; accessing properties on it directly will throw.\n2. **`any` cast** — you can tighten this with a proper generic or discriminated union.\n3. **Side effect in render** — that mutation should be in a `useEffect` or `useCallback`.\n\nWant me to rewrite the relevant section?",
    "The issue is likely a closure over a stale value. In React, each render captures its own `state` and `props` — if your callback was defined in an earlier render, it sees the old values.\n\nFix: either add the dependency to your `useCallback`/`useEffect` array, or use a ref to always read the latest value.",
  ],
  Research: [
    "Based on what's publicly known:\n\n**Background:** This is an active area with several competing approaches. The main tradeoffs are between accuracy, latency, and implementation complexity.\n\n**Current consensus:** Most practitioners lean toward the simpler approach when the accuracy delta is small — the engineering overhead of the complex solution rarely pays off at moderate scale.\n\n**Key papers to look at:** I'd start with the 2023 surveys on this topic before going deep on any specific method.\n\nWant me to go deeper on any of these angles?",
    "Here's a quick synthesis of what I know on this:\n\nThe topic breaks into two distinct questions that often get conflated. The first is about the *mechanism* — how does it actually work? The second is about *applicability* — when should you use it?\n\nFor the mechanism: ...\n\nFor applicability: the main signals that this approach is right for your situation are (1) you have sufficient labeled data, (2) the distribution is stable, and (3) latency constraints allow for it.\n\nShould I dig into either side further?",
    "Good question to research. Here's what I'd look at:\n\n- **Primary sources** — original papers or official docs, not summaries\n- **Criticism** — what are the known failure modes and who's documented them?\n- **Practical reports** — engineering blogs from teams who've shipped this in production\n\nI can help you outline a research plan or start synthesizing anything you find.",
  ],
};

// ── Message store ──────────────────────────────────────────────────────────

let msgCounter = 100;
function newId() {
  return ++msgCounter;
}
function now() {
  return new Date().toISOString();
}

type StoredMessage = {
  id: number;
  workspace: string;
  bot: string;
  role: string;
  content: string;
  created_at: string;
};

const messageStore: Record<string, StoredMessage[]> = {};
const unreadStore: Record<string, Record<string, number>> = {};

function initStore(workspace: string) {
  if (messageStore[workspace]) return;
  messageStore[workspace] = [];
  for (const bot of BOTS) {
    const seeds = SEED_MESSAGES[bot.name] ?? [];
    for (const m of seeds) {
      messageStore[workspace].push({
        id: newId(),
        workspace,
        bot: bot.name,
        role: m.role,
        content: m.content,
        created_at: now(),
      });
    }
  }
  // seed Research with 2 unread to demo the badge on first load
  unreadStore[workspace] = { Research: 2 };
}

function getMessages(workspace: string, bot: string, limit = 30): StoredMessage[] {
  initStore(workspace);
  return messageStore[workspace].filter((m) => m.bot === bot).slice(-limit);
}

function addMessage(workspace: string, bot: string, role: string, content: string): StoredMessage {
  initStore(workspace);
  const msg: StoredMessage = { id: newId(), workspace, bot, role, content, created_at: now() };
  messageStore[workspace].push(msg);
  return msg;
}

// ── Fake WebSocket ─────────────────────────────────────────────────────────

let activeFakeWs: FakeWebSocket | null = null;

class FakeWebSocket {
  onopen: ((e: Event) => void) | null = null;
  onclose: ((e: Event) => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;
  readyState = 1;

  constructor(_url: string) {
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    activeFakeWs = this;
    setTimeout(() => this.onopen?.(new Event("open")), 30);
  }

  send() {}
  close() {
    activeFakeWs = null;
    this.readyState = 3;
  }

  emit(data: object) {
    this.onmessage?.(new MessageEvent("message", { data: JSON.stringify(data) }));
  }
}

function delay(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

function pickResponse(bot: string): string {
  const pool = CANNED_RESPONSES[bot] ?? CANNED_RESPONSES["Main"];
  return pool[Math.floor(Math.random() * pool.length)];
}

async function simulateResponse(workspace: string, bot: string) {
  const ws = activeFakeWs;
  if (!ws) return;

  const emit = (data: object) => ws.emit(data);

  // thinking
  emit({
    type: "bot_status",
    workspace,
    bot,
    status: "thinking",
    tool_name: null,
    streaming_content: "",
  });
  await delay(600 + Math.random() * 600);

  // stream the response in chunks
  const fullResponse = pickResponse(bot);
  const words = fullResponse.split(" ");
  let accumulated = "";

  emit({
    type: "bot_status",
    workspace,
    bot,
    status: "streaming",
    tool_name: null,
    streaming_content: "",
  });

  for (let i = 0; i < words.length; i++) {
    accumulated += (i === 0 ? "" : " ") + words[i];
    emit({
      type: "bot_status",
      workspace,
      bot,
      status: "streaming",
      tool_name: null,
      streaming_content: accumulated,
    });
    await delay(25 + Math.random() * 30);
  }

  // store + emit final message
  const msg = addMessage(workspace, bot, "assistant", fullResponse);
  emit({
    type: "bot_status",
    workspace,
    bot,
    status: "idle",
    tool_name: null,
    streaming_content: "",
  });
  emit({ type: "message", ...msg });
}

// ── Fetch interceptor ──────────────────────────────────────────────────────

const realFetch = window.fetch.bind(window);

function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function mockFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  const method = (init?.method ?? "GET").toUpperCase();

  // GET /api/workspaces
  if (method === "GET" && url.match(/\/api\/workspaces$/)) {
    return Promise.resolve(jsonResponse([{ name: "demo", description: "Demo workspace" }]));
  }

  // GET /api/workspaces/:ws/bots
  const botsMatch = url.match(/\/api\/workspaces\/([^/]+)\/bots$/);
  if (method === "GET" && botsMatch) {
    return Promise.resolve(jsonResponse(BOTS));
  }

  // GET /api/workspaces/:ws/conversations/:bot
  const convMatch = url.match(/\/api\/workspaces\/([^/]+)\/conversations\/([^/?]+)/);
  if (method === "GET" && convMatch) {
    const workspace = decodeURIComponent(convMatch[1]);
    const bot = decodeURIComponent(convMatch[2]);
    const limit = Number(new URL(url, "http://x").searchParams.get("limit") ?? "30");
    return Promise.resolve(jsonResponse(getMessages(workspace, bot, limit)));
  }

  // GET /api/workspaces/:ws/unread
  const unreadMatch = url.match(/\/api\/workspaces\/([^/]+)\/unread$/);
  if (method === "GET" && unreadMatch) {
    const workspace = decodeURIComponent(unreadMatch[1]);
    initStore(workspace);
    return Promise.resolve(jsonResponse(unreadStore[workspace] ?? {}));
  }

  // POST /api/workspaces/:ws/seen/:bot
  const seenMatch = url.match(/\/api\/workspaces\/([^/]+)\/seen\/([^/?]+)/);
  if (method === "POST" && seenMatch) {
    const workspace = decodeURIComponent(seenMatch[1]);
    const bot = decodeURIComponent(seenMatch[2]);
    if (unreadStore[workspace]) unreadStore[workspace][bot] = 0;
    return Promise.resolve(jsonResponse({ ok: true }));
  }

  // POST /api/workspaces/:ws/chat/:bot
  const chatMatch = url.match(/\/api\/workspaces\/([^/]+)\/chat\/([^/?]+)/);
  if (method === "POST" && chatMatch) {
    const workspace = decodeURIComponent(chatMatch[1]);
    const bot = decodeURIComponent(chatMatch[2]);
    const body = JSON.parse((init?.body as string) ?? "{}");
    addMessage(workspace, bot, "user", body.message ?? "");
    // fire off async simulated response — don't await
    simulateResponse(workspace, bot);
    return Promise.resolve(jsonResponse({ ok: true }));
  }

  // POST /api/workspaces/:ws/bots/:bot/cancel
  if (method === "POST" && url.match(/\/api\/workspaces\/[^/]+\/bots\/[^/]+\/cancel/)) {
    return Promise.resolve(jsonResponse({ ok: true }));
  }

  // anything else — pass through (or return 404 cleanly)
  if (url.startsWith("/api/")) {
    return Promise.resolve(jsonResponse({ ok: false, error: "mock: not implemented" }, 404));
  }

  return realFetch(input, init);
}

// ── Public trigger API (for demo controls) ─────────────────────────────────

/** Push an instant incoming assistant message to a bot (no streaming). */
export function triggerIncomingMessage(workspace: string, bot: string, content?: string) {
  const ws = activeFakeWs;
  if (!ws) return;
  const text = content ?? pickResponse(bot);
  const msg = addMessage(workspace, bot, "assistant", text);
  ws.emit({ type: "message", ...msg });
}

/** Run a full thinking → streaming → idle sequence for a bot. */
export function triggerStreamingResponse(workspace: string, bot: string) {
  simulateResponse(workspace, bot);
}

/** Emit a tool-use thinking state for a bot (stays until reset or message arrives). */
export function triggerToolUse(workspace: string, bot: string, toolName = "read_file") {
  const ws = activeFakeWs;
  if (!ws) return;
  ws.emit({
    type: "bot_status",
    workspace,
    bot,
    status: "thinking",
    tool_name: toolName,
    streaming_content: "",
  });
}

/** Reset a bot back to idle. */
export function triggerIdle(workspace: string, bot: string) {
  const ws = activeFakeWs;
  if (!ws) return;
  ws.emit({
    type: "bot_status",
    workspace,
    bot,
    status: "idle",
    tool_name: null,
    streaming_content: "",
  });
}

/** Clear all messages and re-seed initial data. */
export function resetMockStore() {
  for (const key of Object.keys(messageStore)) delete messageStore[key];
  for (const key of Object.keys(unreadStore)) delete unreadStore[key];
  msgCounter = 100;
}

export const MOCK_BOTS = BOTS;

// ── Install ────────────────────────────────────────────────────────────────

export function installMockServer() {
  window.fetch = mockFetch as typeof window.fetch;
  // @ts-expect-error replace global WebSocket with fake
  window.WebSocket = FakeWebSocket;
}

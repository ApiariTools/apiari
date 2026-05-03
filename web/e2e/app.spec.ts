import { expect, test, type Page, type Route } from "@playwright/test";

type ApiMessage = {
  id: number;
  workspace: string;
  bot: string;
  role: string;
  content: string;
  attachments: null;
  created_at: string;
};

type AppFixture = {
  workspaces: Array<{ name: string; remote?: string }>;
  botsByWorkspace: Record<string, Array<{ name: string; provider: string; model: string; watch: string[] }>>;
  reposByWorkspace: Record<string, Array<{
    name: string;
    path: string;
    has_swarm: boolean;
    is_clean: boolean;
    branch: string;
    workers: Array<{
      id: string;
      branch: string;
      status: string;
      agent: string;
      pr_url: string | null;
      pr_title: string | null;
      description: string | null;
      elapsed_secs: number | null;
      dispatched_by: string | null;
    }>;
  }>>;
  workersByWorkspace: Record<string, Array<{
    id: string;
    branch: string;
    status: string;
    agent: string;
    pr_url: string | null;
    pr_title: string | null;
    description: string | null;
    elapsed_secs: number | null;
    dispatched_by: string | null;
  }>>;
  workerDetails: Record<string, {
    id: string;
    branch: string;
    status: string;
    agent: string;
    pr_url: string | null;
    pr_title: string | null;
    description: string | null;
    elapsed_secs: number | null;
    dispatched_by: string | null;
    prompt: string | null;
    output: string | null;
    conversation: Array<{ role: string; content: string; timestamp?: string }>;
  }>;
  conversationsByKey: Record<string, ApiMessage[]>;
  unreadByWorkspace: Record<string, Record<string, number>>;
  researchByWorkspace: Record<string, Array<{
    id: string;
    workspace: string;
    topic: string;
    status: string;
    error: string | null;
    started_at: string;
    completed_at: string | null;
    output_file: string | null;
  }>>;
  followupsByWorkspace: Record<string, Array<{
    id: string;
    workspace: string;
    bot: string;
    action: string;
    created_at: string;
    fires_at: string;
    status: "pending" | "fired" | "cancelled";
  }>>;
  docsByWorkspace: Record<string, Array<{
    name: string;
    title: string;
    content: string;
    updated_at: string;
  }>>;
  usage?: { installed: boolean; providers: Array<unknown>; updated_at: string | null };
};

declare global {
  interface Window {
    __pushWsEvent?: (event: unknown) => void;
    __mockWs?: {
      onmessage?: ((event: MessageEvent) => void) | null;
      onclose?: ((event: CloseEvent) => void) | null;
    };
  }
}

function workspaceKey(workspace: string, remote?: string): string {
  return `${remote ?? "local"}::${workspace}`;
}

function defaultFixture(): AppFixture {
  return {
    workspaces: [{ name: "apiari" }, { name: "mgm" }],
    botsByWorkspace: {
      [workspaceKey("apiari")]: [
        { name: "Main", provider: "claude", model: "sonnet", watch: [] },
        { name: "Gemini", provider: "gemini", model: "gemini-2.5-flash", watch: [] },
      ],
      [workspaceKey("mgm")]: [
        { name: "Main", provider: "codex", model: "gpt-5.3-codex", watch: [] },
      ],
    },
    reposByWorkspace: {
      [workspaceKey("apiari")]: [
        {
          name: "apiari",
          path: "/dev/apiari",
          has_swarm: true,
          is_clean: false,
          branch: "main",
          workers: [],
        },
        {
          name: "common",
          path: "/dev/common",
          has_swarm: true,
          is_clean: true,
          branch: "main",
          workers: [
            {
              id: "common-sdk-fix",
              branch: "common/fix-sdk",
              status: "running",
              agent: "codex",
              pr_url: "https://example.com/pr/1",
              pr_title: "Fix SDK mapping",
              description: "Repair shared repo detection",
              elapsed_secs: 125,
              dispatched_by: "Main",
            },
          ],
        },
      ],
      [workspaceKey("mgm")]: [
        {
          name: "mgm",
          path: "/dev/mgm",
          has_swarm: false,
          is_clean: true,
          branch: "main",
          workers: [],
        },
      ],
    },
    workersByWorkspace: {
      [workspaceKey("apiari")]: [
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "running",
          agent: "codex",
          pr_url: "https://example.com/pr/1",
          pr_title: "Fix SDK mapping",
          description: "Repair shared repo detection",
          elapsed_secs: 125,
          dispatched_by: "Main",
        },
      ],
      [workspaceKey("mgm")]: [],
    },
    workerDetails: {
      "common-sdk-fix": {
        id: "common-sdk-fix",
        branch: "common/fix-sdk",
        status: "running",
        agent: "codex",
        pr_url: "https://example.com/pr/1",
        pr_title: "Fix SDK mapping",
        description: "Repair shared repo detection",
        elapsed_secs: 125,
        dispatched_by: "Main",
        prompt: "Investigate repo slug resolution",
        output: "Working through daemon/http.rs",
        conversation: [
          { role: "user", content: "Investigate repo slug resolution" },
          { role: "assistant", content: "Found fallback to workspace root." },
        ],
      },
    },
    conversationsByKey: {
      "apiari::Main": [
        {
          id: 1,
          workspace: "apiari",
          bot: "Main",
          role: "assistant",
          content: "Existing assistant reply",
          attachments: null,
          created_at: "2026-05-02T00:00:00.000Z",
        },
      ],
      "apiari::Gemini": [],
      "mgm::Main": [
        {
          id: 11,
          workspace: "mgm",
          bot: "Main",
          role: "assistant",
          content: "MGM workspace is ready.",
          attachments: null,
          created_at: "2026-05-02T00:10:00.000Z",
        },
      ],
    },
    unreadByWorkspace: {
      [workspaceKey("apiari")]: { Main: 2, Gemini: 0 },
      [workspaceKey("mgm")]: { Main: 1 },
    },
    researchByWorkspace: {
      [workspaceKey("apiari")]: [],
      [workspaceKey("mgm")]: [],
    },
    followupsByWorkspace: {
      [workspaceKey("apiari")]: [],
      [workspaceKey("mgm")]: [],
    },
    docsByWorkspace: {
      [workspaceKey("apiari")]: [
        {
          name: "architecture.md",
          title: "Architecture",
          content: "# Architecture\n\nCurrent system layout.",
          updated_at: "2026-05-02T00:00:00.000Z",
        },
        {
          name: "setup-guide.md",
          title: "Setup Guide",
          content: "# Setup Guide\n\nInstall dependencies.",
          updated_at: "2026-05-02T00:00:00.000Z",
        },
      ],
      [workspaceKey("mgm")]: [],
    },
    usage: { installed: true, providers: [], updated_at: "2026-05-02T00:00:00.000Z" },
  };
}

async function installMockWebSocket(page: Page) {
  await page.addInitScript(() => {
    class MockWebSocket {
      url: string;
      readyState = 1;
      onopen: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;
      onclose: ((event: CloseEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;

      constructor(url: string) {
        this.url = url;
        window.__mockWs = this;
        window.setTimeout(() => this.onopen?.(new Event("open")), 0);
      }

      send() {}

      close() {
        this.readyState = 3;
        this.onclose?.(new CloseEvent("close"));
      }
    }

    window.__pushWsEvent = (event: unknown) => {
      window.__mockWs?.onmessage?.(
        new MessageEvent("message", {
          data: JSON.stringify(event),
        }),
      );
    };

    Object.defineProperty(window, "WebSocket", {
      configurable: true,
      writable: true,
      value: MockWebSocket,
    });
  });
}

async function fulfillJson(route: Route, body: unknown) {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function wireMockApi(page: Page, fixture: AppFixture) {
  let nextMessageId = 100;

  await page.route("**/api/**", async (route) => {
    const req = route.request();
    const url = new URL(req.url());
    const method = req.method();
    const path = url.pathname;

    if (method === "GET" && path === "/api/workspaces") {
      return fulfillJson(route, fixture.workspaces);
    }
    if (method === "GET" && path === "/api/usage") {
      return fulfillJson(route, fixture.usage ?? { installed: false, providers: [], updated_at: null });
    }

    const match = path.match(
      /^\/api(?:\/remotes\/([^/]+))?\/workspaces\/([^/]+)(?:\/(.*))?$/,
    );
    if (!match) {
      throw new Error(`Unhandled API request in Playwright test: ${method} ${path}`);
    }

    const remote = match[1] || undefined;
    const workspace = match[2];
    const suffix = match[3] || "";
    const wsKey = workspaceKey(workspace, remote);

    if (method === "GET" && suffix === "bots") {
      return fulfillJson(route, fixture.botsByWorkspace[wsKey] ?? []);
    }
    if (method === "GET" && suffix === "workers") {
      return fulfillJson(route, fixture.workersByWorkspace[wsKey] ?? []);
    }
    if (method === "GET" && suffix === "repos") {
      return fulfillJson(route, fixture.reposByWorkspace[wsKey] ?? []);
    }
    if (method === "GET" && suffix === "unread") {
      return fulfillJson(route, fixture.unreadByWorkspace[wsKey] ?? {});
    }
    if (method === "GET" && suffix === "research") {
      return fulfillJson(route, fixture.researchByWorkspace[wsKey] ?? []);
    }
    if (method === "GET" && suffix === "followups") {
      return fulfillJson(route, fixture.followupsByWorkspace[wsKey] ?? []);
    }
    if (method === "GET" && suffix === "docs") {
      return fulfillJson(
        route,
        (fixture.docsByWorkspace[wsKey] ?? []).map((doc) => ({
          name: doc.name,
          title: doc.title,
          updated_at: doc.updated_at,
        })),
      );
    }

    if (method === "GET" && suffix.startsWith("conversations/")) {
      const bot = decodeURIComponent(suffix.slice("conversations/".length));
      return fulfillJson(route, fixture.conversationsByKey[`${workspace}::${bot}`] ?? []);
    }
    if (method === "GET" && suffix.startsWith("bots/") && suffix.endsWith("/status")) {
      return fulfillJson(route, { status: "idle", streaming_content: "", tool_name: null });
    }
    if (method === "POST" && suffix.startsWith("seen/")) {
      return fulfillJson(route, { ok: true });
    }
    if (method === "GET" && suffix.startsWith("docs/")) {
      const filename = decodeURIComponent(suffix.slice("docs/".length));
      const doc = (fixture.docsByWorkspace[wsKey] ?? []).find((entry) => entry.name === filename);
      return fulfillJson(route, doc ?? null);
    }
    if (method === "PUT" && suffix.startsWith("docs/")) {
      const filename = decodeURIComponent(suffix.slice("docs/".length));
      const body = req.postDataJSON() as { content: string };
      const docs = fixture.docsByWorkspace[wsKey] ?? [];
      const existing = docs.find((entry) => entry.name === filename);
      if (existing) {
        existing.content = body.content;
      } else {
        docs.push({
          name: filename,
          title: filename.replace(/\.md$/, "").replace(/[-_]/g, " ").replace(/\b\w/g, (m) => m.toUpperCase()),
          content: body.content,
          updated_at: "2026-05-02T00:00:00.000Z",
        });
        fixture.docsByWorkspace[wsKey] = docs;
      }
      return fulfillJson(route, { ok: true });
    }
    if (method === "DELETE" && suffix.startsWith("docs/")) {
      const filename = decodeURIComponent(suffix.slice("docs/".length));
      fixture.docsByWorkspace[wsKey] = (fixture.docsByWorkspace[wsKey] ?? []).filter((entry) => entry.name !== filename);
      return fulfillJson(route, { ok: true });
    }
    if (method === "GET" && suffix.startsWith("workers/") && !suffix.endsWith("/diff")) {
      const workerId = decodeURIComponent(suffix.slice("workers/".length));
      return fulfillJson(route, fixture.workerDetails[workerId] ?? null);
    }
    if (method === "GET" && suffix.endsWith("/diff")) {
      return fulfillJson(route, {
        diff: "diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@\n-fn old() {}\n+fn new() {}\n",
      });
    }
    if (method === "POST" && suffix.startsWith("workers/") && suffix.endsWith("/send")) {
      return fulfillJson(route, { ok: true });
    }

    if (method === "POST" && suffix.startsWith("chat/")) {
      const bot = decodeURIComponent(suffix.slice("chat/".length));
      const body = req.postDataJSON() as { message: string };
      const conversationKey = `${workspace}::${bot}`;
      const conversation = fixture.conversationsByKey[conversationKey] ?? [];
      const userMessage: ApiMessage = {
        id: nextMessageId++,
        workspace,
        bot,
        role: "user",
        content: body.message,
        attachments: null,
        created_at: "2026-05-02T00:00:01.000Z",
      };
      const assistantMessage: ApiMessage = {
        id: nextMessageId++,
        workspace,
        bot,
        role: "assistant",
        content: "Mock assistant reply",
        attachments: null,
        created_at: "2026-05-02T00:00:02.000Z",
      };
      fixture.conversationsByKey[conversationKey] = [...conversation, userMessage, assistantMessage];

      setTimeout(() => {
        void page.evaluate((event) => window.__pushWsEvent?.(event), {
          type: "message",
          ...userMessage,
        });
      }, 10);
      setTimeout(() => {
        void page.evaluate((event) => window.__pushWsEvent?.(event), {
          type: "bot_status",
          workspace,
          bot,
          status: "streaming",
          streaming_content: assistantMessage.content,
          tool_name: null,
        });
      }, 30);
      setTimeout(() => {
        void page.evaluate((event) => window.__pushWsEvent?.(event), {
          type: "message",
          ...assistantMessage,
        });
      }, 50);
      setTimeout(() => {
        void page.evaluate((event) => window.__pushWsEvent?.(event), {
          type: "bot_status",
          workspace,
          bot,
          status: "idle",
          streaming_content: "",
          tool_name: null,
        });
      }, 70);
      return fulfillJson(route, { ok: true });
    }

    if (method === "POST" && suffix === "research") {
      const body = req.postDataJSON() as { topic: string };
      return fulfillJson(route, { id: "research-1", topic: body.topic, status: "running" });
    }

    throw new Error(`Unhandled API request in Playwright test: ${method} ${path}`);
  });
}

async function bootApp(page: Page, fixture: AppFixture) {
  await installMockWebSocket(page);
  await wireMockApi(page, fixture);
  await page.goto("/");
  await page.getByLabel("Open bot Main").click();
}

test.describe("apiari web", () => {
  test("keeps optimistic user sends and websocket assistant replies visible", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await expect(page.getByText("Existing assistant reply")).toBeVisible();

    await page.getByPlaceholder("Message Main...").fill("playwright smoke");
    await page.getByRole("button", { name: "Send message" }).click();

    await expect(page.getByText("playwright smoke")).toBeVisible();
    await expect(page.getByText("Mock assistant reply")).toHaveCount(1);
    await expect(page.getByText("playwright smoke")).toBeVisible();
  });

  test("switches workspaces and loads the correct bot conversation", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await page.getByRole("button", { name: "mgm" }).click();
    await page.getByLabel("Open bot Main").click();

    await expect(page.getByText("MGM workspace is ready.")).toBeVisible();
    await expect(page.getByPlaceholder("Message Main...")).toBeVisible();
  });

  test("renders repo and worker state and opens worker detail from the sidebar", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await expect(page.getByText("common", { exact: true })).toBeVisible();
    await expect(page.getByText("common-sdk-fix")).toBeVisible();
    await expect(page.getByText("modified")).toBeVisible();

    await page.getByText("common-sdk-fix").click();

    await expect(page.getByText("Working through daemon/http.rs")).toBeVisible();
    await page.getByRole("button", { name: "Task" }).last().click();
    await expect(page.getByText("Investigate repo slug resolution")).toBeVisible();
    await page.getByRole("button", { name: "Chat" }).last().click();
    await expect(page.getByPlaceholder("Message worker...")).toBeVisible();
  });

  test("opens docs and loads document content", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await page.getByRole("button", { name: "Docs" }).click();
    await expect(page.getByText("Architecture")).toBeVisible();
    await page.getByText("Architecture").click();

    await expect(page.getByText("Current system layout.")).toBeVisible();
    await expect(page.getByRole("button", { name: "Switch to preview" })).toBeVisible();
  });

  test("uses the mobile mode bar for single-column navigation", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await installMockWebSocket(page);
    await wireMockApi(page, defaultFixture());
    await page.goto("/");

    await expect(page.getByRole("navigation", { name: "Mobile workspace modes" })).toBeVisible();
    await expect(page.getByPlaceholder("Message Main...")).toBeVisible();

    await page.getByRole("button", { name: "Open Repos" }).click();
    await expect(page.getByText("common", { exact: true })).toBeVisible();

    await page.getByRole("button", { name: "Open Workers" }).click();
    await expect(page.getByText("common-sdk-fix")).toBeVisible();
  });
});

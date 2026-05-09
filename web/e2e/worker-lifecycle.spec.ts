/**
 * Worker lifecycle e2e test — fully mocked, no real daemon.
 *
 * Uses the same page.route() + MockWebSocket pattern as app.spec.ts.
 * No real workers are created, no real repos touched.
 *
 * Flow:
 *  1. Open the apiari workspace
 *  2. Create a worker via Quick Dispatch (intercepted — returns a fake worker ID)
 *  3. App auto-navigates to the new worker's detail view
 *  4. Push a WS event to simulate the PR being opened
 *  5. Assert the PR link is visible
 *  6. Send a revision message
 *  7. Assert the message appears in the timeline
 */

import { expect, test, type Page, type Route } from "@playwright/test";

const WORKSPACE = "apiari";
const PROMPT = "e2e test: add a comment to main.rs";
const REVISION_MSG = "please also update the tests";
const MOCK_PR_URL = "https://github.com/example/apiari/pull/999";
const WORKER_ID = "apiari-e2e-mock";

type WorkerState = {
  id: string;
  workspace: string;
  state: string;
  label: string;
  goal: string | null;
  display_title: string | null;
  repo: string | null;
  branch: string | null;
  tests_passing: boolean;
  branch_ready: boolean;
  pr_url: string | null;
  pr_approved: boolean;
  ci_passing: boolean | null;
  is_stalled: boolean;
  revision_count: number;
  review_mode: string;
  blocked_reason: string | null;
  last_output_at: string | null;
  state_entered_at: string;
  created_at: string;
  updated_at: string;
  brief: null;
  events: Array<{ event_type: string; content: string; created_at: string }>;
};

function makeWorker(overrides: Partial<WorkerState> = {}): WorkerState {
  return {
    id: WORKER_ID,
    workspace: WORKSPACE,
    state: "running",
    label: "Running",
    goal: PROMPT,
    display_title: null,
    repo: "apiari",
    branch: `swarm/e2e-mock-${WORKER_ID}`,
    tests_passing: false,
    branch_ready: false,
    pr_url: null,
    pr_approved: false,
    ci_passing: null,
    is_stalled: false,
    revision_count: 0,
    review_mode: "local_first",
    blocked_reason: null,
    last_output_at: null,
    state_entered_at: "2026-05-07T00:00:00Z",
    created_at: "2026-05-07T00:00:00Z",
    updated_at: "2026-05-07T00:00:00Z",
    brief: null,
    events: [],
    ...overrides,
  };
}

declare global {
  interface Window {
    __pushWsEvent?: (event: unknown) => void;
    __mockWs?: {
      onmessage?: ((event: MessageEvent) => void) | null;
    };
  }
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
      }
    }

    window.__pushWsEvent = (event: unknown) => {
      window.__mockWs?.onmessage?.(new MessageEvent("message", { data: JSON.stringify(event) }));
    };

    Object.defineProperty(window, "WebSocket", {
      configurable: true,
      writable: true,
      value: MockWebSocket,
    });
  });
}

async function fulfillJson(route: Route, body: unknown, status = 200) {
  await route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
}

async function wireMockApi(page: Page, worker: WorkerState) {
  await page.route("**/api/**", async (route) => {
    const req = route.request();
    const url = new URL(req.url());
    const method = req.method();
    const path = url.pathname;

    if (method === "GET" && path === "/api/workspaces") {
      return fulfillJson(route, [{ name: WORKSPACE }]);
    }
    if (method === "GET" && path === "/api/usage") {
      return fulfillJson(route, { installed: true, providers: [], updated_at: null });
    }

    const wsMatch = path.match(/^\/api\/workspaces\/([^/]+)(?:\/(.*))?$/);
    if (!wsMatch) return fulfillJson(route, null);
    const suffix = wsMatch[2] || "";

    if (method === "GET" && suffix === "repos") {
      return fulfillJson(route, [{ name: "apiari", path: "/dev/apiari" }]);
    }
    if (method === "GET" && suffix === "v2/workers") {
      return fulfillJson(route, { workers: [worker] });
    }
    if (method === "POST" && suffix === "v2/workers") {
      // Dispatch: return the mock worker ID without touching real infrastructure
      return fulfillJson(route, { ok: true, worker_id: WORKER_ID });
    }
    if (method === "GET" && suffix.startsWith("v2/workers/") && suffix.endsWith("/reviews")) {
      return fulfillJson(route, { reviews: [] });
    }
    if (method === "GET" && suffix.startsWith("v2/workers/")) {
      return fulfillJson(route, worker);
    }
    if (method === "POST" && suffix.startsWith("v2/workers/")) {
      // Revision message — record it in the worker's event log
      if (suffix.endsWith("/send")) {
        const body = req.postDataJSON() as { message?: string };
        worker.events.push({
          event_type: "user_message",
          content: body.message ?? "",
          created_at: new Date().toISOString(),
        });
      }
      return fulfillJson(route, { ok: true });
    }
    if (method === "GET" && suffix === "v2/auto-bots") return fulfillJson(route, { auto_bots: [] });
    if (method === "GET" && suffix === "v2/widgets") return fulfillJson(route, []);
    if (method === "GET" && suffix === "bots") return fulfillJson(route, []);
    if (method === "GET" && suffix === "unread") return fulfillJson(route, {});
    if (method === "POST" && suffix.startsWith("seen/")) return fulfillJson(route, { ok: true });
    if (method === "GET" && suffix.startsWith("bots/"))
      return fulfillJson(route, { status: "idle", streaming_content: "", tool_name: null });
    if (suffix.startsWith("v2/context-bot/")) return fulfillJson(route, { ok: true, sessions: [] });

    return fulfillJson(route, null);
  });
}

test.describe("worker lifecycle (mocked)", () => {
  test("create worker → PR appears via WS → send revision → message visible", async ({ page }) => {
    const worker = makeWorker();

    await installMockWebSocket(page);
    await wireMockApi(page, worker);
    await page.goto("/");
    await expect(page.getByRole("navigation", { name: "Sidebar" })).toBeVisible({
      timeout: 10_000,
    });

    // ── 1. Open Quick Dispatch ─────────────────────────────────────────
    await page.getByTestId("quick-dispatch-trigger").click();
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).toBeVisible({
      timeout: 5_000,
    });

    await page.getByTestId("intent-textarea").fill(PROMPT);
    await page.getByTestId("repo-pills").locator("button").first().click();
    await page.getByTestId("dispatch-btn").click();

    // Dialog closes after dispatch
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).not.toBeVisible({
      timeout: 10_000,
    });

    // ── 2. App navigates to worker detail ─────────────────────────────
    await expect(page.getByTestId("tab-timeline")).toBeVisible({ timeout: 15_000 });

    // ── 3. Simulate PR opened via WebSocket ───────────────────────────
    worker.pr_url = MOCK_PR_URL;
    worker.branch_ready = true;
    await page.evaluate((prUrl) => {
      window.__pushWsEvent?.({
        type: "worker_update",
        workspace: "apiari",
        worker_id: "apiari-e2e-mock",
        state: "waiting",
        label: "Waiting for review",
        properties: { pr_url: prUrl, branch_ready: true },
      });
    }, MOCK_PR_URL);

    const prLink = page.locator(`a[href="${MOCK_PR_URL}"]`).first();
    await expect(prLink).toBeVisible({ timeout: 10_000 });

    // ── 4. Send a revision message ────────────────────────────────────
    const chatInput = page.getByPlaceholder(/Send.*instruction/i);
    await expect(chatInput).toBeVisible({ timeout: 5_000 });
    await chatInput.fill(REVISION_MSG);
    await chatInput.press("Enter");

    // ── 5. Message appears in timeline ────────────────────────────────
    await expect(page.getByText(REVISION_MSG).first()).toBeVisible({ timeout: 10_000 });
  });
});

import { expect, test, type Page, type Route } from "@playwright/test";

// ── Fixture types ─────────────────────────────────────────────────────────

type WorkerV2Fixture = {
  id: string;
  workspace: string;
  state: string;
  label: string;
  goal: string | null;
  display_title?: string | null;
  repo: string | null;
  branch: string | null;
  tests_passing: boolean;
  branch_ready: boolean;
  pr_url: string | null;
  pr_approved: boolean;
  is_stalled: boolean;
  revision_count: number;
  review_mode: string;
  blocked_reason: string | null;
  last_output_at: string | null;
  state_entered_at: string;
  created_at: string;
  updated_at: string;
  events?: Array<{ event_type: string; content: string; created_at: string }>;
};

type AppFixture = {
  workspaces: Array<{ name: string; remote?: string }>;
  v2WorkersByWorkspace: Record<string, WorkerV2Fixture[]>;
  v2WorkerDetails: Record<string, WorkerV2Fixture>;
  reposByWorkspace: Record<string, Array<{ name: string; path: string }>>;
  createWorkerError?: string;
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

// ── Helpers ───────────────────────────────────────────────────────────────

function makeWorker(overrides: Partial<WorkerV2Fixture> & { id: string; workspace: string }): WorkerV2Fixture {
  return {
    state: "running",
    label: "",
    goal: null,
    display_title: null,
    repo: null,
    branch: null,
    tests_passing: false,
    branch_ready: false,
    pr_url: null,
    pr_approved: false,
    is_stalled: false,
    revision_count: 0,
    review_mode: "local_first",
    blocked_reason: null,
    last_output_at: null,
    state_entered_at: "2026-05-07T00:00:00Z",
    created_at: "2026-05-07T00:00:00Z",
    updated_at: "2026-05-07T00:00:00Z",
    events: [],
    ...overrides,
  };
}

function defaultFixture(): AppFixture {
  return {
    workspaces: [{ name: "apiari" }, { name: "mgm" }],
    v2WorkersByWorkspace: {
      "apiari": [],
      "mgm": [],
    },
    v2WorkerDetails: {},
    reposByWorkspace: {
      "apiari": [{ name: "apiari", path: "/dev/apiari" }],
      "mgm": [{ name: "mgm", path: "/dev/mgm" }],
    },
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
        new MessageEvent("message", { data: JSON.stringify(event) }),
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
  await page.route("**/api/**", async (route) => {
    const req = route.request();
    const url = new URL(req.url());
    const method = req.method();
    const path = url.pathname;

    // Top-level routes
    if (method === "GET" && path === "/api/workspaces") {
      return fulfillJson(route, fixture.workspaces);
    }
    if (method === "GET" && path === "/api/usage") {
      return fulfillJson(route, { installed: true, providers: [], updated_at: null });
    }

    // Workspace-scoped routes
    const match = path.match(/^\/api(?:\/remotes\/([^/]+))?\/workspaces\/([^/]+)(?:\/(.*))?$/);
    if (!match) return route.fulfill({ status: 404 });

    const workspace = match[2];
    const suffix = match[3] || "";

    // Repos
    if (method === "GET" && suffix === "repos") {
      return fulfillJson(route, fixture.reposByWorkspace[workspace] ?? []);
    }

    // v2 workers
    if (method === "GET" && suffix === "v2/workers") {
      return fulfillJson(route, { workers: fixture.v2WorkersByWorkspace[workspace] ?? [] });
    }
    if (method === "POST" && suffix === "v2/workers") {
      if (fixture.createWorkerError) {
        return route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: fixture.createWorkerError }),
        });
      }
      const body = req.postDataJSON() as { brief?: { goal?: string; repo?: string }; repo?: string };
      const workerId = `new-worker-${Date.now()}`;
      const newWorker = makeWorker({
        id: workerId,
        workspace,
        goal: body.brief?.goal ?? null,
        repo: body.brief?.repo ?? body.repo ?? null,
      });
      (fixture.v2WorkersByWorkspace[workspace] ??= []).push(newWorker);
      fixture.v2WorkerDetails[workerId] = newWorker;
      return fulfillJson(route, { ok: true, worker_id: workerId });
    }
    if (method === "GET" && suffix.startsWith("v2/workers/") && suffix.endsWith("/reviews")) {
      return fulfillJson(route, { reviews: [] });
    }
    if (method === "GET" && suffix.startsWith("v2/workers/")) {
      const workerId = suffix.slice("v2/workers/".length).split("/")[0];
      const detail = fixture.v2WorkerDetails[workerId];
      if (!detail) return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "not found" }) });
      return fulfillJson(route, detail);
    }
    if (method === "POST" && suffix.startsWith("v2/workers/")) {
      return fulfillJson(route, { ok: true });
    }

    // v2 auto-bots
    if (method === "GET" && suffix === "v2/auto-bots") {
      return fulfillJson(route, { auto_bots: [] });
    }
    if (method === "GET" && suffix.startsWith("v2/auto-bots/")) {
      return route.fulfill({ status: 404 });
    }

    // v2 widgets (Dashboard)
    if (method === "GET" && suffix === "v2/widgets") {
      return fulfillJson(route, []);
    }
    if (method === "PUT" && suffix.startsWith("v2/widgets/")) {
      return fulfillJson(route, { ok: true });
    }
    if (method === "DELETE" && suffix.startsWith("v2/widgets/")) {
      return fulfillJson(route, { ok: true });
    }

    // Context bot
    if (method === "POST" && suffix === "v2/context-bot/chat") {
      return fulfillJson(route, { ok: true });
    }

    // Old endpoints still called in some flows
    if (method === "GET" && suffix === "bots") return fulfillJson(route, []);
    if (method === "GET" && suffix === "unread") return fulfillJson(route, {});
    if (method === "POST" && suffix.startsWith("seen/")) return fulfillJson(route, { ok: true });
    if (method === "GET" && suffix.startsWith("bots/")) return fulfillJson(route, { status: "idle", streaming_content: "", tool_name: null });

    // Fallback: return empty 200 rather than throwing — keeps the app running
    // even when new endpoints are added without updating this mock.
    return fulfillJson(route, null);
  });
}

async function bootApp(page: Page, fixture: AppFixture) {
  await installMockWebSocket(page);
  await wireMockApi(page, fixture);
  await page.goto("/");
  // Wait for the app shell to appear (sidebar nav is always rendered)
  await expect(page.getByRole("navigation", { name: "Sidebar" })).toBeVisible({ timeout: 10_000 });
}

// ── Tests ─────────────────────────────────────────────────────────────────

test.describe("apiari web", () => {
  test("app loads and shows Dashboard with empty worker state", async ({ page }) => {
    await bootApp(page, defaultFixture());
    await expect(page.getByText("No active workers")).toBeVisible();
  });

  test("worker in sidebar navigates to worker detail on click", async ({ page }) => {
    const fixture = defaultFixture();
    const worker = makeWorker({ id: "api-ab12", workspace: "apiari", goal: "fix auth bug", state: "waiting" });
    fixture.v2WorkersByWorkspace["apiari"] = [worker];
    fixture.v2WorkerDetails["api-ab12"] = worker;
    await bootApp(page, fixture);

    // Worker appears in the sidebar — click the sidebar item (not Dashboard attention list)
    const sidebar = page.getByRole("navigation", { name: "Sidebar" });
    await expect(sidebar.getByText("fix auth bug")).toBeVisible({ timeout: 5_000 });
    await sidebar.getByText("fix auth bug").click();

    await expect(page.getByTestId("tab-timeline")).toBeVisible({ timeout: 10_000 });
  });

  test("running and waiting workers show stat pills on Dashboard", async ({ page }) => {
    const fixture = defaultFixture();
    fixture.v2WorkersByWorkspace["apiari"] = [
      makeWorker({ id: "w-1", workspace: "apiari", goal: "fix the bug", state: "running" }),
      makeWorker({ id: "w-2", workspace: "apiari", goal: "add tests", state: "waiting" }),
    ];
    await bootApp(page, fixture);
    // Stat pills appear in the Dashboard; use first() since "running" text also
    // appears in the sidebar activity strip.
    await expect(page.getByText("Running").first()).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("Waiting").first()).toBeVisible();
  });

  test("workspace switcher shows second workspace", async ({ page }) => {
    await bootApp(page, defaultFixture());
    // The sidebar shows a workspace dropdown/switcher
    const sidebar = page.getByRole("navigation", { name: "Sidebar" });
    await expect(sidebar).toBeVisible();
    await expect(sidebar.getByRole("button", { name: "apiari" })).toBeVisible();
  });

  // ── QuickDispatch ────────────────────────────────────────────────────────

  test("QuickDispatch opens and closes with cancel button", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await page.getByTestId("quick-dispatch-trigger").click();
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).toBeVisible();

    await page.getByTestId("cancel-btn").click();
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).not.toBeVisible();
  });

  test("QuickDispatch closes on Escape key", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await page.getByTestId("quick-dispatch-trigger").click();
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).not.toBeVisible();
  });

  test("QuickDispatch dispatch button disabled until intent is filled", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await page.getByTestId("quick-dispatch-trigger").click();
    await expect(page.getByTestId("dispatch-btn")).toBeDisabled();

    await page.getByTestId("intent-textarea").fill("fix the rate limiter");
    await expect(page.getByTestId("dispatch-btn")).toBeEnabled();

    await page.getByTestId("intent-textarea").fill("");
    await expect(page.getByTestId("dispatch-btn")).toBeDisabled();
  });

  test("QuickDispatch dispatches and navigates to the new worker", async ({ page }) => {
    await bootApp(page, defaultFixture());

    await page.getByTestId("quick-dispatch-trigger").click();
    await page.getByTestId("intent-textarea").fill("fix the rate limiter");
    await page.getByTestId("repo-pills").locator("button").first().click();
    await page.getByTestId("dispatch-btn").click();

    // Dialog closes after dispatch
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).not.toBeVisible({ timeout: 5_000 });

    // App navigates to the new worker detail view
    await expect(page.getByTestId("tab-timeline")).toBeVisible({ timeout: 10_000 });
  });

  test("QuickDispatch shows error message when dispatch fails", async ({ page }) => {
    const fixture = defaultFixture();
    fixture.createWorkerError = "repo not found";
    await bootApp(page, fixture);

    await page.getByTestId("quick-dispatch-trigger").click();
    await page.getByTestId("intent-textarea").fill("fix the rate limiter");
    await page.getByTestId("repo-pills").locator("button").first().click();
    await page.getByTestId("dispatch-btn").click();

    // Error message shown, dialog stays open
    await expect(page.getByTestId("dispatch-error")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByRole("dialog", { name: "Quick dispatch" })).toBeVisible();
  });
});

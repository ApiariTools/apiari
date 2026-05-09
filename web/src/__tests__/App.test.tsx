import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "../App";
import * as api from "@apiari/api";
import type {
  WorkerV2,
  WorkerDetailV2 as WorkerDetailV2Data,
  AutoBot,
  AutoBotDetail,
} from "@apiari/types";

vi.mock("@apiari/api");
vi.mock("react-markdown", () => ({
  default: ({ children }: { children: string }) => <span>{children}</span>,
}));

const mockWorkers: WorkerV2[] = [
  {
    id: "w-1",
    workspace: "default",
    state: "running",
    label: "Working",
    brief: null,
    repo: "apiari",
    branch: "swarm/fix-auth",
    goal: "fix-auth-rate-limit",
    display_title: "Fix auth rate limiting",
    tests_passing: true,
    branch_ready: false,
    pr_url: null,
    pr_approved: false,
    is_stalled: false,
    revision_count: 0,
    review_mode: "local_first",
    blocked_reason: null,
    last_output_at: null,
    state_entered_at: "2026-05-04T10:00:00Z",
    created_at: "2026-05-04T09:00:00Z",
    updated_at: "2026-05-04T10:00:00Z",
  },
  {
    id: "w-2",
    workspace: "default",
    state: "waiting",
    label: "Waiting",
    brief: null,
    repo: "apiari",
    branch: "swarm/update-deps",
    goal: "update-deps",
    display_title: "Update dependencies",
    tests_passing: false,
    branch_ready: false,
    pr_url: null,
    pr_approved: false,
    is_stalled: false,
    revision_count: 0,
    review_mode: "local_first",
    blocked_reason: null,
    last_output_at: null,
    state_entered_at: "2026-05-04T10:00:00Z",
    created_at: "2026-05-04T09:00:00Z",
    updated_at: "2026-05-04T10:00:00Z",
  },
  {
    id: "w-3",
    workspace: "default",
    state: "done",
    label: "Done",
    brief: null,
    repo: "apiari",
    branch: "swarm/ship-auth",
    goal: "ship-auth-fix",
    tests_passing: true,
    branch_ready: true,
    pr_url: "https://github.com/example/apiari/pull/44",
    pr_approved: true,
    is_stalled: false,
    revision_count: 0,
    review_mode: "local_first",
    blocked_reason: null,
    last_output_at: null,
    state_entered_at: "2026-05-04T11:00:00Z",
    created_at: "2026-05-04T09:30:00Z",
    updated_at: "2026-05-04T11:00:00Z",
  },
];

const mockWorkerDetail: WorkerDetailV2Data = {
  ...mockWorkers[0],
  events: [],
};

const mockAutoBots: AutoBot[] = [
  {
    id: "triage",
    workspace: "default",
    name: "Triage",
    color: "#f5c542",
    trigger_type: "signal",
    cron_schedule: null,
    signal_source: "github",
    signal_filter: null,
    prompt: "Triage new issues",
    provider: "claude",
    model: null,
    enabled: true,
    status: "idle",
    created_at: "2026-05-04T09:00:00Z",
    updated_at: "2026-05-04T09:00:00Z",
  },
  {
    id: "standup",
    workspace: "default",
    name: "Standup",
    color: "#f5c542",
    trigger_type: "cron",
    cron_schedule: "0 9 * * 1-5",
    signal_source: null,
    signal_filter: null,
    prompt: "Generate standup",
    provider: "claude",
    model: null,
    enabled: true,
    status: "running",
    created_at: "2026-05-04T09:00:00Z",
    updated_at: "2026-05-04T09:00:00Z",
  },
];

const mockAutoBotDetail: AutoBotDetail = {
  ...mockAutoBots[0],
  runs: [],
};

beforeEach(() => {
  vi.mocked(api.getWorkspaces).mockResolvedValue([{ name: "default" }]);
  vi.mocked(api.listWorkersV2).mockResolvedValue(mockWorkers);
  vi.mocked(api.getWorkerV2).mockResolvedValue(mockWorkerDetail);
  vi.mocked(api.listAutoBots).mockResolvedValue(mockAutoBots);
  vi.mocked(api.getAutoBot).mockResolvedValue(mockAutoBotDetail);
  // listWidgets may not be in the auto-mock if the export was added after initial mock setup
  Object.assign(api, { listWidgets: vi.fn().mockResolvedValue([]) });
});

describe("App shell", () => {
  it("renders the sidebar with Auto Bots and Workers sections", () => {
    render(<App />);
    expect(screen.getAllByText("Auto Bots").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Workers").length).toBeGreaterThan(0);
  });

  it("renders auto bots from API in the sidebar", async () => {
    render(<App />);
    // Names appear in both sidebar and dashboard, so use findAllByText
    expect((await screen.findAllByText("Triage")).length).toBeGreaterThan(0);
    expect((await screen.findAllByText("Standup")).length).toBeGreaterThan(0);
  });

  it("renders workers from API in the sidebar", async () => {
    render(<App />);
    // Names appear in both sidebar and dashboard, so use findAllByText
    expect((await screen.findAllByText("Fix auth rate limiting")).length).toBeGreaterThan(0);
    expect((await screen.findAllByText("Update dependencies")).length).toBeGreaterThan(0);
  });

  it("shows empty state when nothing is selected", async () => {
    // Override mocks to return empty lists so Dashboard shows no-workers state
    vi.mocked(api.listWorkersV2).mockResolvedValue([]);
    vi.mocked(api.listAutoBots).mockResolvedValue([]);
    render(<App />);
    // Wait for workspace to load, then Dashboard shows "No active workers"
    expect(await screen.findByText("No active workers")).toBeInTheDocument();
  });

  it("shows worker detail when a worker is selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    // Wait for sidebar to load then click the sidebar worker button
    const sidebarNav = await screen.findByRole("navigation", { name: "Sidebar" });
    await screen.findAllByText("Fix auth rate limiting");
    // Get all buttons with that name and click the first (sidebar) one
    const workerBtns = screen.getAllByRole("button", { name: /Fix auth rate limiting/ });
    await user.click(workerBtns[0]);
    // WorkerDetailV2 renders the display title as heading
    expect(
      await screen.findByText("Fix auth rate limiting", { selector: "h1" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("Select something")).not.toBeInTheDocument();
    // Keep sidebarNav reference to suppress unused warning
    expect(sidebarNav).toBeInTheDocument();
  });

  it("shows auto bot detail when an auto bot is selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    // Wait for auto bots to load then click the sidebar button
    await screen.findAllByText("Triage");
    // Click the Triage sidebar item button (find by role within navigation)
    const sidebarNav = screen.getByRole("navigation", { name: "Sidebar" });
    // Get the Triage button within the sidebar nav (not the workspace trigger)
    const triageBtns = Array.from(sidebarNav.querySelectorAll("button")).filter(
      (b) => b.textContent?.includes("Triage") && !b.getAttribute("aria-haspopup"),
    );
    await user.click(triageBtns[0]);
    // AutoBotDetail renders the bot name as heading
    expect(await screen.findByTestId("bot-name")).toHaveTextContent("Triage");
    expect(screen.queryByText("Select something")).not.toBeInTheDocument();
  });

  it("switches between workers", async () => {
    const user = userEvent.setup();
    vi.mocked(api.getWorkerV2).mockImplementation(async (_ws, id) => ({
      ...mockWorkers[id === "w-1" ? 0 : 1],
      events: [],
    }));
    render(<App />);
    // Wait for sidebar buttons
    await screen.findAllByText("Fix auth rate limiting");
    const workerBtns = screen.getAllByRole("button", { name: /Fix auth rate limiting/ });
    await user.click(workerBtns[0]);
    expect(
      await screen.findByText("Fix auth rate limiting", { selector: "h1" }),
    ).toBeInTheDocument();
    const updateDepsBtns = screen.getAllByRole("button", { name: /Update dependencies/ });
    await user.click(updateDepsBtns[0]);
    await waitFor(() => {
      expect(
        screen.queryByText("Fix auth rate limiting", { selector: "h1" }),
      ).not.toBeInTheDocument();
    });
    expect(await screen.findByText("Update dependencies", { selector: "h1" })).toBeInTheDocument();
  });

  it("renders the mobile bottom tab bar with Auto Bots and Workers tabs", () => {
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "Mobile navigation" });
    expect(nav).toBeInTheDocument();
    expect(nav).toHaveTextContent("Auto Bots");
    expect(nav).toHaveTextContent("Workers");
  });

  it("sidebar navigation has accessible label", () => {
    render(<App />);
    expect(screen.getByRole("navigation", { name: "Sidebar" })).toBeInTheDocument();
  });

  it("opens the completed workers view from the sidebar footer", async () => {
    const user = userEvent.setup();
    render(<App />);
    const footer = await screen.findByTestId("done-workers-footer");
    await user.click(footer);
    expect(await screen.findByText("Completed workers", { selector: "h1" })).toBeInTheDocument();
    expect(screen.getByText("ship-auth-fix")).toBeInTheDocument();
  });
});

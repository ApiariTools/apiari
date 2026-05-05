import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "../App";
import * as api from "../api";
import type { WorkerV2, WorkerDetailV2 as WorkerDetailV2Data, AutoBot, AutoBotDetail } from "../types";

vi.mock("../api");
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
  vi.mocked(api.listWorkersV2).mockResolvedValue(mockWorkers);
  vi.mocked(api.getWorkerV2).mockResolvedValue(mockWorkerDetail);
  vi.mocked(api.listAutoBots).mockResolvedValue(mockAutoBots);
  vi.mocked(api.getAutoBot).mockResolvedValue(mockAutoBotDetail);
});

describe("App shell", () => {
  it("renders the sidebar with Auto Bots and Workers sections", () => {
    render(<App />);
    expect(screen.getAllByText("Auto Bots").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Workers").length).toBeGreaterThan(0);
  });

  it("renders auto bots from API in the sidebar", async () => {
    render(<App />);
    expect(await screen.findByText("Triage")).toBeInTheDocument();
    expect(await screen.findByText("Standup")).toBeInTheDocument();
  });

  it("renders workers from API in the sidebar", async () => {
    render(<App />);
    expect(await screen.findByText("fix-auth-rate-limit")).toBeInTheDocument();
    expect(await screen.findByText("update-deps")).toBeInTheDocument();
  });

  it("shows empty state when nothing is selected", () => {
    render(<App />);
    expect(screen.getByText("Select something")).toBeInTheDocument();
    expect(screen.getByText("Choose a worker or auto bot from the sidebar")).toBeInTheDocument();
  });

  it("shows worker detail when a worker is selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("fix-auth-rate-limit");
    await user.click(screen.getByText("fix-auth-rate-limit"));
    // WorkerDetailV2 renders the goal as heading
    expect(await screen.findByText("fix-auth-rate-limit", { selector: "h1" })).toBeInTheDocument();
    expect(screen.queryByText("Select something")).not.toBeInTheDocument();
  });

  it("shows auto bot detail when an auto bot is selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("Triage");
    await user.click(screen.getByText("Triage"));
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
    await screen.findByText("fix-auth-rate-limit");
    await user.click(screen.getByText("fix-auth-rate-limit"));
    expect(await screen.findByText("fix-auth-rate-limit", { selector: "h1" })).toBeInTheDocument();
    await user.click(screen.getByText("update-deps"));
    await waitFor(() => {
      expect(screen.queryByText("fix-auth-rate-limit", { selector: "h1" })).not.toBeInTheDocument();
    });
    expect(await screen.findByText("update-deps", { selector: "h1" })).toBeInTheDocument();
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
});

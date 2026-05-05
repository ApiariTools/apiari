import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import Dashboard from "../components/Dashboard/Dashboard";
import type { WorkerV2, AutoBot } from "../types";

// ── Fixtures ──────────────────────────────────────────────────────────────

function makeWorker(overrides: Partial<WorkerV2> = {}): WorkerV2 {
  return {
    id: "w-1",
    workspace: "default",
    state: "running",
    label: "Working",
    brief: null,
    repo: "apiari",
    branch: "swarm/fix-auth",
    goal: "Fix auth rate limiting",
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
    ...overrides,
  };
}

function makeAutoBot(overrides: Partial<AutoBot> = {}): AutoBot {
  return {
    id: "bot-1",
    workspace: "default",
    name: "Triage",
    color: "#f5c542",
    trigger_type: "signal",
    cron_schedule: null,
    signal_source: "github",
    signal_filter: null,
    prompt: "Triage issues",
    provider: "claude",
    model: null,
    enabled: true,
    status: "idle",
    created_at: "2026-05-04T09:00:00Z",
    updated_at: "2026-05-04T10:00:00Z",
    ...overrides,
  };
}

const defaultProps = {
  workspace: "default",
  workers: [],
  autoBots: [],
  onSelectWorker: vi.fn(),
  onSelectAutoBot: vi.fn(),
};

// ── Tests ─────────────────────────────────────────────────────────────────

describe("Dashboard", () => {
  it("renders EmptyState when no workers and no auto bots", () => {
    render(<Dashboard {...defaultProps} />);
    // EmptyState text
    expect(screen.getByText("Select something")).toBeInTheDocument();
  });

  it("renders stat cards when workers exist", () => {
    const workers = [
      makeWorker({ id: "w-1", state: "running" }),
      makeWorker({ id: "w-2", state: "running" }),
      makeWorker({ id: "w-3", state: "waiting" }),
      makeWorker({ id: "w-4", state: "failed" }),
      makeWorker({ id: "w-5", state: "merged" }),
    ];
    render(<Dashboard {...defaultProps} workers={workers} />);

    const statCards = screen.getByTestId("stat-cards");
    expect(statCards).toBeInTheDocument();
    // Check section headings
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.getByText("Waiting")).toBeInTheDocument();
    expect(screen.getByText("Failed")).toBeInTheDocument();
    expect(screen.getByText("Merged")).toBeInTheDocument();
  });

  it("renders correct running count in stat cards", () => {
    const workers = [
      makeWorker({ id: "w-1", state: "running" }),
      makeWorker({ id: "w-2", state: "running" }),
      makeWorker({ id: "w-3", state: "waiting" }),
    ];
    render(<Dashboard {...defaultProps} workers={workers} />);
    const statCards = screen.getByTestId("stat-cards");
    // "2" running, "1" waiting
    const counts = statCards.querySelectorAll("[class*='statCount']");
    const texts = Array.from(counts).map((el) => el.textContent);
    expect(texts).toContain("2"); // running
    expect(texts).toContain("1"); // waiting
  });

  it("stalled workers are NOT counted as running", () => {
    const workers = [
      makeWorker({ id: "w-1", state: "running", is_stalled: true }),
      makeWorker({ id: "w-2", state: "running", is_stalled: false }),
    ];
    render(<Dashboard {...defaultProps} workers={workers} />);
    const statCards = screen.getByTestId("stat-cards");
    const counts = statCards.querySelectorAll("[class*='statCount']");
    const runningCount = counts[0]?.textContent; // first card is Running
    expect(runningCount).toBe("1"); // only the non-stalled one
  });

  it("renders worker rows", () => {
    const workers = [
      makeWorker({ id: "w-1", goal: "Fix auth" }),
      makeWorker({ id: "w-2", goal: "Update deps" }),
    ];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("Fix auth")).toBeInTheDocument();
    expect(screen.getByText("Update deps")).toBeInTheDocument();
  });

  it("worker row shows revision pill when revision_count > 0", () => {
    const workers = [makeWorker({ id: "w-1", revision_count: 2 })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("pass 2")).toBeInTheDocument();
  });

  it("worker row does NOT show revision pill when revision_count == 0", () => {
    const workers = [makeWorker({ id: "w-1", revision_count: 0 })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.queryByText(/pass/)).not.toBeInTheDocument();
  });

  it("calls onSelectWorker when worker row is clicked", () => {
    const onSelectWorker = vi.fn();
    const workers = [makeWorker({ id: "w-42" })];
    render(
      <Dashboard
        {...defaultProps}
        workers={workers}
        onSelectWorker={onSelectWorker}
      />,
    );
    const row = screen.getByTestId("dashboard-worker-row");
    fireEvent.click(row);
    expect(onSelectWorker).toHaveBeenCalledWith("w-42");
  });

  it("renders auto bot rows", () => {
    const autoBots = [
      makeAutoBot({ id: "bot-1", name: "Triage" }),
      makeAutoBot({ id: "bot-2", name: "Standup" }),
    ];
    render(<Dashboard {...defaultProps} autoBots={autoBots} />);
    expect(screen.getByText("Triage")).toBeInTheDocument();
    expect(screen.getByText("Standup")).toBeInTheDocument();
  });

  it("calls onSelectAutoBot when auto bot row is clicked", () => {
    const onSelectAutoBot = vi.fn();
    const autoBots = [makeAutoBot({ id: "bot-99", name: "Triage" })];
    render(
      <Dashboard
        {...defaultProps}
        autoBots={autoBots}
        onSelectAutoBot={onSelectAutoBot}
      />,
    );
    const row = screen.getByTestId("dashboard-auto-bot-row");
    fireEvent.click(row);
    expect(onSelectAutoBot).toHaveBeenCalledWith("bot-99");
  });

  it("renders workers section heading", () => {
    const workers = [makeWorker()];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByTestId("workers-section")).toBeInTheDocument();
  });

  it("renders auto bots section heading", () => {
    const autoBots = [makeAutoBot()];
    render(<Dashboard {...defaultProps} autoBots={autoBots} />);
    expect(screen.getByTestId("auto-bots-section")).toBeInTheDocument();
  });

  it("does not render workers section when workers is empty", () => {
    const autoBots = [makeAutoBot()];
    render(<Dashboard {...defaultProps} autoBots={autoBots} />);
    expect(screen.queryByTestId("workers-section")).not.toBeInTheDocument();
  });

  it("does not render auto bots section when autoBots is empty", () => {
    const workers = [makeWorker()];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.queryByTestId("auto-bots-section")).not.toBeInTheDocument();
  });
});

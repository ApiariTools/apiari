import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import Dashboard from "../components/Dashboard/Dashboard";
import type { WorkerV2, AutoBot } from "../types";

vi.mock("../api", () => ({
  listWidgets: vi.fn().mockResolvedValue([]),
}));

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

beforeEach(() => {
  vi.clearAllMocks();
});

// ── Tests ─────────────────────────────────────────────────────────────────

describe("Dashboard", () => {
  it("renders 'No active workers' when workers list is empty", () => {
    render(<Dashboard {...defaultProps} />);
    expect(screen.getByText("No active workers")).toBeInTheDocument();
  });

  it("renders Running stat pill when running workers exist", () => {
    const workers = [
      makeWorker({ id: "w-1", state: "running" }),
      makeWorker({ id: "w-2", state: "running" }),
    ];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
  });

  it("renders Waiting stat pill when waiting workers exist", () => {
    const workers = [makeWorker({ id: "w-1", state: "waiting" })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("Waiting")).toBeInTheDocument();
    expect(screen.getByText("1")).toBeInTheDocument();
  });

  it("only renders non-zero stat pills", () => {
    const workers = [makeWorker({ id: "w-1", state: "running" })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.queryByText("Waiting")).not.toBeInTheDocument();
    expect(screen.queryByText("Stalled")).not.toBeInTheDocument();
  });

  it("stalled workers do NOT count as running", () => {
    const workers = [
      makeWorker({ id: "w-1", state: "stalled" }),
      makeWorker({ id: "w-2", state: "running" }),
    ];
    render(<Dashboard {...defaultProps} workers={workers} />);
    // Running=1, Stalled=1 — "2" should NOT appear as a count
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.getByText("Stalled")).toBeInTheDocument();
    // "2" would appear if stalled was counted as running — it should not
    expect(screen.queryByText("2")).not.toBeInTheDocument();
  });

  it("shows attention list for waiting workers", () => {
    const workers = [makeWorker({ id: "w-1", state: "waiting", goal: "Fix bug" })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("Needs attention")).toBeInTheDocument();
    expect(screen.getByText("Fix bug")).toBeInTheDocument();
  });

  it("shows attention list for stalled workers", () => {
    const workers = [makeWorker({ id: "w-1", state: "stalled", goal: "Stalled task" })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.getByText("Needs attention")).toBeInTheDocument();
    expect(screen.getByText("Stalled task")).toBeInTheDocument();
  });

  it("calls onSelectWorker when attention row is clicked", () => {
    const onSelectWorker = vi.fn();
    const workers = [makeWorker({ id: "w-42", state: "waiting", goal: "Fix bug" })];
    render(
      <Dashboard
        {...defaultProps}
        workers={workers}
        onSelectWorker={onSelectWorker}
      />,
    );
    fireEvent.click(screen.getByText("Fix bug").closest("button")!);
    expect(onSelectWorker).toHaveBeenCalledWith("w-42");
  });

  it("does not show attention list when all workers are running", () => {
    const workers = [makeWorker({ id: "w-1", state: "running" })];
    render(<Dashboard {...defaultProps} workers={workers} />);
    expect(screen.queryByText("Needs attention")).not.toBeInTheDocument();
  });

  it("renders dashboard header", () => {
    render(<Dashboard {...defaultProps} />);
    expect(screen.getByText("Overview")).toBeInTheDocument();
  });

  it("accepts autoBots prop without errors", () => {
    const autoBots = [makeAutoBot()];
    // Dashboard receives autoBots but doesn't render them directly — just smoke test
    expect(() => render(<Dashboard {...defaultProps} autoBots={autoBots} />)).not.toThrow();
  });
});

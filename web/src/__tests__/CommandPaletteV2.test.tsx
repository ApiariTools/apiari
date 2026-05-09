import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import CommandPalette from "../components/CommandPalette/CommandPalette";
import type { WorkerV2, AutoBot } from "@apiari/types";

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

const workers = [
  makeWorker({ id: "w-1", goal: "worker prompt text", display_title: "Fix auth rate limiting", branch: "swarm/fix-auth" }),
  makeWorker({ id: "w-2", goal: "generic prompt", display_title: "Dependency refresh", branch: "swarm/update-deps", state: "waiting", label: "Waiting" }),
];

const autoBots = [
  makeAutoBot({ id: "bot-1", name: "Triage", status: "idle" }),
  makeAutoBot({ id: "bot-2", name: "Standup", status: "running" }),
];

const defaultProps = {
  workers,
  autoBots,
  onSelectWorker: vi.fn(),
  onSelectAutoBot: vi.fn(),
  onClose: vi.fn(),
};

// ── Tests ─────────────────────────────────────────────────────────────────

describe("CommandPalette", () => {
  it("renders the search input", () => {
    render(<CommandPalette {...defaultProps} />);
    expect(screen.getByTestId("command-palette-input")).toBeInTheDocument();
  });

  it("renders worker rows", () => {
    render(<CommandPalette {...defaultProps} />);
    expect(screen.getByText("Fix auth rate limiting")).toBeInTheDocument();
    expect(screen.getByText("Dependency refresh")).toBeInTheDocument();
    expect(screen.queryByText("worker prompt text")).not.toBeInTheDocument();
  });

  it("renders auto bot rows", () => {
    render(<CommandPalette {...defaultProps} />);
    expect(screen.getByText("Triage")).toBeInTheDocument();
    expect(screen.getByText("Standup")).toBeInTheDocument();
  });

  it("filters workers by query", async () => {
    const user = userEvent.setup();
    render(<CommandPalette {...defaultProps} />);
    const input = screen.getByTestId("command-palette-input");
    await user.type(input, "auth");
    expect(screen.getByText("Fix auth rate limiting")).toBeInTheDocument();
    expect(screen.queryByText("Dependency refresh")).not.toBeInTheDocument();
  });

  it("matches workers by display title", async () => {
    const user = userEvent.setup();
    render(<CommandPalette {...defaultProps} />);
    const input = screen.getByTestId("command-palette-input");
    await user.type(input, "refresh");
    expect(screen.getByText("Dependency refresh")).toBeInTheDocument();
  });

  it("filters auto bots by query", async () => {
    const user = userEvent.setup();
    render(<CommandPalette {...defaultProps} />);
    const input = screen.getByTestId("command-palette-input");
    await user.type(input, "triage");
    expect(screen.getByText("Triage")).toBeInTheDocument();
    expect(screen.queryByText("Standup")).not.toBeInTheDocument();
  });

  it("shows no results message when nothing matches", async () => {
    const user = userEvent.setup();
    render(<CommandPalette {...defaultProps} />);
    const input = screen.getByTestId("command-palette-input");
    await user.type(input, "zzznomatch");
    expect(screen.getByText("No results")).toBeInTheDocument();
  });

  it("calls onSelectWorker when a worker row is clicked", async () => {
    const onSelectWorker = vi.fn();
    render(<CommandPalette {...defaultProps} onSelectWorker={onSelectWorker} />);
    const rows = screen.getAllByTestId("palette-worker-row");
    fireEvent.click(rows[0]);
    expect(onSelectWorker).toHaveBeenCalledWith("w-1");
  });

  it("calls onSelectAutoBot when an auto bot row is clicked", async () => {
    const onSelectAutoBot = vi.fn();
    render(<CommandPalette {...defaultProps} onSelectAutoBot={onSelectAutoBot} />);
    const rows = screen.getAllByTestId("palette-bot-row");
    fireEvent.click(rows[0]);
    expect(onSelectAutoBot).toHaveBeenCalledWith("bot-1");
  });

  it("calls onClose when Escape is pressed", () => {
    const onClose = vi.fn();
    render(<CommandPalette {...defaultProps} onClose={onClose} />);
    const input = screen.getByTestId("command-palette-input");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });

  it("calls onClose when clicking the overlay backdrop", () => {
    const onClose = vi.fn();
    render(<CommandPalette {...defaultProps} onClose={onClose} />);
    const overlay = screen.getByTestId("command-palette-overlay");
    // Simulate click directly on the overlay (not a child)
    fireEvent.click(overlay, { target: overlay });
    expect(onClose).toHaveBeenCalled();
  });

  it("selects item with Enter key", async () => {
    const onSelectWorker = vi.fn();
    render(<CommandPalette {...defaultProps} onSelectWorker={onSelectWorker} />);
    const input = screen.getByTestId("command-palette-input");
    // First item (index 0) is the first worker
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSelectWorker).toHaveBeenCalledWith("w-1");
  });

  it("navigates to next item with ArrowDown then selects with Enter", async () => {
    const onSelectWorker = vi.fn();
    render(<CommandPalette {...defaultProps} onSelectWorker={onSelectWorker} />);
    const input = screen.getByTestId("command-palette-input");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    // Second worker is w-2
    expect(onSelectWorker).toHaveBeenCalledWith("w-2");
  });

  it("ArrowUp does not go below index 0", () => {
    const onSelectWorker = vi.fn();
    render(<CommandPalette {...defaultProps} onSelectWorker={onSelectWorker} />);
    const input = screen.getByTestId("command-palette-input");
    // ArrowUp from index 0 should stay at 0
    fireEvent.keyDown(input, { key: "ArrowUp" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSelectWorker).toHaveBeenCalledWith("w-1");
  });
});

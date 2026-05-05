import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { ReposPanel } from "../components/ReposPanel";
import type { Repo, ResearchTask } from "../types";

const repos: Repo[] = [
  { name: "hive", path: "/dev/hive", has_swarm: true, is_clean: true, branch: "main", workers: [] },
  { name: "swarm", path: "/dev/swarm", has_swarm: true, is_clean: false, branch: "feat/test", workers: [
    { id: "cli-3", branch: "swarm/fix-bug", status: "running", agent: "claude", pr_url: "https://github.com/test/pull/1", pr_title: "Fix bug", description: null, elapsed_secs: 120, dispatched_by: "Main" },
  ]},
  { name: "common", path: "/dev/common", has_swarm: false, is_clean: true, branch: "main", workers: [] },
];

const reposMultiStatus: Repo[] = [
  { name: "hive", path: "/dev/hive", has_swarm: true, is_clean: true, branch: "main", workers: [
    { id: "w-run", branch: "swarm/a", status: "running", agent: "claude", pr_url: null, pr_title: null, description: null, elapsed_secs: 60, dispatched_by: null },
    { id: "w-wait", branch: "swarm/b", status: "waiting", agent: "claude", pr_url: null, pr_title: null, description: null, elapsed_secs: 30, dispatched_by: null },
    { id: "w-fail", branch: "swarm/c", status: "failed", agent: "claude", pr_url: null, pr_title: null, description: null, elapsed_secs: null, dispatched_by: null },
    { id: "w-merge", branch: "swarm/d", status: "merged", agent: "claude", pr_url: null, pr_title: null, description: null, elapsed_secs: null, dispatched_by: null },
  ]},
];

const defaultProps = {
  repos,
  onSelectWorker: vi.fn(),
  mobileOpen: false,
  onClose: vi.fn(),
};

describe("ReposPanel", () => {
  it("renders repo names", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("hive")).toBeInTheDocument();
    expect(screen.getByText("swarm")).toBeInTheDocument();
    expect(screen.getByText("common")).toBeInTheDocument();
  });

  it("shows branch names", () => {
    render(<ReposPanel {...defaultProps} />);
    const mains = screen.getAllByText("main");
    expect(mains.length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("feat/test")).toBeInTheDocument();
  });

  it("shows modified badge for dirty repos", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("modified")).toBeInTheDocument();
  });

  it("does not show modified badge for clean repos", () => {
    render(<ReposPanel {...defaultProps} repos={[repos[0]]} />);
    expect(screen.queryByText("modified")).not.toBeInTheDocument();
  });

  it("renders workers under their repo", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("cli-3")).toBeInTheDocument();
  });

  it("shows PR badge on workers with PRs", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("PR")).toBeInTheDocument();
  });

  it("calls onSelectWorker when worker clicked", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<ReposPanel {...defaultProps} onSelectWorker={onSelect} />);
    await user.click(screen.getByText("cli-3"));
    expect(onSelect).toHaveBeenCalledWith("cli-3");
  });

  it("shows empty state", () => {
    render(<ReposPanel {...defaultProps} repos={[]} />);
    expect(screen.getByText("No repos found")).toBeInTheDocument();
  });

  it("shows Repos title", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("Repos")).toBeInTheDocument();
  });

  it("always shows Research header even with no tasks", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("Research")).toBeInTheDocument();
    expect(screen.getByText("Use /research <topic> to start")).toBeInTheDocument();
  });

  it("renders all four stat card labels", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.getByText("Waiting")).toBeInTheDocument();
    expect(screen.getByText("Failed")).toBeInTheDocument();
    expect(screen.getByText("Merged")).toBeInTheDocument();
  });

  it("shows correct worker counts in stat cards", () => {
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    const runningCard = screen.getByText("Running").closest("button")!;
    const waitingCard = screen.getByText("Waiting").closest("button")!;
    const failedCard = screen.getByText("Failed").closest("button")!;
    const mergedCard = screen.getByText("Merged").closest("button")!;
    expect(runningCard).toHaveTextContent("1");
    expect(waitingCard).toHaveTextContent("1");
    expect(failedCard).toHaveTextContent("1");
    expect(mergedCard).toHaveTextContent("1");
  });

  it("filters to running workers when Running card clicked", async () => {
    const user = userEvent.setup();
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    await user.click(screen.getByText("Running"));
    expect(screen.getByText("w-run")).toBeInTheDocument();
    expect(screen.queryByText("w-wait")).not.toBeInTheDocument();
    expect(screen.queryByText("w-fail")).not.toBeInTheDocument();
    expect(screen.queryByText("w-merge")).not.toBeInTheDocument();
  });

  it("filters to waiting workers when Waiting card clicked", async () => {
    const user = userEvent.setup();
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    await user.click(screen.getByText("Waiting"));
    expect(screen.queryByText("w-run")).not.toBeInTheDocument();
    expect(screen.getByText("w-wait")).toBeInTheDocument();
    expect(screen.queryByText("w-fail")).not.toBeInTheDocument();
    expect(screen.queryByText("w-merge")).not.toBeInTheDocument();
  });

  it("filters to failed workers when Failed card clicked", async () => {
    const user = userEvent.setup();
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    await user.click(screen.getByText("Failed"));
    expect(screen.queryByText("w-run")).not.toBeInTheDocument();
    expect(screen.queryByText("w-wait")).not.toBeInTheDocument();
    expect(screen.getByText("w-fail")).toBeInTheDocument();
    expect(screen.queryByText("w-merge")).not.toBeInTheDocument();
  });

  it("filters to merged workers when Merged card clicked", async () => {
    const user = userEvent.setup();
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    await user.click(screen.getByText("Merged"));
    expect(screen.queryByText("w-run")).not.toBeInTheDocument();
    expect(screen.queryByText("w-wait")).not.toBeInTheDocument();
    expect(screen.queryByText("w-fail")).not.toBeInTheDocument();
    expect(screen.getByText("w-merge")).toBeInTheDocument();
  });

  it("clears filter when active card clicked again", async () => {
    const user = userEvent.setup();
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    await user.click(screen.getByText("Running"));
    expect(screen.queryByText("w-wait")).not.toBeInTheDocument();
    await user.click(screen.getByText("Running"));
    expect(screen.getByText("w-wait")).toBeInTheDocument();
    expect(screen.getByText("w-run")).toBeInTheDocument();
  });

  it("switches filter when different card clicked", async () => {
    const user = userEvent.setup();
    render(<ReposPanel {...defaultProps} repos={reposMultiStatus} />);
    await user.click(screen.getByText("Running"));
    expect(screen.getByText("w-run")).toBeInTheDocument();
    await user.click(screen.getByText("Waiting"));
    expect(screen.queryByText("w-run")).not.toBeInTheDocument();
    expect(screen.getByText("w-wait")).toBeInTheDocument();
  });

  it("shows research tasks when provided", () => {
    const tasks: ResearchTask[] = [
      {
        id: "r1",
        workspace: "apiari",
        topic: "auth patterns",
        status: "running",
        error: null,
        started_at: "2026-05-02T10:00:00Z",
        completed_at: null,
        output_file: null,
      },
      {
        id: "r2",
        workspace: "apiari",
        topic: "caching strategies",
        status: "complete",
        error: null,
        started_at: "2026-05-02T10:00:00Z",
        completed_at: "2026-05-02T10:05:00Z",
        output_file: "caching.md",
      },
    ];
    render(<ReposPanel {...defaultProps} researchTasks={tasks} />);
    expect(screen.getByText("Research")).toBeInTheDocument();
    expect(screen.getByText("auth patterns")).toBeInTheDocument();
    expect(screen.getByText("caching strategies")).toBeInTheDocument();
    expect(screen.getByText("caching.md")).toBeInTheDocument();
    expect(screen.queryByText("Use /research <topic> to start")).not.toBeInTheDocument();
  });
});

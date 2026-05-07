import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { ReposPanel } from "../components/ReposPanel";
import type { Repo, ResearchTask } from "../types";

const repos: Repo[] = [
  { name: "hive", path: "/dev/hive", has_swarm: true, is_clean: true, branch: "main", workers: [] },
  { name: "swarm", path: "/dev/swarm", has_swarm: true, is_clean: false, branch: "feat/test", workers: [
    { id: "cli-3", branch: "swarm/fix-bug", status: "running", agent: "claude", pr_url: "https://github.com/test/pull/1", pr_title: "Fix bug", description: "Fix login race", elapsed_secs: 120, dispatched_by: "Main" },
  ]},
  { name: "common", path: "/dev/common", has_swarm: false, is_clean: true, branch: "main", workers: [] },
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
    expect(screen.getByText("Fix login race")).toBeInTheDocument();
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
    await user.click(screen.getByText("Fix login race"));
    expect(onSelect).toHaveBeenCalledWith("cli-3");
  });

  it("shows empty state", () => {
    render(<ReposPanel {...defaultProps} repos={[]} />);
    expect(screen.getByText("No repos found")).toBeInTheDocument();
  });

  it("shows workspace repos title", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("Workspace repos")).toBeInTheDocument();
  });

  it("always shows research outputs header even with no tasks", () => {
    render(<ReposPanel {...defaultProps} />);
    expect(screen.getByText("Research outputs")).toBeInTheDocument();
    expect(screen.getByText("Use /research <topic> to start")).toBeInTheDocument();
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
    expect(screen.getByText("Research outputs")).toBeInTheDocument();
    expect(screen.getByText("auth patterns")).toBeInTheDocument();
    expect(screen.getByText("caching strategies")).toBeInTheDocument();
    expect(screen.getByText("caching.md")).toBeInTheDocument();
    expect(screen.queryByText("Use /research <topic> to start")).not.toBeInTheDocument();
  });
});

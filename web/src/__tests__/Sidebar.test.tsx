import { render, screen } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import Sidebar from "../components/Sidebar/Sidebar";
import type { SidebarItem } from "../components/Sidebar/Sidebar";

const defaultProps = {
  selectedType: null as null,
  selectedId: null,
  onSelect: vi.fn(),
  autoBots: [] as SidebarItem[],
  workers: [] as SidebarItem[],
  workspaces: ["default"],
  workspace: "default",
  onWorkspaceChange: vi.fn(),
};

const workerItems: SidebarItem[] = [
  { id: "w-1", name: "Fix auth", status: "running" },
  { id: "w-2", name: "Update deps", status: "waiting" },
];

const botItems: SidebarItem[] = [{ id: "bot-1", name: "Triage", status: "idle" }];

describe("Sidebar empty states", () => {
  it("shows 'No workers yet' when workers array is empty", () => {
    render(<Sidebar {...defaultProps} />);
    expect(screen.getByText("No workers yet")).toBeInTheDocument();
  });

  it("shows 'No auto bots' when autoBots array is empty", () => {
    render(<Sidebar {...defaultProps} />);
    expect(screen.getByText("No auto bots")).toBeInTheDocument();
  });

  it("does NOT show 'No workers yet' when workers are present", () => {
    render(<Sidebar {...defaultProps} workers={workerItems} />);
    expect(screen.queryByText("No workers yet")).not.toBeInTheDocument();
    expect(screen.getByText("Fix auth")).toBeInTheDocument();
    expect(screen.getByText("Update deps")).toBeInTheDocument();
  });

  it("does NOT show 'No auto bots' when autoBots are present", () => {
    render(<Sidebar {...defaultProps} autoBots={botItems} />);
    expect(screen.queryByText("No auto bots")).not.toBeInTheDocument();
    expect(screen.getByText("Triage")).toBeInTheDocument();
  });

  it("shows items without empty state messages when both lists are populated", () => {
    render(<Sidebar {...defaultProps} workers={workerItems} autoBots={botItems} />);
    expect(screen.queryByText("No workers yet")).not.toBeInTheDocument();
    expect(screen.queryByText("No auto bots")).not.toBeInTheDocument();
    expect(screen.getByText("Fix auth")).toBeInTheDocument();
    expect(screen.getByText("Triage")).toBeInTheDocument();
  });
});

describe("Sidebar done workers filtering", () => {
  it("does not show done-workers footer when doneWorkerCount is 0", () => {
    render(<Sidebar {...defaultProps} workers={workerItems} doneWorkerCount={0} />);
    expect(screen.queryByTestId("done-workers-footer")).not.toBeInTheDocument();
  });

  it("shows 'N completed' footer when doneWorkerCount > 0", () => {
    render(<Sidebar {...defaultProps} workers={workerItems} doneWorkerCount={3} />);
    const footer = screen.getByTestId("done-workers-footer");
    expect(footer).toBeInTheDocument();
    expect(footer).toHaveTextContent("3 completed");
  });

  it("calls onShowDoneWorkers when the footer is clicked", () => {
    const onShowDoneWorkers = vi.fn();
    render(<Sidebar {...defaultProps} doneWorkerCount={2} onShowDoneWorkers={onShowDoneWorkers} />);
    screen.getByTestId("done-workers-footer").click();
    expect(onShowDoneWorkers).toHaveBeenCalled();
  });

  it("shows 'No workers yet' when workers is empty and doneWorkerCount is 0", () => {
    render(<Sidebar {...defaultProps} doneWorkerCount={0} />);
    expect(screen.getByText("No workers yet")).toBeInTheDocument();
  });

  it("does NOT show 'No workers yet' when workers is empty but doneWorkerCount > 0", () => {
    // Active list is empty but there are done workers — suppress empty message
    render(<Sidebar {...defaultProps} doneWorkerCount={2} />);
    expect(screen.queryByText("No workers yet")).not.toBeInTheDocument();
    expect(screen.getByTestId("done-workers-footer")).toHaveTextContent("2 completed");
  });

  it("shows active workers but not done workers in the main list", () => {
    // App.tsx filters done workers before passing to Sidebar — Sidebar just
    // renders what it receives. Verify active workers appear.
    const activeItems: SidebarItem[] = [{ id: "w-active", name: "Active task", status: "running" }];
    render(<Sidebar {...defaultProps} workers={activeItems} doneWorkerCount={2} />);
    expect(screen.getByText("Active task")).toBeInTheDocument();
    expect(screen.getByTestId("done-workers-footer")).toHaveTextContent("2 completed");
  });
});

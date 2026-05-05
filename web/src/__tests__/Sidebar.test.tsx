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

const botItems: SidebarItem[] = [
  { id: "bot-1", name: "Triage", status: "idle" },
];

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

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { TopBar } from "../components/TopBar";

const workspaces = [{ name: "apiari" }, { name: "mgm" }, { name: "cookedbooks" }];

describe("TopBar", () => {
  it("renders hive logo", () => {
    render(<TopBar workspaces={workspaces} active="apiari" onSelect={vi.fn()} />);
    expect(screen.getByText("hive")).toBeInTheDocument();
  });

  it("renders all workspace tabs", () => {
    render(<TopBar workspaces={workspaces} active="apiari" onSelect={vi.fn()} />);
    expect(screen.getByText("apiari")).toBeInTheDocument();
    expect(screen.getByText("mgm")).toBeInTheDocument();
    expect(screen.getByText("cookedbooks")).toBeInTheDocument();
  });

  it("calls onSelect when tab clicked", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<TopBar workspaces={workspaces} active="apiari" onSelect={onSelect} />);
    await user.click(screen.getByText("mgm"));
    expect(onSelect).toHaveBeenCalledWith("mgm", undefined);
  });

  it("calls onMenuToggle when hamburger clicked", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    render(<TopBar workspaces={workspaces} active="apiari" onSelect={vi.fn()} onMenuToggle={onToggle} />);
    // Hamburger has 3 spans — find the button
    const buttons = screen.getAllByRole("button");
    // First button should be hamburger
    await user.click(buttons[0]);
    expect(onToggle).toHaveBeenCalled();
  });

  it("calls onOpenPalette when search button clicked", async () => {
    const user = userEvent.setup();
    const onOpenPalette = vi.fn();
    render(<TopBar workspaces={workspaces} active="apiari" onSelect={vi.fn()} onOpenPalette={onOpenPalette} />);
    await user.click(screen.getByRole("button", { name: "Open command palette" }));
    expect(onOpenPalette).toHaveBeenCalled();
  });

  it("renders with no workspaces", () => {
    render(<TopBar workspaces={[]} active="" onSelect={vi.fn()} />);
    expect(screen.getByText("hive")).toBeInTheDocument();
  });

  it("calls onToggleSimulator when simulator button clicked", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    render(<TopBar workspaces={workspaces} active="apiari" onSelect={vi.fn()} onToggleSimulator={onToggle} />);
    await user.click(screen.getByRole("button", { name: "Toggle simulator" }));
    expect(onToggle).toHaveBeenCalled();
  });

  it("scrolls active tab into view on workspace change", () => {
    const scrollIntoView = vi.fn();
    Element.prototype.scrollIntoView = scrollIntoView;

    const { rerender } = render(
      <TopBar workspaces={workspaces} active="apiari" onSelect={vi.fn()} />
    );
    scrollIntoView.mockClear();

    rerender(<TopBar workspaces={workspaces} active="mgm" onSelect={vi.fn()} />);
    expect(scrollIntoView).toHaveBeenCalledWith({ behavior: "smooth", inline: "center", block: "nearest" });
  });
});

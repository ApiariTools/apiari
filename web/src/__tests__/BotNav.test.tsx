import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { BotNav } from "../components/BotNav";

const bots = [
  { name: "Main", color: "#f5c542", role: "Assistant", watch: [] as string[] },
  { name: "Customer", color: "#e85555", role: "Customer bot", watch: ["sentry"] },
  { name: "Performance", color: "#5cb85c", role: "Perf bot", watch: ["sentry"] },
];

const defaultProps = {
  bots,
  workers: [],
  activeBot: "Main" as string | null,
  activeWorkerId: null as string | null,
  onSelectBot: vi.fn(),
  onSelectWorker: vi.fn(),
  mobileOpen: false,
  unread: {} as Record<string, number>,
};

describe("BotNav", () => {
  it("renders all bot names", () => {
    render(<BotNav {...defaultProps} />);
    expect(screen.getByText("Main")).toBeInTheDocument();
    expect(screen.getByText("Customer")).toBeInTheDocument();
    expect(screen.getByText("Performance")).toBeInTheDocument();
  });

  it("shows unread badge", () => {
    render(<BotNav {...defaultProps} unread={{ Customer: 5 }} />);
    expect(screen.getByText("5")).toBeInTheDocument();
  });

  it("does not show badge for active bot", () => {
    render(<BotNav {...defaultProps} activeBot="Customer" unread={{ Customer: 3 }} />);
    expect(screen.queryByText("3")).not.toBeInTheDocument();
  });

  it("does not show badge when count is 0", () => {
    render(<BotNav {...defaultProps} unread={{ Customer: 0 }} />);
    // No badge element should exist
    const badges = screen.queryAllByText("0");
    expect(badges.length).toBe(0);
  });

  it("calls onSelectBot when clicked", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<BotNav {...defaultProps} onSelectBot={onSelect} />);
    await user.click(screen.getByText("Customer"));
    expect(onSelect).toHaveBeenCalledWith("Customer");
  });

  it("renders Bots label", () => {
    render(<BotNav {...defaultProps} />);
    expect(screen.getByText("Bots")).toBeInTheDocument();
  });
});

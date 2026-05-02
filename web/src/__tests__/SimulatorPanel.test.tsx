import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { SimulatorPanel } from "../components/SimulatorPanel";

beforeEach(() => {
  vi.restoreAllMocks();
  // Mock fetch for simulator status
  globalThis.fetch = vi.fn().mockResolvedValue({
    json: () => Promise.resolve({ booted: false, device: null, udid: null }),
  }) as typeof fetch;
});

describe("SimulatorPanel", () => {
  it("does not show backdrop when closed", () => {
    const { container } = render(<SimulatorPanel open={false} onClose={vi.fn()} />);
    // Panel is translated offscreen when closed, no backdrop rendered
    expect(container.querySelector("[class*='backdrop']")).not.toBeInTheDocument();
  });

  it("shows 'No simulator running' when not booted", async () => {
    render(<SimulatorPanel open={true} onClose={vi.fn()} />);
    // Status defaults to null initially, then fetches unbooted status
    await waitFor(() => {
      expect(screen.getByText("No simulator running")).toBeInTheDocument();
    });
  });

  it("shows device name when booted", async () => {
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValue({
      json: () =>
        Promise.resolve({
          booted: true,
          device: "iPhone 16 Pro",
          udid: "ABC-123",
        }),
    });
    render(<SimulatorPanel open={true} onClose={vi.fn()} />);
    await waitFor(() => {
      expect(screen.getByText("iPhone 16 Pro")).toBeInTheDocument();
    });
  });

  it("calls onClose when close button clicked", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<SimulatorPanel open={true} onClose={onClose} />);
    await waitFor(() => expect(screen.getByText("Simulator")).toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: "Close simulator" }));
    expect(onClose).toHaveBeenCalled();
  });

  it("calls onClose when backdrop clicked", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { container } = render(<SimulatorPanel open={true} onClose={onClose} />);
    // Backdrop is the first child element when open
    const backdrop = container.firstElementChild;
    if (backdrop) await user.click(backdrop);
    expect(onClose).toHaveBeenCalled();
  });
});

import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../api");

import { FollowupCard, FollowupIndicator } from "../components/FollowupCard";
import * as api from "../api";
import type { Followup } from "../types";

function makeFollowup(overrides: Partial<Followup> = {}): Followup {
  return {
    id: "fu_test123",
    workspace: "ws",
    bot: "Main",
    action: "Check PR #59 status",
    created_at: new Date().toISOString(),
    fires_at: new Date(Date.now() + 300000).toISOString(), // 5 min from now
    status: "pending",
    ...overrides,
  };
}

describe("FollowupCard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders pending followup with countdown", () => {
    render(<FollowupCard followup={makeFollowup()} workspace="ws" />);
    expect(screen.getByText(/Follow-up in/)).toBeInTheDocument();
    expect(screen.getByText(/Check PR #59 status/)).toBeInTheDocument();
    expect(screen.getByText("Cancel")).toBeInTheDocument();
  });

  it("renders fired followup", () => {
    render(<FollowupCard followup={makeFollowup({ status: "fired" })} workspace="ws" />);
    expect(screen.getByText("Follow-up triggered")).toBeInTheDocument();
    expect(screen.queryByText("Cancel")).not.toBeInTheDocument();
  });

  it("renders cancelled followup", () => {
    render(<FollowupCard followup={makeFollowup({ status: "cancelled" })} workspace="ws" />);
    expect(screen.getByText("Follow-up cancelled")).toBeInTheDocument();
    expect(screen.queryByText("Cancel")).not.toBeInTheDocument();
  });

  it("calls cancelFollowup API on cancel click", async () => {
    const user = userEvent.setup();
    const onCancelled = vi.fn();
    render(
      <FollowupCard
        followup={makeFollowup()}
        workspace="ws"
        onCancelled={onCancelled}
      />
    );

    await user.click(screen.getByText("Cancel"));
    expect(api.cancelFollowup).toHaveBeenCalledWith("ws", "fu_test123");
  });

  it("renders inline variant without cancel button", () => {
    const { container } = render(
      <FollowupCard followup={makeFollowup({ status: "fired" })} workspace="ws" inline />
    );
    expect(screen.getByText("Follow-up triggered")).toBeInTheDocument();
    expect(screen.queryByText("Cancel")).not.toBeInTheDocument();
    // Should have inline class
    expect(container.querySelector('[class*="inline"]')).not.toBeNull();
  });

  it("updates countdown over time", async () => {
    vi.useFakeTimers();
    const followup = makeFollowup({
      fires_at: new Date(Date.now() + 120000).toISOString(), // 2 min
    });
    render(<FollowupCard followup={followup} workspace="ws" />);

    expect(screen.getByText(/Follow-up in/)).toBeInTheDocument();

    // Advance 60 seconds
    act(() => {
      vi.advanceTimersByTime(60000);
    });

    // Should still show countdown (about 1 min remaining)
    expect(screen.getByText(/Follow-up in/)).toBeInTheDocument();

    vi.useRealTimers();
  });
});

describe("FollowupIndicator", () => {
  it("renders for pending followup", () => {
    render(<FollowupIndicator followup={makeFollowup()} />);
    expect(screen.getByText(/Follow-up in/)).toBeInTheDocument();
  });

  it("does not render for fired followup", () => {
    const { container } = render(
      <FollowupIndicator followup={makeFollowup({ status: "fired" })} />
    );
    expect(container.firstChild).toBeNull();
  });
});

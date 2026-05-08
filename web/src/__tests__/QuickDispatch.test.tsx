import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import QuickDispatch from "../components/QuickDispatch/QuickDispatch";
import * as api from "../api";

vi.mock("../api");

const mockRepos = [
  { name: "apiari", path: "/repos/apiari", has_swarm: true, is_clean: true, branch: "main", workers: [] },
  { name: "mgm", path: "/repos/mgm", has_swarm: false, is_clean: true, branch: "main", workers: [] },
];

const defaultProps = {
  workspace: "apiari",
  onClose: vi.fn(),
  onDispatched: vi.fn(),
};

beforeEach(() => {
  vi.mocked(api.getRepos).mockResolvedValue(mockRepos);
  vi.mocked(api.createWorkerV2).mockResolvedValue({
    ok: true,
    worker_id: "worker-new",
  });
  vi.mocked(api.chatWithContextBot).mockResolvedValue({
    response: "Brief generated.",
    session_id: "session-1",
  });
  defaultProps.onClose.mockClear();
  defaultProps.onDispatched.mockClear();
});

describe("QuickDispatch", () => {
  it("renders the intent textarea", () => {
    render(<QuickDispatch {...defaultProps} />);
    expect(screen.getByTestId("intent-textarea")).toBeInTheDocument();
  });

  it("renders repo pills after fetch", async () => {
    render(<QuickDispatch {...defaultProps} />);
    expect(await screen.findByTestId("repo-pill-apiari")).toBeInTheDocument();
    expect(screen.getByTestId("repo-pill-mgm")).toBeInTheDocument();
  });

  it("dispatch button is disabled when intent is empty", async () => {
    render(<QuickDispatch {...defaultProps} />);
    // Wait for repos to load (first repo auto-selected)
    await screen.findByTestId("repo-pill-apiari");
    expect(screen.getByTestId("dispatch-btn")).toBeDisabled();
  });

  it("dispatch button is disabled when no repo is selected", async () => {
    vi.mocked(api.getRepos).mockResolvedValue([]);
    render(<QuickDispatch {...defaultProps} />);
    // No repos → no auto-select
    const textarea = screen.getByTestId("intent-textarea");
    fireEvent.change(textarea, { target: { value: "Fix the auth flow" } });
    // Need to wait for the (empty) repo fetch
    await waitFor(() => {
      expect(screen.getByTestId("dispatch-btn")).toBeDisabled();
    });
  });

  it("dispatch button is enabled when intent and repo are both filled", async () => {
    render(<QuickDispatch {...defaultProps} />);
    await screen.findByTestId("repo-pill-apiari");
    const textarea = screen.getByTestId("intent-textarea");
    fireEvent.change(textarea, { target: { value: "Fix the auth flow" } });
    expect(screen.getByTestId("dispatch-btn")).not.toBeDisabled();
  });

  it("Escape key calls onClose", () => {
    render(<QuickDispatch {...defaultProps} />);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(defaultProps.onClose).toHaveBeenCalled();
  });

  it("clicking Cancel calls onClose", () => {
    render(<QuickDispatch {...defaultProps} />);
    fireEvent.click(screen.getByTestId("cancel-btn"));
    expect(defaultProps.onClose).toHaveBeenCalled();
  });

  it("clicking the overlay backdrop calls onClose", () => {
    render(<QuickDispatch {...defaultProps} />);
    const overlay = screen.getByTestId("quick-dispatch-overlay");
    fireEvent.mouseDown(overlay, { target: overlay });
    expect(defaultProps.onClose).toHaveBeenCalled();
  });

  it("successful dispatch calls onDispatched with worker id", async () => {
    render(<QuickDispatch {...defaultProps} />);
    await screen.findByTestId("repo-pill-apiari");

    const textarea = screen.getByTestId("intent-textarea");
    fireEvent.change(textarea, { target: { value: "Add rate limiting" } });
    fireEvent.click(screen.getByTestId("dispatch-btn"));

    await waitFor(() => {
      expect(defaultProps.onDispatched).toHaveBeenCalledWith("worker-new");
    });
  });

  it("calls createWorkerV2 with correct arguments", async () => {
    render(<QuickDispatch {...defaultProps} />);
    await screen.findByTestId("repo-pill-apiari");

    const textarea = screen.getByTestId("intent-textarea");
    fireEvent.change(textarea, { target: { value: "Fix rate limit bug" } });
    fireEvent.click(screen.getByTestId("dispatch-btn"));

    await waitFor(() => {
      expect(api.createWorkerV2).toHaveBeenCalledWith(
        "apiari",
        expect.objectContaining({
          repo: "apiari",
          brief: expect.objectContaining({
            goal: "Fix rate limit bug",
            repo: "apiari",
            review_mode: "local_first",
          }),
        }),
      );
    });
  });

  it("shows error message when dispatch fails", async () => {
    vi.mocked(api.createWorkerV2).mockRejectedValue(new Error("Network error"));
    render(<QuickDispatch {...defaultProps} />);
    await screen.findByTestId("repo-pill-apiari");

    const textarea = screen.getByTestId("intent-textarea");
    fireEvent.change(textarea, { target: { value: "Fix bug" } });
    fireEvent.click(screen.getByTestId("dispatch-btn"));

    expect(await screen.findByTestId("dispatch-error")).toHaveTextContent("Network error");
    expect(defaultProps.onDispatched).not.toHaveBeenCalled();
  });

  it("selecting a different repo pill updates selection", async () => {
    render(<QuickDispatch {...defaultProps} />);
    await screen.findByTestId("repo-pill-apiari");

    const mgmPill = screen.getByTestId("repo-pill-mgm");
    fireEvent.click(mgmPill);
    expect(mgmPill).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByTestId("repo-pill-apiari")).toHaveAttribute("aria-pressed", "false");
  });

  it("review mode toggles between local_first and pr_first", async () => {
    render(<QuickDispatch {...defaultProps} />);
    const localBtn = screen.getByTestId("review-mode-local");
    const prBtn = screen.getByTestId("review-mode-pr");

    expect(localBtn).toHaveAttribute("aria-pressed", "true");
    expect(prBtn).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(prBtn);
    expect(prBtn).toHaveAttribute("aria-pressed", "true");
    expect(localBtn).toHaveAttribute("aria-pressed", "false");
  });
});

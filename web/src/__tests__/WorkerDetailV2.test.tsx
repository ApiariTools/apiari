import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import WorkerDetailV2 from "../components/WorkerDetailV2/WorkerDetailV2";
import * as api from "../api";
import type { WorkerDetailV2 as WorkerDetailV2Data } from "../types";

vi.mock("../api");
vi.mock("react-markdown", () => ({
  default: ({ children }: { children: string }) => <span>{children}</span>,
}));

const mockWorker: WorkerDetailV2Data = {
  id: "w-abc",
  workspace: "default",
  state: "running",
  label: "Working",
  brief: null,
  repo: "apiari",
  branch: "swarm/fix-auth",
  goal: "Add rate limiting to /api/chat",
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
  events: [
    {
      event_type: "assistant_text",
      content: "I will add rate limiting now.",
      created_at: "2026-05-04T10:01:00Z",
    },
    {
      event_type: "tool_use",
      content: "edit(src/routes.rs)",
      created_at: "2026-05-04T10:02:00Z",
    },
    {
      event_type: "user_message",
      content: "Please also add tests.",
      created_at: "2026-05-04T10:03:00Z",
    },
  ],
};

beforeEach(() => {
  vi.mocked(api.getWorkerV2).mockResolvedValue(mockWorker);
});

describe("WorkerDetailV2", () => {
  it("renders status badge with correct label", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByTestId("status-badge")).toHaveTextContent("Working");
  });

  it("renders goal as heading", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByText("Add rate limiting to /api/chat")).toBeInTheDocument();
  });

  it("renders branch in monospace", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByText("swarm/fix-auth")).toBeInTheDocument();
  });

  it("renders property pills", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const pills = await screen.findByTestId("property-pills");
    expect(pills).toBeInTheDocument();
    expect(pills).toHaveTextContent("Tests passing");
    expect(pills).toHaveTextContent("local first");
  });

  it("renders action bar with send button", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByTestId("action-bar")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeInTheDocument();
  });

  it("renders events thread with all event types", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const thread = await screen.findByTestId("events-thread");
    expect(thread).toBeInTheDocument();
    // assistant_text
    expect(thread).toHaveTextContent("I will add rate limiting now.");
    // tool_use
    expect(thread).toHaveTextContent("edit(src/routes.rs)");
    // user_message
    expect(thread).toHaveTextContent("You");
    expect(thread).toHaveTextContent("Please also add tests.");
  });

  it("shows cancel button when state is running", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByRole("button", { name: "Cancel" })).toBeInTheDocument();
  });

  it("does not show cancel button when state is merged", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, state: "merged", label: "Merged" });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  });

  it("shows retry button when state is failed", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, state: "failed", label: "Failed" });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("shows stalled pill when is_stalled is true", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      is_stalled: true,
      label: "Stalled",
      state: "running",
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const pills = await screen.findByTestId("property-pills");
    expect(pills).toHaveTextContent("Stalled");
  });

  it("shows PR link when pr_url is set", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      pr_url: "https://github.com/org/repo/pull/42",
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const link = await screen.findByRole("link", { name: /PR/i });
    expect(link).toHaveAttribute("href", "https://github.com/org/repo/pull/42");
  });

  it("shows revision pill when revision_count > 0", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, revision_count: 2, label: "Revising (pass 2)" });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByText("Pass 2")).toBeInTheDocument();
  });

  it("calls sendWorkerMessageV2 when send is clicked", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const textarea = await screen.findByPlaceholderText("Send a message to the worker...");
    fireEvent.change(textarea, { target: { value: "hello worker" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => {
      expect(api.sendWorkerMessageV2).toHaveBeenCalledWith("default", "w-abc", "hello worker");
    });
  });

  it("calls cancelWorkerV2 when cancel is clicked", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(api.cancelWorkerV2).toHaveBeenCalledWith("default", "w-abc");
    });
  });

  it("shows loading state initially", () => {
    vi.mocked(api.getWorkerV2).mockImplementation(() => new Promise(() => {}));
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });

  it("shows error state on failure", async () => {
    vi.mocked(api.getWorkerV2).mockRejectedValue(new Error("network error"));
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByText("network error")).toBeInTheDocument();
  });
});

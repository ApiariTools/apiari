import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import WorkerDetailV2 from "../components/WorkerDetailV2/WorkerDetailV2";
import * as api from "../api";
import type { WorkerDetailV2 as WorkerDetailV2Data, WorkerReview } from "../types";

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
  vi.mocked(api.listWorkerReviews).mockResolvedValue([]);
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
    expect(pills).toHaveTextContent("Local tests ✓");
    expect(pills).toHaveTextContent("local first");
  });

  it("renders action bar with send button on Timeline tab", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    // Timeline is the default tab; running state still shows the bar (just disabled)
    expect(await screen.findByTestId("action-bar")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeInTheDocument();
  });

  it("renders events thread with all event types on Timeline tab", async () => {
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
    // Use waiting state so the input is enabled
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, state: "waiting", label: "Waiting" });
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

  it("shows loading skeleton initially", () => {
    vi.mocked(api.getWorkerV2).mockImplementation(() => new Promise(() => {}));
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(screen.getByTestId("loading-skeleton")).toBeInTheDocument();
  });

  it("shows error state on failure", async () => {
    vi.mocked(api.getWorkerV2).mockRejectedValue(new Error("network error"));
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByTestId("error-state")).toBeInTheDocument();
    expect(screen.getByText("network error")).toBeInTheDocument();
    expect(screen.getByTestId("retry-btn")).toBeInTheDocument();
  });

  it("shows 'Local tests ✓' pill when tests_passing is true", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, tests_passing: true });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const pills = await screen.findByTestId("property-pills");
    expect(pills).toHaveTextContent("Local tests ✓");
  });

  it("does NOT show 'Local tests ✓' pill when tests_passing is false", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, tests_passing: false });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const pills = await screen.findByTestId("property-pills");
    expect(pills).not.toHaveTextContent("Local tests ✓");
  });

  // ── Tab switching tests ───────────────────────────────────────────────────

  it("renders three tabs: Timeline, Reviews, Brief", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.getByTestId("tab-timeline")).toBeInTheDocument();
    expect(screen.getByTestId("tab-reviews")).toBeInTheDocument();
    expect(screen.getByTestId("tab-brief")).toBeInTheDocument();
  });

  it("switches to Reviews tab on click and shows reviews section", async () => {
    const mockReview: WorkerReview = {
      id: 1,
      reviewer: "General",
      verdict: "request_changes",
      summary: "Missing error handling.",
      issues: [],
      worker_message: null,
      created_at: "2026-05-04T10:00:00Z",
    };
    vi.mocked(api.listWorkerReviews).mockResolvedValue([mockReview]);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    fireEvent.click(screen.getByTestId("tab-reviews"));
    expect(await screen.findByTestId("reviews-section")).toBeInTheDocument();
    expect(screen.getByText("Missing error handling.")).toBeInTheDocument();
  });

  it("switches to Brief tab and shows 'No brief recorded' when brief is null", async () => {
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    fireEvent.click(screen.getByTestId("tab-brief"));
    expect(await screen.findByText("No brief recorded for this worker.")).toBeInTheDocument();
  });

  it("Brief tab renders goal from brief object", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      brief: {
        goal: "My brief goal",
        context: { relevant_files: [], recent_changes: "", known_issues: [], conventions: "" },
        constraints: ["Must be fast"],
        repo: "apiari",
        scope: [],
        acceptance_criteria: ["Works end to end"],
        review_mode: "local_first",
      },
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    fireEvent.click(screen.getByTestId("tab-brief"));
    expect(await screen.findByText("My brief goal")).toBeInTheDocument();
    expect(screen.getByText("Must be fast")).toBeInTheDocument();
    expect(screen.getByText("Works end to end")).toBeInTheDocument();
  });

  // ── Input state tests ─────────────────────────────────────────────────────

  it("input is disabled when state is running", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, state: "running", label: "Working" });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("action-bar");
    const textarea = screen.getByPlaceholderText("Worker is running…");
    expect(textarea).toBeDisabled();
  });

  it("input is hidden when state is merged", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, state: "merged", label: "Merged" });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.queryByTestId("action-bar")).not.toBeInTheDocument();
  });

  it("input is hidden when state is abandoned", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({ ...mockWorker, state: "abandoned", label: "Abandoned" });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.queryByTestId("action-bar")).not.toBeInTheDocument();
  });

  // ── Review feature tests ──────────────────────────────────────────────────

  it("shows 'Request Review' button when waiting and branch_ready", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      state: "waiting",
      label: "Ready for local review",
      branch_ready: true,
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    expect(await screen.findByTestId("review-btn")).toBeInTheDocument();
    expect(screen.getByTestId("review-btn")).toHaveTextContent("Request Review");
  });

  it("hides 'Request Review' button when state is running", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      state: "running",
      label: "Working",
      branch_ready: false,
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.queryByTestId("review-btn")).not.toBeInTheDocument();
  });

  it("shows 'Reviewed' state divider (not 'Waiting for review') when waiting and reviews exist", async () => {
    const mockReview: WorkerReview = {
      id: 1,
      reviewer: "General",
      verdict: "approve",
      summary: "Looks good.",
      issues: [],
      worker_message: null,
      created_at: "2026-05-04T10:00:00Z",
    };
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      state: "waiting",
      label: "Waiting for review",
      branch_ready: true,
    });
    vi.mocked(api.listWorkerReviews).mockResolvedValue([mockReview]);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.queryByText("Waiting for review", { selector: '[class*="stateDividerText"]' })).not.toBeInTheDocument();
    expect(screen.getByText("Reviewed")).toBeInTheDocument();
  });

  it("shows 'Waiting for review' state divider when waiting and no reviews exist", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      state: "waiting",
      label: "Waiting",
      branch_ready: true,
    });
    vi.mocked(api.listWorkerReviews).mockResolvedValue([]);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.getByText("Waiting for review")).toBeInTheDocument();
  });

  it("hides 'Request Review' button when waiting but branch_ready=false", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      state: "waiting",
      label: "Waiting",
      branch_ready: false,
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    expect(screen.queryByTestId("review-btn")).not.toBeInTheDocument();
  });

  it("calls requestWorkerReview when review button clicked", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      state: "waiting",
      label: "Ready for local review",
      branch_ready: true,
    });
    vi.mocked(api.requestWorkerReview).mockResolvedValue(undefined);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    fireEvent.click(await screen.findByTestId("review-btn"));
    await waitFor(() => {
      expect(api.requestWorkerReview).toHaveBeenCalledWith("default", "w-abc");
    });
  });

  it("renders reviews section when reviews exist (via Reviews tab)", async () => {
    const mockReview: WorkerReview = {
      id: 1,
      reviewer: "General",
      verdict: "request_changes",
      summary: "Missing error handling in the main function.",
      issues: [
        {
          severity: "blocking",
          file: "src/main.rs",
          description: "Unwrap on line 42 will panic.",
        },
      ],
      worker_message: "Please fix the unwrap on line 42.",
      created_at: "2026-05-04T10:00:00Z",
    };
    vi.mocked(api.listWorkerReviews).mockResolvedValue([mockReview]);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    fireEvent.click(screen.getByTestId("tab-reviews"));
    const section = await screen.findByTestId("reviews-section");
    expect(section).toBeInTheDocument();
    expect(section).toHaveTextContent("General");
    expect(section).toHaveTextContent("Changes requested");
    expect(section).toHaveTextContent("Missing error handling");
    expect(section).toHaveTextContent("blocking");
    expect(section).toHaveTextContent("src/main.rs");
    expect(section).toHaveTextContent("Please fix the unwrap");
  });

  it("renders approve verdict badge correctly", async () => {
    const mockReview: WorkerReview = {
      id: 2,
      reviewer: "General",
      verdict: "approve",
      summary: "All changes look correct.",
      issues: [],
      worker_message: null,
      created_at: "2026-05-04T10:00:00Z",
    };
    vi.mocked(api.listWorkerReviews).mockResolvedValue([mockReview]);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    fireEvent.click(screen.getByTestId("tab-reviews"));
    const section = await screen.findByTestId("reviews-section");
    expect(section).toHaveTextContent("Approved");
    expect(section).not.toHaveTextContent("Sent to worker");
  });

  it("shows review count badge on Reviews tab when reviews exist", async () => {
    const mockReview: WorkerReview = {
      id: 1,
      reviewer: "General",
      verdict: "approve",
      summary: "Looks good.",
      issues: [],
      worker_message: null,
      created_at: "2026-05-04T10:00:00Z",
    };
    vi.mocked(api.listWorkerReviews).mockResolvedValue([mockReview]);
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    await screen.findByTestId("status-badge");
    const reviewTab = screen.getByTestId("tab-reviews");
    expect(reviewTab).toHaveTextContent("1");
  });

  // ── Timeline event display tests ─────────────────────────────────────────

  it("merges consecutive assistant_text events into a single block", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      events: [
        { event_type: "assistant_text", content: "I'll add the `", created_at: "2026-05-04T10:01:00Z" },
        { event_type: "assistant_text", content: "<!-- apiari-test -->", created_at: "2026-05-04T10:01:00Z" },
        { event_type: "assistant_text", content: " HTML comment", created_at: "2026-05-04T10:01:00Z" },
        { event_type: "assistant_text", content: ".", created_at: "2026-05-04T10:01:00Z" },
      ],
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const thread = await screen.findByTestId("events-thread");
    // All four fragments should appear as a single concatenated text
    expect(thread).toHaveTextContent("I'll add the `<!-- apiari-test --> HTML comment.");
    // Only one child row should exist for assistant_text (not four separate rows)
    const rows = thread.querySelectorAll('[class*="eventRow"]');
    expect(rows).toHaveLength(1);
  });

  it("formats tool_use events readably instead of showing raw JSON", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      events: [
        {
          event_type: "tool_use",
          tool: "Bash",
          input: { command: "cd /path && git status", description: "Check git status" },
          content: 'Bash: {"command":"cd /path && git status","description":"Check git status"}',
          created_at: "2026-05-04T10:01:00Z",
        },
        {
          event_type: "tool_use",
          tool: "Read",
          input: { file_path: "/Users/josh/Developer/apiari/web/src/App.tsx" },
          content: 'Read: {"file_path":"/Users/josh/Developer/apiari/web/src/App.tsx"}',
          created_at: "2026-05-04T10:02:00Z",
        },
        {
          event_type: "tool_use",
          tool: "Glob",
          input: { pattern: ".task/**/*" },
          content: 'Glob: {"pattern":".task/**/*"}',
          created_at: "2026-05-04T10:03:00Z",
        },
      ],
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const thread = await screen.findByTestId("events-thread");
    // All three consecutive tool calls are grouped into a single collapsed row
    const group = await screen.findByTestId("tool-group");
    expect(group).toBeInTheDocument();
    // Collapsed preview shows first 3 formatted tool names
    expect(group).toHaveTextContent("Bash · cd /path && git status");
    expect(group).toHaveTextContent("Read · App.tsx");
    expect(group).toHaveTextContent("Glob · .task/**/*");
    // Raw JSON should not appear
    expect(thread).not.toHaveTextContent('"command"');
    expect(thread).not.toHaveTextContent('"file_path"');
  });

  it("groups consecutive tool_use events into a single collapsible row", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      events: [
        { event_type: "assistant_text", content: "Starting work.", created_at: "2026-05-04T10:00:00Z" },
        {
          event_type: "tool_use",
          tool: "Read",
          input: { file_path: "/src/foo.ts" },
          content: "Read: /src/foo.ts",
          created_at: "2026-05-04T10:01:00Z",
        },
        {
          event_type: "tool_use",
          tool: "Edit",
          input: { file_path: "/src/foo.ts" },
          content: "Edit: /src/foo.ts",
          created_at: "2026-05-04T10:02:00Z",
        },
        { event_type: "assistant_text", content: "Done!", created_at: "2026-05-04T10:03:00Z" },
      ],
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const thread = await screen.findByTestId("events-thread");
    // The two tool calls should be merged into one group row
    const groups = thread.querySelectorAll('[data-testid="tool-group"]');
    expect(groups).toHaveLength(1);
    // The group shows both tools in preview
    expect(groups[0]).toHaveTextContent("Read · foo.ts");
    expect(groups[0]).toHaveTextContent("Edit · foo.ts");
  });

  it("expands tool group on click to show individual tools", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      events: [
        {
          event_type: "tool_use",
          tool: "Bash",
          input: { command: "npm test" },
          content: "Bash: npm test",
          created_at: "2026-05-04T10:01:00Z",
        },
        {
          event_type: "tool_use",
          tool: "Read",
          input: { file_path: "/src/index.ts" },
          content: "Read: /src/index.ts",
          created_at: "2026-05-04T10:02:00Z",
        },
      ],
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const group = await screen.findByTestId("tool-group");
    // Collapsed by default — shows ▶
    expect(group).toHaveTextContent("▶");
    // Click to expand
    fireEvent.click(group);
    // Now shows ▼ and individual tool items
    expect(group).toHaveTextContent("▼");
    expect(group).toHaveTextContent("2 tool calls");
    expect(group).toHaveTextContent("· Bash · npm test");
    expect(group).toHaveTextContent("· Read · index.ts");
  });

  it("single tool_use event is still grouped (collapsed group of 1)", async () => {
    vi.mocked(api.getWorkerV2).mockResolvedValue({
      ...mockWorker,
      events: [
        {
          event_type: "tool_use",
          tool: "Bash",
          input: { command: "cargo build" },
          content: "Bash: cargo build",
          created_at: "2026-05-04T10:01:00Z",
        },
      ],
    });
    render(<WorkerDetailV2 workspace="default" workerId="w-abc" />);
    const group = await screen.findByTestId("tool-group");
    expect(group).toBeInTheDocument();
    expect(group).toHaveTextContent("Bash · cargo build");
  });
});

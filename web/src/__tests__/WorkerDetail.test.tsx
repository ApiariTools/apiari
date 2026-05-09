import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { WorkerDetail } from "../components/WorkerDetail";
import type { Worker, WorkerDetail as WorkerDetailData } from "@apiari/types";

vi.mock("@git-diff-view/react", () => ({
  DiffView: () => null,
  DiffModeEnum: { Unified: 0 },
}));

vi.mock("@git-diff-view/core", () => ({
  DiffFile: {
    createInstance: () => ({ initTheme: vi.fn(), init: vi.fn(), buildUnifiedDiffLines: vi.fn() }),
  },
  getLang: () => "text",
}));

const worker: Worker = {
  id: "worker-1",
  branch: "swarm/test",
  status: "stalled",
  agent: "claude",
  execution_note: "Uncommitted diff, no ready branch, and no active session.",
  ready_branch: null,
  has_uncommitted_changes: true,
  task_id: "task-1",
  task_title: "Tighten worker lifecycle",
  task_stage: "Human Review",
  task_lifecycle_state: "Blocked",
  task_repo: "apiari",
  latest_attempt: {
    worker_id: "worker-1",
    role: "implementation",
    state: "failed",
    detail: "Worker finished without a ready branch or PR handoff.",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:02:00Z",
    completed_at: "2026-01-01T00:02:00Z",
  },
  pr_url: null,
  pr_title: null,
  description: null,
  elapsed_secs: null,
  dispatched_by: null,
};

const detail: WorkerDetailData = {
  ...worker,
  prompt: "Do the thing",
  output: "Done",
  task_packet: {
    worker_mode: "implementation",
    task_md: "# Task\n\nTighten the mobile cards.",
    context_md: "# Context\n\n- Prior worker drifted into the wrong panel.",
    shaping_md:
      "# Coordinator Shaping\n\n## Goal\n- Tighten worker cards on mobile.\n\n## Likely Files\n- `web/src/components/WorkersPanel.module.css`",
    plan_md: "# Plan\n\n1. Find the right panel.\n2. Make the change.",
    progress_md: "Checked two likely surfaces before editing.",
  },
  conversation: [
    { role: "user", content: "hello", timestamp: "2025-01-15T13:42:00Z" },
    { role: "assistant", content: "hi there", timestamp: "2025-01-15T13:43:00Z" },
    { role: "tool", content: "*Using edit*" },
    { role: "assistant", content: "done" },
  ],
};

const promoteWorker = vi.fn(async () => ({
  ok: true,
  detail: "Created PR for branch `swarm/test`.",
}));
const redispatchWorker = vi.fn(async () => ({
  ok: true,
  worker_id: "worker-2",
  detail: "Spawned replacement worker `worker-2`.",
}));
const closeWorker = vi.fn(async () => ({
  ok: true,
  detail: "Closed worker and dismissed its task.",
}));

describe("WorkerDetail", () => {
  it("renders timestamps on messages that have them", () => {
    render(
      <WorkerDetail
        worker={worker}
        detail={detail}
        workspace="test"
        onBack={vi.fn()}
        onPromoteWorker={promoteWorker}
        onRedispatchWorker={redispatchWorker}
        onCloseWorker={closeWorker}
      />,
    );
    // Switch to chat tab
    fireEvent.click(screen.getByRole("tab", { name: "Chat" }));

    // Messages with timestamps should show formatted time
    // Pattern handles both 12h ("1:42 PM") and 24h ("13:42") locales
    const timePattern = /\d{1,2}:\d{2}/;

    // The strong "You" is inside a div, look for it within the rendered content
    const youEl = screen.getByText((_, el) => el?.tagName === "STRONG" && el.textContent === "You");
    const youMeta = youEl.parentElement!.textContent!;
    expect(youMeta).toMatch(timePattern);
    expect(youMeta).toContain("·");

    // First worker-1 label (assistant message with timestamp)
    const workerEls = screen.getAllByText(
      (_, el) => el?.tagName === "STRONG" && el.textContent === "worker-1",
    );
    const workerMeta = workerEls[0].parentElement!.textContent!;
    expect(workerMeta).toMatch(timePattern);
    expect(workerMeta).toContain("·");
  });

  it("does not render timestamp when absent", () => {
    const detailNoTs: WorkerDetailData = {
      ...worker,
      prompt: null,
      output: null,
      conversation: [{ role: "assistant", content: "no timestamp here" }],
    };
    render(
      <WorkerDetail
        worker={worker}
        detail={detailNoTs}
        workspace="test"
        onBack={vi.fn()}
        onPromoteWorker={promoteWorker}
        onRedispatchWorker={redispatchWorker}
        onCloseWorker={closeWorker}
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Chat" }));

    const workerLabel = screen.getByText(
      (_, el) => el?.tagName === "STRONG" && el.textContent === "worker-1",
    );
    expect(workerLabel.parentElement!.textContent).not.toMatch(/\d{1,2}:\d{2}/);
  });

  it("renders already-formatted human timestamps without Invalid Date", () => {
    const detailHumanTs: WorkerDetailData = {
      ...worker,
      prompt: null,
      output: null,
      conversation: [{ role: "assistant", content: "thinking...", timestamp: "10:54 AM" }],
    };
    render(
      <WorkerDetail
        worker={worker}
        detail={detailHumanTs}
        workspace="test"
        onBack={vi.fn()}
        onPromoteWorker={promoteWorker}
        onRedispatchWorker={redispatchWorker}
        onCloseWorker={closeWorker}
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Chat" }));

    expect(screen.queryByText(/Invalid Date/)).not.toBeInTheDocument();
    expect(
      screen.getByText((_, el) => el?.className?.toString().includes("msgMeta") ?? false)
        .textContent,
    ).toContain("10:54 AM");
  });

  it("shows task-owned lifecycle context in the task tab", () => {
    render(
      <WorkerDetail
        worker={worker}
        detail={detail}
        workspace="test"
        onBack={vi.fn()}
        onPromoteWorker={promoteWorker}
        onRedispatchWorker={redispatchWorker}
        onCloseWorker={closeWorker}
      />,
    );

    fireEvent.click(screen.getByRole("tab", { name: "Task" }));

    expect(screen.getByText(/Task:/)).toBeInTheDocument();
    expect(screen.getByText(/Tighten worker lifecycle/)).toBeInTheDocument();
    expect(screen.getByText(/Lifecycle:/)).toBeInTheDocument();
    expect(screen.getAllByText(/Blocked/).length).toBeGreaterThan(0);
    expect(screen.getByText(/Internal stage:/)).toBeInTheDocument();
    expect(screen.getByText(/Human Review/)).toBeInTheDocument();
    expect(screen.getByText((_, el) => el?.textContent === "Repo: apiari")).toBeInTheDocument();
    expect(screen.getByText(/Latest attempt:/)).toBeInTheDocument();
    expect(screen.getByText(/implementation failed/)).toBeInTheDocument();
    expect(screen.getByText(/Attempt detail:/)).toBeInTheDocument();
    expect(
      screen.getByText(/Worker finished without a ready branch or PR handoff/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Execution:/)).toBeInTheDocument();
    expect(screen.getByText(/Uncommitted diff present/)).toBeInTheDocument();
    expect(screen.getByText(/Ready branch:/)).toBeInTheDocument();
    expect(screen.getByText(/not signalled/)).toBeInTheDocument();
    expect(screen.getByText(/Worker kind:/)).toBeInTheDocument();
    expect(screen.getAllByText(/implementation/).length).toBeGreaterThan(0);
    expect(screen.getByText(/Inherited task/)).toBeInTheDocument();
    expect(screen.getByText(/Inherited context/)).toBeInTheDocument();
    expect(screen.getByText(/Coordinator shaping/)).toBeInTheDocument();
    expect(screen.getByText(/Tighten worker cards on mobile/)).toBeInTheDocument();
    expect(screen.getByText(/web\/src\/components\/WorkersPanel\.module\.css/)).toBeInTheDocument();
    expect(screen.getByText(/Execution plan/)).toBeInTheDocument();
    expect(screen.getByText(/Worker notes/)).toBeInTheDocument();
  });

  it("shows action feedback after promoting a worker", async () => {
    render(
      <WorkerDetail
        worker={worker}
        detail={detail}
        workspace="test"
        onBack={vi.fn()}
        onPromoteWorker={promoteWorker}
        onRedispatchWorker={redispatchWorker}
        onCloseWorker={closeWorker}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Promote to PR" }));

    expect(await screen.findByText(/Created PR for branch/)).toBeInTheDocument();
  });

  it("confirms before closing a worker and shows feedback", async () => {
    render(
      <WorkerDetail
        worker={worker}
        detail={detail}
        workspace="test"
        onBack={vi.fn()}
        onPromoteWorker={promoteWorker}
        onRedispatchWorker={redispatchWorker}
        onCloseWorker={closeWorker}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.getByText(/Close this worker and dismiss its task/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(await screen.findByText(/Closed worker and dismissed its task/)).toBeInTheDocument();
  });
});

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { WorkersPanel } from "../components/WorkersPanel";
import type { Worker } from "../types";

const workers: Worker[] = [
  {
    id: "apiari-ebbc",
    branch: "swarm/mobile-cards",
    status: "failed",
    agent: "codex",
    execution_note: "Worker finished without a ready branch or PR handoff.",
    ready_branch: null,
    has_uncommitted_changes: false,
    task_id: "task-1",
    task_title: "Tighten mobile cards",
    task_stage: "In Progress",
    task_lifecycle_state: "Ready",
    task_repo: "apiari",
    latest_attempt: {
      worker_id: "apiari-ebbc",
      role: "implementation",
      state: "failed",
      detail: "Worker closed without PR",
      created_at: "2026-05-04T00:00:00Z",
      updated_at: "2026-05-04T00:01:00Z",
      completed_at: "2026-05-04T00:01:00Z",
    },
    pr_url: null,
    pr_title: null,
    description: null,
    elapsed_secs: 90,
    dispatched_by: "Main",
  },
];

describe("WorkersPanel", () => {
  it("shows task titles as the primary worker label", () => {
    const onSelectWorker = vi.fn();
    render(<WorkersPanel workers={workers} onSelectWorker={onSelectWorker} />);

    expect(screen.getByText("Tighten mobile cards")).toBeInTheDocument();
    expect(screen.getByText("apiari-ebbc · mobile-cards")).toBeInTheDocument();
    expect(screen.getByText("Ready")).toBeInTheDocument();
    expect(screen.getByText("Implementation failed")).toBeInTheDocument();
    expect(screen.getByText("Worker closed without PR")).toBeInTheDocument();

    fireEvent.click(screen.getByText("Tighten mobile cards"));
    expect(onSelectWorker).toHaveBeenCalledWith("apiari-ebbc");
  });
});

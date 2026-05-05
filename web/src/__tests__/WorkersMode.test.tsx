import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { WorkersMode } from "../modes/WorkersMode";
import type { Worker, WorkerEnvironmentStatus } from "../types";

const workers: Worker[] = [];

const blockedEnvironment: WorkerEnvironmentStatus = {
  repo: "apiari",
  ready: false,
  git_worktree_metadata_writable: false,
  frontend_toolchain_required: true,
  frontend_toolchain_ready: true,
  worktree_links_ready: false,
  blockers: [
    "Git worktree metadata is not writable, so workers cannot commit or push.",
    "Frontend toolchain exists in the repo root, but worker worktrees do not inherit `web/node_modules`, so frontend verification will fail.",
  ],
  suggested_fixes: [
    "Run the daemon in an environment that can write under the repo's .git/worktrees metadata path.",
    "Add `web/node_modules` to `.swarm/worktree-links` so worker worktrees inherit the frontend toolchain.",
  ],
};

describe("WorkersMode", () => {
  it("shows worker environment blockers and suggested fixes", () => {
    render(
      <WorkersMode
        workspace="apiari"
        workers={workers}
        workerEnvironment={blockedEnvironment}
        workerId={null}
        selectedWorker={null}
        workerDetail={null}
        isMobile={false}
        onSelectWorker={vi.fn()}
        onBackFromWorker={vi.fn()}
        onPromoteWorker={vi.fn()}
        onRedispatchWorker={vi.fn()}
        onCloseWorker={vi.fn()}
      />,
    );

    expect(screen.getByText("Worker environment")).toBeInTheDocument();
    expect(screen.getByText("Blocked")).toBeInTheDocument();
    expect(screen.getByText("repo apiari")).toBeInTheDocument();
    expect(screen.getByText(/Git worktree metadata is not writable/)).toBeInTheDocument();
    expect(screen.getByText(/Add `web\/node_modules` to `.swarm\/worktree-links`/)).toBeInTheDocument();
  });
});

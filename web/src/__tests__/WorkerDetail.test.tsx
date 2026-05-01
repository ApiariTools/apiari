import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { WorkerDetail } from "../components/WorkerDetail";
import type { Worker, WorkerDetail as WorkerDetailData } from "../types";

vi.mock("@git-diff-view/react", () => ({
  DiffView: () => null,
  DiffModeEnum: { Unified: 0 },
}));

vi.mock("@git-diff-view/core", () => ({
  DiffFile: { createInstance: () => ({ initTheme: vi.fn(), init: vi.fn(), buildUnifiedDiffLines: vi.fn() }) },
  getLang: () => "text",
}));

const worker: Worker = {
  id: "worker-1",
  branch: "swarm/test",
  status: "done",
  agent: "claude",
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
  conversation: [
    { role: "user", content: "hello", timestamp: "2025-01-15T13:42:00Z" },
    { role: "assistant", content: "hi there", timestamp: "2025-01-15T13:43:00Z" },
    { role: "tool", content: "*Using edit*" },
    { role: "assistant", content: "done" },
  ],
};

describe("WorkerDetail", () => {
  it("renders timestamps on messages that have them", () => {
    render(
      <WorkerDetail
        worker={worker}
        detail={detail}
        workspace="test"
        onBack={vi.fn()}
      />
    );
    // Switch to chat tab
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));

    // Messages with timestamps should show formatted time
    // Pattern handles both 12h ("1:42 PM") and 24h ("13:42") locales
    const timePattern = /\d{1,2}:\d{2}/;

    // The strong "You" is inside a div, look for it within the rendered content
    const youEl = screen.getByText((_, el) => el?.tagName === "STRONG" && el.textContent === "You");
    const youMeta = youEl.parentElement!.textContent!;
    expect(youMeta).toMatch(timePattern);
    expect(youMeta).toContain("·");

    // First worker-1 label (assistant message with timestamp)
    const workerEls = screen.getAllByText((_, el) => el?.tagName === "STRONG" && el.textContent === "worker-1");
    const workerMeta = workerEls[0].parentElement!.textContent!;
    expect(workerMeta).toMatch(timePattern);
    expect(workerMeta).toContain("·");
  });

  it("does not render timestamp when absent", () => {
    const detailNoTs: WorkerDetailData = {
      ...worker,
      prompt: null,
      output: null,
      conversation: [
        { role: "assistant", content: "no timestamp here" },
      ],
    };
    render(
      <WorkerDetail
        worker={worker}
        detail={detailNoTs}
        workspace="test"
        onBack={vi.fn()}
      />
    );
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));

    const workerLabel = screen.getByText((_, el) => el?.tagName === "STRONG" && el.textContent === "worker-1");
    expect(workerLabel.parentElement!.textContent).not.toMatch(/\d{1,2}:\d{2}/);
  });
});

import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import AutoBotDetail from "../components/AutoBotDetail/AutoBotDetail";
import * as api from "@apiari/api";
import type { AutoBotDetail as AutoBotDetailData } from "@apiari/types";

vi.mock("@apiari/api", () => ({
  getAutoBot: vi.fn(),
  triggerAutoBot: vi.fn(),
  updateAutoBot: vi.fn(),
  listWidgets: vi.fn(),
  chatWithAutoBot: vi.fn(),
}));

const baseBot: AutoBotDetailData = {
  id: "bot-1",
  workspace: "default",
  name: "Triage Bot",
  color: "#f5c542",
  trigger_type: "signal",
  cron_schedule: null,
  signal_source: "github",
  signal_filter: null,
  prompt: "Triage incoming issues",
  provider: "claude",
  model: null,
  enabled: true,
  status: "idle",
  created_at: "2026-05-04T09:00:00Z",
  updated_at: "2026-05-04T09:00:00Z",
  runs: [],
};

const botWithRuns: AutoBotDetailData = {
  ...baseBot,
  runs: [
    {
      id: "run-1",
      auto_bot_id: "bot-1",
      workspace: "default",
      triggered_by: "signal:github:42",
      started_at: "2026-05-04T10:00:00Z",
      finished_at: "2026-05-04T10:01:00Z",
      outcome: "dispatched_worker",
      summary: "Dispatched fix-auth worker to address rate limiting issue.",
      worker_id: "w-abc",
    },
    {
      id: "run-2",
      auto_bot_id: "bot-1",
      workspace: "default",
      triggered_by: "signal:github:43",
      started_at: "2026-05-04T09:00:00Z",
      finished_at: "2026-05-04T09:01:00Z",
      outcome: "noise",
      summary: "Signal did not meet threshold for action.",
      worker_id: null,
    },
    {
      id: "run-3",
      auto_bot_id: "bot-1",
      workspace: "default",
      triggered_by: "signal:github:44",
      started_at: "2026-05-04T08:00:00Z",
      finished_at: "2026-05-04T08:01:00Z",
      outcome: "notified",
      summary: "Sent notification to team.",
      worker_id: null,
    },
    {
      id: "run-4",
      auto_bot_id: "bot-1",
      workspace: "default",
      triggered_by: "signal:github:45",
      started_at: "2026-05-04T07:00:00Z",
      finished_at: "2026-05-04T07:01:00Z",
      outcome: "error",
      summary: "Failed to connect to GitHub API.",
      worker_id: null,
    },
  ],
};

beforeEach(() => {
  vi.mocked(api.getAutoBot).mockResolvedValue(baseBot);
  vi.mocked(api.listWidgets).mockResolvedValue([]);
  vi.mocked(api.chatWithAutoBot).mockResolvedValue({ run_id: "r-1" });
});

describe("AutoBotDetail", () => {
  it("renders bot name and trigger type", async () => {
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    const name = await screen.findByTestId("bot-name");
    expect(name).toHaveTextContent("Triage Bot");
    expect(screen.getByText(/Watches: github signals/)).toBeInTheDocument();
  });

  it("renders cron trigger line for cron bots", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue({
      ...baseBot,
      trigger_type: "cron",
      cron_schedule: "0 9 * * 1-5",
    });
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    expect(await screen.findByText(/Runs on schedule: 0 9 \* \* 1-5/)).toBeInTheDocument();
  });

  it("shows empty state when no runs", async () => {
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("bot-name");
    expect(screen.getByTestId("empty-state")).toBeInTheDocument();
    expect(screen.getByText("No runs yet. This bot hasn't fired.")).toBeInTheDocument();
  });

  it("renders run feed with outcome badges", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("bot-name");

    const feed = screen.getByTestId("run-feed");
    expect(feed).toBeInTheDocument();

    expect(screen.getByTestId("badge-dispatched")).toBeInTheDocument();
    expect(screen.getByTestId("badge-noise")).toBeInTheDocument();
    expect(screen.getByTestId("badge-notified")).toBeInTheDocument();
    expect(screen.getByTestId("badge-error")).toBeInTheDocument();
  });

  it("renders dispatched_worker badge text", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("badge-dispatched");
    expect(screen.getByTestId("badge-dispatched")).toHaveTextContent("Dispatched worker");
  });

  it("renders notified badge text", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("badge-notified");
    expect(screen.getByTestId("badge-notified")).toHaveTextContent("Notified");
  });

  it("renders noise badge text", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("badge-noise");
    expect(screen.getByTestId("badge-noise")).toHaveTextContent("No action");
  });

  it("renders error badge text", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("badge-error");
    expect(screen.getByTestId("badge-error")).toHaveTextContent("Error");
  });

  it("renders running badge when outcome is null", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue({
      ...baseBot,
      status: "running",
      runs: [
        {
          id: "run-live",
          auto_bot_id: "bot-1",
          workspace: "default",
          triggered_by: "signal:github:99",
          started_at: "2026-05-04T10:00:00Z",
          finished_at: null,
          outcome: null,
          summary: null,
          worker_id: null,
        },
      ],
    });
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    expect(await screen.findByTestId("badge-running")).toBeInTheDocument();
    expect(screen.getByTestId("badge-running")).toHaveTextContent("Running");
  });

  it("calls triggerAutoBot when Trigger Now button is clicked", async () => {
    vi.mocked(api.triggerAutoBot).mockResolvedValue();
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    const btn = await screen.findByTestId("trigger-btn");
    fireEvent.click(btn);
    await waitFor(() => {
      expect(api.triggerAutoBot).toHaveBeenCalledWith("default", "bot-1");
    });
  });

  it("shows loading state initially", () => {
    vi.mocked(api.getAutoBot).mockImplementation(() => new Promise(() => {}));
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    expect(screen.getByRole("status")).toBeInTheDocument();
  });

  it("shows error state on load failure", async () => {
    vi.mocked(api.getAutoBot).mockRejectedValue(new Error("network error"));
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    expect(await screen.findByText("network error")).toBeInTheDocument();
  });

  it("shows enable/disable toggle", async () => {
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("bot-name");
    expect(screen.getByTestId("enable-toggle")).toBeInTheDocument();
  });

  it("shows (disabled) label when bot is disabled", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue({ ...baseBot, enabled: false });
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    const name = await screen.findByTestId("bot-name");
    expect(name).toHaveTextContent("(disabled)");
  });

  it("toggle reflects enabled state", async () => {
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("enable-toggle");
    const checkbox = screen.getByRole("checkbox", { name: /Disable bot/i });
    expect(checkbox).toBeChecked();
  });

  it("toggle reflects disabled state", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue({ ...baseBot, enabled: false });
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("enable-toggle");
    const checkbox = screen.getByRole("checkbox", { name: /Enable bot/i });
    expect(checkbox).not.toBeChecked();
  });

  it("renders worker link and calls onSelectWorker when clicked", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    const onSelectWorker = vi.fn();
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" onSelectWorker={onSelectWorker} />);
    await screen.findByTestId("bot-name");
    const workerLink = screen.getByText(/Worker: w-abc/);
    fireEvent.click(workerLink);
    expect(onSelectWorker).toHaveBeenCalledWith("w-abc");
  });

  it("renders run summaries", async () => {
    vi.mocked(api.getAutoBot).mockResolvedValue(botWithRuns);
    render(<AutoBotDetail workspace="default" autoBotId="bot-1" />);
    await screen.findByTestId("bot-name");
    expect(
      screen.getByText("Dispatched fix-auth worker to address rate limiting issue."),
    ).toBeInTheDocument();
  });
});

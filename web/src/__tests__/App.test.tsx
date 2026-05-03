import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../api");

import App from "../App";
import * as api from "../api";

beforeEach(() => {
  vi.clearAllMocks();
  window.location.hash = "";
  window.localStorage.clear();
});

function workspaceTab(name: string) {
  return screen.getByRole("button", { name: `Open workspace ${name}` });
}

function remoteWorkspaceTab(name: string, remote: string) {
  return screen.getByRole("button", { name: `Open workspace ${name} (${remote})` });
}

function botButton(name: string) {
  return screen.getByLabelText(`Open bot ${name}`);
}

async function renderAndSelectBot(name = "Main") {
  const user = userEvent.setup();
  render(<App />);
  await waitFor(() => expect(botButton(name)).toBeInTheDocument());
  await user.click(botButton(name));
  return user;
}

describe("App", () => {
  it("renders workspace tabs", async () => {
    render(<App />);
    await waitFor(() => {
      expect(workspaceTab("apiari")).toBeInTheDocument();
      expect(workspaceTab("mgm")).toBeInTheDocument();
    });
  });

  it("loads bots on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.getBots).toHaveBeenCalled();
    });
  });

  it("loads repos on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.getRepos).toHaveBeenCalled();
    });
  });

  it("renders chat messages", async () => {
    await renderAndSelectBot("Main");
    await waitFor(() => {
      expect(screen.getByText("hello")).toBeInTheDocument();
      expect(screen.getByText(/How can I help/)).toBeInTheDocument();
    });
  });

  it("shows unread badge", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("2").length).toBeGreaterThan(0);
    });
  });

  it("shows hive logo", async () => {
    render(<App />);
    expect(screen.getByText("hive")).toBeInTheDocument();
  });

  it("has a text input", async () => {
    await renderAndSelectBot("Main");
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
  });

  it("calls markSeen on bot select", async () => {
    render(<App />);
    await waitFor(() => expect(botButton("Main")).toBeInTheDocument());
    expect(api.markSeen).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.click(botButton("Main"));
    await waitFor(() => {
      expect(api.markSeen).toHaveBeenCalledWith("apiari", "Main", undefined);
    });
  });

  it("connects websocket on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.connectWebSocket).toHaveBeenCalled();
    });
  });

  it("opens workspace layout settings and applies a saved layout change", async () => {
    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => expect(botButton("Main")).toBeInTheDocument());
    await user.click(botButton("Main"));
    await waitFor(() => {
      expect(screen.getByText("No repos found")).toBeInTheDocument();
    });

    await user.click(screen.getByRole("button", { name: "Open command palette" }));
    await waitFor(() => {
      expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    });
    await user.click(screen.getByText("Customize Workspace Layout..."));

    await waitFor(() => {
      expect(screen.getByRole("dialog", { name: "Workspace layout settings" })).toBeInTheDocument();
    });
    const repoRailToggle = screen.getByLabelText("Show repo rail during chat");
    expect(repoRailToggle).toBeChecked();
    await user.click(repoRailToggle);
    await user.click(screen.getByRole("button", { name: "Save layout" }));

    await waitFor(() => {
      expect(screen.queryByText("No repos found")).not.toBeInTheDocument();
    });
  });
});

describe("Bot switching", () => {
  it("calls getConversations with new bot", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(botButton("Customer")).toBeInTheDocument());
    await user.click(botButton("Customer"));
    await waitFor(() => {
      const mock = api.getConversations as ReturnType<typeof vi.fn>;
      expect(mock.mock.calls.some((c: string[]) => c[1] === "Customer")).toBe(true);
    });
  });
});

describe("Polling cancellation on bot switch", () => {
  it("does not apply stale poll responses after switching bots", async () => {
    const statusMock = api.getBotStatus as ReturnType<typeof vi.fn>;

    // Make getBotStatus return a delayed promise that we control
    let resolveStale: (v: { status: string; streaming_content: string; tool_name: null }) => void;
    const stalePromise = new Promise<{ status: string; streaming_content: string; tool_name: null }>((r) => {
      resolveStale = r;
    });

    // First getBotStatus call (Main's initial load) gets the delayed promise
    statusMock.mockReturnValueOnce(stalePromise);

    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(botButton("Main")).toBeInTheDocument());

    // Select Main bot — triggers initial load, getBotStatus gets stalePromise
    await user.click(botButton("Main"));
    await waitFor(() => expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument());

    // Switch to Customer bot before the delayed Main response resolves
    // This triggers cleanup (cancelled=true) on Main's initial load effect
    await user.click(botButton("Customer"));
    await waitFor(() => expect(screen.getByPlaceholderText(/Message Customer/)).toBeInTheDocument());

    // Now resolve the stale status from the old Main bot context
    resolveStale!({ status: "streaming", streaming_content: "stale content from Main", tool_name: null });

    // Wait a tick for the promise to settle
    await new Promise((r) => setTimeout(r, 10));

    // The stale streaming content should NOT appear in the Customer bot's view
    expect(screen.queryByText("stale content from Main")).not.toBeInTheDocument();
  });
});

describe("Workspace switching", () => {
  it("calls getBots with new workspace", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(workspaceTab("mgm")).toBeInTheDocument());
    await user.click(workspaceTab("mgm"));
    await waitFor(() => {
      const mock = api.getBots as ReturnType<typeof vi.fn>;
      expect(mock.mock.calls.some((c: string[]) => c[0] === "mgm")).toBe(true);
    });
  });

  it("auto-selects Main bot on mobile when switching workspaces", async () => {
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    window.dispatchEvent(new Event("resize"));
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(workspaceTab("mgm")).toBeInTheDocument());
    await user.click(workspaceTab("mgm"));
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
    Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
  });

  it("passes remote workspace routing through API calls", async () => {
    (api.getWorkspaces as ReturnType<typeof vi.fn>).mockResolvedValue([
      { name: "apiari" },
      { name: "apiari", remote: "staging" },
    ]);
    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => expect(remoteWorkspaceTab("apiari", "staging")).toBeInTheDocument());
    await user.click(remoteWorkspaceTab("apiari", "staging"));

    await waitFor(() => {
      expect(api.getBots).toHaveBeenCalledWith("apiari", "staging");
      expect(api.getRepos).toHaveBeenCalledWith("apiari", "staging");
      expect(api.getWorkers).toHaveBeenCalledWith("apiari", "staging");
    });
  });
});

describe("Mobile auto-select", () => {
  it("auto-selects Main bot on mobile initial load without bot in hash", async () => {
    window.location.hash = "";
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    render(<App />);
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
    Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
  });

  it("uses the bottom mode bar to switch into repos on mobile", async () => {
    window.location.hash = "";
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    render(<App />);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
    expect(screen.queryByText("No repos found")).not.toBeInTheDocument();

    const mobileNav = screen.getByRole("navigation", { name: "Mobile workspace modes" });
    expect(mobileNav).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "Open Repos" }));

    await waitFor(() => {
      expect(screen.getByText("No repos found")).toBeInTheDocument();
    });
    Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
  });
});

describe("WebSocket message dedup", () => {
  it("fetches conversations on WS message event instead of appending directly", async () => {
    // Capture the WS callback so we can simulate events
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");

    // Wait for initial messages to render before clearing mocks
    await waitFor(() => {
      expect(screen.getByText("hello")).toBeInTheDocument();
    });

    // Clear call counts from initial load
    (api.getConversations as ReturnType<typeof vi.fn>).mockClear();

    // Return a new message set to simulate a new message in DB
    const updatedMsgs = [
      { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
      { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
      { id: 3, workspace: "apiari", bot: "Main", role: "user", content: "new message", attachments: null, created_at: new Date().toISOString() },
    ];
    (api.getConversations as ReturnType<typeof vi.fn>).mockResolvedValueOnce(updatedMsgs);

    // Simulate a WS message event for the active bot
    wsCallback({
      type: "message",
      id: 3,
      workspace: "apiari",
      bot: "Main",
      role: "user",
      content: "new message",
      created_at: new Date().toISOString(),
    });

    // Should trigger getConversations fetch (not a direct append)
    await waitFor(() => {
      expect(api.getConversations).toHaveBeenCalledWith("apiari", "Main", 30, undefined);
    });

    // The new message should appear exactly once
    await waitFor(() => {
      expect(screen.getByText("new message")).toBeInTheDocument();
    });
    const matches = screen.getAllByText("new message");
    expect(matches).toHaveLength(1);
  });

  it("keeps websocket message visible even if the immediate refetch is stale", async () => {
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");

    await waitFor(() => {
      expect(screen.getByText("hello")).toBeInTheDocument();
    });

    (api.getConversations as ReturnType<typeof vi.fn>).mockResolvedValueOnce([
      { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
      { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
    ]);

    wsCallback({
      type: "message",
      id: 3,
      workspace: "apiari",
      bot: "Main",
      role: "assistant",
      content: "fresh websocket reply",
      created_at: new Date().toISOString(),
    });

    await waitFor(() => {
      expect(screen.getByText("fresh websocket reply")).toBeInTheDocument();
    });
  });

  it("does not duplicate an assistant reply when streaming status, message, and idle all arrive", async () => {
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");

    (api.getConversations as ReturnType<typeof vi.fn>).mockResolvedValueOnce([
      { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
      { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
      { id: 3, workspace: "apiari", bot: "Main", role: "assistant", content: "streamed reply", attachments: null, created_at: new Date().toISOString() },
    ]);

    wsCallback({
      type: "bot_status",
      workspace: "apiari",
      bot: "Main",
      status: "streaming",
      streaming_content: "streamed reply",
      tool_name: null,
    });
    wsCallback({
      type: "message",
      id: 3,
      workspace: "apiari",
      bot: "Main",
      role: "assistant",
      content: "streamed reply",
      created_at: new Date().toISOString(),
    });
    wsCallback({
      type: "bot_status",
      workspace: "apiari",
      bot: "Main",
      status: "idle",
      streaming_content: "",
      tool_name: null,
    });

    await waitFor(() => {
      expect(screen.getAllByText("streamed reply")).toHaveLength(1);
    });
  });

  it("ignores websocket message events for other workspaces", async () => {
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");
    (api.getConversations as ReturnType<typeof vi.fn>).mockClear();

    wsCallback({
      type: "message",
      id: 99,
      workspace: "mgm",
      bot: "Main",
      role: "assistant",
      content: "ignore me",
      created_at: new Date().toISOString(),
    });

    await new Promise((r) => setTimeout(r, 10));
    expect(screen.queryByText("ignore me")).not.toBeInTheDocument();
    expect(api.getConversations).not.toHaveBeenCalled();
  });

  it("ignores websocket events for the same workspace when the remote does not match", async () => {
    (api.getWorkspaces as ReturnType<typeof vi.fn>).mockResolvedValue([
      { name: "apiari" },
      { name: "apiari", remote: "staging" },
    ]);
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(remoteWorkspaceTab("apiari", "staging")).toBeInTheDocument());
    await user.click(remoteWorkspaceTab("apiari", "staging"));
    await waitFor(() => expect(botButton("Main")).toBeInTheDocument());
    await user.click(botButton("Main"));
    await waitFor(() => expect(api.getConversations).toHaveBeenCalledWith("apiari", "Main", 30, "staging"));
    (api.getConversations as ReturnType<typeof vi.fn>).mockClear();

    wsCallback({
      type: "message",
      id: 50,
      workspace: "apiari",
      remote: "prod",
      bot: "Main",
      role: "assistant",
      content: "wrong remote",
      created_at: new Date().toISOString(),
    });

    await new Promise((r) => setTimeout(r, 10));
    expect(screen.queryByText("wrong remote")).not.toBeInTheDocument();
    expect(api.getConversations).not.toHaveBeenCalled();
  });

});

describe("Realtime side panels", () => {
  it("refreshes followups after a followup websocket event", async () => {
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");
    (api.getFollowups as ReturnType<typeof vi.fn>).mockClear();

    wsCallback({
      type: "followup_fired",
      workspace: "apiari",
      bot: "Main",
    });

    await waitFor(() => {
      expect(api.getFollowups).toHaveBeenCalledWith("apiari", undefined);
    });
  });

  it("refreshes research tasks and shows a system message when research completes", async () => {
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");
    (api.getResearchTasks as ReturnType<typeof vi.fn>).mockClear();

    wsCallback({
      type: "research_update",
      workspace: "apiari",
      bot: "Main",
      status: "complete",
      topic: "monorepo cleanup",
      output_file: "monorepo-cleanup.md",
    });

    await waitFor(() => {
      expect(api.getResearchTasks).toHaveBeenCalledWith("apiari", undefined);
    });
    await waitFor(() => {
      expect(screen.getByText("Research complete: monorepo cleanup → docs/monorepo-cleanup.md")).toBeInTheDocument();
    });
  });
});

describe("Research command", () => {
  it("intercepts /research and calls startResearch API", async () => {
    const user = await renderAndSelectBot("Main");
    await waitFor(() => expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument());

    const textarea = screen.getByPlaceholderText(/Message Main/);
    await user.type(textarea, "/research test topic");
    await user.keyboard("{Meta>}{Enter}{/Meta}");

    await waitFor(() => {
      expect(api.startResearch).toHaveBeenCalledWith("apiari", "test topic", undefined);
    });
    // Should NOT call sendMessage for /research commands
    const sendMock = api.sendMessage as ReturnType<typeof vi.fn>;
    expect(sendMock.mock.calls.some((c: string[]) => c[2]?.startsWith("/research"))).toBe(false);
  });

  it("shows system message after starting research", async () => {
    const user = await renderAndSelectBot("Main");
    await waitFor(() => expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument());

    const textarea = screen.getByPlaceholderText(/Message Main/);
    await user.type(textarea, "/research test topic");
    await user.keyboard("{Meta>}{Enter}{/Meta}");

    await waitFor(() => {
      expect(screen.getByText("Research started: test topic")).toBeInTheDocument();
    });
  });
});

describe("Optimistic chat", () => {
  it("shows the user message immediately after send", async () => {
    const user = await renderAndSelectBot("Main");
    await waitFor(() => expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument());

    const textarea = screen.getByPlaceholderText(/Message Main/);
    await user.type(textarea, "optimistic hello");
    await user.keyboard("{Meta>}{Enter}{/Meta}");

    await waitFor(() => {
      expect(screen.getByText("optimistic hello")).toBeInTheDocument();
    });
  });
});

describe("Worker lifecycle", () => {
  it("loads worker detail when a worker is selected", async () => {
    (api.getWorkers as ReturnType<typeof vi.fn>).mockResolvedValueOnce([
      {
        id: "common-sdk-fix",
        branch: "common/fix-sdk",
        status: "running",
        agent: "codex",
        pr_url: "https://example.com/pr/1",
        pr_title: "Fix SDK mapping",
        description: "Repair shared repo detection",
        elapsed_secs: 125,
        dispatched_by: "Main",
      },
    ]);
    (api.getRepos as ReturnType<typeof vi.fn>).mockResolvedValueOnce([
      {
        name: "common",
        path: "/dev/common",
        has_swarm: true,
        is_clean: false,
        branch: "main",
        workers: [
          {
            id: "common-sdk-fix",
            branch: "common/fix-sdk",
            status: "running",
            agent: "codex",
            pr_url: "https://example.com/pr/1",
            pr_title: "Fix SDK mapping",
            description: "Repair shared repo detection",
            elapsed_secs: 125,
            dispatched_by: "Main",
          },
        ],
      },
    ]);
    (api.getWorkerDetail as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      id: "common-sdk-fix",
      branch: "common/fix-sdk",
      status: "running",
      agent: "codex",
      pr_url: "https://example.com/pr/1",
      pr_title: "Fix SDK mapping",
      description: "Repair shared repo detection",
      elapsed_secs: 125,
      dispatched_by: "Main",
      prompt: "Investigate repo slug resolution",
      output: "Working through daemon/http.rs",
      conversation: [
        { role: "user", content: "Investigate repo slug resolution" },
        { role: "assistant", content: "Found fallback to workspace root." },
      ],
    });

    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("common-sdk-fix")).toBeInTheDocument();
    });

    await user.click(screen.getByText("common-sdk-fix"));

    await waitFor(() => {
      expect(api.getWorkerDetail).toHaveBeenCalledWith("apiari", "common-sdk-fix", undefined);
    });
    await waitFor(() => {
      expect(screen.getByText("Working through daemon/http.rs")).toBeInTheDocument();
    });
  });

  it("refreshes the selected worker status from the worker poll", async () => {
    (api.getWorkers as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "running",
          agent: "codex",
          pr_url: null,
          pr_title: null,
          description: "Repair shared repo detection",
          elapsed_secs: 125,
          dispatched_by: "Main",
        },
      ])
      .mockResolvedValue([
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "waiting",
          agent: "codex",
          pr_url: null,
          pr_title: null,
          description: "Repair shared repo detection",
          elapsed_secs: 130,
          dispatched_by: "Main",
        },
      ]);
    (api.getRepos as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        name: "common",
        path: "/dev/common",
        has_swarm: true,
        is_clean: false,
        branch: "main",
        workers: [
          {
            id: "common-sdk-fix",
            branch: "common/fix-sdk",
            status: "running",
            agent: "codex",
            pr_url: null,
            pr_title: null,
            description: "Repair shared repo detection",
            elapsed_secs: 125,
            dispatched_by: "Main",
          },
        ],
      },
    ]);
    (api.getWorkerDetail as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: "common-sdk-fix",
      branch: "common/fix-sdk",
      status: "running",
      agent: "codex",
      pr_url: null,
      pr_title: null,
      description: "Repair shared repo detection",
      elapsed_secs: 125,
      dispatched_by: "Main",
      prompt: "Investigate repo slug resolution",
      output: "Working through daemon/http.rs",
      conversation: [],
    });

    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("common-sdk-fix")).toBeInTheDocument();
    });
    await user.click(screen.getByText("common-sdk-fix"));

    await waitFor(() => {
      expect(screen.getByText("running · common/fix-sdk")).toBeInTheDocument();
    });

    await new Promise((resolve) => setTimeout(resolve, 5200));
    await waitFor(() => {
      expect(screen.getByText("waiting · common/fix-sdk")).toBeInTheDocument();
    });
  }, 10000);

  it("keeps selected worker detail aligned when the worker transitions into PR review", async () => {
    (api.getWorkers as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "running",
          agent: "codex",
          pr_url: null,
          pr_title: null,
          description: "Repair shared repo detection",
          elapsed_secs: 125,
          dispatched_by: "Main",
        },
      ])
      .mockResolvedValue([
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "waiting",
          agent: "codex",
          pr_url: "https://example.com/pr/1",
          pr_title: "Fix SDK mapping",
          description: "Repair shared repo detection",
          elapsed_secs: 130,
          dispatched_by: "Main",
          review_state: "open",
        },
      ]);
    (api.getRepos as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        name: "common",
        path: "/dev/common",
        has_swarm: true,
        is_clean: false,
        branch: "main",
        workers: [
          {
            id: "common-sdk-fix",
            branch: "common/fix-sdk",
            status: "running",
            agent: "codex",
            pr_url: null,
            pr_title: null,
            description: "Repair shared repo detection",
            elapsed_secs: 125,
            dispatched_by: "Main",
          },
        ],
      },
    ]);
    (api.getWorkerDetail as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({
        id: "common-sdk-fix",
        branch: "common/fix-sdk",
        status: "running",
        agent: "codex",
        pr_url: null,
        pr_title: null,
        description: "Repair shared repo detection",
        elapsed_secs: 125,
        dispatched_by: "Main",
        prompt: "Investigate repo slug resolution",
        output: "Working through daemon/http.rs",
        conversation: [],
      })
      .mockResolvedValue({
        id: "common-sdk-fix",
        branch: "common/fix-sdk",
        status: "waiting",
        agent: "codex",
        pr_url: "https://example.com/pr/1",
        pr_title: "Fix SDK mapping",
        description: "Repair shared repo detection",
        elapsed_secs: 130,
        dispatched_by: "Main",
        review_state: "open",
        prompt: "Investigate repo slug resolution",
        output: "Waiting on review for PR #1",
        conversation: [],
      });

    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => expect(screen.getByText("common-sdk-fix")).toBeInTheDocument());
    await user.click(screen.getByText("common-sdk-fix"));
    await waitFor(() => expect(screen.getByText("Working through daemon/http.rs")).toBeInTheDocument());

    await new Promise((resolve) => setTimeout(resolve, 5200));
    await waitFor(() => {
      expect(screen.getByText("Waiting on review for PR #1")).toBeInTheDocument();
    });
  }, 10000);

  it("keeps selected worker detail aligned through merge completion", async () => {
    (api.getWorkers as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "running",
          agent: "codex",
          pr_url: null,
          pr_title: null,
          description: "Repair shared repo detection",
          elapsed_secs: 125,
          dispatched_by: "Main",
        },
      ])
      .mockResolvedValue([
        {
          id: "common-sdk-fix",
          branch: "common/fix-sdk",
          status: "completed",
          agent: "codex",
          pr_url: "https://example.com/pr/1",
          pr_title: "Fix SDK mapping",
          description: "Repair shared repo detection",
          elapsed_secs: 140,
          dispatched_by: "Main",
          review_state: "merged",
        },
      ]);
    (api.getRepos as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        name: "common",
        path: "/dev/common",
        has_swarm: true,
        is_clean: false,
        branch: "main",
        workers: [
          {
            id: "common-sdk-fix",
            branch: "common/fix-sdk",
            status: "running",
            agent: "codex",
            pr_url: null,
            pr_title: null,
            description: "Repair shared repo detection",
            elapsed_secs: 125,
            dispatched_by: "Main",
          },
        ],
      },
    ]);
    (api.getWorkerDetail as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({
        id: "common-sdk-fix",
        branch: "common/fix-sdk",
        status: "running",
        agent: "codex",
        pr_url: null,
        pr_title: null,
        description: "Repair shared repo detection",
        elapsed_secs: 125,
        dispatched_by: "Main",
        prompt: "Investigate repo slug resolution",
        output: "Working through daemon/http.rs",
        conversation: [],
      })
      .mockResolvedValue({
        id: "common-sdk-fix",
        branch: "common/fix-sdk",
        status: "completed",
        agent: "codex",
        pr_url: "https://example.com/pr/1",
        pr_title: "Fix SDK mapping",
        description: "Repair shared repo detection",
        elapsed_secs: 140,
        dispatched_by: "Main",
        review_state: "merged",
        prompt: "Investigate repo slug resolution",
        output: "Merged into main",
        conversation: [],
      });

    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => expect(screen.getByText("common-sdk-fix")).toBeInTheDocument());
    await user.click(screen.getByText("common-sdk-fix"));
    await waitFor(() => expect(screen.getByText("Working through daemon/http.rs")).toBeInTheDocument());

    await new Promise((resolve) => setTimeout(resolve, 5200));
    await waitFor(() => {
      expect(screen.getByText("Merged into main")).toBeInTheDocument();
      expect(screen.getByText("completed · common/fix-sdk")).toBeInTheDocument();
    });
  }, 10000);
});

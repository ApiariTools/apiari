import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../api");

import App from "../App";
import * as api from "../api";

beforeEach(() => {
  vi.clearAllMocks();
  window.location.hash = "";
});

async function renderAndSelectBot(name = "Main") {
  const user = userEvent.setup();
  render(<App />);
  await waitFor(() => expect(screen.getByText(name)).toBeInTheDocument());
  await user.click(screen.getByText(name));
  return user;
}

describe("App", () => {
  it("renders workspace tabs", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("apiari")).toBeInTheDocument();
      expect(screen.getByText("mgm")).toBeInTheDocument();
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
      expect(screen.getByText("2")).toBeInTheDocument();
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
    await waitFor(() => expect(screen.getByText("Main")).toBeInTheDocument());
    expect(api.markSeen).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.click(screen.getByText("Main"));
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
});

describe("Bot switching", () => {
  it("calls getConversations with new bot", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(screen.getByText("Customer")).toBeInTheDocument());
    await user.click(screen.getByText("Customer"));
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
    await waitFor(() => expect(screen.getByText("Main")).toBeInTheDocument());

    // Select Main bot — triggers initial load, getBotStatus gets stalePromise
    await user.click(screen.getByText("Main"));
    await waitFor(() => expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument());

    // Switch to Customer bot before the delayed Main response resolves
    // This triggers cleanup (cancelled=true) on Main's initial load effect
    await user.click(screen.getByText("Customer"));
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
    await waitFor(() => expect(screen.getByText("mgm")).toBeInTheDocument());
    await user.click(screen.getByText("mgm"));
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
    await waitFor(() => expect(screen.getByText("mgm")).toBeInTheDocument());
    await user.click(screen.getByText("mgm"));
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
    Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
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
      workspace: "apiari",
      bot: "Main",
      role: "user",
      content: "new message",
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

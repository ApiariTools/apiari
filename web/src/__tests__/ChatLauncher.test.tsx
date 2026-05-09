import { render, screen, waitFor, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

// jsdom doesn't implement matchMedia
Object.defineProperty(window, "matchMedia", {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  }),
});

// ── Mock @apiari/api ────────────────────────────────────────────────────────

const mockWsClose = vi.fn();
const mockWsHandler = vi.fn();

vi.mock("@apiari/api", () => ({
  getBots: vi.fn(),
  getUnread: vi.fn(),
  getConversations: vi.fn(),
  sendMessage: vi.fn(),
  cancelBot: vi.fn(),
  markSeen: vi.fn(),
  connectWebSocket: vi.fn((handler: (e: unknown) => void) => {
    mockWsHandler.mockImplementation(handler);
    return { close: mockWsClose };
  }),
}));

import { getBots, getUnread, getConversations, markSeen } from "@apiari/api";
import { ChatLauncher } from "@apiari/chat";

const BOTS = [
  { name: "Main", color: "#f5c542", watch: [] },
  { name: "Research", color: "#b56ef0", watch: [] },
];

function setupMocks({
  unread = {},
}: {
  unread?: Record<string, number>;
} = {}) {
  vi.mocked(getBots).mockResolvedValue(BOTS);
  vi.mocked(getUnread).mockResolvedValue(unread);
  vi.mocked(getConversations).mockResolvedValue([]);
  vi.mocked(markSeen).mockResolvedValue(undefined);
}

describe("ChatLauncher", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockWsClose.mockReset();
    mockWsHandler.mockReset();
  });

  // ── Button rendering ──────────────────────────────────────────────────────

  it("renders launcher button", async () => {
    setupMocks();
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(screen.getByRole("button")).toBeInTheDocument());
  });

  it("shows MessageCircle icon when no active conversations and no unread", async () => {
    setupMocks();
    render(<ChatLauncher workspace="test" />);
    // aria-label defaults to "Open chat" when nothing is open or unread
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Open chat" })).toBeInTheDocument(),
    );
  });

  it("shows unread badge when there are unread messages", async () => {
    setupMocks({ unread: { Research: 3 } });
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(screen.getAllByText("3").length).toBeGreaterThanOrEqual(1));
  });

  it("does not show unread badge when unread is zero", async () => {
    setupMocks({ unread: {} });
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => {
      expect(screen.queryByText("3")).not.toBeInTheDocument();
    });
  });

  // ── Popover ───────────────────────────────────────────────────────────────

  it("shows bot list popover when launcher is clicked", async () => {
    setupMocks();
    const user = userEvent.setup();
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => screen.getByRole("button"));
    await user.click(screen.getByRole("button"));
    await waitFor(() => expect(screen.getByText("Chats")).toBeInTheDocument());
    expect(screen.getByText("Main")).toBeInTheDocument();
    expect(screen.getByText("Research")).toBeInTheDocument();
  });

  it("opens a chat window when bot is selected from the list", async () => {
    setupMocks();
    const user = userEvent.setup();
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => screen.getByRole("button"));
    await user.click(screen.getByRole("button"));
    const botButtons = await screen.findAllByRole("button", { name: /^Main$/ });
    await user.click(botButtons[botButtons.length - 1]);
    await waitFor(() => expect(screen.getAllByText("Main").length).toBeGreaterThan(0));
  });

  // ── Active conversation count ─────────────────────────────────────────────

  it("shows active conversation count after opening a bot", async () => {
    setupMocks();
    const user = userEvent.setup();
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => screen.getByRole("button", { name: "Open chat" }));
    await user.click(screen.getByRole("button", { name: "Open chat" }));
    const botButtons = await screen.findAllByRole("button", { name: /^Main$/ });
    await user.click(botButtons[botButtons.length - 1]);
    // opening Main increments activeConversationCount to 1
    await waitFor(() => expect(screen.getByText("1")).toBeInTheDocument());
  });

  it("decrements active count when a bot window is closed", async () => {
    setupMocks();
    const user = userEvent.setup();
    render(<ChatLauncher workspace="test" />);
    // open Main
    await waitFor(() => screen.getByRole("button", { name: "Open chat" }));
    await user.click(screen.getByRole("button", { name: "Open chat" }));
    const botButtons = await screen.findAllByRole("button", { name: /^Main$/ });
    await user.click(botButtons[botButtons.length - 1]);
    await waitFor(() => expect(screen.getByText("1")).toBeInTheDocument());
    // close the window via the X button
    const closeBtns = screen.getAllByTitle("Close");
    await user.click(closeBtns[0]);
    // count drops back to 0 → icon returns, "1" gone
    await waitFor(() => expect(screen.queryByText("1")).not.toBeInTheDocument());
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Open chat" })).toBeInTheDocument(),
    );
  });

  // ── Unread via WebSocket ──────────────────────────────────────────────────

  it("increments unread badge on incoming message for closed bot", async () => {
    setupMocks({ unread: {} });
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(mockWsHandler).toBeDefined());
    act(() => {
      mockWsHandler({
        type: "message",
        workspace: "test",
        bot: "Main",
        id: 42,
        role: "assistant",
        content: "hey",
        created_at: new Date().toISOString(),
      });
    });
    await waitFor(() => expect(screen.getAllByText("1").length).toBeGreaterThanOrEqual(1));
  });

  it("clears unread count when a bot with unreads is opened", async () => {
    setupMocks({ unread: { Research: 2 } });
    const user = userEvent.setup();
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(screen.getAllByText("2").length).toBeGreaterThanOrEqual(1));
    await user.click(screen.getByRole("button", { name: /unread/i }));
    const researchButtons = await screen.findAllByRole("button", { name: /Research/ });
    await user.click(researchButtons[researchButtons.length - 1]);
    await waitFor(() => {
      expect(markSeen).toHaveBeenCalledWith("test", "Research");
    });
  });

  it("unread_sync event replaces unread state entirely", async () => {
    setupMocks({ unread: { Research: 5 } });
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(screen.getAllByText("5").length).toBeGreaterThanOrEqual(1));
    act(() => {
      mockWsHandler({ type: "unread_sync", workspace: "test", unread: { Research: 1 } });
    });
    await waitFor(() => expect(screen.getAllByText("1").length).toBeGreaterThanOrEqual(1));
    expect(screen.queryByText("5")).not.toBeInTheDocument();
  });

  it("seen event clears unread for the named bot", async () => {
    setupMocks({ unread: { Research: 3 } });
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(screen.getAllByText("3").length).toBeGreaterThanOrEqual(1));
    act(() => {
      mockWsHandler({ type: "seen", workspace: "test", bot: "Research" });
    });
    await waitFor(() => expect(screen.queryByText("3")).not.toBeInTheDocument());
  });

  it("ignores WebSocket events from other workspaces", async () => {
    setupMocks({ unread: {} });
    render(<ChatLauncher workspace="test" />);
    await waitFor(() => expect(mockWsHandler).toBeDefined());
    act(() => {
      mockWsHandler({
        type: "message",
        workspace: "other",
        bot: "Research",
        id: 99,
        role: "assistant",
        content: "hello",
        created_at: new Date().toISOString(),
      });
    });
    // no unread should appear for our workspace
    expect(screen.queryByText("1")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open chat" })).toBeInTheDocument();
  });
});

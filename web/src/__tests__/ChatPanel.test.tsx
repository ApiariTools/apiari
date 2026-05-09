import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";

// Mock Howler globally so it doesn't interfere with other test files
vi.mock("howler", () => {
  class MockHowl {
    play = vi.fn();
    stop = vi.fn();
    unload = vi.fn();
    on = vi.fn();
    _opts: Record<string, unknown>;
    constructor(opts: Record<string, unknown>) {
      this._opts = opts;
      // Fire onload synchronously so enqueueGeneration works in tests
      if (opts.preload && typeof opts.onload === "function") {
        setTimeout(() => (opts.onload as () => void)(), 0);
      }
    }
  }
  return { Howl: MockHowl, Howler: { ctx: null } };
});

// Mock sound cues
const mockStartThinkingCue = vi.fn(() => vi.fn());
const mockPlaySentCue = vi.fn();
const mockPlaySpeakingCue = vi.fn();
vi.mock("../soundCues", () => ({
  playSentCue: () => mockPlaySentCue(),
  startThinkingCue: () => mockStartThinkingCue(),
  playSpeakingCue: () => mockPlaySpeakingCue(),
  setSharedAudioContext: vi.fn(),
}));

import { ChatPanel } from "@apiari/chat";
import type { Message, Followup } from "@apiari/types";

const mockMessages: Message[] = [
  {
    id: 1,
    workspace: "test",
    bot: "Main",
    role: "user",
    content: "hello",
    attachments: null,
    created_at: new Date().toISOString(),
  },
  {
    id: 2,
    workspace: "test",
    bot: "Main",
    role: "assistant",
    content: "Hi there! How can I help?",
    attachments: null,
    created_at: new Date().toISOString(),
  },
];

const defaultProps = {
  bot: "Main",
  messages: mockMessages,
  messagesLoading: false,
  loading: false,
  loadingStatus: undefined,
  streamingContent: "",
  onSend: vi.fn(),
  workerCount: 0,
  onWorkersToggle: vi.fn(),
  onCancel: undefined,
};

describe("ChatPanel", () => {
  it("renders messages", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.getByText("hello")).toBeInTheDocument();
    expect(screen.getByText(/How can I help/)).toBeInTheDocument();
  });

  it("renders bot name in header", () => {
    render(<ChatPanel {...defaultProps} />);
    const mains = screen.getAllByText("Main");
    expect(mains.length).toBeGreaterThanOrEqual(1);
  });

  it("renders user messages with 'You' label", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.getByText("You")).toBeInTheDocument();
  });

  it("renders assistant messages with bot name", () => {
    render(<ChatPanel {...defaultProps} />);
    const botLabels = screen.getAllByText("Main");
    expect(botLabels.length).toBeGreaterThanOrEqual(1);
  });

  it("shows empty state when no messages", () => {
    render(<ChatPanel {...defaultProps} messages={[]} />);
    expect(screen.getByText(/Start a conversation/)).toBeInTheDocument();
  });

  it("shows thinking dots when loading", () => {
    render(<ChatPanel {...defaultProps} loading={true} loadingStatus="Thinking..." />);
    expect(screen.getByText("Thinking...")).toBeInTheDocument();
  });

  it("shows tool name when using a tool", () => {
    render(<ChatPanel {...defaultProps} loading={true} loadingStatus="Using Read..." />);
    expect(screen.getByText("Using Read...")).toBeInTheDocument();
  });

  it("shows streaming content while loading", () => {
    render(<ChatPanel {...defaultProps} loading={true} streamingContent="I'm working on..." />);
    expect(screen.getByText(/working on/)).toBeInTheDocument();
  });

  it("shows stop button when loading with onCancel", () => {
    render(
      <ChatPanel
        {...defaultProps}
        loading={true}
        loadingStatus="Thinking..."
        onCancel={() => {}}
      />,
    );
    expect(screen.getByText("Stop")).toBeInTheDocument();
  });

  it("does not show stop button when not loading", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.queryByText("Stop")).not.toBeInTheDocument();
  });

  it("shows workers button with count", () => {
    render(<ChatPanel {...defaultProps} workerCount={3} />);
    expect(screen.getByText("3 workers")).toBeInTheDocument();
  });

  it("shows 'No workers' when count is 0", () => {
    render(<ChatPanel {...defaultProps} workerCount={0} />);
    expect(screen.getByText("No workers")).toBeInTheDocument();
  });

  it("has a textarea input", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
  });

  it("renders markdown in assistant messages", () => {
    const msgs: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "assistant",
        content: "**bold text**",
        attachments: null,
        created_at: new Date().toISOString(),
      },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    expect(screen.getByText("bold text")).toBeInTheDocument();
  });

  it("renders image attachments", () => {
    const msgs: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "user",
        content: "see this",
        attachments: JSON.stringify([
          { name: "photo.jpg", type: "image/jpeg", dataUrl: "data:image/jpeg;base64,abc" },
        ]),
        created_at: new Date().toISOString(),
      },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    const img = screen.getByAltText("photo.jpg");
    expect(img).toBeInTheDocument();
  });

  it("shows system messages", () => {
    const msgs: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "system",
        content: "Session reset — bot configuration was updated.",
        attachments: null,
        created_at: new Date().toISOString(),
      },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    expect(screen.getByText(/Session reset/)).toBeInTheDocument();
  });

  it("does not show loading and empty state simultaneously", () => {
    render(
      <ChatPanel {...defaultProps} messages={[]} loading={true} loadingStatus="Thinking..." />,
    );
    expect(screen.queryByText(/Start a conversation/)).not.toBeInTheDocument();
    expect(screen.getByText("Thinking...")).toBeInTheDocument();
  });

  it("renders bot description when provided", () => {
    render(<ChatPanel {...defaultProps} botDescription="Monitors errors via Sentry" />);
    expect(screen.getByText("Monitors errors via Sentry")).toBeInTheDocument();
  });

  it("renders provider badge when botProvider is set", () => {
    render(
      <ChatPanel {...defaultProps} botProvider="claude" botModel="claude-sonnet-4-20250514" />,
    );
    const badge = screen.getByText("Claude");
    expect(badge).toBeInTheDocument();
    expect(badge.getAttribute("title")).toBe("claude-sonnet-4-20250514");
    expect(badge.getAttribute("aria-label")).toBe(
      "Provider: claude, model: claude-sonnet-4-20250514",
    );
  });

  it("does not render provider badge when botProvider is not set", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.queryByText("Claude")).not.toBeInTheDocument();
    expect(screen.queryByText("Codex")).not.toBeInTheDocument();
    expect(screen.queryByText("Gemini")).not.toBeInTheDocument();
  });

  it("does not render description element when not provided", () => {
    const { container } = render(<ChatPanel {...defaultProps} />);
    expect(container.querySelector('[class*="headerDescription"]')).toBeNull();
  });

  it("displays time correctly for old timestamps without Z suffix", () => {
    const msgs: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "user",
        content: "old msg",
        attachments: null,
        created_at: "2026-04-26 15:30:00",
      },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    // Should render a valid time string (not "Invalid Date")
    expect(screen.queryByText(/Invalid Date/)).not.toBeInTheDocument();
    expect(screen.getByText("old msg")).toBeInTheDocument();
  });

  it("displays time correctly for ISO timestamps with Z suffix", () => {
    const msgs: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "user",
        content: "new msg",
        attachments: null,
        created_at: "2026-04-26T15:30:00Z",
      },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    expect(screen.queryByText(/Invalid Date/)).not.toBeInTheDocument();
    expect(screen.getByText("new msg")).toBeInTheDocument();
  });

  it("shows play button on assistant messages but not user messages", () => {
    render(<ChatPanel {...defaultProps} />);
    // Only the assistant message should have a play button
    const playButtons = screen.getAllByLabelText("Play");
    expect(playButtons).toHaveLength(1);
  });

  it("clicking play changes button state", async () => {
    const user = userEvent.setup();

    render(<ChatPanel {...defaultProps} messagesLoading={false} />);
    await user.click(screen.getByLabelText("Play"));

    // After clicking play, button should show Loading or Stop
    await waitFor(() => {
      const play = screen.queryByLabelText("Play");
      expect(play).not.toBeInTheDocument();
    });
  });

  it("queues messages sent while loading and sends after loading completes", () => {
    const onSend = vi.fn();
    const { rerender } = render(
      <ChatPanel {...defaultProps} onSend={onSend} loading={true} loadingStatus="Thinking..." />,
    );

    // Send a message while bot is loading — should be queued, not sent
    const textarea = screen.getByPlaceholderText(/Message Main/);
    (textarea as HTMLTextAreaElement).value = "queued msg";
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByText("1 message queued")).toBeInTheDocument();

    // Bot finishes — queued message should be sent
    rerender(<ChatPanel {...defaultProps} onSend={onSend} loading={false} />);
    expect(onSend).toHaveBeenCalledWith("queued msg", undefined);
  });

  it("input is not disabled while bot is responding", () => {
    render(<ChatPanel {...defaultProps} loading={true} loadingStatus="Thinking..." />);
    const textarea = screen.getByPlaceholderText(/Message Main/) as HTMLTextAreaElement;
    expect(textarea.readOnly).toBe(false);
  });

  it("clicking active play button stops and restores play", async () => {
    const user = userEvent.setup();

    render(<ChatPanel {...defaultProps} messagesLoading={false} />);
    await user.click(screen.getByLabelText("Play"));

    await waitFor(() => {
      expect(screen.queryByLabelText("Play")).not.toBeInTheDocument();
    });

    // Click the active button (Loading or Stop) to cancel
    const btn = screen.queryByLabelText("Loading") || screen.queryByLabelText("Stop");
    if (btn) {
      await user.click(btn);
      await waitFor(() => {
        expect(screen.getByLabelText("Play")).toBeInTheDocument();
      });
    }
  });

  // ── Sound cue tests ──

  it("voice mode bypasses message queue and sends immediately while loading", async () => {
    const onSend = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(<ChatPanel {...defaultProps} onSend={onSend} loading={false} />);

    // Enable voice mode
    await user.click(screen.getByLabelText("Enter voice mode"));

    // Now set loading=true (bot is responding)
    rerender(
      <ChatPanel {...defaultProps} onSend={onSend} loading={true} loadingStatus="Thinking..." />,
    );

    // Send a message while loading — in voice mode it should NOT be queued
    const textarea = screen.getByPlaceholderText(/Message Main/);
    (textarea as HTMLTextAreaElement).value = "voice msg";
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    expect(onSend).toHaveBeenCalledWith("voice msg", undefined);
    expect(screen.queryByText(/queued/)).not.toBeInTheDocument();
  });

  it("renders fired followups inline in message feed", () => {
    const now = new Date();
    const msgs: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "user",
        content: "check PR",
        attachments: null,
        created_at: new Date(now.getTime() - 5000).toISOString(),
      },
      {
        id: 2,
        workspace: "test",
        bot: "Main",
        role: "assistant",
        content: "I'll check it",
        attachments: null,
        created_at: new Date(now.getTime() - 3000).toISOString(),
      },
    ];
    const followups: Followup[] = [
      {
        id: "fu_1",
        workspace: "test",
        bot: "Main",
        action: "Check PR status",
        created_at: new Date(now.getTime() - 4000).toISOString(),
        fires_at: new Date(now.getTime() - 3500).toISOString(),
        status: "fired",
      },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} followups={followups} workspace="test" />);
    // Fired followup should render inline with "Follow-up triggered" label
    expect(screen.getByText("Follow-up triggered")).toBeInTheDocument();
    expect(screen.getByText(/Check PR status/)).toBeInTheDocument();
  });

  it("does not render cancelled followups", () => {
    const followups: Followup[] = [
      {
        id: "fu_2",
        workspace: "test",
        bot: "Main",
        action: "Cancelled action",
        created_at: new Date().toISOString(),
        fires_at: new Date().toISOString(),
        status: "cancelled",
      },
    ];
    render(<ChatPanel {...defaultProps} followups={followups} workspace="test" />);
    expect(screen.queryByText(/Cancelled action/)).not.toBeInTheDocument();
  });

  it("renders pending followups at bottom, not inline", () => {
    const followups: Followup[] = [
      {
        id: "fu_3",
        workspace: "test",
        bot: "Main",
        action: "Future check",
        created_at: new Date().toISOString(),
        fires_at: new Date(Date.now() + 60000).toISOString(),
        status: "pending",
      },
    ];
    render(<ChatPanel {...defaultProps} followups={followups} workspace="test" />);
    expect(screen.getByText(/Follow-up in/)).toBeInTheDocument();
    expect(screen.getByText(/Future check/)).toBeInTheDocument();
    expect(screen.getByText("Cancel")).toBeInTheDocument();
  });

  it("does not play sound cues when voice mode is off", () => {
    mockPlaySentCue.mockClear();
    mockStartThinkingCue.mockClear();

    const msgs1: Message[] = [
      {
        id: 1,
        workspace: "test",
        bot: "Main",
        role: "user",
        content: "hello",
        attachments: null,
        created_at: new Date().toISOString(),
      },
    ];
    const { rerender } = render(<ChatPanel {...defaultProps} messages={msgs1} loading={true} />);

    // Add a user message — should NOT trigger sent cue since voice mode is off
    const msgs2: Message[] = [
      ...msgs1,
      {
        id: 2,
        workspace: "test",
        bot: "Main",
        role: "user",
        content: "world",
        attachments: null,
        created_at: new Date().toISOString(),
      },
    ];
    rerender(<ChatPanel {...defaultProps} messages={msgs2} loading={true} />);

    expect(mockPlaySentCue).not.toHaveBeenCalled();
    expect(mockStartThinkingCue).not.toHaveBeenCalled();
  });

  it("uses auto scrolling for streaming updates to avoid jumpy smooth restarts", async () => {
    const scrollTo = vi.fn();
    Element.prototype.scrollTo = scrollTo;

    const { rerender } = render(
      <ChatPanel
        {...defaultProps}
        loading
        streamingContent="Streaming..."
        loadingStatus="Thinking..."
      />,
    );
    await waitFor(() => expect(scrollTo).toHaveBeenCalled());

    scrollTo.mockClear();
    rerender(
      <ChatPanel
        {...defaultProps}
        loading
        streamingContent="Streaming more..."
        loadingStatus="Still thinking..."
      />,
    );

    await waitFor(() => {
      expect(scrollTo).toHaveBeenCalledWith(expect.objectContaining({ behavior: "auto" }));
    });
  });

  it("keeps smooth scrolling for discrete message additions", async () => {
    const scrollTo = vi.fn();
    Element.prototype.scrollTo = scrollTo;

    const { rerender } = render(<ChatPanel {...defaultProps} />);
    await waitFor(() => expect(scrollTo).toHaveBeenCalled());

    scrollTo.mockClear();
    rerender(
      <ChatPanel
        {...defaultProps}
        messages={[
          ...mockMessages,
          {
            id: 3,
            workspace: "test",
            bot: "Main",
            role: "assistant",
            content: "Another reply",
            attachments: null,
            created_at: new Date().toISOString(),
          },
        ]}
      />,
    );

    await waitFor(() => {
      expect(scrollTo).toHaveBeenCalledWith(expect.objectContaining({ behavior: "smooth" }));
    });
  });
});

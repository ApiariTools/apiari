import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import ContextBotPanel from "../components/ContextBot/ContextBotPanel";
import type { ContextBotSession } from "../types";

vi.mock("react-markdown", () => ({
  default: ({ children }: { children: string }) => <span>{children}</span>,
}));

const makeSession = (overrides: Partial<ContextBotSession> = {}): ContextBotSession => ({
  id: "session-1",
  context: {
    view: "worker_detail",
    entity_id: "w-abc",
    entity_snapshot: { state: "running", goal: "Add rate limiting" },
  },
  title: "Viewing: fix-auth",
  model: "claude-sonnet-4-6",
  messages: [],
  minimized: false,
  loading: false,
  ...overrides,
});

const defaultProps = {
  onSend: vi.fn(),
  onChangeModel: vi.fn(),
  onMinimize: vi.fn(),
  onClose: vi.fn(),
};

describe("ContextBotPanel", () => {
  it("renders the session title in the header", () => {
    render(<ContextBotPanel session={makeSession()} {...defaultProps} />);
    expect(screen.getByTestId("panel-title")).toHaveTextContent("Viewing: fix-auth");
  });

  it("shows user and assistant messages", () => {
    const session = makeSession({
      messages: [
        { role: "user", content: "What is wrong?", timestamp: "2026-05-04T10:00:00Z" },
        { role: "assistant", content: "The tests fail because...", timestamp: "2026-05-04T10:01:00Z" },
      ],
    });
    render(<ContextBotPanel session={session} {...defaultProps} />);
    expect(screen.getByText("What is wrong?")).toBeInTheDocument();
    expect(screen.getByText("The tests fail because...")).toBeInTheDocument();
    expect(screen.getByText("You")).toBeInTheDocument();
  });

  it("calls onMinimize with session id when minimize button is clicked", () => {
    const onMinimize = vi.fn();
    render(<ContextBotPanel session={makeSession()} {...defaultProps} onMinimize={onMinimize} />);
    fireEvent.click(screen.getByTestId("minimize-btn"));
    expect(onMinimize).toHaveBeenCalledWith("session-1");
  });

  it("requires confirmation before calling onClose", () => {
    const onClose = vi.fn();
    render(<ContextBotPanel session={makeSession()} {...defaultProps} onClose={onClose} />);

    // First click shows confirm — onClose NOT called yet
    fireEvent.click(screen.getByTestId("close-btn"));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByText("End chat?")).toBeInTheDocument();

    // Confirm — onClose called
    fireEvent.click(screen.getByTestId("confirm-end-btn"));
    expect(onClose).toHaveBeenCalledWith("session-1");
  });

  it("can cancel the end confirmation", () => {
    const onClose = vi.fn();
    render(<ContextBotPanel session={makeSession()} {...defaultProps} onClose={onClose} />);

    fireEvent.click(screen.getByTestId("close-btn"));
    expect(screen.getByText("End chat?")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Cancel"));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByText("End chat?")).not.toBeInTheDocument();
  });

  it("calls onSend with session id and message when send button is clicked", () => {
    const onSend = vi.fn();
    render(<ContextBotPanel session={makeSession()} {...defaultProps} onSend={onSend} />);
    const input = screen.getByTestId("chat-input");
    fireEvent.change(input, { target: { value: "What is happening?" } });
    fireEvent.click(screen.getByTestId("send-btn"));
    expect(onSend).toHaveBeenCalledWith("session-1", "What is happening?");
  });

  it("does not call onSend when message is empty", () => {
    const onSend = vi.fn();
    render(<ContextBotPanel session={makeSession()} {...defaultProps} onSend={onSend} />);
    fireEvent.click(screen.getByTestId("send-btn"));
    expect(onSend).not.toHaveBeenCalled();
  });

  it("shows loading dots when loading is true", () => {
    render(<ContextBotPanel session={makeSession({ loading: true })} {...defaultProps} />);
    expect(screen.getByTestId("loading-dots")).toBeInTheDocument();
  });

  it("hides messages area when minimized", () => {
    render(<ContextBotPanel session={makeSession({ minimized: true })} {...defaultProps} />);
    expect(screen.queryByTestId("messages-area")).not.toBeInTheDocument();
    expect(screen.queryByTestId("send-btn")).not.toBeInTheDocument();
  });

  it("shows empty hint when no messages and not loading", () => {
    render(<ContextBotPanel session={makeSession({ messages: [], loading: false })} {...defaultProps} />);
    expect(screen.getByText("Ask anything about what you're viewing.")).toBeInTheDocument();
  });

  it("disables send button when loading", () => {
    render(<ContextBotPanel session={makeSession({ loading: true })} {...defaultProps} />);
    expect(screen.getByTestId("send-btn")).toBeDisabled();
  });
});

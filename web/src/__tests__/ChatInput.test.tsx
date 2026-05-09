import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { ChatInput } from "@apiari/chat";

describe("ChatInput", () => {
  it("renders textarea with placeholder", () => {
    render(<ChatInput placeholder="Type here..." onSend={vi.fn()} />);
    expect(screen.getByPlaceholderText("Type here...")).toBeInTheDocument();
  });

  it("shows attach button when showAttachments is true (default)", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Attach file" })).toBeInTheDocument();
  });

  it("hides attach button when showAttachments is false", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} showAttachments={false} />);
    expect(screen.queryByRole("button", { name: "Attach file" })).not.toBeInTheDocument();
  });

  it("calls onSend with text on Cmd+Enter", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} />);
    const textarea = screen.getByPlaceholderText("msg");
    fireEvent.input(textarea, { target: { value: "hello" } });
    // Manually set value since fireEvent.input doesn't update uncontrolled textarea
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSend).toHaveBeenCalledWith("hello", undefined);
  });

  it("calls onSend with text on Ctrl+Enter", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} />);
    const textarea = screen.getByPlaceholderText("msg");
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter", ctrlKey: true });
    expect(onSend).toHaveBeenCalledWith("hello", undefined);
  });

  it("does not send on bare Enter", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} />);
    const textarea = screen.getByPlaceholderText("msg");
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("sends even when disabled (queue handled by parent)", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} disabled />);
    const textarea = screen.getByPlaceholderText("msg");
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSend).toHaveBeenCalledWith("hello", undefined);
  });

  it("shows mic button by default (no text)", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Record voice" })).toBeInTheDocument();
  });

  it("shows queue indicator when queueCount > 0", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} queueCount={2} />);
    expect(screen.getByText("2 messages queued")).toBeInTheDocument();
  });

  it("hides queue indicator when queueCount is 0", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} queueCount={0} />);
    expect(screen.queryByText(/queued/)).not.toBeInTheDocument();
  });
});

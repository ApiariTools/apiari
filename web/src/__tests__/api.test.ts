import { describe, it, expect, vi, beforeEach } from "vitest";
import { getBotStatus, getConversations, markSeen, sendMessage } from "@apiari/api";

describe("chat API path encoding", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ ok: true, status: "idle", streaming_content: "", tool_name: null }),
      }),
    );
  });

  it("encodes bot names with spaces for chat endpoints", async () => {
    const fetchMock = vi.mocked(fetch);

    await getConversations("apiari", "Main Bot");
    await getBotStatus("apiari", "Main Bot");
    await markSeen("apiari", "Main Bot");
    await sendMessage("apiari", "Main Bot", "hello");

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/workspaces/apiari/conversations/Main%20Bot");
    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/workspaces/apiari/bots/Main%20Bot/status");
    expect(fetchMock.mock.calls[2]?.[0]).toBe("/api/workspaces/apiari/seen/Main%20Bot");
    expect(fetchMock.mock.calls[3]?.[0]).toBe("/api/workspaces/apiari/chat/Main%20Bot");
  });
});

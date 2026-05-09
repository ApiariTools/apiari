import { describe, it, expect, beforeEach } from "vitest";

describe("mockServer", () => {
  beforeEach(() => {
    // patch window globals so installMockServer doesn't crash in jsdom
    (globalThis as Record<string, unknown>).WebSocket = class {
      close() {}
    };
  });

  it("seeds Research unread=2 on first load", async () => {
    const { installMockServer } = await import("../../packages/chat/demo/mockServer");
    installMockServer();

    const res = await fetch("/api/workspaces/demo/unread");
    const data = await res.json();
    expect(data).toEqual({ Research: 2 });
  });

  it("reset clears all unreads to zero", async () => {
    const { installMockServer, resetMockStore } =
      await import("../../packages/chat/demo/mockServer");
    installMockServer();

    // sanity: starts with Research=2
    const before = await (await fetch("/api/workspaces/demo/unread")).json();
    expect(before).toEqual({ Research: 2 });

    resetMockStore();

    const after = await (await fetch("/api/workspaces/demo/unread")).json();
    expect(after).toEqual({ Research: 0 });
  });

  it("reset zeroes unreads even after additional messages arrived", async () => {
    const { installMockServer, resetMockStore, triggerIncomingMessage } =
      await import("../../packages/chat/demo/mockServer");
    installMockServer();
    triggerIncomingMessage("demo", "Main");

    resetMockStore();

    const after = await (await fetch("/api/workspaces/demo/unread")).json();
    expect(after).toEqual({ Research: 0, Main: 0 });
  });

  it("triggerIncomingMessage emits a WS message event", async () => {
    const { installMockServer, resetMockStore, triggerIncomingMessage } =
      await import("../../packages/chat/demo/mockServer");
    resetMockStore();
    installMockServer();

    const ws = new WebSocket("ws://localhost/ws");
    const received: MessageEvent[] = [];
    ws.onmessage = (e) => received.push(e);

    await new Promise((r) => setTimeout(r, 50));

    triggerIncomingMessage("demo", "Main");

    await new Promise((r) => setTimeout(r, 0));

    expect(received.length).toBeGreaterThan(0);
    const msg = JSON.parse(received[received.length - 1].data);
    expect(msg.type).toBe("message");
    expect(msg.bot).toBe("Main");
  });
});

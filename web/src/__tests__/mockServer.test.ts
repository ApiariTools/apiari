import { describe, it, expect, beforeEach } from "vitest";

// Import the mock server internals by side-loading the module fresh each test
// We use dynamic import + vi.resetModules to get a clean module state per test.

describe("mockServer", () => {
  beforeEach(() => {
    // patch window globals so installMockServer doesn't crash in jsdom
    (globalThis as Record<string, unknown>).WebSocket = class {
      close() {}
    };
  });

  it("seeds Research unread=2 on fresh install", async () => {
    const { installMockServer, resetMockStore } =
      await import("../../packages/chat/demo/mockServer");
    resetMockStore();
    installMockServer();

    const res = await fetch("/api/workspaces/demo/unread");
    const data = await res.json();
    expect(data).toEqual({ Research: 2 });
  });

  it("getUnread restores initial unread after resetMockStore", async () => {
    const { installMockServer, resetMockStore } =
      await import("../../packages/chat/demo/mockServer");
    installMockServer();
    resetMockStore();

    const res = await fetch("/api/workspaces/demo/unread");
    const data = await res.json();
    expect(data).toEqual({ Research: 2 });
  });

  it("reset restores Research=2 even after markSeen cleared it", async () => {
    const { installMockServer, resetMockStore } =
      await import("../../packages/chat/demo/mockServer");
    installMockServer();
    resetMockStore();

    // Simulate markSeen clearing Research unread
    await fetch("/api/workspaces/demo/seen/Research", { method: "POST" });
    const afterSeen = await (await fetch("/api/workspaces/demo/unread")).json();
    expect(afterSeen).toEqual({ Research: 0 });

    // Reset should restore Research=2
    resetMockStore();
    const afterReset = await (await fetch("/api/workspaces/demo/unread")).json();
    expect(afterReset).toEqual({ Research: 2 });
  });

  it("triggerIncomingMessage emits a WS message event", async () => {
    const { installMockServer, resetMockStore, triggerIncomingMessage } =
      await import("../../packages/chat/demo/mockServer");
    resetMockStore();
    installMockServer();

    // Connect a fake WebSocket
    const ws = new WebSocket("ws://localhost/ws");
    const received: MessageEvent[] = [];
    ws.onmessage = (e) => received.push(e);

    // Wait for ws open
    await new Promise((r) => setTimeout(r, 50));

    triggerIncomingMessage("demo", "Main");

    // Allow microtasks to flush
    await new Promise((r) => setTimeout(r, 0));

    expect(received.length).toBeGreaterThan(0);
    const msg = JSON.parse(received[received.length - 1].data);
    expect(msg.type).toBe("message");
    expect(msg.bot).toBe("Main");
  });
});

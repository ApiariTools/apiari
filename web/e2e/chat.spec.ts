import { expect, test } from "@playwright/test";

declare global {
  interface Window {
    __pushWsEvent?: (event: unknown) => void;
    __mockWs?: {
      onmessage?: ((event: MessageEvent) => void) | null;
      onclose?: ((event: CloseEvent) => void) | null;
    };
  }
}

test("chat keeps sent and websocket-delivered messages visible", async ({ page }) => {
  const conversations = [
    {
      id: 1,
      workspace: "apiari",
      bot: "Main",
      role: "assistant",
      content: "Existing assistant reply",
      attachments: null,
      created_at: "2026-05-02T00:00:00.000Z",
    },
  ];
  let nextId = 2;

  await page.addInitScript(() => {
    class MockWebSocket {
      url: string;
      readyState = 1;
      onopen: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;
      onclose: ((event: CloseEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;

      constructor(url: string) {
        this.url = url;
        window.__mockWs = this;
        window.setTimeout(() => this.onopen?.(new Event("open")), 0);
      }

      send() {}

      close() {
        this.readyState = 3;
        this.onclose?.(new CloseEvent("close"));
      }
    }

    window.__pushWsEvent = (event: unknown) => {
      const ws = window.__mockWs;
      ws?.onmessage?.(
        new MessageEvent("message", {
          data: JSON.stringify(event),
        }),
      );
    };

    Object.defineProperty(window, "WebSocket", {
      configurable: true,
      writable: true,
      value: MockWebSocket,
    });
  });

  await page.route("**/api/**", async (route) => {
    const req = route.request();
    const url = new URL(req.url());
    const path = url.pathname;
    const method = req.method();

    const json = async (body: unknown) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    };

    if (method === "GET" && path === "/api/workspaces") {
      return json([{ name: "apiari" }]);
    }
    if (method === "GET" && path === "/api/workspaces/apiari/bots") {
      return json([
        { name: "Main", provider: "claude", model: "sonnet", watch: [] },
        { name: "Gemini", provider: "gemini", model: "gemini-2.5-flash", watch: [] },
      ]);
    }
    if (method === "GET" && path === "/api/workspaces/apiari/workers") {
      return json([]);
    }
    if (method === "GET" && path === "/api/workspaces/apiari/repos") {
      return json([]);
    }
    if (method === "GET" && path === "/api/workspaces/apiari/unread") {
      return json({});
    }
    if (method === "GET" && path === "/api/workspaces/apiari/research") {
      return json([]);
    }
    if (method === "GET" && path === "/api/workspaces/apiari/followups") {
      return json([]);
    }
    if (method === "GET" && path === "/api/usage") {
      return json({ installed: false, providers: [], updated_at: null });
    }
    if (method === "GET" && path === "/api/workspaces/apiari/conversations/Main") {
      return json(conversations);
    }
    if (method === "GET" && path === "/api/workspaces/apiari/bots/Main/status") {
      return json({ status: "idle", streaming_content: "", tool_name: null });
    }
    if (method === "POST" && path === "/api/workspaces/apiari/seen/Main") {
      return json({ ok: true });
    }
    if (method === "POST" && path === "/api/workspaces/apiari/chat/Main") {
      const body = req.postDataJSON() as { message: string };
      const userMessage = {
        id: nextId++,
        workspace: "apiari",
        bot: "Main",
        role: "user",
        content: body.message,
        attachments: null,
        created_at: "2026-05-02T00:00:01.000Z",
      };
      const assistantMessage = {
        id: nextId++,
        workspace: "apiari",
        bot: "Main",
        role: "assistant",
        content: "Mock assistant reply",
        attachments: null,
        created_at: "2026-05-02T00:00:02.000Z",
      };
      conversations.push(userMessage, assistantMessage);
      setTimeout(() => {
        void page.evaluate((event) => window.__pushWsEvent?.(event), {
          type: "message",
          ...assistantMessage,
        });
      }, 50);
      return json({ ok: true });
    }

    throw new Error(`Unhandled API request in Playwright test: ${method} ${path}`);
  });

  await page.goto("/");
  await page.getByRole("button", { name: "Main" }).click();

  await expect(page.getByText("Existing assistant reply")).toBeVisible();

  await page.getByPlaceholder("Message Main...").fill("playwright smoke");
  await page.getByRole("button", { name: "Send message" }).click();

  await expect(page.getByText("playwright smoke")).toBeVisible();
  await expect(page.getByText("Mock assistant reply")).toBeVisible();
  await expect(page.getByText("playwright smoke")).toBeVisible();
});

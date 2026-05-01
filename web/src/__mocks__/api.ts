import { vi } from "vitest";

export const getWorkspaces = vi.fn().mockResolvedValue([
  { name: "apiari" },
  { name: "mgm" },
]);

export const getBots = vi.fn().mockResolvedValue([
  { name: "Main", color: "#f5c542", role: "Assistant", watch: [] },
  { name: "Customer", color: "#e85555", role: "Customer bot", watch: ["sentry"] },
]);

export const getWorkers = vi.fn().mockResolvedValue([]);

export const getRepos = vi.fn().mockResolvedValue([]);

export const getConversations = vi.fn().mockResolvedValue([
  { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
  { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
]);

export const getBotStatus = vi.fn().mockResolvedValue({
  status: "idle",
  streaming_content: "",
  tool_name: null,
});

export const getUnread = vi.fn().mockResolvedValue({ Customer: 2 });
export const markSeen = vi.fn().mockResolvedValue(undefined);
export const sendMessage = vi.fn().mockResolvedValue({ ok: true });
export const cancelBot = vi.fn().mockResolvedValue({ ok: true });
export const getWorkerDetail = vi.fn().mockResolvedValue(null);
export const sendWorkerMessage = vi.fn().mockResolvedValue({ ok: true });
export const getUsage = vi.fn().mockResolvedValue({ installed: false, providers: [], updated_at: null });
export const getDocs = vi.fn().mockResolvedValue([
  { name: "architecture.md", title: "Architecture", updated_at: "2026-01-01T00:00:00Z" },
  { name: "setup.md", title: "Setup Guide", updated_at: "2026-01-01T00:00:00Z" },
]);
export const getDoc = vi.fn().mockResolvedValue({
  name: "architecture.md",
  title: "Architecture",
  content: "# Architecture\n\nDetails here",
  updated_at: "2026-01-01T00:00:00Z",
});
export const saveDoc = vi.fn().mockResolvedValue({ ok: true });
export const deleteDoc = vi.fn().mockResolvedValue({ ok: true });
export const getFollowups = vi.fn().mockResolvedValue([]);
export const cancelFollowup = vi.fn().mockResolvedValue({ ok: true });
export const getResearchTasks = vi.fn().mockResolvedValue([]);
export const startResearch = vi.fn().mockResolvedValue({ id: "research-1", topic: "test", status: "running" });
export const connectWebSocket = vi.fn().mockReturnValue({ close: vi.fn() });

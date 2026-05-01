import "@testing-library/jest-dom/vitest";

// jsdom doesn't implement ResizeObserver
class MockResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as unknown as Record<string, unknown>).ResizeObserver = MockResizeObserver;

// jsdom doesn't implement scrollIntoView
Element.prototype.scrollIntoView = () => {};

// Mock WebSocket
class MockWebSocket {
  onmessage: ((e: MessageEvent) => void) | null = null;
  onclose: (() => void) | null = null;
  close() {}
  send() {}
}
(globalThis as unknown as Record<string, unknown>).WebSocket = MockWebSocket;

// Mock speechSynthesis (not available in jsdom)
(globalThis as unknown as Record<string, unknown>).speechSynthesis = {
  speak: () => {},
  cancel: () => {},
  getVoices: () => [],
};

# @apiari/chat

Embeddable chat launcher widget for the apiari platform. Drop-in floating button with multi-bot
popover, real-time streaming, and minimizable windows.

## Usage

```ts
import { ChatLauncher } from "@apiari/chat";

<ChatLauncher workspace="my-workspace" position="right" theme={{ accent: "#4a9eff" }} />
```

Requires a running apiari daemon at `localhost:4200` (or the port set by `VITE_API_PORT`).

## Key exports

| Export | Description |
|---|---|
| `ChatLauncher` | Floating launcher button + multi-window chat shell |
| `ChatPanel` | Full chat view (messages + input + streaming) — used inside windows |
| `ChatInput` | Standalone textarea with send, voice, and attachment support |
| `useChatState` | React hook with all launcher state (bots, open windows, unread) |

## Launcher button states

| Condition | Button shows |
|---|---|
| No active conversations, no unread | MessageCircle icon |
| Active conversations (≥ 1 bot opened) | Count of open conversations |
| Unread messages | Red badge overlaid on button (in addition to the above) |
| Popover open | X icon |

"Active conversation count" tracks bots you've opened in the current session. It goes up when you
open a bot and back down when you close the window.

## Demo app (mock mode)

The `demo/` folder is a standalone Vite app that runs the ChatLauncher without a real daemon.
It intercepts all `fetch` and `WebSocket` calls with a fake server (`mockServer.ts`).

**Run it:**

```bash
cd web
npm run chat:demo       # starts Vite dev server on port 5174
```

Or open `http://localhost:5174/` after it starts.

**Live mode** (requires daemon running on `:4200`):

```
http://localhost:5174/?mock=false
```

### What the demo does

- Boots 3 mock bots: `Main` (gold), `Code` (blue), `Research` (purple)
- Seeds `Research` with 2 unread messages on load
- Seeds `Main` and `Code` with a short conversation history
- Sidebar controls let you trigger events without typing:
  - **Trigger unread** — push an instant assistant message to any bot
  - **Trigger streaming response** — run a full thinking → streaming → idle sequence
  - **Trigger tool use** — freeze a bot in tool-use/thinking state
  - **Reset to idle** — snap a bot back to idle
  - **Reset everything** — clear all messages/unreads, cancel in-flight responses, remount the launcher

### Demo file map

```
demo/
  main.tsx         — Entry point; installs mock server before React renders
  App.tsx          — Root component: debug bar, sidebar controls, ChatLauncher
  mockServer.ts    — Fake fetch + WebSocket implementation
```

### Mock server internals

- `installMockServer()` replaces `window.fetch` and `window.WebSocket` with fakes
- `FakeWebSocket` tracks all active connections in a `Set`; `broadcast()` fans out to all
- On WebSocket open, an `unread_sync` event is sent so clients get current state immediately
- `generation` counter: incremented on reset so in-flight `simulateResponse` calls check
  `generation === myGen` before emitting — prevents stale events after reset
- `freshUnread()` deep-copies `INITIAL_UNREAD` so `markSeen` mutations never corrupt the seed data
- `seen` event broadcast on `POST /seen/:bot` so all WS listeners can zero out the right bot

## Playwright tests

```bash
cd web
npx playwright test --config=playwright.chat.config.ts
```

Tests run against the demo in headless Chromium (no daemon needed). The config starts
`npm run chat:demo` automatically if port 5174 is not already listening.

Spec files in `e2e/`:
- `chat-demo.spec.ts` — launcher button, badge, popover, active count, unread flow
- `reset-check.spec.ts` — full reset: opens window, triggers streaming, resets, asserts clean state

## Known issues

### No error state if daemon is unreachable in live mode

When running with `?mock=false` and the daemon is not on `:4200`, all API calls silently fail.
There is no user-visible error or retry UI — the launcher renders with no bots and no unread.

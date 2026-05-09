# @apiari/api

All API calls and WebSocket client for the apiari platform. Typed against `@apiari/types`.

## Usage

```ts
import { getConversations, sendMessage, connectWebSocket, getBots } from "@apiari/api";
```

## Function Groups

- **Workspaces / Bots** — `getWorkspaces`, `getBots`, `getBotStatus`, `cancelBot`
- **Conversations** — `getConversations` (all bots or per-bot), `sendMessage`, `getUnread`, `markSeen`
- **Workers (v1)** — `getWorkers`, `getWorkerDetail`, `getWorkerEnvironment`, `sendWorkerMessage`, `promoteWorker`, `redispatchWorker`, `closeWorker`
- **Workers (v2)** — `getWorkersV2`, `getWorkerDetailV2`, `sendWorkerMessageV2`, `requeueWorkerV2`, `createWorkerV2`, `getWorkerDiff`
- **Reviews** — `listWorkerReviews`
- **Repos / Tasks / Docs / Signals** — `getRepos`, `getTasks`, `getDocs`, `saveDoc`, `deleteDoc`, `getSignals`
- **Auto Bots** — `getAutoBots`, `getAutoBotDetail`, `updateAutoBot`, `triggerAutoBot`, `cancelAutoBot`
- **Context Bots** — `getContextBotSessions`, `deleteContextBotSession`
- **Dashboard** — `getDashboardWidgets`, `deleteDashboardWidget`
- **Research** — `startResearch`, `getResearchTasks`
- **Misc** — `connectWebSocket`, `transcribe`, `getUsage`, `getProviderCapabilities`, `getBotDebugData`, `cancelFollowup`, `getFollowups`

## WebSocket

`connectWebSocket(handler)` returns a `ManagedWebSocket` with a `.close()` method.
Auto-reconnects after 3 seconds on disconnect. Events match the server broadcast shape:

```ts
{ type: "message",     workspace, bot, role, content }
{ type: "bot_status",  workspace, bot, status, tool_name, streaming_content }
{ type: "worker_update", workspace, worker_id, status }
```

## Remote Workspaces

Most functions accept an optional `remote?: string` parameter. When set, requests are
routed through `/remotes/{remote}/workspaces/{workspace}/...` instead of `/workspaces/...`.

## Rules

- Only one dependency: `@apiari/types` (for request/response types).
- All fetch calls go through the internal `get<T>` / `post<T>` helpers — don't add raw `fetch` calls.
- Base URL is `/api` (relative) — works with any Vite proxy or same-origin server.
- Mock at `src/__mocks__/index.ts` — imported automatically by Vitest via the `__mocks__` convention.

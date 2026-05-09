# @apiari/chat

Chat UI package for the apiari platform. Provides `ChatPanel`, `ChatInput`, voice utilities,
and sound cues. Has a standalone dev server for isolated development.

## Usage

```ts
import { ChatPanel, ChatInput, FollowupCard } from "@apiari/chat";
```

## Exports

| Export | Description |
|--------|-------------|
| `ChatPanel` | Full chat view: message list + input + streaming + followups |
| `ChatInput` | Standalone textarea with send button, voice input, attachments |
| `FollowupCard` | Card rendering a single followup suggestion |
| `FollowupIndicator` | Compact badge showing pending followup count |
| `Attachment`, `VoiceState` | Types for ChatInput props |
| `splitSentences`, `stripMarkdown`, `cleanTranscription`, `matchConfirmation` | Voice text helpers |
| `float32ToWav`, `transcribe` | Audio encoding + Whisper transcription via `/api/transcribe` |
| `ConfirmResult` | Type for `matchConfirmation` return value |
| `playSentCue`, `startThinkingCue`, `playSpeakingCue`, `setSharedAudioContext` | Sound feedback |

## Standalone Dev Server

Prerequisites: apiari daemon must already be running (`apiari daemon start`).

```bash
# From the repo root:
cd web && npm install        # first time only
cd packages/chat
npm run dev
```

The dev server binds to `0.0.0.0:5174` and is reachable on the local network:
- **Mac Mini (local):** `http://localhost:5174`
- **iPad / other device:** `http://<mac-mini-ip>:5174`
  - Find your IP: `ipconfig getifaddr en0` (Wi-Fi) or `en1` (Ethernet)

Configure workspace and bot with URL params:
```
http://<host>:5174/?ws=WORKSPACE&bot=BOT
```

`/api` and `/ws` are proxied to the daemon on `localhost:4200` (same machine).
Use `VITE_API_PORT=XXXX npm run dev` to point at a different port.

To build the demo as a static bundle:
```bash
npm run build:demo   # outputs to demo-dist/
```

## Key Props — ChatPanel

```ts
<ChatPanel
  bot={string}
  bots={Bot[]}
  messages={Message[]}
  messagesLoading={boolean}
  loading={boolean}
  loadingStatus={string | undefined}  // tool name shown during thinking
  streamingContent={string}
  onSend={(text: string) => void}
  onCancel={() => void}
  workspace={string}
/>
```

## Rules

- React, ReactDOM, howler, and VAD libs are **peer dependencies** — do not bundle them.
- Dependencies: `@apiari/types` (domain types) + `@apiari/api` (for followup/transcription calls).
- Voice features require the browser VAD library (`@ricky0123/vad-web`) and `howler`.
- VAD WASM/ONNX assets must be served from `/` — the `vite.config.ts` copy plugin handles this in dev.
- CSS Modules only (`.module.css`) — never global styles.
- `enterKeyHint="enter"` on textarea: mobile shows return key; Enter inserts newline on mobile,
  sends on desktop. Mobile users tap the send button.
- `onMouseDown preventDefault` on send button keeps the iOS keyboard open.

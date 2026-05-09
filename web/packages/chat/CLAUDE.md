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

```bash
cd web/packages/chat
npm run dev           # starts Vite dev server on :5174
                      # proxies /api and /ws to localhost:4200 (running apiari daemon)
```

Configure the target workspace/bot with URL params: `?ws=WORKSPACE&bot=BOT`

To build the demo bundle separately:
```bash
npm run build:demo
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

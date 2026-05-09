# @apiari/types

All TypeScript domain types for the apiari platform. No runtime code — pure type definitions.

## Usage

```ts
import type { WorkerV2, Bot, Message, DashboardWidget } from "@apiari/types";
```

## Type Groups

- **Workspace / Bot** — `Workspace`, `Bot`, `CrossWorkspaceBot`
- **Workers (v1)** — `Worker`, `WorkerDetail`, `WorkerMessage`, `WorkerEnvironmentStatus`
- **Workers (v2)** — `WorkerV2`, `WorkerDetailV2`, `WorkerEvent`, `WorkerBrief`, `WorkerTaskPacket`
- **Reviews** — `WorkerReview`, `ReviewIssue`
- **Tasks** — `Task`, `TaskAttemptSummary`
- **Chat** — `Message`, `Followup`
- **Auto Bots** — `AutoBot`, `AutoBotRun`, `AutoBotDetail`
- **Context Bots** — `ContextBotContext`, `ContextBotSession`, `ContextBotMessage`, `ContextBotChatResponse`
- **Dashboard Widgets** — `DashboardWidget` (discriminated union of 12 widget types: stat, list, bar_chart, donut, activity_feed, etc.)
- **Other** — `Repo`, `Doc`, `Signal`, `ResearchTask`, `ProviderCapability`, `BotDebugData`

## Rules

- No imports — this package has zero dependencies.
- Add types here when 2+ packages need them. If only one package uses a type, keep it local.
- All widget types share a `WidgetBase` with `slot`, `title`, `updated_at`, `href`, `source`, `editable`.

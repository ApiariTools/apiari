# UI Component Map

This document maps the current web app implementation to the target rewrite structure from [UI Architecture](ui-architecture.md) and [UI Primitives](ui-primitives.md).

The point is to guide a staged migration, not to force a full rewrite in one change.

## Current Top-Level Structure

Current entry points:

- `web/src/App.tsx`
- `web/src/useWorkspaceConsoleState.ts`
- `web/src/consoleConfig.ts`

Current shell-related components:

- `TopBar`
- `WorkspaceNav`
- `MobileModeBar`
- `CommandPalette`
- `WorkspaceLayoutDialog`

Current mode-like components:

- `OverviewPanel`
- `ChatPanel`
- `WorkersPanel`
- `WorkerDetail`
- `ReposPanel`
- `DocsPanel`

## Target Structure

### Shell Layer

Target components:

- `AppShell`
- `WorkspaceHeader`
- `ModeRail`
- `MobileModeBar`
- `WorkspaceDrawer`
- `InspectorPane`

Current mapping:

- `TopBar` -> split into `WorkspaceHeader` and shell actions
- `WorkspaceNav` -> becomes `ModeRail` / `WorkspaceDrawer`
- `MobileModeBar` -> keep concept, rebuild against `AppShell`
- `CommandPalette` -> keep as global utility
- `WorkspaceLayoutDialog` -> keep, but move under shell configuration ownership

### State Layer

Target hooks:

- `useAppShellState`
- `useOverviewModeState`
- `useChatModeState`
- `useWorkersModeState`
- `useReposModeState`
- `useDocsModeState`

Current mapping:

- `useWorkspaceConsoleState` currently owns too much

Migration target:

- keep route/workspace coordination in shell state
- move mode-specific fetching and remembered substate closer to each mode

### Mode Layer

Target mode roots:

- `OverviewMode`
- `ChatMode`
- `WorkersMode`
- `ReposMode`
- `DocsMode`

Current mapping:

- `OverviewPanel` -> `OverviewMode`
- `ChatPanel` -> `ChatMode`
- `WorkersPanel` + `WorkerDetail` -> `WorkersMode`
- `ReposPanel` -> `ReposMode`
- `DocsPanel` -> `DocsMode`

## Recommended Migration Order

### 1. Shell Extraction

Create:

- `AppShell`
- `ModeScaffold`
- `PageHeader`

Move responsibilities out of `App.tsx` first.

Reason:

- `App.tsx` is currently coordinating layout, mode switching, and rendering decisions at the same time

### 2. Overview Rewrite

Current component:

- `OverviewPanel`

Rewrite goal:

- make it the canonical Control Room screen
- use `PageHeader`, `MetricCard`, `SectionCard`, and `ObjectRow`

Reason:

- Overview defines the visual and information hierarchy for the rest of the app

### 3. Workers Rewrite

Current components:

- `WorkersPanel`
- `WorkerDetail`
- `WorkerDiffPanel`

Rewrite goal:

- convert these into one coherent `WorkersMode`
- use list + detail + inspector semantics instead of sidebar leftovers

Reason:

- workers are the strongest non-chat first-order object

### 4. Docs Rewrite

Current component:

- `DocsPanel`

Rewrite goal:

- convert to `DocumentSurface`
- keep durable selection and explicit mobile list/detail flow

Reason:

- docs currently expose many of the shell/state weaknesses most clearly

### 5. Repos Rewrite

Current component:

- `ReposPanel`

Rewrite goal:

- make repo state a full tool instead of a list that sometimes behaves like a side rail

### 6. Chat Simplification

Current components:

- `ChatPanel`
- `ChatInput`
- `BotNav`
- `FollowupCard`

Rewrite goal:

- keep chat excellent
- remove responsibilities that belong to workers/repos/docs
- align with `ConversationSurface`

## Keep vs Replace

### Keep Conceptually

- command palette
- workspace layout preferences
- bottom mode bar idea
- current route memory behavior
- current bot/worker/docs contract logic

### Replace Structurally

- ad hoc `App.tsx` composition
- panel-vs-screen ambiguity
- duplicated list styling systems
- inconsistent status badge systems
- mode-specific mobile hacks

## File System Direction

Suggested future structure:

```text
web/src/
  shell/
    AppShell.tsx
    WorkspaceHeader.tsx
    ModeRail.tsx
    WorkspaceDrawer.tsx
    InspectorPane.tsx
  modes/
    overview/
    chat/
    workers/
    repos/
    docs/
  primitives/
    PageHeader.tsx
    SectionCard.tsx
    MetricCard.tsx
    ObjectList.tsx
    ObjectRow.tsx
    StatusBadge.tsx
    EmptyState.tsx
```

This does not need to happen immediately, but it should be the target rather than continuing to grow the current flat `components/` folder indefinitely.

## Success Criteria For Phase 1

Phase 1 is done when:

- shell responsibilities are clearly separated
- mode responsibilities are named and documented
- the first rewritten mode can be built without inventing a new layout language

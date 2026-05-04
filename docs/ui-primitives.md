# UI Primitives

This document defines the shared building blocks for the web UI rewrite described in [UI Architecture](ui-architecture.md).

These primitives should exist before large mode rewrites continue. They are the way we stop each mode from inventing its own layout system.

## Principles

1. One primitive should solve one recurring layout problem.
2. Primitives should encode hierarchy, not just styling.
3. Mobile behavior should be part of the primitive, not an afterthought.
4. The same primitive should work across Overview, Chat, Workers, Repos, and Docs wherever the meaning is the same.

## Shell Primitives

### `AppShell`

Owns:

- top app header
- mode navigation
- workspace drawer
- bottom mobile mode bar
- main content slot
- optional inspector slot

Responsibilities:

- ensure only one major navigation surface is competing for attention at a time
- coordinate mobile, iPad, and desktop shell behavior
- provide safe-area-aware spacing

Should replace ad hoc coordination currently spread across:

- `App.tsx`
- `TopBar`
- `WorkspaceNav`
- `MobileModeBar`

### `ModeScaffold`

Owns:

- page header
- main content area
- optional subordinate navigation or list panel
- mode-specific empty/loading/error states

Responsibilities:

- give every primary mode a consistent top-level structure
- keep “full-screen tool” semantics intact

Every primary mode should render inside a `ModeScaffold`.

### `InspectorPane`

Owns:

- secondary object detail
- contextual actions
- optional tabs for related surfaces

Responsibilities:

- show details for a selected worker, repo, bot, or followup
- remain secondary to the active tool

This replaces the current pattern where secondary context is sometimes a sidebar and sometimes the main screen.

## Page-Level Primitives

### `PageHeader`

Owns:

- title
- subtitle or status
- primary action
- secondary actions

Use when:

- a mode needs a clear identity and action row

### `SectionCard`

Owns:

- titled grouped content
- optional icon
- optional header action

Use when:

- Overview or inspectors need grouped state

### `MetricCard`

Owns:

- one metric
- one short label
- one small supporting note

Use when:

- the value should work as both information and a navigation affordance

## Collection Primitives

### `ObjectList`

Owns:

- list layout
- loading/empty affordances
- row spacing and rhythm

Use when:

- rendering homogeneous rows such as bots, workers, repos, or docs

### `ObjectRow`

Owns:

- title
- metadata
- left status indicator
- right badges/actions

Should support variants for:

- bot
- worker
- repo
- doc
- followup

### `ObjectBoard`

Owns:

- grouped columns or sections for status-based collections

Use when:

- Workers eventually grow into a board-like lifecycle view

This is optional for the first rewrite pass, but it should be the target primitive instead of hardcoding lifecycle columns inside a mode.

## Status Primitives

### `StatusBadge`

Use for:

- clean/modified
- approved/changes requested/pending
- CI status
- provider state

Rule:

- same state means same visual language everywhere

### `StatusDot`

Use for:

- lightweight running/waiting/idle indicators

Rule:

- don’t invent a separate dot system per panel

## Empty and Feedback Primitives

### `EmptyState`

Owns:

- message
- optional action
- optional hint

Use when:

- a mode or panel has no active content

### `InlineNotice`

Owns:

- low-severity informational state

Use when:

- the user needs a small explanation without a full empty state

## Mode-Specific Shared Primitives

### `ConversationSurface`

Shared structure for Chat and worker chat/task surfaces.

Owns:

- message stream
- streaming indicator
- input area
- scroll-to-bottom affordance

### `DocumentSurface`

Shared structure for Docs.

Owns:

- doc list
- editor/preview switch
- save/delete actions
- mobile list-to-document transition

### `DiffSurface`

Shared structure for worker diff and future repo diff views.

Owns:

- diff container
- file navigation
- loading/empty states

## Mobile-Specific Rules

Primitives should encode these rules by default:

- one major primary pane visible at a time
- fixed bottom mode bar spacing handled centrally
- touch targets remain large enough without mode-specific hacks
- list/detail transitions are explicit

## Suggested Initial Build Order

1. `AppShell`
2. `ModeScaffold`
3. `PageHeader`
4. `SectionCard`
5. `ObjectList`
6. `ObjectRow`
7. `StatusBadge` / `StatusDot`
8. `EmptyState`
9. `ConversationSurface`
10. `DocumentSurface`

## Non-Goals

Do not build a giant design system for its own sake.

These primitives should exist to stabilize product structure, not to maximize abstraction.

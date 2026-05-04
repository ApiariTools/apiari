# Design System

This document defines the UI system `apiari` should follow while the web app is being rewritten.

The goal is not a generic component library. The goal is to stop local visual decisions from drifting mode by mode.

## Product Rules

1. Mobile is the source of truth for hierarchy.
2. A screen should expose one primary action, not several peers.
3. Repeated objects belong in lists, not decorative cards.
4. Metadata should be quiet.
5. Conversation signal should be louder than provider/runtime signal.
6. Shell chrome should stay lighter than content.

## First-Order Objects

The system is designed around these objects:

- workspace
- bot
- worker
- repo
- doc
- followup
- research task

Each object should have one canonical visual treatment when it appears in a list, a card, or an inspector.

## Layout System

### Breakpoints

- `mobile`: `<= 768px`
- `tablet`: `769px - 1200px`
- `desktop`: `> 1200px`

### Spacing Scale

Use the token scale instead of ad hoc values:

- `--space-1`: 4px
- `--space-2`: 8px
- `--space-3`: 12px
- `--space-4`: 16px
- `--space-5`: 20px
- `--space-6`: 24px
- `--space-7`: 32px

### Mobile Rule

On phone screens:

- one primary pane at a time
- headers must be compact
- repeated content should read like a list/feed
- content should scroll before chrome grows

## Typography

Use role-based type, not one-off sizes.

### Roles

- `display`: workspace/mode title only when a screen truly needs a title
- `section-title`: small uppercase section label
- `title`: object name or major row title
- `body`: normal readable content
- `meta`: secondary detail
- `micro`: tiny status labels and chips

### Rules

- `meta` is never louder than `body`
- section labels should orient, not decorate
- message previews should use `body` or strong `meta`, not faint placeholder styling

## Surfaces

### Page Surface

Use for mode bodies. It can carry atmosphere, but should not compete with content.

### List Surface

Use for repeated scrollable objects:

- bots
- workers
- docs
- repos

Repeated mobile objects should usually be:

- flat rows
- separated by dividers
- optionally grouped by section

### Card Surface

Use only when the grouping itself matters.

Good uses:

- overview action groups
- metric summaries
- inspector groups

Bad uses:

- every bot in a mobile list
- every doc in a mobile list

### Inspector Surface

Secondary context only. Never stronger than the active tool canvas.

## Actions

### Primary Button

Use once per screen or group.

### Secondary Button

Use for neighboring lower-priority actions.

### Icon Button

Use for compact utility actions only.

### Chip

Use for compact metadata:

- provider/model
- watch sources
- status labels
- unread counts

Chips should never feel like form inputs.

## Lists

### Row Structure

Canonical row hierarchy:

1. title
2. optional short supporting line
3. optional quiet metadata/chips
4. optional trailing status/action

### Mobile Row Rules

- rows should be dense
- rows should not have stacked mini-panels inside them
- separators are usually enough

## Chat Rules

### Chat Root

The chat landing should behave like a prioritized conversation list:

1. needs attention
2. recent
3. all bots

### Bot Entry

Priority inside each bot entry:

1. bot identity
2. last meaningful message preview
3. compact context chips

Provider/runtime info must never overpower the preview.

### Conversation View

Priority:

1. messages
2. composer
3. bot switcher
4. auxiliary actions

On mobile:

- keep header compact
- keep composer compact
- maximize visible message area

## Status Language

Use shared tones only:

- `neutral`
- `attention`
- `success`
- `danger`
- `accent`

Same meaning should map to the same tone everywhere.

## Current Enforcement Targets

These files should follow this system first:

- `web/src/theme.css`
- `web/src/primitives/*`
- `web/src/components/ChatLanding.module.css`
- `web/src/components/ChatPanel.module.css`
- `web/src/components/OverviewPanel.module.css`
- `web/src/components/WorkersPanel.module.css`

## Immediate Do / Don’t

Do:

- prefer rows over cards for mobile lists
- use chips for compact metadata
- make previews readable
- trim headers aggressively on mobile

Don't:

- stack multiple heavy surfaces before content
- use tall cards for repeated mobile objects
- style metadata like primary content
- add new one-off spacing and radius values without a token

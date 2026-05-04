# UI Architecture

This document defines the target UI direction for `apiari`'s web app.

The goal is not a cosmetic refresh. It is a controlled architectural and design rewrite of the app shell and mode hierarchy while preserving the working runtime, contracts, and tests.

## Product Model

`apiari` is not a single conversation window with helper panels.

It is a workspace-first product with these first-order objects:

- workspace
- bot
- worker
- repo
- doc
- followup
- research task

These objects should be visible, navigable, and inspectable without forcing the user through chat.

## Recommended Direction

Use a **Multi-Tool Workbench** as the structural model.

Use a **Control Room** as the home screen philosophy.

That means:

- `Overview` is the operational home screen
- `Chat`, `Workers`, `Repos`, and `Docs` are full tools
- chat remains first-class, but it is not the whole app

## Core Principles

1. The workspace is the main container, not the thread.
2. Each primary mode is a real screen, not a side panel pretending to be a screen.
3. Secondary context belongs in an inspector pattern, not in random duplicated rails.
4. Mobile, iPad, and desktop are peers, not desktop plus compression.
5. Runtime truth comes from the daemon contract, not UI-local assumptions.

## Primary Modes

### Overview

Purpose:

- show what needs attention now
- summarize runtime state
- route the user into the correct tool

Contains:

- workspace status
- unread bot activity
- workers in flight
- repos needing attention
- pending followups
- recent research/docs activity

This is where the **Control Room** pattern lives.

### Chat

Purpose:

- talk to a bot
- inspect bot-specific history and live execution state

Contains:

- conversation
- bot identity and status
- followups for the active bot
- lightweight contextual actions only

Chat should not also be the place where repo browsing, worker management, and docs editing live by default.

### Workers

Purpose:

- inspect active and review-stage work
- open detail for a worker
- follow worker lifecycle transitions

Contains:

- worker list/queue
- worker detail
- worker task/chat/diff surfaces

Workers should feel like a tool, not a sidebar.

### Repos

Purpose:

- inspect repo health and attached worker activity

Contains:

- repo list
- branch/dirty state
- repo-linked workers
- research outputs where relevant

### Docs

Purpose:

- browse and edit workspace documentation

Contains:

- doc library
- document editor or preview
- clear mobile list-to-document flow

Docs should preserve selection and behave like a durable tool, not a disposable modal panel.

## Cross-Device Shell

### Mobile

Pattern:

- one primary mode per screen
- persistent bottom mode bar
- workspace drawer for secondary navigation
- no competing sidebars

Default structure:

- top: compact workspace header
- body: current tool screen
- bottom: mode bar

Expected modes in bottom bar:

- Overview
- Chat
- Workers
- Docs

`Repos` can remain accessible through Overview or Workers if tab count needs to stay tight, or it can be promoted later if usage proves it needs first-class mobile presence.

Mobile rules:

- tapping a mode returns to that mode's last substate
- re-tapping the active mode resets it to the mode root
- only one navigation surface should demand attention at a time

### iPad

Pattern:

- navigation rail + main tool + optional inspector

Default structure:

- left: workspace/mode rail
- center: active mode
- right: contextual inspector when needed

The iPad layout should feel like a spacious workbench, not a stretched phone UI or cramped desktop clone.

### Desktop

Pattern:

- navigation rail + full tool canvas + optional inspector

Default structure:

- left: workspace + modes
- center: active tool
- right: contextual inspector

Desktop should support multi-pane work, but each primary mode still owns the center of the screen.

## Inspector Pattern

The inspector is the correct place for secondary context.

It may show:

- selected worker detail
- selected repo metadata
- bot details
- followup details
- related activity

It should not replace the main tool itself.

## Information Hierarchy

When the user opens the app, the hierarchy should read like this:

1. Which workspace am I in?
2. What needs attention?
3. Which tool should I use next?
4. What object is currently selected?

Not:

1. Which chat thread am I in?
2. Where did side panels end up this time?

## Current Implementation Guidance

Do not restart from scratch.

Preserve:

- daemon/API/WebSocket contracts
- current workspace-first routing
- current test coverage
- working provider/runtime behavior

Rewrite:

- shell composition
- mode page structure
- inspector behavior
- visual system cohesion
- cross-device navigation semantics

## Migration Strategy

Implement this as a controlled shell rewrite, not a big-bang replacement.

### Phase 1

- finalize this architecture brief
- freeze further ad hoc shell changes
- define shared UI primitives

### Phase 2

- rebuild `Overview` in the target system
- rebuild `Workers` as a true tool
- rebuild `Docs` as a durable tool

### Phase 3

- rebuild `Repos`
- simplify `Chat` so it is first-class but not overloaded

### Phase 4

- unify inspector behavior
- revisit mobile tab composition after real usage

## Shared UI Primitives To Define

Before more major UI work, define these explicitly:

- app shell
- page header
- mode scaffold
- inspector scaffold
- mobile mode bar behavior
- list row patterns
- card patterns
- empty states
- status badges

These should become the new building blocks instead of letting each mode invent its own layout language.

## Success Criteria

The rewrite is successful when:

- users can name the main tools and know what each is for
- mobile feels intentionally designed, not compressed
- switching modes preserves meaningful context
- the home screen explains the workspace state at a glance
- workers, repos, and docs no longer feel like chat accessories

## Anti-Goals

Do not:

- turn the app into a generic dashboard
- bury chat
- add more overlapping navigation surfaces
- keep patching the old hierarchy indefinitely
- couple UI structure to provider-specific behavior

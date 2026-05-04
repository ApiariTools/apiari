# Scenario Evals

This directory is for named runtime scenarios that should stay stable across refactors, provider changes, and UI rewrites.

These are not meant to replace ordinary tests. They exist to make high-value product behavior explicit.

## What Belongs Here

- cross-layer scenarios
- provider parity scenarios
- restart/resume scenarios
- operator-surface compatibility scenarios

## What Does Not Belong Here

- tiny helper behavior
- formatting-only checks
- scenarios that are already fully captured by a small unit test

## Initial Spec Format

The first pass uses small YAML scenario specs. That keeps the expected behavior easy to review before a dedicated eval runner exists.

Each spec should identify:

- the runtime surface under test
- setup assumptions
- trigger/action
- normalized expectations
- primary existing tests or code paths that currently cover it

## Initial Scenarios

- `chat-main-compat.yaml`
- `worker-lifecycle-happy-path.yaml`
- `followup-restart-resume.yaml`
- `provider-parity-basic.yaml`
- `workspace-mobile-shell.yaml`

## Later Work

The next step is to wire these specs into a lightweight runner or at least a documented mapping to existing test commands.

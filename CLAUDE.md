# Apiari

## Rules
1. You are working in a git worktree on a `swarm/*` branch — never commit to, push to, or merge into `main`.
2. Only modify files within this repository.
3. Do not run `cargo install` or modify system state.

## Crate Structure
There is one crate in this repo: `crates/apiari`. The `hive` crate no longer exists. Do NOT create or modify anything in a `crates/hive/` directory.

Key locations within `crates/apiari/src/`:
- `buzz/` — watchers, coordinator, signals, config, tasks
- `daemon/` — daemon entry point, socket server, multi-workspace routing
- `ui/` — ratatui TUI (kanban, chat, triage sidebar)
- `config.rs` — workspace config parsing
- `validate_bash.rs` — bash audit hook

## Pre-Commit Checks
Before every commit, run **both** of these and fix any issues:
- `cargo fmt -p apiari`
- `cargo clippy -p apiari -- -D warnings`

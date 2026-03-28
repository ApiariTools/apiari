# Apiari

## Rules
1. You are working in a git worktree on a `swarm/*` branch. Never commit to main.
2. Only modify files within this repository.
3. Do not run `cargo install` or modify system state.
4. Stay on your `swarm/*` branch — NEVER push to or merge into `main`.

## Crate Structure
There is one crate in this repo: `crates/apiari`. The `hive` crate no longer exists. Do NOT create or modify anything in a `crates/hive/` directory.

- `src/buzz/` — watchers, coordinator, signals, config, tasks
- `src/daemon/` — daemon entry point, socket server, multi-workspace routing
- `src/ui/` — ratatui TUI (kanban, chat, triage sidebar)
- `src/config.rs` — workspace config parsing
- `src/validate_bash.rs` — bash audit hook

## Pre-Commit Checks
Before every commit, run **both** of these and fix any issues:
- `cargo fmt -p apiari`
- `cargo clippy -p apiari -- -D warnings`

# Apiari CLI

Unified CLI tool for managing apiari workspaces — watchers, coordinator, and Telegram integration.

## Quick Reference

```bash
cargo build -p apiari                    # Build
cargo test -p apiari                     # Run tests
cargo run -p apiari -- init              # Create workspace config from cwd
cargo run -p apiari -- daemon            # Start daemon (foreground)
cargo run -p apiari -- daemon --background  # Start daemon (background)
cargo run -p apiari -- status            # Show signals across all workspaces
cargo run -p apiari -- status apiari     # Show signals for one workspace
cargo run -p apiari -- chat apiari "msg" # CLI chat with workspace coordinator
```

## Config Layout

All configuration lives at `~/.config/apiari/`:

```
~/.config/apiari/
  workspaces/
    apiari.toml     # self-contained workspace config
    mgm.toml        # another workspace
  apiari.db         # single SQLite DB (workspace column on all tables)
  daemon.pid
  daemon.log
```

## Architecture

```
src/
  main.rs           # clap CLI entry point
  config.rs         # WorkspaceConfig struct, discover_workspaces()
  init.rs           # apiari init scaffolding
  daemon.rs         # multi-workspace event loop (tokio::select!)
```

## Swarm Worker Rules

1. **You are working in a git worktree.** Always create a new branch, never commit to `main`.
2. **Only modify files within this repo (`cli/`).** Do not touch other repos.
3. **When done, create a PR** to `ApiariTools/apiari` (or wherever the remote is set).
4. **Always run `cargo fmt -p apiari` before committing.**
5. **Do not run `cargo install` or modify system state.**
6. **Plan and execute in one go.** Do not pause mid-task for confirmation.

# apiari

A unified Rust CLI for managing AI coding workspaces. It runs a daemon that:

- Watches GitHub repos for CI failures, recovery, and new PRs
- Watches swarm workers (coding agents) for status changes
- Sends Telegram notifications
- Responds to Telegram messages via an AI coordinator

## Install

```sh
cargo install --path crates/apiari
```

On macOS, codesign the binary after install:

```sh
codesign -f -s - ~/.cargo/bin/apiari
```

## Usage

```sh
apiari init                        # Initialize a workspace config from the current directory
apiari daemon                      # Run the daemon (foreground)
apiari daemon --background         # Run the daemon in the background
apiari status [workspace]          # Show open signals, optionally filtered by workspace
apiari chat <workspace> [message]  # Chat with a workspace's coordinator
apiari ui                          # Launch the TUI dashboard
```

## Configuration

Workspace configs live at:

```
~/.config/apiari/workspaces/{name}.toml
```

Run `apiari init` inside a project directory to generate one.

## Crate structure

This repo is a Cargo workspace:

- **`crates/apiari`** — main CLI binary
  - `buzz` — signal watchers (GitHub, swarm, Sentry), coordinator, Telegram channel, signal pipeline
  - `daemon` — daemon lifecycle and IPC socket
  - `config` — workspace config loading
  - `init` — workspace initialization
  - `ui` — TUI dashboard
- **`crates/hive`** — quest planning, coordination, and context management

## License

MIT

# apiari-common

Crash-safe IPC and state primitives for the [Apiari](https://github.com/ApiariTools) toolchain — so every consumer gets the same battle-tested file I/O without reinventing it.

## Install

```toml
[dependencies]
apiari-common = "0.1"
```

## Modules

### `ipc` — JSONL read/write with cursor-based polling

Generic reader and writer for line-delimited JSON files.

- **`JsonlReader<T>`** — Tracks a byte offset so each call to `poll()` returns only newly appended records. Supports `skip_to_end()` and `with_offset()` for resuming from persisted cursors. Malformed lines are silently skipped.
- **`JsonlWriter<T>`** — Appends serialized records as JSON lines. Creates parent directories and the file automatically.

```rust
use apiari_common::ipc::{JsonlWriter, JsonlReader};

let writer = JsonlWriter::<MyMessage>::new(".swarm/inbox.jsonl");
writer.append(&msg)?;

let mut reader = JsonlReader::<MyMessage>::new(".swarm/inbox.jsonl");
let new_messages = reader.poll()?;
```

### `state` — Atomic JSON state persistence

Two functions for loading and saving arbitrary state to JSON files:

- **`load_state<T>(path)`** — Reads and deserializes a JSON file. Returns `T::default()` if the file doesn't exist.
- **`save_state<T>(path, state)`** — Writes atomically (write to `.tmp`, then rename) so a crash mid-write never corrupts the on-disk file. Creates parent directories automatically.

```rust
use apiari_common::state::{load_state, save_state};

let state: MyState = load_state(path)?; // returns Default if missing
save_state(path, &state)?;              // atomic write
```

## Design philosophy

`apiari-common` exists so that every tool in the Apiari ecosystem reads and writes files the same way — same JSONL format, same atomic-rename dance, same cursor semantics. Extracting these into a shared crate means bug fixes land once and propagate everywhere.

The dependency footprint is intentionally minimal: only `serde` and `serde_json`. No async runtime, no logging framework, no feature flags. This keeps compile times low, avoids transitive dependency surprises, and makes the crate trivial to audit. If a primitive doesn't need to be shared, it doesn't belong here.

## Ecosystem

`apiari-common` is used by:

- [**apiari**](https://github.com/ApiariTools/apiari) — CLI agent orchestrator
- [**swarm**](https://github.com/ApiariTools/swarm) — TUI agent multiplexer

## License

[MIT](LICENSE)

---
sidebar_position: 2
---

# Rust SDK (`beachcomber-client`)

The `beachcomber-client` crate provides a typed, synchronous Rust API with no tokio dependency. Published on [crates.io](https://crates.io/crates/beachcomber-client).

## Installation

```toml
[dependencies]
beachcomber-client = "0.1"
```

Or with cargo-add:

```sh
cargo add beachcomber-client
```

## Quick start

```rust
use beachcomber_client::{Client, CombResult};

let client = Client::new(); // auto-discovers socket, starts daemon if needed

// Single field query
match client.get("git.branch", Some("/path/to/repo"))? {
    CombResult::Hit { data, .. } => println!("branch: {}", data.as_text().unwrap()),
    CombResult::Miss => println!("not cached yet — will be ready on next query"),
}

// Full provider query with typed field access
match client.get("git", Some("/path/to/repo"))? {
    CombResult::Hit { data, age_ms, stale } => {
        println!("branch: {}", data.get_str("branch").unwrap_or("?"));
        println!("dirty: {}", data.get_bool("dirty").unwrap_or(false));
        println!("ahead: {}", data.get_i64("ahead").unwrap_or(0));
        println!("age: {}ms, stale: {}", age_ms, stale);
    }
    CombResult::Miss => {}
}

// Persistent session for multiple queries (one connection, multiple requests)
let mut session = client.session()?;
session.set_context("/path/to/repo")?;
let branch = session.get("git.branch", None)?;
let battery = session.get("battery.percent", None)?;
```

## Features

- **Synchronous** — no async runtime needed
- **Socket activation** — starts the daemon automatically if not running
- **Typed access** — `get_str()`, `get_bool()`, `get_i64()`, `get_f64()`
- **Persistent sessions** — reuse one connection for multiple queries (15µs/query vs 34µs)
- **Configurable timeouts** — default 100ms, adjustable via `ClientConfig`

## API reference

### `Client`

```rust
let client = Client::new();                          // auto-discover socket
let client = Client::with_config(ClientConfig { .. }); // explicit config
```

#### `client.get(key, path)`

Read a cached value. Returns `CombResult::Hit { data, age_ms, stale }` or `CombResult::Miss`.

```rust
client.get("git.branch", Some("/path/to/repo"))?;
client.get("hostname", None)?;  // global provider, no path needed
client.get("git", Some("/path/to/repo"))?;  // full provider object
```

#### `client.refresh(key, path)`

Force the daemon to recompute a provider.

```rust
client.refresh("git", Some("/path/to/repo"))?;
```

#### `client.status()`

Return daemon scheduler and cache status.

#### `client.hello()`

Handshake — returns a `HelloInfo` with daemon and protocol version strings.

#### `client.put(key, data, ttl, path)`

Write a JSON value into the cache as a virtual provider. `ttl` and `path` are `Option<&str>`. Pass `None` for `data` (via `put_null`) to clear the entry.

#### `client.introspect(subject, duration_secs)`

Inspect a daemon subsystem. `subject` is an `IntrospectSubject` enum variant: `Daemon`, `Providers`, `Config`, `Cache`, `Lifecycle`, `Watches`, `Timers`, `Demand`, `Procs`. `duration_secs` is `Option<u64>` (only used by `Procs`).

#### `client.watch(key, path)`

Subscribe to live cache updates. Returns a `WatchStream`; call `next_event()` in a loop and drop the stream when done. The first event is always the current cached value.

```rust
let mut stream = client.watch("git.branch", Some("/path/to/repo"))?;
while let Some(event) = stream.next_event()? {
    println!("branch update: {:?} (age {}ms)", event.data, event.age_ms);
}
```

#### `client.session()`

Open a persistent connection. Reduces per-query overhead to ~15µs.

```rust
let mut session = client.session()?;
session.set_context("/path/to/repo")?;
let branch = session.get("git.branch", None)?;
let dirty = session.get("git.dirty", None)?;
```

### `CombResult`

```rust
match result {
    CombResult::Hit { data, age_ms, stale } => { /* cache hit */ }
    CombResult::Miss => { /* not yet cached */ }
}
```

`data` provides typed accessors: `as_text()`, `get_str(field)`, `get_bool(field)`, `get_i64(field)`, `get_f64(field)`.

#### `client.put_null(key, path)`

Clear a virtual provider entry (equivalent to `put` with no data).

## Socket discovery

The SDK discovers the daemon socket at:

1. `$XDG_RUNTIME_DIR/beachcomber/sock` (if `XDG_RUNTIME_DIR` is set)
2. `$TMPDIR/beachcomber-<uid>/sock` (`TMPDIR` defaults to `/tmp` when unset)

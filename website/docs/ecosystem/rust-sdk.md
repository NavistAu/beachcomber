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

#### `client.poke(key, path)`

Force the daemon to recompute a provider.

```rust
client.poke("git", Some("/path/to/repo"))?;
```

#### `client.list()`

List available providers.

#### `client.status()`

Return daemon scheduler and cache status.

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

## Socket discovery

The SDK discovers the daemon socket at:

1. `$XDG_RUNTIME_DIR/beachcomber/sock`
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `/tmp/beachcomber-<uid>/sock`

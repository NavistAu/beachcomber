---
sidebar_position: 2
---

# Rust SDK (`libbeachcomber`)

The `libbeachcomber` crate provides a typed, synchronous Rust API with no tokio dependency. Published on [crates.io](https://crates.io/crates/libbeachcomber).

## Installation

```toml
[dependencies]
libbeachcomber = "0.8"
```

Or with cargo-add:

```sh
cargo add libbeachcomber
```

## Quick start

```rust
use libbeachcomber::{Client, CombResult};

let client = Client::new(); // auto-discovers socket, starts daemon if needed

// Single field query
match client.get("git.branch", Some("/path/to/repo"))? {
    CombResult::Hit { data, .. } => println!("branch: {}", data.as_text().unwrap()),
    CombResult::Template { .. } => println!("a template is in effect — resolve renders it"),
    CombResult::Miss => println!("not cached yet — will be ready on next query"),
}

// Full provider query with typed field access
match client.get("git", Some("/path/to/repo"))? {
    CombResult::Hit { data, age_ms, stale, .. } => {
        println!("branch: {}", data.get_str("branch").unwrap_or("?"));
        println!("dirty: {}", data.get_bool("dirty").unwrap_or(false));
        println!("ahead: {}", data.get_i64("ahead").unwrap_or(0));
        println!("age: {}ms, stale: {}", age_ms, stale);
    }
    CombResult::Template { .. } | CombResult::Miss => {}
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
let client = Client::with_config(ClientConfig { exec: true, ..Default::default() }); // templates may call exec()
```

`ClientConfig.exec` is off by default: a template that calls `exec()` on a client without it fails with `exec() is disabled in this context`.

#### `client.get(key, path)`

Read a node raw — both halves, nothing rendered. Returns `CombResult::Hit { data, age_ms, stale, template, rest }`, `CombResult::Template { template, rest }` or `CombResult::Miss` (see `CombResult`).

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

Write a JSON value into the cache as static data. `ttl` and `path` are `Option<&str>`. Pass `serde_json::Value::Null` as `data` to remove the node.

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

`get` returns the node as the daemon holds it — both halves, nothing rendered. A node may hold a cached value, a template, or both. `Hit` carries `data` (with `age_ms`/`stale`) and, when one is in effect, `template: Option<Value>` — a string for a leaf, an object for a group, unrendered — plus `rest: Option<String>`, the address left over when the template walk stopped at a string (`get("p.t.x")` where `p.t` is a template string gives the template of `p.t` and `rest == Some("x")`). `Template` is a template-only node: neither a hit (no value until it is rendered) nor a miss (something is stored there) — an address whose value `resolve` renders. `get` is the raw half; `resolve` is how you get the value. `is_hit()`, `is_miss()`, `template()` and `rest()` read across the variants.

```rust
match result {
    CombResult::Hit { data, age_ms, stale, template, rest } => { /* cache hit; template/rest ride along when set */ }
    CombResult::Template { template, rest } => { /* template only — render it with resolve */ }
    CombResult::Miss => { /* nothing stored */ }
}
```

`data` provides typed accessors: `as_text()`, `get_str(field)`, `get_bool(field)`, `get_i64(field)`, `get_f64(field)`.

### `libbeachcomber::resolve`

The one client-side resolver — the function `comb get`, `comb eval`, `bc_resolve`/`bc_eval` and every SDK answer from. `get` returns what the daemon stores; `resolve` returns what `comb get` prints: the daemon holds templates as data (`<provider>:template`, seeded from configuration or `put`) and never evaluates one, so the client renders them with the caller's environment and directory. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. `Ok(None)` is a miss; `Err(ResolveError::Config)` is a template that does not compile or cycles, `Runtime` a failed or disabled `exec()`, `Fetch` a daemon the library could not ask or a key it refused.

```rust
use libbeachcomber::Client;
use libbeachcomber::resolve::{resolve, Inputs, Slot};
use std::collections::HashMap;

let client = Client::new();
let env: HashMap<String, String> = std::env::vars().collect();
let inputs = Inputs {
    cwd: "/path/to/repo",
    env: &env,
    slot: Slot::Cwd,               // or Slot::Path(..) / Slot::Global
    force: false,
    wait: false,
    allow_exec: client.allow_exec(),
};
let value = resolve("python.version", &inputs, &mut &client)?;
// Option<serde_json::Value> — the same value `comb get python.version` prints here
```

`resolve::eval(template_str, &inputs, &mut fetch)` renders a template that is stored nowhere, the same way `comb eval` does. The library reads no ambient state: every input is in `Inputs`. The `Fetch` is a `&Client` (one connection per node) or a `Session` (one connection for the whole resolution). A failure classifies with `kind()` — `CombError`, `ResolveError` and `WatchError` alike — into the slugs the C ABI's `error.kind` carries; a read or write that runs out the client's timeout is `CombError::Timeout` on every op, on a client and a session alike.

## Socket discovery

The SDK discovers the daemon socket at:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

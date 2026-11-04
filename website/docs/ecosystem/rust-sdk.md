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
use libbeachcomber::Client;

let client = Client::new(); // auto-discovers socket, starts daemon if needed

// What `comb get git.branch` prints in that directory; `None` is a miss
match client.get("git.branch", Some("/path/to/repo"))? {
    Some(branch) => println!("branch: {branch}"),
    None => println!("nothing there"),
}

// A provider is its whole object
if let Some(git) = client.get("git", Some("/path/to/repo"))? {
    println!("dirty: {}", git["dirty"]);
}

// The unrendered stored value is a facet address
let stored = client.get("git:cache.branch", Some("/path/to/repo"))?;

// Persistent session for multiple queries (one connection, multiple requests)
let mut session = client.session()?;
session.set_context("/path/to/repo")?;
let branch = session.get("git.branch", None)?;
let battery = session.get("battery.percent", None)?;
```

## Features

- **Synchronous** — no async runtime needed
- **Socket activation** — starts the daemon automatically if not running
- **The value `comb get` prints** — templates rendered, `serde_json::Value` out
- **Persistent sessions** — reuse one connection for multiple queries (15µs/query vs 34µs)
- **Configurable timeouts** — default 250ms, adjustable via `ClientConfig`

## API reference

### `Client`

```rust
let client = Client::new();                          // auto-discover socket
let client = Client::with_config(ClientConfig { .. }); // explicit config
let client = Client::with_config(ClientConfig { exec: true, ..Default::default() }); // templates may call exec()
```

`ClientConfig.exec` is off by default: a template that calls `exec()` on a client without it fails with `exec() is disabled in this context`.

#### `client.get(key, path)`

The value `comb get` prints, through `libbeachcomber::resolve`: `Result<Option<serde_json::Value>, ResolveError>`, `Ok(None)` a miss. A template the daemon holds is rendered with this process's environment; `path` of `None` is the global entry. The unrendered stored value is a facet address: `git:cache.branch`, `aws:template.profile`, `p:put.x`, `p:source.x`. For text, pass the value to `render::render_value(key, value.as_ref(), Format::Text)`.

```rust
client.get("git.branch", Some("/path/to/repo"))?;
client.get("hostname", None)?;  // global provider, no path needed
client.get("git", Some("/path/to/repo"))?;  // full provider object
```

A caller's own `env`, `force` or `wait` call `resolve::resolve` directly (below).

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

### `libbeachcomber::resolve`

The one client-side resolver — the function `comb get`, `comb eval`, `bc_get`/`bc_eval` and every SDK's `get` and `eval` answer from. `Client::get` and `Session::get` are this function with the process's environment; the daemon holds templates as data (`<provider>:template`, seeded from configuration or `put`) and never evaluates one, so the client renders them with the caller's environment and directory. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. `Ok(None)` is a miss; `Err(ResolveError::Config)` is a template that does not compile or cycles, `Runtime` a failed or disabled `exec()`, `Fetch` a daemon the library could not ask or a key it refused.

```rust
use libbeachcomber::{Client, Wait};
use libbeachcomber::resolve::{resolve, Inputs, Slot};
use std::collections::HashMap;
use std::time::Duration;

let client = Client::new();
let env: HashMap<String, String> = std::env::vars().collect();
let inputs = Inputs {
    cwd: "/path/to/repo",
    env: &env,
    slot: Slot::Cwd,               // or Slot::Path(..) / Slot::Global
    force: false,
    wait: Wait::Budget,            // or Wait::For(Duration::ZERO) / Wait::Forever
    allow_exec: client.allow_exec(),
    now: std::time::SystemTime::now(),
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

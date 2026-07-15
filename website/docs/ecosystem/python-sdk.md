---
sidebar_position: 3
---

# Python SDK

Python client for the beachcomber (`comb`) shell-state daemon. Published on [PyPI](https://pypi.org/project/libbeachcomber/).

## Requirements

- Python 3.9+
- No external dependencies (stdlib only)
- A running `comb` daemon

## Installation

```sh
pip install libbeachcomber
```

Or with `uv`:

```sh
uv add libbeachcomber
```

## Quick start

```python
from beachcomber import Client

client = Client()

# Read a single field
result = client.get("git.branch", path="/path/to/repo")
if result.is_hit:
    print(result.data)    # "main"
    print(result.age_ms)  # 234
    print(result.stale)   # False

# Read a full provider (returns dict)
result = client.get("git", path="/path/to/repo")
if result.is_hit:
    print(result["branch"])  # "main"
    print(result["dirty"])   # False

# Force recomputation
client.refresh("git", path="/path/to/repo")

# Daemon status
status = client.status()
```

## Sessions

For multiple queries use a session to reuse a single connection:

```python
with client.session() as session:
    session.set_context("/path/to/repo")
    branch = session.get("git.branch")
    dirty = session.get("git.dirty")
    hostname = session.get("hostname")
```

## Custom socket path

```python
client = Client(socket_path="/run/user/1000/beachcomber/sock")
```

## Socket discovery

The SDK discovers the daemon socket at:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

## API reference

### `Client`

```python
client = Client()
client = Client(socket_path="/custom/path")
```

#### `client.get(key, path=None)`

Read a cached value. Returns a `Result`.

#### `client.refresh(key, path=None)`

Force the daemon to recompute a provider.

#### `client.status()`

Return typed cache rows as a list of `CacheRow` dataclasses (one per warm cache entry).

#### `client.hello()`

Handshake — returns a `HelloInfo` with daemon and protocol version strings.

#### `client.put(key, data=None, ttl=None, path=None)`

Write a value into the cache as a virtual provider. `data=None` clears the entry.

#### `client.introspect(subject, duration_secs=None)`

Inspect a daemon subsystem. `subject` is an `IntrospectSubject` enum value (`DAEMON`, `PROVIDERS`, `CONFIG`, `CACHE`, `LIFECYCLE`, `WATCHES`, `TIMERS`, `DEMAND`, `PROCS`). Returns an `IntrospectResponse`.

#### `client.watch(key, path=None)`

Subscribe to live cache updates. Returns a `WatchStream` (context manager / iterator) yielding `WatchEvent` instances.

#### `client.get_with_flags(key, path=None, force=False, wait=False)`

Read a cached value with optional protocol flags: `force` triggers recomputation before returning; `wait` blocks until a fresh value is available.

#### `client.session()`

Returns a context manager that yields a `Session` for persistent connections.

### `Result`

| Attribute | Type | Description |
|---|---|---|
| `is_hit` | `bool` | `True` when the cache had a value |
| `data` | `Any` | Cached value (scalar or dict) |
| `age_ms` | `int` | Cache age in milliseconds |
| `stale` | `bool` | Whether the value is stale |

For full provider results, `result["field"]` delegates to the underlying dict.

## Exceptions

| Exception | When raised |
|---|---|
| `DaemonNotRunning` | Cannot connect to the socket |
| `ServerError` | Daemon returns `ok: false` |
| `ProtocolError` | Malformed JSON or I/O failure |
| `CombError` | Base class for all SDK errors |

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

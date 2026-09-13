---
sidebar_position: 3
---

# Python SDK

Python client for the beachcomber (`comb`) shell-state daemon. Published on [PyPI](https://pypi.org/project/libbeachcomber/).

## Requirements

- Python 3.9+
- No external dependencies (stdlib `ctypes` binds directly to the shared `libbeachcomber` C ABI — no subprocess, no wire-protocol code in this SDK)
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
client = Client(exec=True)   # templates this client resolves may call exec(); off by default
```

#### `client.get(key, path=None)`

Read a node raw — both halves, nothing rendered. Returns a `Result` (see below).

#### `client.resolve(key, cwd, env=None)` / `client.eval(template_str, cwd, env=None)`

`resolve` returns the same value `comb get` returns for this key, cwd and env: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. `eval` renders a template that is stored nowhere — the same value `comb eval` prints. Both `cwd` arguments are required. The template accepts a single `{{ expr }}` tag, or literal text/several tags — the former keeps the expression's natural type, the latter is always a string (a source with no tags at all is its literal text). `exec()` inside a template runs only on a `Client(exec=True)`; otherwise it fails with `exec() is disabled in this context`.

#### `client.refresh(key, path=None)`

Force the daemon to recompute a provider.

#### `client.status()`

Return typed cache rows as a list of `CacheRow` dataclasses (one per warm cache entry).

#### `client.hello()`

Handshake — returns a `HelloInfo` with daemon and protocol version strings.

#### `client.put(key, data=None, ttl=None, path=None)`

Write a value into the cache as static data. `data=None` clears the entry.

#### `client.introspect(subject, duration_secs=None)`

Inspect a daemon subsystem. `subject` is an `IntrospectSubject` enum value (`DAEMON`, `PROVIDERS`, `CONFIG`, `CACHE`, `LIFECYCLE`, `WATCHES`, `TIMERS`, `DEMAND`, `PROCS`). Returns an `IntrospectResponse`.

#### `client.watch(key, path=None)`

Subscribe to live cache updates. Returns a `WatchStream` (context manager / iterator) yielding `WatchEvent` instances.

#### `client.get_with_flags(key, path=None, force=False, wait=False)`

Read a cached value with optional protocol flags: `force` triggers recomputation before returning; `wait` blocks until a fresh value is available.

#### `client.session()`

Returns a context manager that yields a `Session` for persistent connections.

### `Result`

`get` returns the node as the daemon holds it — both halves, nothing rendered. A node may hold a cached value (`data`), a template (`template`), or both. A template-only result is neither a hit nor a miss: it is an address whose value `resolve` renders — `get` is the raw half, `resolve(key, cwd, env)` is how you get the value.

| Attribute | Type | Description |
|---|---|---|
| `is_hit` | `bool` | `True` when the cache had a value |
| `is_miss` | `bool` | `True` when nothing is stored at the address: no value **and** no template |
| `data` | `Any` | Cached value (scalar or dict); `None` when nothing is cached |
| `age_ms` | `int` | Cache age in milliseconds |
| `stale` | `bool` | Whether the value is stale |
| `template` | `Any` | The template in effect at the address, unrendered: a string for a leaf, a dict for a group; `None` when none is in effect |
| `rest` | `Optional[str]` | The address left over when the template walk stopped at a string — `get("p.t.x")` where `p.t` is a template string gives the template of `p.t` and `rest == "x"` |

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

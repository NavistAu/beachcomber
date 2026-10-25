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
import os

from beachcomber import Client

client = Client()

# What `comb get git.branch` prints in that directory; None on a miss
branch = client.get("git.branch", cwd="/path/to/repo")   # "main"

# A full provider (a dict)
git = client.get("git", cwd="/path/to/repo")
print(git["branch"], git["dirty"])

# The caller's environment for env.* refs, and the force/wait flags
client.get("aws.profile", cwd="/path/to/repo", env=dict(os.environ))
client.get("git.branch", cwd="/path/to/repo", force=True, wait=True)

# The template itself, unrendered: a facet, read like any key
client.get("aws:template.profile")

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
client = Client(exec=True)   # templates this client renders may call exec(); off by default
```

#### `client.get(key, cwd=None, env=None, force=False, wait=False)` / `client.eval(template_str, cwd, env=None)`

`get` returns the value `comb get` prints for this key, cwd and env — the bare value, `None` on a miss: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. `cwd=None` is the global entry; `force` re-executes every Source the read demands first, and `wait` re-executes a stale entry inline. The unrendered halves are facets, read like any key: `p:template.x` is the template as stored, `p:put.x` and `p:source.x` those layers' values. `eval` renders a template that is stored nowhere — the same value `comb eval` prints. The template accepts a single `{{ expr }}` tag, or literal text/several tags — the former keeps the expression's natural type, the latter is always a string (a source with no tags at all is its literal text). `exec()` inside a template runs only on a `Client(exec=True)`; otherwise it fails with `exec() is disabled in this context`.

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

#### `client.watch(key, cwd, env=None)`

Subscribe to a key's value — the same value `get(key, cwd, env)` returns — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `cwd` is required. Returns a `WatchStream` (context manager / iterator) yielding `WatchEvent` instances, whose `data` carries no `age_ms`/`stale` — a value resolved from several cache records has no single age or staleness.

#### `client.session()`

Returns a context manager that yields a `Session` for persistent connections.

## Exceptions

| Exception | When raised |
|---|---|
| `DaemonNotRunning` | Cannot connect to the socket |
| `ServerError` | Daemon returns `ok: false` |
| `ProtocolError` | Malformed JSON or I/O failure |
| `CombError` | Base class for all SDK errors |

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

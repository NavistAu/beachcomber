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

# The caller's environment for env.* refs; force a run, and wait for it
client.get("aws.profile", cwd="/path/to/repo", env=dict(os.environ))
client.get("git.branch", cwd="/path/to/repo", force=True, wait="forever")
# Answer at once with what is stored, however stale
client.get("git.branch", cwd="/path/to/repo", wait="0")

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

## Library discovery

The library is found by the one rule every SDK follows
([`docs/sdk-api.md`, "Library discovery"](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md#library-discovery)). The file is
`libbeachcomber.dylib` on macOS and `libbeachcomber.so` on every other
platform, and the SDK tries, in order:

1. `$BEACHCOMBER_LIB`, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library.
2. Beside `comb`: `<dir>/../lib/<file>`, where `<dir>` is the directory of
   the first executable `comb` on `$PATH`, its symlinks resolved. A Homebrew
   `comb` is a symlink into the Cellar, and the library sits in the Cellar
   beside it.
3. The system loader, given the bare file name.

When none loads, `LibraryNotFoundError` names every candidate and why each
failed; there is no fallback to spawning `comb` as a subprocess. The tests
check this against the shared fixture, `sdks/discovery.json`.

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
client = Client(timeout_ms=500, connect_timeout_ms=0)
```

| Option | Default | Meaning |
|---|---|---|
| `socket_path` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeout_ms` | 250 | How long one read or write on the socket may take; at least 1. |
| `connect_timeout_ms` | 1500 | How long connecting keeps retrying before it gives up; 0 is one attempt. |
| `autostart` | on | Start a daemon when none answers an auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`. |

The defaults are the library's (`bc_client_new`): this SDK passes on the
options you give and adds none of its own.

#### `client.get(key, cwd=None, env=None, force=False, wait=None)` / `client.eval(template_str, cwd=None, env=None)`

`get` returns the value `comb get` prints for this key, cwd and env — the bare value, `None` on a miss: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. `cwd=None` is the global entry; `force` re-executes every Source the read demands first, and `wait` is how long to wait for a run the read needs before answering what is stored, as `comb get --wait`: a duration (`"0"`, `"50ms"`, `"2s"`) or `"forever"`, anything else a `ParseError`. The unrendered halves are facets, read like any key: `p:template.x` is the template as stored, `p:put.x` and `p:source.x` those layers' values. `eval` renders a template that is stored nowhere — the same value `comb eval` prints. The template accepts a single `{{ expr }}` tag, or literal text/several tags — the former keeps the expression's natural type, the latter is always a string (a source with no tags at all is its literal text). `exec()` inside a template runs only on a `Client(exec=True)`; otherwise it fails with `exec() is disabled in this context`.

#### `client.refresh(key, path=None)`

Force the daemon to recompute a provider.

#### `client.status()`

Return typed cache rows as a list of `CacheRow` dataclasses (one per warm cache entry). A payload that is not an array of rows raises `ParseError`.

#### `client.hello()`

Handshake — returns a `HelloInfo` with daemon and protocol version strings.

#### `client.put(key, data=None, ttl=None, path=None)`

Write a value into the cache as static data. `data=None` clears the entry.

#### `client.introspect(subject, duration_secs=None)`

Inspect a daemon subsystem. `subject` is an `IntrospectSubject` enum value (`DAEMON`, `PROVIDERS`, `CONFIG`, `CACHE`, `LIFECYCLE`, `WATCHES`, `TIMERS`, `DEMAND`, `PROCS`). Returns an `IntrospectResponse`: for `DAEMON` its `daemon` is a typed `DaemonHealth` (with `watch_backend`, a `ReaperStatus` and `Verdict`s); for the rest its `other` is the JSON the daemon answered.

#### `client.watch(key, cwd=None, env=None)`

Subscribe to a key's value — the same value `get(key, cwd, env)` returns — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `cwd=None` watches the global entry. Returns a `WatchStream` (context manager / iterator) yielding `WatchEvent` instances, whose `data` carries no `age_ms`/`stale` — a value resolved from several cache records has no single age or staleness. `stream.next_event(timeout_ms=None)` answers the next event, or `None` once the stream has ended; a `timeout_ms` that runs out with no event raises `TimeoutError` and leaves the stream open. `stream.cancel()` ends the stream from any thread; `stream.close()` ends it and frees it.

#### `client.session()`

Returns a context manager that yields a `Session` for persistent connections: `set_context(path)`, `get`, `put`, `refresh` and `close`. `status`, `hello` and `introspect` are the client's alone.

#### `client.close()`

Free the client's native handle.

## Exceptions

One `CombError` subclass per error kind of the [SDK API table](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md), each carrying `.kind` (the slug), `.message` (the ABI's message, unchanged; `str(exc)` is the same) and `.library_version`.

| Exception | `.kind` | When raised |
|---|---|---|
| `DaemonNotRunningError` | `daemon_not_running` | No daemon answers, and none was started |
| `ConnectionFailedError` | `connection_failed` | An explicit `socket_path` could not be connected to |
| `IoError` | `io_error` | A read or write on the socket failed |
| `ParseError` | `parse_error` | A request, a response or a template could not be read |
| `ServerError` | `server_error` | The daemon refused the request, or a template failed to render |
| `TimeoutError` | `timeout` | A read or write ran out `timeout_ms`, or `next_event` ran out its `timeout_ms` |
| `BusyError` | `busy` | A session or watch is in use by another caller |
| `PanicError` | `panic` | The library panicked |
| `VersionSkewError` | `version_skew` | The daemon is a different version from the library |
| `LibraryNotFoundError` | `library_not_found` | No `libbeachcomber` was found where discovery looks |
| `LibrarySymbolError` | `library_symbol` | The library found lacks a function this SDK calls |
| `UnsupportedError` | `unsupported` | A transport cannot do this operation (never raised here) |
| `CombError` | varies | Base class for all of them |

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

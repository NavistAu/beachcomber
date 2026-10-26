# beachcomber Python SDK

Python client for the [beachcomber](https://github.com/NavistAu/beachcomber) (`comb`) shell-state daemon.
It follows the SDK API table ([`docs/sdk-api.md`](../../docs/sdk-api.md)): the
same methods, options, value shapes and error kinds as every other SDK, which
`tests/test_api_surface.py` checks against [`sdks/api.json`](../api.json).

## Requirements

- Python 3.9+
- No external dependencies (stdlib `ctypes` binds directly to the native
  `libbeachcomber.{so,dylib}` — no subprocess, no socket code in this SDK)
- A running `comb` daemon (or `autostart` left enabled, the default)
- `libbeachcomber.{so,dylib}` discoverable — see "Library discovery" below

## Installation

```sh
pip install beachcomber
```

Or with `uv`:

```sh
uv add beachcomber
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

## `get`

`client.get(key, cwd=None, env=None, force=False, wait=None)` returns the
value `comb get` prints for this key, cwd and env — the bare value, `None`
on a miss. The daemon holds the templates (`<provider>:template`, seeded
from configuration or `put`) and the client renders them: a plain field is
its cached value, a templated one is rendered, and a bare provider name is
the whole merged namespace. `cwd=None` is the global entry. The unrendered
halves are facets, read like any key: `p:template.x` is the template as
stored, `p:put.x` and `p:source.x` those layers' values.

`force=True` re-executes every Source the read demands, as `comb get
--force`. `wait` is how long the read waits for a run it needs before it
answers what is stored, as `comb get --wait`: a duration (`"0"`, `"50ms"`,
`"2s"`) or `"forever"`; anything else is a `ParseError`. Left out, a stored
record waits its Source's budget and a cold read waits for its run. The read
may take `timeout_ms` plus the wait (`"forever"` has no deadline), so a cold
read with no `wait` can outlast the default 100 ms and raise `TimeoutError`;
its run goes on, and the next read has the value.

## Sessions

For multiple queries use a session to reuse a single connection:

```python
with client.session() as session:
    session.set_context("/path/to/repo")
    branch = session.get("git.branch")   # reads at the session's directory
    dirty = session.get("git.dirty")
    hostname = session.get("hostname")
```

## Eval

`client.eval(template_str, cwd=None, env=None)` renders a template that is
stored nowhere — the same value `comb eval` prints; `eval("{{ p.f }}")` and
`get("p.f")` agree. The template accepts a single `{{ expr }}` tag, or
literal text/several tags — the former keeps the expression's natural type,
the latter is always a string (a source with no tags at all is its literal
text).

`exec()` inside a template runs only on a client created with
`Client(exec=True)`; it is off by default, and a template that calls it
then fails with `exec() is disabled in this context`.

## Watch

`client.watch(key, cwd=None, env=None)` streams `get`'s value for that key,
cwd and env, again each time it changes:

```python
with client.watch("git.branch", cwd="/path/to/repo") as stream:
    for event in stream:          # ends when the stream does
        print(event.data)         # None is a miss, not the end
```

`stream.next_event(timeout_ms=None)` answers the next `WatchEvent`, or
`None` once the stream has ended; a `timeout_ms` that runs out with no event
raises `TimeoutError`, and the stream stays open. `stream.cancel()` ends it
from any thread, and `stream.close()` ends it and frees it.

`status()`, `hello()` and `introspect(subject, duration_secs=None)` are the
client's; a session has none of them. `status()` answers typed `CacheRow`s,
`hello()` a `HelloInfo`, and `introspect()` an `IntrospectResponse` whose
`daemon` is a typed `DaemonHealth` (with its `ReaperStatus` and `Verdict`s)
for the `daemon` subject and whose `other` is the JSON for the rest.

## Client options

```python
client = Client(socket_path="/tmp/beachcomber-1000/sock", timeout_ms=500, connect_timeout_ms=0)
```

| Option | Default | Meaning |
|---|---|---|
| `socket_path` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeout_ms` | 100 | How long one read or write on the socket may take; at least 1. |
| `connect_timeout_ms` | 1500 | How long connecting keeps retrying before it gives up; 0 is one attempt. |
| `autostart` | on | Start a daemon when none answers an auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`. |

The defaults are the library's (`bc_client_new`): this SDK passes on the
options you give and adds none of its own.

## Library discovery

This SDK is a `ctypes` binding over `libbeachcomber`'s C ABI — no socket
code or wire-protocol framing lives in Python; the native library owns the
connection (including the daemon socket's own auto-discovery and
autostart). It is loaded on first use.

The library is found by the one rule every SDK follows
([`docs/sdk-api.md`, "Library discovery"](../../docs/sdk-api.md#library-discovery)). The file is
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

Once the library is loaded, the daemon socket path itself is resolved by
`libbeachcomber` (not this SDK): `$BEACHCOMBER_SOCKET` if set, else a
stable per-user default. Pass `Client(socket_path=...)` to override it
directly.

## Exceptions

Every exception is one `CombError` subclass per error kind, carrying
`.kind` (the slug, matching the C ABI envelope's `error.kind`), `.message`
(the ABI's message, unchanged — `str(exc)` is the same) and
`.library_version` (the loaded library's `bc_version()`), so callers should
not need to string-match `str(exc)`.

| Exception | `.kind` | When raised |
|---|---|---|
| `DaemonNotRunningError` | `daemon_not_running` | No daemon answers, and none was started |
| `ConnectionFailedError` | `connection_failed` | An explicit `socket_path` could not be connected to |
| `IoError` | `io_error` | A read or write on the socket failed |
| `ParseError` | `parse_error` | A request, a response or a template could not be read |
| `ServerError` | `server_error` | The daemon refused the request, or a template failed to render |
| `TimeoutError` | `timeout` | A read or write ran out `timeout_ms`, or `next_event` ran out its `timeout_ms` |
| `BusyError` | `busy` | A session or watch is in use by another caller |
| `PanicError` | `panic` | The library panicked; the panic was caught at the boundary |
| `VersionSkewError` | `version_skew` | The daemon is a different version from the library |
| `LibraryNotFoundError` | `library_not_found` | No `libbeachcomber` was found where discovery looks |
| `LibrarySymbolError` | `library_symbol` | The library found lacks a function this SDK calls |
| `UnsupportedError` | `unsupported` | A transport cannot do this operation (never raised here: this SDK has one transport) |
| `CombError` | varies | Base class for all of them |

## Running tests

```sh
cd sdks/python
uv run --with pytest python -m pytest                # unit and integration tests
COMB_BIN=/path/to/comb python3 conformance_runner.py # the shared fixtures under tests/conformance/
```

The integration tests spawn a `comb daemon` and load `libbeachcomber` from
the workspace's debug build (`../../target/debug/`, made by `cargo build`)
unless `BEACHCOMBER_LIB` / `COMB_BIN` are set.

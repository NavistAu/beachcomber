# beachcomber Python SDK

Python client for the [beachcomber](https://github.com/NavistAu/beachcomber) (`comb`) shell-state daemon.

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

## `get`

`client.get(key, cwd=None, env=None, force=False, wait=False)` returns the
value `comb get` prints for this key, cwd and env — the bare value, `None`
on a miss. The daemon holds the templates (`<provider>:template`, seeded
from configuration or `put`) and the client renders them: a plain field is
its cached value, a templated one is rendered, and a bare provider name is
the whole merged namespace. `cwd=None` is the global entry. The unrendered
halves are facets, read like any key: `p:template.x` is the template as
stored, `p:put.x` and `p:source.x` those layers' values.

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

`client.eval(template_str, cwd, env=None)` renders a template that is
stored nowhere — the same value `comb eval` prints; `eval("{{ p.f }}")` and
`get("p.f")` agree. The template accepts a single `{{ expr }}` tag, or
literal text/several tags — the former keeps the expression's natural type,
the latter is always a string (a source with no tags at all is its literal
text).

`exec()` inside a template runs only on a client created with
`Client(exec=True)`; it is off by default, and a template that calls it
then fails with `exec() is disabled in this context`.

## Custom socket path

```python
client = Client(socket_path="/tmp/beachcomber-1000/sock")
```

## Library discovery

This SDK is a `ctypes` binding over `libbeachcomber`'s C ABI — no socket
code or wire-protocol framing lives in Python; the native library owns the
connection (including the daemon socket's own auto-discovery and
autostart). At import/first-use, the native library itself is located, in
order:

1. `$BEACHCOMBER_LIB` — exact path to the shared library.
2. `../lib/<libname>` relative to the `comb` binary resolved on `$PATH`.
3. The platform default dynamic-linker search path.

If none resolve, `LibraryDiscoveryError` is raised naming every location
tried — there is no silent fallback to spawning `comb` as a subprocess.

## Socket discovery

Once the library is loaded, the daemon socket path itself is resolved by
`libbeachcomber` (not this SDK): `$BEACHCOMBER_SOCKET` if set, else a
stable per-user default. Pass `Client(socket_path=...)` to override it
directly.

## Exceptions

Every exception is a `CombError` subclass with a `.kind` attribute — a
stable, machine-readable slug matching the C ABI envelope's `error.kind`
(see `libbeachcomber/exceptions.py`), so callers should not need to
string-match `str(exc)`.

| Exception | `.kind` | When raised |
|---|---|---|
| `DaemonNotRunning` | `daemon_not_running` | Daemon unreachable and autostart failed/disabled |
| `ConnectionFailedError` | `connection_failed` | An explicit `socket_path` couldn't be dialed |
| `ServerError` | `server_error` | Daemon returns `ok: false` for a request-level reason |
| `ProtocolError` | `io_error` / `parse_error` / `None` | I/O failure, malformed response, or bad envelope |
| `TimeoutError` | `timeout` | Operation timed out |
| `BusyError` | `busy` | A session/watch handle already in use by another caller |
| `BadFlagsError` | `bad_flags` | Unrecognised bit set in a `get` flags argument |
| `VersionSkewError` | `version_skew` | Daemon version doesn't match the loaded library's |
| `PanicError` | `panic` | The native library panicked |
| `LibraryDiscoveryError` | `library_discovery` | No candidate location yielded a loadable library |
| `LibrarySymbolError` | `library_symbol` | Loaded library is missing a required `bc_*` symbol |
| `CombError` | varies | Base class for all SDK errors |

## Running tests

```sh
cd sdks/python
uv run --with pytest python -m pytest                # unit and integration tests
COMB_BIN=/path/to/comb python3 conformance_runner.py # the shared fixtures under tests/conformance/
```

The integration tests spawn a `comb daemon` and load `libbeachcomber` from
the workspace's debug build (`../../target/debug/`, made by `cargo build`)
unless `BEACHCOMBER_LIB` / `COMB_BIN` are set.

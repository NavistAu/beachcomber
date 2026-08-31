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

## Resolve and eval

`client.resolve(key, cwd, env=None, overrides=None)` evaluates a declared
virtual field client-side; `client.eval(template_str, cwd, env=None,
overrides=None)` evaluates a raw expression the same way. Both `cwd`
arguments are required. The expression itself accepts a bare expression, a
single `{{ expr }}` tag, or literal text/several tags — the first two keep
the expression's natural type, the third is always a string.

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

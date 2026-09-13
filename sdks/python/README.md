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

## The `get` result

`get` returns the node as the daemon holds it — both halves, nothing
rendered. `CombResult` carries `data` (the cached value, `None` when nothing
is cached), `age_ms`/`stale` (meaningful only with `data`), `template` (the
template in effect at the address, unrendered: a string for a leaf, a dict
for a group; `None` when none is in effect) and `rest` (the address left
over when the template walk stopped at a string — `get("p.t.x")` where
`p.t` is a template string gives `template` of `p.t` and `rest == "x"`).
`is_hit` is "data present"; `is_miss` is "no data **and** no template". A
template-only result is neither: it is an address whose value `resolve`
renders — `get` is the raw half, `resolve(key, cwd, env)` is how you get the
value.

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

`client.resolve(key, cwd, env=None)` returns the same value `comb get`
returns for this key, cwd and env: the daemon holds the templates
(`<provider>:template`, seeded from configuration or `put`) and the client
renders them. A plain field is its cached value, a templated one is
rendered, and a bare provider name is the whole merged namespace.
`client.eval(template_str, cwd, env=None)` renders a template that is
stored nowhere — the same value `comb eval` prints. Both `cwd` arguments
are required. The template accepts a single `{{ expr }}` tag, or literal
text/several tags — the former keeps the expression's natural type, the
latter is always a string (a source with no tags at all is its literal
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

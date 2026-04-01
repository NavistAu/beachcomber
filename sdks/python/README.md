# beachcomber Python SDK

Python client for the [beachcomber](https://github.com/jhogendorn/beachcomber) (`comb`) shell-state daemon.

## Requirements

- Python 3.9+
- No external dependencies (stdlib only)
- A running `comb` daemon

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
client.poke("git", path="/path/to/repo")

# List available providers
providers = client.list()

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

1. `$XDG_RUNTIME_DIR/beachcomber/sock`
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `/tmp/beachcomber-<uid>/sock`

## Exceptions

| Exception | When raised |
|---|---|
| `DaemonNotRunning` | Cannot connect to the socket |
| `ServerError` | Daemon returns `ok: false` |
| `ProtocolError` | Malformed JSON or I/O failure |
| `CombError` | Base class for all SDK errors |

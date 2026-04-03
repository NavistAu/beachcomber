---
sidebar_position: 4
title: Protocol Reference
---

# Protocol Reference

beachcomber uses a simple newline-delimited JSON protocol over a Unix socket. Any language that can open a Unix socket and read/write JSON can be a client — no client library required.

## Connection

Socket path resolution order:
1. `daemon.socket_path` in config, if set
2. `$XDG_RUNTIME_DIR/beachcomber/sock`
3. `$TMPDIR/beachcomber-<uid>/sock`

Connect with `SOCK_STREAM`. Each message is a JSON object followed by `\n`. Each response is a JSON object followed by `\n`.

## Request Format

```json
{"op": "get", "key": "git.branch", "path": "/home/user/project"}
{"op": "get", "key": "git", "path": "/home/user/project"}
{"op": "get", "key": "battery"}
{"op": "get", "key": "git.branch", "path": "/home/user/project", "format": "text"}
{"op": "poke", "key": "git", "path": "/home/user/project"}
{"op": "context", "path": "/home/user/project"}
{"op": "list"}
{"op": "status"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `op` | string | Operation: `get`, `poke`, `context`, `list`, `status` |
| `key` | string | Provider name (`git`) or field path (`git.branch`) |
| `path` | string | Absolute path for path-scoped providers. Optional if connection context is set. |
| `format` | string | Response format: `"json"` (default) or `"text"` |

## Response Format

```json
{"ok": true, "data": {"branch": "main", "dirty": true}, "age_ms": 1240, "stale": false}
{"ok": true, "data": "main", "age_ms": 1240, "stale": false}
{"ok": true, "data": null, "age_ms": null, "stale": false}
{"ok": false, "error": "unknown provider: git2"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `ok` | bool | Whether the operation succeeded |
| `data` | any | Result: object (full provider), scalar (single field), or null (cache miss) |
| `age_ms` | int | Milliseconds since the cached value was last computed |
| `stale` | bool | Whether the value is past its expected refresh time |
| `error` | string | Error message when `ok` is false |

## Operations

**`get`:** Read from cache. Always returns immediately. If the key has never been computed, `data` is null and `ok` is true. A null response means "no data yet" — retry after a moment or `poke` to trigger computation.

**`poke`:** Trigger immediate provider recomputation. Returns `{"ok": true}` after acknowledging. The recomputation happens asynchronously — subsequent `get` calls will return the refreshed value once it completes.

**`context`:** Set the working directory for this connection. Subsequent path-scoped `get` requests without an explicit `path` will resolve relative to this directory. Useful for clients that query multiple values for the same path.

**`list`:** Returns an array of all active cache entries with their metadata.

**`status`:** Returns daemon health information.

## Text Format

When `"format": "text"` is specified:
- Single field queries return the raw value followed by `\n` (e.g., `main\n`)
- Full provider queries return `key=value` lines sorted alphabetically, one per line, terminated with `\n`
- Errors return nothing on stdout; `ok` is false in the JSON response

## Connection Context Example

```python
# Set context once, then query multiple values without repeating the path
sock.send(b'{"op":"context","path":"/home/user/myproject"}\n')
response = read_line(sock)  # {"ok": true}

sock.send(b'{"op":"get","key":"git.branch"}\n')
branch = read_line(sock)    # {"ok": true, "data": "main", ...}

sock.send(b'{"op":"get","key":"git.dirty"}\n')
dirty = read_line(sock)     # {"ok": true, "data": false, ...}
```

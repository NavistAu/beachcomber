---
sidebar_position: 4
title: Protocol Reference
---

# Protocol Reference

beachcomber uses a simple newline-delimited JSON protocol over a Unix socket. Any language that can open a Unix socket and read/write JSON can be a client — no client library required.

## Connection

Socket path resolution order:
1. `daemon.socket_path` in config, if set
2. `BEACHCOMBER_SOCKET` env var, if non-empty
3. `/tmp/beachcomber-<uid>/sock`

Connect with `SOCK_STREAM`. Each message is a JSON object followed by `\n`. Each response is a JSON object followed by `\n`.

## Request Format

```json
{"op": "get", "key": "git.branch", "path": "/home/user/project"}
{"op": "get", "key": "git", "path": "/home/user/project"}
{"op": "get", "key": "battery"}
{"op": "get", "key": "git.branch", "path": "/home/user/project", "format": "text"}
{"op": "refresh", "key": "git", "path": "/home/user/project"}
{"op": "context", "path": "/home/user/project"}
{"op": "status"}
{"op": "introspect", "subject": "daemon"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `op` | string | Operation: `hello`, `get`, `refresh`, `context`, `status`, `introspect`, `put`, `watch` |
| `key` | string | Provider name (`git`) or field path (`git.branch`) |
| `path` | string | Absolute path for path-scoped providers. Optional if connection context is set. |
| `format` | string | Response format: `"json"` (default), `"text"`, `"sh"`. CSV/TSV/FMT are CLI-only output modes applied client-side, not wire formats. |

## Response Format

```json
{"ok": true, "data": {"branch": "main", "dirty": true}, "age_ms": 1240, "stale": false}
{"ok": true, "data": "main", "age_ms": 1240, "stale": false}
{"ok": true, "data": null, "stale": false}
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

**`hello`:** Version negotiation. Clients should send this as the first op on any new connection.

```json
{"op":"hello"}
```

Response:

```json
{"ok":true,"data":{"protocol_version":"1.0","daemon_version":"0.6.0"}}
```

`protocol_version` follows semver (major.minor) and is independent of the daemon binary version.

**`get`:** Read a cached value. If the key has never been computed, the daemon executes the provider synchronously before returning. Successive calls are served from cache until the value's refresh interval elapses. A null `data` with `ok: true` indicates the provider exists but returned no value (e.g., a path-scoped provider queried outside a matching directory).

**`refresh`:** Trigger immediate provider recomputation. Returns `{"ok": true}` after acknowledging. The recomputation happens asynchronously — subsequent `get` calls will return the refreshed value once it completes.

**`context`:** Set the working directory for this connection. Subsequent path-scoped `get` requests without an explicit `path` will resolve relative to this directory. Useful for clients that query multiple values for the same path.

**`status`:** Returns warm cache entries as an array of row objects. Each row has `provider`, `path`, `source`, `field`, `value`, `age_ms`, `stale`, `kind`, and `failure` fields (`kind` and `failure` are omitted when null/not applicable; `source` is always present). This is the data that `comb s` renders as a table. For daemon internals (pid, uptime, etc.), use `{"op":"introspect","subject":"daemon"}`.

```json
{"op":"status"}
```

Response:

```json
{"ok":true,"data":[
  {"provider":"git","path":"/project","source":"refs","field":"branch","value":"main","age_ms":234,"stale":false},
  {"provider":"battery","path":null,"source":"state","field":"percent","value":85,"age_ms":4200,"stale":false}
]}
```

**`introspect`:** Inspect daemon internals. The `subject` field selects what to inspect: `daemon`, `providers`, `config`, `cache`, `lifecycle`, `watches`, `timers`, `demand`, `procs`.

```json
{"op":"introspect","subject":"daemon"}
{"op":"introspect","subject":"watches"}
```

**`put`:** Write data as a virtual provider. The `key` names the virtual provider, and `data` must be a JSON object. An optional `ttl` duration string (e.g. `"30s"`) marks entries stale if the writer stops updating. An optional `path` scopes the entry to a directory.

```json
{"op":"put","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}
{"op":"put","key":"myapp","data":{"status":"ok"},"ttl":"30s","path":"/project"}
```

Response: `{"ok":true}`

Returns an error if the key conflicts with a built-in or script provider.

**`watch`:** Stream cache updates for a key. The server holds the connection open and emits a response line on each cache update for the watched key. An optional `path` scopes the watch to a directory. An optional `format` field accepts any of the supported format values (see Request Format table).

```json
{"op":"watch","key":"git.branch","path":"/project"}
{"op":"watch","key":"git.branch","format":"text"}
```

The server streams responses:

```json
{"ok":true,"data":"main","age_ms":0,"stale":false}
{"ok":true,"data":"feature/foo","age_ms":0,"stale":false}
```

The first line is emitted immediately with the current cached value (or a null data miss). Subsequent lines are emitted on each cache update. Watching a field path (e.g. `git.branch`) only emits when that field's value changes, not on every provider update. The client disconnects to stop the stream.

## Text Format

When `"format": "text"` is specified:
- Single field queries return the raw value followed by `\n` (e.g., `main\n`)
- Full provider queries return raw values only, one per line, sorted alphabetically by field name
- Errors emit `error: <message>\n\n`; the JSON response's `ok` is false

## Shell Format

When `"format": "sh"` is specified:
- Single scalar-field queries return the raw value followed by `\n\n` (suitable for shell `eval` or `source` when concatenating with known prefixes).
- Full-provider queries return `key=value` lines sorted alphabetically, one per line, terminated with `\n\n`.
- Object-valued fields (e.g., `mise.project`) flatten to `key=value` lines for each entry.
- Values are not quoted — consumers are responsible for safe handling of whitespace.
- Errors emit `error: <message>\n\n`; the JSON response's `ok` is false.

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

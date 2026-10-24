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

## Response Format

```json
{"ok": true, "data": {"branch": "main", "dirty": true}, "age_ms": 1240, "stale": false}
{"ok": true, "data": "main", "age_ms": 1240, "stale": false}
{"ok": true, "data": null, "stale": false}
{"ok": false, "error": "invalid facet key: a:b:c"}
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

**`refresh`:** Re-execute the Sources the key demands — every Source of the namespace for a bare `provider`, only the one owning the field for `provider.field` — each at the instance that answers for the requested directory, out-of-band; it does not return the new value. Returns `{"ok": true}` after acknowledging; subsequent `get` calls return the refreshed value once re-execution completes. A key naming no Source — a static namespace, a facet key, an unknown name, or a field no Source owns — is an error: `{"ok": false, "error": "nothing to refresh: <key> has no sources"}`.

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

`introspect providers` lists each provider with `name`, `source` (`static`, `builtin` or `unknown`), `sources` (each with `name`, `fields` and `invalidation`), `invalidation`, `in_backoff` and `disabled`. A provider carries no `scope`: whether a Source has a root is a question asked of a directory. `disabled` is true for a provider switched off with `[daemon.providers.<p>] enabled = false`, and its `invalidation` then reads `disabled`. `introspect config` returns `path` (the `config.toml` that was loaded, or null), `parsed`, `errors`, `config_source_count` (the config Sources declared under `[daemon.providers.<p>.sources.<s>]`) and `verdicts`.

**`put`:** Write static data at any node. The `key` is `namespace` or `namespace.node…` — the same dotted address `get` takes — and the write replaces whatever a put left at that node; `"data": null` removes it. A `put` replaces the node at that directory, hiding Source and seed values beneath it; removing the put shows them again. `data` at a namespace root must be a JSON object; below the root, any JSON. An optional `ttl` duration string (e.g. `"30s"`) marks entries stale if the writer stops updating. An optional `path` scopes the entry to a directory.

```json
{"op":"put","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}
{"op":"put","key":"myapp.deploy.count","data":3}
{"op":"put","key":"myapp","data":{"status":"ok"},"ttl":"30s","path":"/project"}
```

Response: `{"ok":true}`

Returns `put data at the root must be an object` for a non-object at a namespace root.

**`watch`:** Stream cache updates for a key. The server holds the connection open and emits a response line on each cache update for the watched key. An optional `path` scopes the watch to a directory.

```json
{"op":"watch","key":"git.branch","path":"/project"}
{"op":"watch","key":"git.branch"}
```

The server streams responses:

```json
{"ok":true,"data":"main","age_ms":0,"stale":false}
{"ok":true,"data":"feature/foo","age_ms":0,"stale":false}
```

The first line is emitted immediately with the current cached value (or a null data miss). Subsequent lines are emitted on each cache update. Watching a field path (e.g. `git.branch`) only emits when that field's value changes, not on every provider update. The client disconnects to stop the stream.

## Text and Shell Rendering

The wire protocol is JSON-only — there is no `format` field. Text (`-t`) and shell (`-s`) rendering are performed client-side by the CLI and SDKs after receiving the JSON response.

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

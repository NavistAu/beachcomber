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
{"op": "status"}
{"op": "introspect", "subject": "daemon"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `op` | string | Operation: `hello`, `get`, `refresh`, `status`, `introspect`, `put`, `memo`, `watch`, `layers` |
| `key` | string | Provider name (`git`) or field path (`git.branch`) |
| `path` | string | Absolute directory the request addresses; omitted, the global entry. A path that is not absolute is refused. |

## Response Format

```json
{"ok": true, "data": {"branch": "main", "dirty": true}, "age_ms": 1240}
{"ok": true, "data": "main", "age_ms": 1240}
{"ok": true}
{"ok": false, "error": "invalid facet key: a:b:c (a facet is template, macro, memo, put, source or cache)"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `ok` | bool | Whether the operation succeeded |
| `data` | any | Result: object (full provider) or scalar (single field); omitted on a miss |
| `age_ms` | int | Milliseconds since the cached value was last computed |
| `error` | string | Error message when `ok` is false |

## Operations

**`hello`:** Version negotiation. Clients should send this as the first op on any new connection.

```json
{"op":"hello"}
```

Response:

```json
{"ok":true,"data":{"protocol_version":"1.0","daemon_version":"0.9.1"}}
```

`protocol_version` follows semver (major.minor) and is independent of the daemon binary version.

**`get`:** Read a cached value. If the key has never been computed, the daemon executes the provider synchronously before returning. Successive calls are served from cache until the value's refresh interval elapses. `ok: true` with no `data` is a miss: nothing answers that address (e.g., a rooted provider queried outside a matching directory).

**`refresh`:** Re-execute the Sources the key demands — every Source of the namespace for a bare `provider`, only the one owning the field for `provider.field` — each at the instance that answers for the requested directory, out-of-band; it does not return the new value. Returns `{"ok": true}` after acknowledging; subsequent `get` calls return the refreshed value once re-execution completes. A key naming no Source — a static namespace, a facet key, an unknown name, or a field no Source owns — is an error: `{"ok": false, "error": "nothing to refresh: <key> has no sources"}`.

**`status`:** Returns warm cache entries as an array of row objects. Each row has `provider`, `path`, `source`, `field`, `value`, `age_ms`, `kind`, and `failure` fields (`kind` and `failure` are omitted when null/not applicable; `source` is always present). This is the data that `comb s` renders as a table. For daemon internals (pid, uptime, etc.), use `{"op":"introspect","subject":"daemon"}`.

```json
{"op":"status"}
```

Response:

```json
{"ok":true,"data":[
  {"provider":"git","path":"/project","source":"source(head)","field":"branch","value":"main","age_ms":234},
  {"provider":"battery","path":null,"source":"source(state)","field":"percent","value":85,"age_ms":4200}
]}
```

**`introspect`:** Inspect daemon internals. The `subject` field selects what to inspect: `daemon`, `providers`, `config`, `cache`, `lifecycle`, `watches`, `timers`, `demand`, `procs`.

```json
{"op":"introspect","subject":"daemon"}
{"op":"introspect","subject":"watches"}
```

`introspect demand`, `lifecycle` and `timers` list the scheduler's entries (Active ones, decaying ones, and those with a poll) by `provider`, `path` and `source`, the Source's name or `put` or `memo` for a static address, which also has its `node`: `{"provider":"kubecontext","path":"/project","source":"memo","node":"context",...}`. `introspect providers` lists each provider with `name`, `source` (`static`, `source` or `unknown`), `sources` (each with `name`, `fields` and `invalidation`), `invalidation`, `in_backoff` and `disabled`. A provider carries no `scope`: whether a Source has a root is a question asked of a directory. `disabled` is true for a provider switched off with `[daemon.providers.<p>] enabled = false`, and its `invalidation` then reads `disabled`. `introspect config` returns `path` (the `config.toml` that was loaded, or null), `parsed`, `errors`, `config_source_count` (the config Sources declared under `[daemon.providers.<p>.sources.<s>]`) and `verdicts`.

**`put`:** Write static data at any node. The `key` is `namespace` or `namespace.node…` — the same dotted address `get` takes — and the write replaces whatever a put left at that node; `"data": null` removes it. A `put` replaces the node at that directory, hiding Source and seed values beneath it; removing the put shows them again. `data` at a namespace root must be a JSON object; below the root, any JSON. An optional `ttl` duration string (e.g. `"30s"`) marks entries stale if the writer stops updating. An optional `path` scopes the entry to a directory.

```json
{"op":"put","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}
{"op":"put","key":"myapp.deploy.count","data":3}
{"op":"put","key":"myapp","data":{"status":"ok"},"ttl":"30s","path":"/project"}
```

Response: `{"ok":true}`

Returns `put data at the root must be an object` for a non-object at a namespace root.

**`memo`:** Store what a template leaf's `| cache()` computed — the client's resolver sends it after a render that missed, and no SDK binds it. `key` is the leaf (`provider.field`, never a facet), `path` the reader's directory, `hash` the client's sixteen lowercase hex digits over the leaf's inputs, `data` the leaf's value (absent for a leaf that rendered nothing), `watch` the absolute paths whose change invalidates it, `ttl` its lifetime. A `get` that attaches a template half then carries the memo beside it as `memo` (`{hash, stale, age_ms, data}`, a tree of them for a group), found up the chain from the requested directory; `<p>:cache.<field>` answers `source: memo` with its stored keys. A template or data write at the leaf, a `refresh` of the key, the `ttl` running out and a change to a `watch` path drop the memo's `hash`. The write starts the memo's lifecycle and nothing else renews it: its watch paths are watched while it is Active, and it is evicted one decay step after its `ttl` (twice the ttl), or with no `ttl` at the end of its provider's decay ladder. Returns `{"ok": true}`.

```json
{"op":"memo","key":"kubecontext.context","path":"/project","hash":"9c6001d328a94690","data":"prod","watch":["/home/me/.kube/config"],"ttl":"10m"}
```

**`watch`:** Stream cache updates for a key. The server holds the connection open and emits a response line on each cache update for the watched key. An optional `path` scopes the watch to a directory.

```json
{"op":"watch","key":"git.branch","path":"/project"}
{"op":"watch","key":"git.branch"}
```

The server streams responses:

```json
{"ok":true,"data":"main","age_ms":0}
{"ok":true,"data":"feature/foo","age_ms":0}
```

The first line is emitted immediately with the current cached value (or a null data miss). Subsequent lines are emitted on each cache update. Watching a field path (e.g. `git.branch`) only emits when that field's value changes, not on every provider update. The client disconnects to stop the stream.

## Text and Shell Rendering

The wire protocol is JSON-only — there is no `format` field. Text (`-t`) and shell (`-s`) rendering are performed client-side by the CLI and SDKs after receiving the JSON response.

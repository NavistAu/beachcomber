# beachcomber Protocol Specification

**Version:** 1.0
**Encoding:** NDJSON (newline-delimited JSON) over Unix domain socket
**Status:** Authoritative reference for SDK implementers.

This document defines the wire contract between the `comb` daemon and any
client (CLI, SDK). For the design rationale see
`docs/superpowers/specs/2026-04-22-interface-architecture-design.md`. For
versioning policy see `docs/versioning.md`.

## Overview

A client opens a Unix-domain-socket connection to the daemon and exchanges
line-delimited JSON. Each request is a single line starting with `{"op": ...}`.
Each response is a single line with `{"ok": bool, ...}`. There is no framing
beyond the newline delimiter.

The daemon is stateless per connection except for:
- A per-connection context path set via the `Context` op.
- `Watch` streams, which hold the connection open until the client disconnects.

All other ops are stateless and can be sent on any connection in any order.

## Connection lifecycle

1. **Discover socket path**:
   - `$BEACHCOMBER_SOCKET` if set and non-empty
   - `/tmp/beachcomber-<uid>/sock` otherwise
2. **Connect** — `AF_UNIX`, `SOCK_STREAM`.
3. **Handshake (recommended)** — send `Hello`, verify the returned
   `protocol_version` is compatible with what the client was built against.
   See "Versioning" below for semver rules.
4. **Interact** — send one or more ops, read one response per op.
5. **Close** — simply close the socket. The daemon does not send a
   goodbye.

Clients that do not send `Hello` still work; the daemon does not require
the handshake. Skipping Hello means the client has no signal when the
daemon advertises a breaking change.

## Request envelope

All requests are a single line of JSON with an `"op"` discriminator:

```json
{"op": "<op_name>", ...}
```

Additional fields vary per op (see Op reference).

## Response envelope

All responses are a single line of JSON:

```json
{
  "ok": true,
  "data": <any>,
  "age_ms": 0,
  "stale": false
}
```

- `ok` (bool, required) — `true` if the op succeeded, `false` on error.
- `data` (any, optional) — response payload. Shape depends on the op.
- `age_ms` (integer, optional) — for `Get` responses, milliseconds since the
  record answering the request was last refreshed. See "Freshness" under `Get`.
- `stale` (bool, optional) — for `Get` responses, whether that record is older
  than its Source's expected refresh interval.
- `error` (string, optional) — present when `ok=false`. A human-readable
  message; not stable enough to parse.
- `template` (any, optional) — for `Get` responses, the template half. See
  "Template half" under `Get`.
- `rest` (string, optional) — for `Get` responses, the part of the key the
  template walk could not consume.

All optional fields are omitted (not set to null) when not applicable.

## Op reference

### `Hello`

Version-negotiation op. Clients SHOULD send this as the first op on any
new connection.

**Request:**
```json
{"op": "hello"}
```

**Response (success):**
```json
{
  "ok": true,
  "data": {
    "protocol_version": "1.0",
    "daemon_version": "0.5.1"
  }
}
```

`protocol_version` follows semver (major.minor). `daemon_version` is the
daemon binary's build version (independent of protocol version).

### `Get`

Read a cached value.

**Request:**
```json
{
  "op": "get",
  "key": "git.branch",
  "path": "/absolute/or/relative/path",
  "force": false,
  "wait": false
}
```

- `key` (string, required) — `provider` or `provider.field`. See "Keys" below.
- `path` (string, optional) — the directory the read is addressed at. Omitting
  it — or giving `$HOME`, which is the same address — reads the **global
  entry**. A `path` returns the **chain-resolved tree**: static records (source
  `put` or `config:<file>`) are collected from that path and every ancestor up
  to `$HOME` (or to `/` for a path outside `$HOME`) and then the global entry,
  the nearest winning; provider-produced records stay at their Source's
  declared scope — path-scoped at the addressed path, global at the global
  entry — and never walk the chain. See "Path resolution" in
  `docs/canon/field_resolution.md`.
- `force` (bool, optional, default false) — evict the cache entry before
  executing, guaranteeing a fresh value.
- `wait` (bool, optional, default false) — if the entry is stale, wait for
  inline re-execution; if fresh, serve the cached value. Idempotent on
  virtual providers.

**Response (hit):**
```json
{
  "ok": true,
  "data": {"branch": "main", "dirty": false},
  "age_ms": 42,
  "stale": false
}
```

**Response (miss):**
```json
{"ok": true}
```

(`data` omitted to signal absence.)

**Response (error):**
```json
{"ok": false, "error": "unknown provider: foo"}
```

#### Freshness

`age_ms` and `stale` describe the *record* that answered the request, not the
cache entry it lives in:

- a leaf reports its own record's Source: how long since that Source last
  refreshed, and whether that is past the Source's expected interval;
- an object assembled from several records reports the largest of their ages,
  and is stale if any one of them is;
- a static record (source `put` or `config:<file>`) reports the time since it
  was written, and is stale only if its `put` carried a `ttl`;
- a response with no `data` carries **neither** field. `:age` and `:stale` on
  such a response return `null`.

#### Template half

A `get` on a key whose namespace carries no facet does two lookups at the
requested path: the data at the addressed node, and the same node in the merged
tree of that namespace's `template` facet (`<provider>:template`). Whichever
exist come back, so a response may carry the template half with no `data` at
all — a namespace holding only templates is a hit, not `unknown provider`.

`template` is a JSON tree whose string leaves are value expressions the
**client** renders and whose objects are groups. The daemon never renders one.
Walking the key through that tree:

- landing on an object or a string with the key consumed → that node is
  `template`;
- landing on a string with segments left over → that string is `template` and
  the unused remainder, rejoined with `.`, is `rest`;
- a miss, or a non-string scalar in the way → neither field is attached.

A `get` on a facet key (`<provider>:<facet>…`) is already inside a facet and
never carries a template half; it reads the facet's own records as `data`.

```json
{"ok": true, "template": "{{ {'cpu': 40, 'mem': 12} }}", "rest": "cpu"}
```

`Watch` frames carry the same two halves, so a streamed value renders exactly
as a fetched one.

#### Keys

Keys are `<provider>` or `<provider>.<field>`. Providers may also accept
metadata suffixes:

- `:age` — returns `age_ms` as the `data` value.
- `:stale` — returns `stale` as the `data` value.
- `:fresh` — returns `!stale` as the `data` value.
- `:cache` — returns `true` if the value was served from cache (no re-exec).
- `:source` — returns `"builtin"`, `"script"`, or `"virtual"`.

### `Refresh`

Trigger a provider re-execution out-of-band. Does not return the new value.

**Request:**
```json
{"op": "refresh", "key": "git", "path": "/some/path"}
```

`path` is optional for path-scoped providers.

**Response:**
```json
{"ok": true}
```

### `Context`

Set a default path for subsequent ops on the same connection.

**Request:**
```json
{"op": "context", "path": "/some/dir"}
```

**Response:**
```json
{"ok": true}
```

Subsequent `Get` / `Refresh` calls without an explicit `path` field use
this default. Stateless per connection — fresh connections have no context.

### `Put`

Store static data at any node of the tree. `key` is `namespace` or
`namespace.node…` — the same dotted address `get` takes. The write
deep-merges into whatever is already stored; `replace` drops the subtree at
the addressed node first; omitting `data` removes it.

Ownership is per **record**, not per namespace: a record one of the
provider's Sources produces is refused, because the next refresh would
overwrite it. Everything else is fair game, so a static record can sit
beside a provider's own records in the same entry. A Source whose field
names are resolved at run time declares a sentinel such as `<tool>` instead
— every http provider, and any script or library provider that does not
declare its field names — and owns every field name in its namespace, warm
or cold.

An object key in `data` may not contain a `.`: it would name one record and
address another. The node part of `key` is capped at ten segments.

`replace` and omitting `data` act on **static records only** — a `put`
never removes a record a Source produces.

**Request (set):**
```json
{
  "op": "put",
  "key": "myapp",
  "data": {"status": "healthy", "last_deploy": "2026-04-22T10:00:00Z"},
  "ttl": "30s",
  "path": "/optional/scope",
  "replace": false
}
```

`data` at the root of a namespace must be an object — the root is a map of
records, not a value. Below the root, any JSON: `{"op": "put", "key":
"myapp.deploy.count", "data": 3}`.

`path` addresses the entry the record is written to, canonicalised the same
way `Get`'s is: omitted — or `$HOME` — writes the global entry, which every
path's chain ends at.

**Request (clear):**
```json
{"op": "put", "key": "myapp"}
```

(Omit `data` to clear the addressed node while keeping the registration. At
the namespace root that is every static record of the entry; the entry
itself survives if a Source still contributes to it.)

**Response:**
```json
{"ok": true}
```

**Error cases:**
- Non-object `data` at a namespace root: `{"ok": false, "error": "put data at the root must be an object"}`
- A record a Source produces: `{"ok": false, "error": "cannot write <namespace>.<node>: owned by source <source>"}`
- An object key containing a `.`: `{"ok": false, "error": "put data keys may not contain '.': <key>"}`
- More than ten node segments in `key`: `{"ok": false, "error": "put key is nested too deeply (limit 10 segments)"}`

### `Status`

List all cache entries currently held by the daemon.

**Request:**
```json
{"op": "status"}
```

**Response:**
```json
{
  "ok": true,
  "data": [
    {
      "provider": "git",
      "field": "branch",
      "path": "/Users/me/project",
      "value": "main",
      "age_ms": 120,
      "stale": false
    },
    ...
  ],
  "age_ms": 0,
  "stale": false
}
```

**Note:** `data` is an array of cache rows, NOT a daemon-health object.
Daemon-health information lives on `Introspect{daemon}`. This was a breaking
change in a pre-1.0 release; clients that expected the old shape have been
migrated.

### `Watch`

Subscribe to changes on a single key. The connection switches to streaming
mode; the daemon will emit a new response line whenever the key's value
changes, and continue until the client disconnects.

**Request:**
```json
{"op": "watch", "key": "git.branch", "path": "/repo"}
```

**Response (initial + per-event):**
```json
{"ok": true, "data": {"branch": "main"}, "age_ms": 0, "stale": false}
```

The daemon emits one initial event with the current value, then one
response per subsequent change. The initial event and every later one are
the same chain-resolved read `Get` returns, and the watch fires on a change
to **any entry on the chain** — a `put` at an ancestor, or at the global
entry, reaches a watch opened at a project path.

### `Introspect`

Diagnostic queries into daemon internals. Subject determines shape.

**Request:**
```json
{"op": "introspect", "subject": "daemon"}
```

Subjects: `daemon`, `providers`, `config`, `cache`, `backoff`, `watches`,
`timers`, `demand`, `procs`.

`procs` accepts an optional `duration_secs` integer for profiling.

**Response (subject=daemon):**
```json
{
  "ok": true,
  "data": {
    "pid": 12345,
    "version": "0.5.1",
    "uptime_secs": 3600,
    "socket_path": "/tmp/beachcomber-501/sock",
    "config_path": "/home/me/.config/beachcomber/config.toml",
    "requests_total": 10293,
    "in_flight": 0,
    "active_watchers": 3,
    "cache_entries": 42,
    "verdicts": [{"level": "PASS", "message": "daemon responsive"}]
  },
  "age_ms": 0,
  "stale": false
}
```

Other subjects return different shapes; see `src/server.rs` for the current
authoritative shape of each subject. Per-subject typed documentation is
Phase 3+ work.

## Versioning

`PROTOCOL_VERSION` uses two-part semver (`MAJOR.MINOR`):

| Change | Bump |
|---|---|
| New op | Minor |
| New optional request field (default preserves prior behaviour) | Minor |
| New optional response field | Minor |
| Remove or rename an op | Major |
| Remove a response field | Major |
| Change an existing field's type or semantics | Major |
| Change wire encoding | Major |

This mirrors the policy in `docs/versioning.md`. The `Hello` handshake
exists so clients can detect incompatibility at connection time rather
than at first-op failure.

## Wire-level text/sh format retirement — breaking change

`Get` and `Watch` no longer accept a `format` field. The daemon now always
responds (and streams `Watch` frames) as JSON — the server-rendered
`text`/`sh` sub-protocol described in earlier revisions of this document is
gone. Rendering a value as plain text or as a shell-export line is now a
client-side concern: it happens after the JSON response is parsed, not on
the wire. `comb get -f text`/`-f sh` and `comb watch -f text`/`-f sh` still
work — the CLI renders locally from the JSON response — only the wire
contract changed.

This was a breaking change (semantics, not framing): a request that still
sets `"format": "text"` or `"format": "sh"` is not rejected — `Get`/`Watch`
requests have never rejected unrecognised fields — it is simply ignored,
and the caller gets a normal JSON response instead of the rendered text/sh
form it may have expected. Clients that depended on server-side rendering
must render locally after this change.

The removed sub-protocol was also unsound: a stored value that began with
`error:` was indistinguishable on the wire from a genuine failure (both
rendered as a bare `error: ...` line), and a value containing a blank line
(`"\n\n"`) collided with the sub-protocol's own frame terminator, desyncing
any request issued afterward on the same persistent connection. JSON framing
has neither failure mode: errors are carried by `ok:false`, and values are
escaped strings.

## Env-cascade P2 — no wire change

Env-cascade P2 (env-selected file resolution, Tier B providers: `kubecontext`, `talos`) adds **no** wire-protocol change. The client resolves the env-selected file path via a path expression (`env.KUBECONFIG or '~/.kube/config'`, etc.) and passes the result as the existing `path` field of the `Get` request. The daemon receives only the resolved path as a cache coordinate — it never reads `$KUBECONFIG`, `$TALOSCONFIG`, or any other selector env var.

The data-provider rename (`aws` → `aws_profiles`, `gcloud` → `gcloud_configs`) is a provider-name change visible in the `key` field of `Get`/`Refresh` requests and in `Status` rows — not a change to request/response shape or encoding.

---

## Glossary

- **Provider** — a source of data (e.g. `git`, `hostname`, `mise`). Produces
  a `ProviderResult` — a map of field name → `Value`.
- **Field** — a named element of a provider's output (e.g. `branch` on `git`).
- **Virtual provider** — a provider whose data is entirely injected via
  `Put`. No execution.
- **Cache row** — one line of `Status` output: `{provider, field, path, value, age_ms, stale}`.
- **Invalidation strategy** — how a provider decides when to re-execute
  (poll-based, watch-based, once-only). See `src/scheduler.rs`.

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
- `stale` (bool, optional) — for `Get` responses, whether the record is past
  its expected refresh window. See "Freshness" under `Get`.
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
- `force` (bool, optional, default false) — re-execute every Source the key
  demands, each at the cache entry its own scope keys, whether or not it was
  stale; guarantees a fresh value without evicting anything. A key naming no
  Source (a static namespace, a facet) is an error: `cannot --force static
  namespace '<name>': no source to re-execute from`.
- `wait` (bool, optional, default false) — if the entry is stale, wait for
  inline re-execution; if fresh, serve the cached value. A no-op on a static
  namespace — it has no Source to re-execute, so the entry is served as-is.

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
{"ok": false, "error": "invalid facet key: a:b:c"}
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

A `get` on a key whose namespace carries no facet does two lookups: the data at
the addressed node, and the same node in the merged tree of that namespace's
`template` facet (`<provider>:template`). Whichever exist come back, so a
response may carry the template half with no `data` at all — a namespace holding
only templates is a hit. An unknown provider is a miss, as is any address nothing
answers; only invalid facet syntax is an error. A sub-path neither half answers
is a miss (`{"ok":true}`), at any depth: an address into an existing object that
has no such key, or past a scalar, misses exactly as a missing top-level field
does. There is no `unknown field` error.

The two halves are addressed from different places. The data half sits where the
provider's records live, which is the requested path narrowed by the provider's
scope (global for a provider with no path-scoped source, the project root for
one that walks to it). A template is a **static record**, so the template half is
read from the requested directory itself and resolves up the chain from there:
`get git.branch --path <repo>/sub` reads git's records at `<repo>` and its
templates from `<repo>/sub`.

`template` is a JSON tree whose string leaves are value expressions the
**client** renders and whose objects are groups. The daemon never renders one.
Walking the key through that tree:

- landing on an object or a string with the key consumed → that node is
  `template`;
- landing on a string with segments left over → that string is `template` and
  the unused remainder, rejoined with `.`, is `rest`;
- a miss, or a non-string scalar — in the way, or *at* the address — → neither
  field is attached. A non-string leaf **inside** a group is passed through
  untouched as part of that group's object: only the client decides an
  expression is not one.

A `get` on a facet key (`<provider>:<facet>…`) is already inside a facet and
never carries a template half; it reads the facet's own records as `data`. Nor
does a `get` carrying a metadata suffix (`:age`, `:stale`, `:fresh`, `:cache`) —
each asks one question about the data half and is answered with that alone.

```json
{"ok": true, "template": "{{ {'cpu': 40, 'mem': 12} }}", "rest": "cpu"}
```

`Watch` frames carry the same two halves, so a streamed value renders exactly as
a fetched one. A watch subscribes both chains — the namespace's and its
`template` facet's — and a frame is emitted whenever **either** half changes, so
editing a template alone delivers a frame with no new `data`.

#### Keys

Keys are `<provider>`, `<provider>.<field>`, or a facet address —
`<provider>:<facet>` or `<provider>:<facet>.<field>` — where a facet is a
namespace holding data about a provider (e.g. `git:template`), addressed and
read exactly like any other name once the colon syntax is admitted (one
colon, facet name matching `[A-Za-z0-9_-]+`; anything else is `invalid facet
key: <key>`). Any key may also carry a metadata suffix, the raw key's last
`:name` segment, stripped before the rest is parsed as a provider/field/facet
address — so `git.branch:source` is field `git.branch` plus the `:source`
suffix, and `p:template:age` is facet `p:template` plus the `:age` suffix. A
suffix strip always wins over facet parsing: the five suffix names below are
not valid facet names, so `key:age` is provider `key` plus the `:age`
suffix, never a facet `key:age`.

- `:age` — returns `age_ms` as the `data` value.
- `:stale` — returns `stale` as the `data` value.
- `:fresh` — returns `!stale` as the `data` value.
- `:cache` — returns `true` if the value was served from cache (no re-exec).
- `:source` — returns the **source label of the record that answered the key**:
  a Source of the namespace's provider (`head`, `refs`, …), or a static label
  (`put`, `config:<file>`). A leaf reports its own record's label; a
  template-only hit reports the template record's; an object — a whole
  namespace, or a group assembled from several records — has no single writer
  and returns `null`, as does a miss.

### `Refresh`

Re-execute Sources out-of-band. Does not return the new value.

**Request:**
```json
{"op": "refresh", "key": "git", "path": "/some/path"}
```

The key selects which Sources run, exactly as it selects which a `get` warms:
`refresh git` re-executes every Source of `git`, `refresh git.branch` only the
Source that owns `branch`. Each runs at the cache entry its own scope keys — the
global entry for a `Global` Source whatever path the request carried, the
addressed path for a `PathScoped` one — so `path` is optional and is only
consulted by the path-scoped ones. A metadata suffix is stripped first, as for a
`get`: `refresh hostname:age` is `refresh hostname`. Invalid facet syntax on
the key is the same error `get`/`put`/`watch` give: `{"ok": false, "error":
"invalid facet key: <key>"}`.

A key that names no Source has nothing to re-execute and is an **error**: a
static namespace (one created by `put` or a config seed), a facet key, an
unknown name, or a field no Source owns. Reading configuration afresh is not a
refresh — the config watcher does that.

**Response:**
```json
{"ok": true}
```

**Response (error):**
```json
{"ok": false, "error": "nothing to refresh: my.a has no sources"}
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
`namespace.node…` — the same dotted address `get` takes, and `namespace` may
itself be a facet address (`<provider>:<facet>`; invalid facet syntax is
`invalid facet key: <key>`, the same check `get`/`refresh`/`watch` apply).
The write replaces whatever a put left at the addressed node; `data: null`
removes it.

A put replaces the node at that directory, hiding Source and seed values.
The Source keeps refreshing its own layer beneath it, and removing the put
shows that layer at once.

An object key in `data` may not contain a `.`: it would name one record and
address another. The node part of `key` is capped at ten segments.

**Request (set):**
```json
{
  "op": "put",
  "key": "myapp",
  "data": {"status": "healthy", "last_deploy": "2026-04-22T10:00:00Z"},
  "ttl": "30s",
  "path": "/optional/scope"
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
- Invalid facet syntax on the namespace segment: `{"ok": false, "error": "invalid facet key: <key>"}`
- Non-object `data` at a namespace root: `{"ok": false, "error": "put data at the root must be an object"}`
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
      "path": "/Users/me/project",
      "source": "head",
      "field": "branch",
      "value": "main",
      "age_ms": 120,
      "stale": false,
      "kind": {"kind": "lifecycle", "decay": 0, "watches_files": true}
    },
    {
      "provider": "myapp",
      "path": null,
      "source": "put",
      "field": "status",
      "value": "healthy",
      "age_ms": 4200,
      "stale": false,
      "kind": {"kind": "static"}
    },
    ...
  ],
  "age_ms": 0,
  "stale": false
}
```

`source` is the row's record's source label — a Source of the provider
(`head`, `refs`, …), or a static label (`put`, `config:<file>`). `kind` is
present when the row's lifecycle discriminator is known: `{"kind":
"lifecycle", "decay": N, "watches_files": bool}` for a lifecycle-tracked row,
`{"kind": "once"}` / `{"kind": "static"}` / `{"kind": "transient"}`
otherwise; omitted when unknown. A handful of further optional fields
(`poll_interval_secs`, `poll_count`, `fsevents_reinstate`,
`polls_elapsed`, `next_poll_in_secs`, `failure`) are omitted here and carry
diagnostic detail `comb status` renders as columns; see `src/cache.rs`'s
`CacheRow` for the authoritative field list.

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
the same chain-resolved read `Get` returns — **both halves**, `data` and
`template` — and the watch fires on a change to **any entry on either
chain**: a `put` at an ancestor, or at the global entry, reaches a watch
opened at a project path, and so does a write to the key's `template` facet.
A frame is emitted whenever either half changes, so a template edited on its
own delivers a frame carrying the new `template` and no new `data`. The
daemon drops the subscription as soon as the client closes the connection,
not on the next write to the watched key. The connection is single-purpose:
after the watch request the client must not write again, and any further
bytes from it — a pipelined line, a stray newline — end the watch just as a
close does.

This op is unchanged by, and knows nothing about, *resolved* watching — a
value re-resolved over every key its resolution reads plus the provider's
`<p>:template`, emitted only when that value actually changes. That layering
is client-side: `libbeachcomber::watch` (`ResolvedWatch`) opens several `Watch`
subscriptions like the one above and re-resolves on their frames, and `comb
watch`, `bc_watch_open` and every SDK's `watch` are clients of it. The wire op
itself still answers about exactly one address, both halves included, as
described above.

### `Introspect`

Diagnostic queries into daemon internals. Subject determines shape.

**Request:**
```json
{"op": "introspect", "subject": "daemon"}
```

Subjects: `daemon`, `providers`, `config`, `cache`, `lifecycle`, `watches`,
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

`active_watchers` counts open `Watch` connections, not distinct watched keys
or clients: a resolved `comb watch` opens one `Watch` subscription per
dependency its key's resolution reads, plus one on the provider's
`<p>:template` facet, so a single client typically reads as 2 or more.

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

## Data-provider rename — no wire change

The data-provider rename (`aws` → `aws_profiles`, `gcloud` → `gcloud_configs`) is a provider-name change visible in the `key` field of `Get`/`Refresh` requests and in `Status` rows — not a change to request/response shape or encoding. The `aws` and `gcloud` names live on as template-backed namespaces: a `get aws.region` returns the template the client renders, not a daemon-computed value.

---

## Glossary

- **Provider** — a source of data (e.g. `git`, `hostname`, `mise`), made up
  of one or more Sources. Produces a `ProviderResult` — a map of field name →
  `Value`.
- **Source** — one of a provider's execution strategies (e.g. `git`'s `head`,
  `refs`, `diff`, `lineage`, `status`), each owning a set of field names and
  declaring its own scope (`Global` or `PathScoped`) and invalidation
  strategy. What `Refresh` re-executes and demand-driven warming keeps warm.
- **Field** — a named element of a provider's output (e.g. `branch` on `git`).
- **Namespace** — the `provider` or `provider:facet` a key's first segment
  names.
- **Record** — one stored value at (key, path), with exactly one source: a
  provider's Source (`head`, `refs`, …), `put`, or `config:<file>`.
- **Static record** — a record whose source is `put` or `config:<file>`; no
  Source produces it, so it is never polled or evicted, and resolves up the
  directory chain to `$HOME` (see "Path resolution" under `Get`).
- **Static namespace** — a namespace no provider or config registers; every
  record in it is static.
- **Facet** — `provider:facet`, a namespace holding data about a provider
  (e.g. `git:template`); always static. `template` is the facet a `Get`
  resolves for its template half.
- **Cache row** — one line of `Status` output: `{provider, path, source,
  field, value, age_ms, stale, kind}`.
- **Invalidation strategy** — how a Source decides when to re-execute
  (poll-based, watch-based, once-only). See `src/scheduler.rs`.

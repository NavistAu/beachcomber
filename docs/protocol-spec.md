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
- a rendered template value is a lookup, not a stored record: it reports age 0
  and not stale;
- a response with no `data` carries **neither** field.

The same two numbers, and the record's source and lifecycle, are readable as
data through the `:cache` facet (see "Keys").

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
never carries a template half; it reads the facet's own records as `data`.

```json
{"ok": true, "template": "{{ {'cpu': 40, 'mem': 12} }}", "rest": "cpu"}
```

`Watch` frames carry the same two halves, so a streamed value renders exactly as
a fetched one. A write to a namespace or to any of its facets notifies the
namespace's watchers, so editing a template alone delivers a frame with no new
`data`.

#### Keys

Keys are `<provider>`, `<provider>.<field>`, or a facet address —
`<provider>:<facet>` or `<provider>:<facet>.<field>`. A key has one colon form:
the facet, with its colon in the first segment, before any dot. A facet name
matches `[A-Za-z0-9_-]+`; a second colon in the first segment is `invalid facet
key: <key>`, and a colon after the first dot (`git.branch:age`) is `invalid
key: <key>`. There are no metadata suffixes.

Every facet has the provider's shape: `get git` returns that part of the tree,
and so does `get git:cache`. An unknown provider is a miss, and so is a facet
nothing has written.

A field's value is the first layer that has it, in this order: the effective
template (rendered by the client), then `put`, then the Source. Each layer is
readable on its own:

- `<p>:template` — the templates (data the client renders; the daemon never
  evaluates one).
- `<p>:put` — what `put` and `config:<file>` seeds stored.
- `<p>:source` — what the provider's Sources produced. A `put` does not hide
  it here.
- `<p>:cache` — the daemon's metadata about `<p>`, below.

`<p>:put` and `<p>:source` are read-only: a `put` to either is `<p>:put and
<p>:source are read-only; write <p>.<node>`, and they never warm a Source or
start a lifecycle. `null` means absent, never `""`: a Source or put that has no
value for a field has no node for it.

##### The `:cache` facet

`<p>:cache.<field>` holds the daemon's metadata about `<p>.<field>` at the same
path and for the same record that a read of `<p>.<field>` resolves to, so a value
cannot be reported with another record's metadata. It mirrors `<p>.<path>` for
any node: a leaf answers with an object, and an object node answers with the
tree of its leaves' answers.

```json
{"age_ms": 1200, "stale": false, "source": "source(head)",
 "watch": {"0": ".git"}, "decay": 4}
```

- `age_ms`, `stale`, `source` are derived at the read. `source` is `put`,
  `template`, `config:<file>`, or `source(<name>)` for a Source. A value served
  by a template has these three only (`age_ms` 0, `stale` false, `source`
  `template`). A miss is a miss.
- `poll` (`{interval, count}`), `watch` and `decay` are stored: written when a
  Source registers or a `put` carries a `ttl`, and read back here. `watch` is an
  index-keyed object, since arrays are returned as objects keyed by index.
- `decay` is the number of decay steps before eviction. It is data: a Source
  takes the nearest `_` prototype, looking at each directory from the requested
  one up to `$HOME` for `<p>:cache._.decay` and then `*:cache._.decay` (every
  provider), and 4 when there is none. A `put` with a `ttl` stores 1. A
  prototype is put like any value (`put git:cache._.decay 6`, `put
  '*:cache._.decay' 3`, with `--path` for a directory) and read back with `get
  git:cache._.decay`; a lifecycle fixes its depth when it registers.
- The only `:cache` key a `put` may write is `_.decay`, a positive integer. A
  derived key is `{key} is derived and cannot be written`; any other key is
  `{key} is written by the daemon`.
- A `:cache` key cannot be refreshed, and a watch on it wakes on a data write to
  its namespace.

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
consulted by the path-scoped ones. Invalid facet syntax on the key is the same
error `get`/`put`/`watch` give: `{"ok": false, "error": "invalid facet key:
<key>"}`.

A key that names no Source has nothing to re-execute and is an **error**: a
static namespace (one created by `put` or a config seed), an unknown name, or a
field no Source owns. A layer facet or `:cache` key is `cannot refresh <key>: a
layer facet is read-only`. Reading configuration afresh is not a
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
`data` is required. A put **replaces** the node it addresses: nothing merges
implicitly, so `put my '{"b":2}'` states all of `my` at that directory, and
`put my.b 2` replaces only `my.b`. `null` means absent: `data: null` removes the
node, and a `null` inside `data` is an absent leaf, never `""`. Arrays are
stored and returned as objects keyed by index.

A put sits above the Source in the layer order, so it hides Source and seed
values at its node. The Source keeps refreshing its own layer beneath it, and
removing the put shows that layer at once.

`ttl` (`"30s"`, `"5m"`, at least 1s) makes the put a lifecycle entry of its own:
it is valid for the ttl, readable but `stale` for one decay step, then evicted.
Each put address (key and directory) has its own lifecycle, so `put --ttl 300
build.status passing` and `put --ttl 60 build.eta 4m` expire independently, and
a re-put without a `ttl` drops the expiry. `status` shows such a row as any
lifecycle entry, source `put`; `<p>:cache.<node>` reports its `poll` and `decay`.

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
{"op": "put", "key": "myapp", "data": null}
```

(A `put` whose `data` is JSON `null` removes the put layer at the addressed
node: the node and every put address at or below it. The layer below shows
through; the entry itself survives if a Source still contributes to it.
There is no other spelling of removal: a request without a `data` field is
invalid, `invalid request: missing field `data``.)

**Response:**
```json
{"ok": true}
```

**Error cases:**
- Invalid facet syntax on the namespace segment: `{"ok": false, "error": "invalid facet key: <key>"}`
- A colon after the first dot: `{"ok": false, "error": "invalid key: <key>"}`
- A write to a layer facet: `<p>:put and <p>:source are read-only; write <p>.<node>`
- A write to `<p>:cache` other than `_.decay`, or a `_.decay` that is not a positive integer: see "The `:cache` facet"
- A `ttl` of `0s`: `ttl must be at least 1s`; an unparsable one: `invalid ttl: <ttl>`; a `ttl` with `data: null`: `a ttl on an absent value has no meaning`
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
      "kind": {"kind": "lifecycle", "decay": 0, "depth": 4, "watches_files": true}
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
"lifecycle", "decay": N, "depth": D, "watches_files": bool}` for a lifecycle-tracked row,
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
changes, and continue until the client disconnects. A watch may address any
key, including one nothing answers yet (an unknown provider, a node not yet
written): the first frame is then a miss, and a later write wakes it. A watch on
a layer facet or `:cache` key warms no Source.

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
opened at a project path. Any write to a namespace or one of its facets
(`p:template`, `p:put`, `p:cache`) wakes the namespace's watchers, so a template
edited on its own delivers a frame carrying the new `template` and no new
`data`. The
daemon drops the subscription as soon as the client closes the connection,
not on the next write to the watched key. The connection is single-purpose:
after the watch request the client must not write again, and any further
bytes from it — a pipelined line, a stray newline — end the watch just as a
close does.

This op is unchanged by, and knows nothing about, *resolved* watching — a
value re-resolved over every key its resolution reads, emitted only when that
value actually changes. That layering
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
dependency its key's resolution reads, so a single client can read as more
than 1.

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
  Source produces it, so it is never polled, and it is evicted only when its
  `put` carried a `ttl`. It resolves up the directory chain to `$HOME` (see "Path resolution" under `Get`).
- **Static namespace** — a namespace no provider or config registers; every
  record in it is static.
- **Facet** — `provider:facet`, a namespace holding data about a provider
  (e.g. `git:template`), addressed like any namespace. `template` is the facet a
  `Get` resolves for its template half; `put` and `source` are the read-only
  layer facets; `cache` is the daemon's metadata (see "The `:cache` facet").
- **Cache row** — one line of `Status` output: `{provider, path, source,
  field, value, age_ms, stale, kind}`.
- **Invalidation strategy** — how a Source decides when to re-execute
  (poll-based, watch-based, once-only). See `src/scheduler.rs`.

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

The daemon is stateless per connection except for `Watch` streams, which hold
the connection open until the client disconnects. A connection holds no
directory of its own: every request carries its `path`, and an SDK session that
keeps a directory sends it with each request.

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

Additional fields vary per op (see Op reference). A field the op does not
take is refused — `{"ok":false,"error":"invalid request: unknown field
`format`, expected …"}` — never silently ignored, on every op including
`status` and `hello`.

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
  "path": "/absolute/path",
  "force": false,
  "wait": false
}
```

- `key` (string, required) — `provider` or `provider.field`. See "Keys" below.
- `path` (string, optional) — the directory the read is addressed at. Omitting
  it — or giving `$HOME`, which is the same address — reads the **global
  entry**. A `path` is absolute on every op that takes one: the client makes a
  relative or `~` path absolute against the caller's own directory and home
  (`libbeachcomber::path::normalise`), which the daemon cannot see. A path that
  is not absolute (`""`, `"."`, `"rel/x"`, `"~/x"`) is refused,
  `path must be absolute: "<path>"`, never joined to the daemon's working
  directory. A `path` returns the **chain-resolved tree**: static records (source
  `put` or `config:<file>`) are collected from that path and every ancestor up
  to `$HOME` (or to `/` for a path outside `$HOME`) and then the global entry,
  the nearest winning. Provider-produced records live at the instances of
  their Source — the one at the root the Source names for the requested path,
  and the one at `~` — and a read walks them instance by instance (below)
  rather than the directory chain. See "Path resolution" in
  `docs/canon/field_resolution.md`.
- `force` (bool, optional, default false) — re-execute every Source the key
  demands, at every instance the read reaches, whether or not it was
  stale, past the failback backoff: a run that begins after the request (one
  queued behind a run in flight). Nothing is evicted first: a run that fails
  leaves the last good value, which the read answers. A key naming no
  Source (a static namespace, a facet) is an error: `cannot --force static
  namespace '<name>': no source to re-execute from`.
- `wait` (bool, optional, default false) — if the entry is stale, wait for a
  run of it; if fresh, serve the cached value. A no-op on a static
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
{"ok": false, "error": "invalid facet key: a:b:c (a facet is template, macro, put, source or cache)"}
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
provider's records live: at the instances of the Source that owns the key. A
Source answers `root(dir)` — the directory whose instance serves `dir`, or none
for the one instance at `~` — and a read asks the nearest instance first
(`R1 = root(dir)`). If it lacks the key, the read moves to the parent of that
root and asks again (`root(parent(R1))`), ending at the instance at `~` and
never past it. A namespace read takes every instance on the way and merges
them, the nearer winning per leaf: from `~/ws/proj` with a `mise.toml`,
`mise.node` is the project's, and `mise.go`, set only in the global config, is
found at `~`. That is a **cascade**, the default. A Source marked `nearest` is
answered by `R1` alone: a key it lacks is absent, the read does not move up, and
no root found means no instance and no value (git works this way: a nested
repository never shows its superproject's values). A template is a **static record**, so the template half is
read from the requested directory itself and resolves up the chain from there:
`get git.branch --path <repo>/sub` reads git's records at `<repo>` and its
templates from `<repo>/sub`.

`template` is a JSON tree whose string leaves are value expressions the
**client** renders and whose objects are groups. The daemon never renders one.
The client is `libbeachcomber::resolve`: `comb get`, `bc_get` and every SDK's
`get` answer with the rendered value, and none of them hands back this
two-halved response. A caller that wants the template unrendered reads it as a
facet, `<provider>:template.<field>`, like any other key.
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

A key is a **head** and an **address** below it. The head is `<provider>` or a
facet address, `<provider>:<facet>`, and runs to the first `.` or `[`. The
address is any run of segments, each written one of three ways — jinja's own
spellings, so a key reads the same here as in a template:

- `.seg` — any characters but `.`, `[` and `]`: `git.branch`,
  `mise.cargo:cargo-nextest`, `mise.npm:@scope/pkg`;
- `[n]` — an array index, the same segment as `.n`: `p.list[0]` is `p.list.0`;
  a negative index counts from the end (`p.list[-1]`, `p.list.-1`);
- `['…']` or `["…"]` — a quoted segment, where a backslash escapes only the quote
  and the backslash: `mise['go:golang.org/x/tools/gopls']`.

Brackets are the one quoting form. Only a colon in the head names a facet; below
it a colon is an ordinary character, so `git.branch:age` is an ordinary miss.
There are no metadata suffixes. The facets are a closed set — `template`,
`macro`, `put`, `source` and `cache` — and any other name after the colon, a
second colon in the head or an empty provider before it is `invalid facet key:
<head> (a facet is template, macro, put, source or cache)`, on every op. Anything else the grammar
cannot read is `invalid key: <key>`: an empty head or segment (`p..a`, a
trailing `.`), a head starting with `/` or `~`, a stray `]`, an unterminated
bracket or quote, an index that is not a canonical decimal (`[01]`).

Every address the daemon or a client prints is in **canonical form**, so it can
be sent back as a key: bare segments joined by `.`, and a segment that is empty
or holds `.`, `[` or `]` written `['…']`. A `rest` (below) is canonical.

Every facet has the provider's shape: `get git` returns that part of the tree,
and so does `get git:cache`. An unknown provider is a miss, and so is a facet
nothing has written.

A field's value is the first layer that has it, in this order: the effective
template (rendered by the client), then `put`, then the Source. Each layer is
readable on its own:

- `<p>:template` — the templates (data the client renders; the daemon never
  evaluates one, but a `put` of one that does not compile is refused, naming
  the leaf).
- `<p>:macro` — `<p>`'s macro libraries, `<p>:macro.<name>` each a text of
  `{% macro %}` definitions. A namespace of its own, read like `:template`:
  `get scanline:macro` answers `{"rate": "…", "usage": "…"}`, with no template
  half, and the nearest directory's library wins. A `put` is checked as a
  configured library is: a string under a name, at `<p>:macro.<name>` or as a
  table of them at `<p>:macro`, that compiles and prints nothing outside its
  macros.
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
 "watch": [".git"], "decay": 4}
```

- `age_ms`, `stale`, `source` are derived at the read. `source` is `put`,
  `template`, `config(<file>)` for a configuration seed, `computed(result)` /
  `computed(return)` for a leaf a provider line computed, or `source(<name>)`
  for a Source. A value served
  by a template has these three only (`age_ms` 0, `stale` false, `source`
  `template`). A miss is a miss.
- `poll` (`{interval, count}`), `watch` and `decay` are stored: written when a
  Source registers or a `put` carries a `ttl`, and read back here. `watch` is an
  array of patterns.
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
Source that owns `branch`. Each runs at the instance that answers for the
requested directory — the root its `root` names for it, or the one instance at
`~` for a Source with no root — so `path` is optional and is only consulted by
Sources that have a root. A facet outside the closed set is the same
error `get`/`put`/`watch` give: `{"ok": false, "error": "invalid facet key:
<head> (a facet is template, macro, put, source or cache)"}`.

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

### `Put`

Store static data at any node of the tree. `key` is `namespace` or
`namespace.node…` — the same address `get` takes, and `namespace` may
itself be a facet address (`<provider>:<facet>`; a facet outside the closed
set is `invalid facet key: <head> …`, the same check `get`/`refresh`/`watch`
apply). Of the facets, `put` writes only `template`, `macro` and a `cache`
`_.decay`.
`data` is required. A put **replaces** the node it addresses: nothing merges
implicitly, so `put my '{"b":2}'` states all of `my` at that directory, and
`put my.b 2` replaces only `my.b`. `null` means absent: `data: null` removes the
node, and a `null` inside `data` is an absent leaf, never `""`. An array is
stored and returned as an array, and is a leaf: a put of an array at an address
replaces whatever is there whole, and a nearer layer's array replaces a farther
one's whole. A `null` element is absent and the array closes up behind it
(`[1, null, 3]` is `[1, 3]`). An element is addressed by decimal index
(`my.list.0`, `my.list[0]`): a put at an index inside the put's own array
replaces that element, at one past its end appends, and `null` there removes it
and closes the array up; a put at any other index, or into an array another
layer holds, writes the address as an object like any other node.

A put sits above the Source in the layer order, so it hides Source and seed
values at its node. The Source keeps refreshing its own layer beneath it, and
removing the put shows that layer at once.

`ttl` (`"30s"`, `"5m"`, at least 1s) makes the put a lifecycle entry of its own:
it is valid for the ttl, readable but `stale` for one decay step, then evicted.
Each put address (key and directory) has its own lifecycle, so `comb put --ttl 300
build.status '"passing"'` and `comb put --ttl 60 build.eta '"4m"'` expire independently, and
a re-put without a `ttl` drops the expiry. `status` shows such a row as any
lifecycle entry, source `put`; `<p>:cache.<node>` reports its `poll` and `decay`.

An object key in `data` may hold a `.`: it is one name, read back as the
quoted segment `['a.b']`, and a node segment of `key` may be that quoted
segment too. The node part of `key` is capped at ten segments.

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
- A facet outside the closed set on the namespace segment: `{"ok": false, "error": "invalid facet key: <head> (a facet is template, macro, put, source or cache)"}`
- A key the grammar cannot read: `{"ok": false, "error": "invalid key: <key>"}`
- A write to a layer facet: `<p>:put and <p>:source are read-only; write <p>.<node>`
- A write to `<p>:cache` other than `_.decay`, or a `_.decay` that is not a positive integer: see "The `:cache` facet"
- A `ttl` of `0s`: `ttl must be at least 1s`; an unparsable one: `invalid ttl: <ttl>`; a `ttl` with `data: null`: `a ttl on an absent value has no meaning`
- Non-object `data` at a namespace root: `{"ok": false, "error": "put data at the root must be an object"}`
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

`source` is the row's record's label, presented as a `:cache` answer's is:
`source(<name>)` for a Source of the provider (`source(head)`, …), `put`,
`config(<file>)` or `computed(<line>)`. `kind` is
present when the row's lifecycle discriminator is known: `{"kind":
"lifecycle", "decay": N, "depth": D, "watches_files": bool}` for a lifecycle-tracked row,
`{"kind": "once"}` / `{"kind": "static"}` / `{"kind": "transient"}`
otherwise; omitted when unknown. A handful of further optional fields
(`poll_interval_secs`, `poll_count`, `fsevents_reinstate`,
`polls_elapsed`, `next_poll_in_secs`, `failure`) are omitted here and carry
diagnostic detail `comb status` renders as columns; see
`libbeachcomber::protocol::CacheRow` for the authoritative field list. Every
request, response and payload on this page is a type in
`libbeachcomber::protocol`: the daemon writes them, and `bc_status` and
`bc_introspect` hand them to the SDKs as written, a field with no value
omitted.

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
    "watch_backend": "native",
    "reaper": {"armed": true, "visibility": "system-wide", "sweeps": 12, "reaped": 0, "kill_denied": 0},
    "verdicts": [{"level": "PASS", "message": "daemon responsive"}]
  },
  "age_ms": 0,
  "stale": false
}
```

`config_path` is omitted when no configuration file was loaded, and `reaper`
on an embedded server that has none.

`in_flight` counts the runs of Source instances in flight, whoever started
them: a read, a poll, a file event, a refresh, a Source's call.

`active_watchers` counts open `Watch` connections, not distinct watched keys
or clients: a resolved `comb watch` opens one `Watch` subscription per
dependency its key's resolution reads, so a single client can read as more
than 1. A watch whose registration failed adds a `WARN` verdict, `watch of
<path> failed: <error>; …`.

**Response (subject=watches):** `data.watches` is every path watched for a
Source, by path, with where its registration stands:

```json
{"watches": [
  {"path": "/home/me/ws/project", "state": "active"},
  {"path": "/home/me/.config/tool/state.json", "state": "active", "via": "/home/me/.config"},
  {"path": "/home/me/ws/big", "state": "pending"},
  {"path": "/srv/locked/state.json", "state": "failed", "error": "Permission denied (os error 13)"}
 ],
 "verdicts": [{"level": "PASS", "message": "watching 4 paths"}, …]}
```

A registration is made off the scheduler's loop, so a path is `pending` until
it is made, and its changes are not seen until then. `active` is registered: on
the path and everything below it or, with `via`, while the path does not exist,
on the entries of the nearest directory above it that does; the path's
creation is then a change like any other. `failed` carries the error, adds a
`WARN` verdict, and is asked for again by the next read of a Source that
watches it.

**Response (subject=providers):** `data.providers` is one object per
provider, sorted by name:

```json
{"name": "git", "source": "builtin",
 "sources": [{"name": "head", "fields": [{"name": "branch", "type": "string"}],
              "invalidation": "watch .git/HEAD"}],
 "invalidation": "watch .git/HEAD", "in_backoff": null, "disabled": false}
```

A provider carries no `scope`: whether a Source has a root is a question asked
of a directory, not a property of the provider. `source` is `static` (a
namespace with no Sources), `builtin` or `unknown`. `disabled` is true for a
provider switched off with `[daemon.providers.<p>] enabled = false`; its
`invalidation`, and each Source's, then reads `disabled`, because nothing of it
runs. `data.verdicts` leads with the provider count.

**Response (subject=config):** `data` is `{path, parsed, errors,
config_source_count, verdicts}`. `path` is the `config.toml` that was loaded, or
null when the file does not exist (configuration is read only from
`$XDG_CONFIG_HOME/beachcomber`: `defaults/`, `config.toml`, `conf.d/`).
`config_source_count` is the number of `[daemon.providers.<p>.sources.<s>]`
tables that declare a config Source (prototypes and built-in retunes are not
counted).

Other subjects return different shapes; see `src/server.rs` for the current
authoritative shape of each subject. Per-subject typed documentation is
Phase 3+ work.

### `Layers`

The stack of layers a read of `key` from `path` consults, per leaf. It is
the data behind `comb status --tree` and is **CLI-only**: no SDK or C ABI
function binds it.

```json
{"op": "layers", "key": "aws.region", "path": "/home/me/ws/project"}
```

`key` is a provider or a field (`p`, `p.f`, `p.group.f`); a facet key is an
error. Like a `:cache` read, it finds the instances a read would reach but
warms none of them and signals no demand: a Source with nothing stored adds
no layer, and a namespace with nothing anywhere is a miss (`{"ok": true}`).

`data` is a tree shaped like the value with an ordered array at each leaf (a
leaf key answers with its array alone):

```json
[{"label": "config(/home/me/.config/beachcomber/defaults/aws.toml)",
  "facet": "template", "path": null,
  "value": "{{ env.AWS_REGION or … }}", "age_ms": 72000, "stale": false,
  "answered": true},
 {"label": "put", "path": "/home/me/ws/project", "value": "eu-west-1",
  "age_ms": 3000, "stale": false}]
```

The order is the read's: the template records for the leaf first
(`<p>:template` up the chain from `path`, nearest first, `facet:
"template"`, `value` the raw expression), then a leaf the provider's
`result` or `return` line computed (`computed(<line>)`), then, for each
directory of the read's chain nearest first, its `put`, its `config(<file>)`
seeds (every file that sets the leaf, the last in composition order first,
so a file's record is followed by those it overrides), and the records of
each Source instance the read reaches there
(`source(<name>)`; cascade and `| nearest` as for `Get`). A leaf's list also
holds a record that stands as a scalar on an ancestor of it, which hides the
leaf in that layer. `label` is presented as a `:cache` answer's `source` is;
`path` is the directory the record is stored at (`null`: the global entry).
`answered` is present, and true, on the one layer the read takes: the first
template record when a template stands at the leaf, otherwise the record the
merged read resolves to (a `put` that replaced a node can hide nearer
records below it, so the answer is not always the first data layer).

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

This was a breaking change: a request that still sets `"format": "text"` or
`"format": "sh"` is refused, as every field an op does not take is. Clients
that depended on server-side rendering must render locally.

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
  declaring its own invalidation strategy, and answering which instance serves
  a directory (`root`). What `Refresh` re-executes and demand-driven warming keeps warm.
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
  (e.g. `git:template`), addressed like any namespace. There are five, and no
  other: `template` is the facet a `Get` resolves for its template half;
  `macro` holds the macro libraries a client template imports; `put` and
  `source` are the read-only layer facets; `cache` is the daemon's metadata
  (see "The `:cache` facet").
- **Cache row** — one line of `Status` output: `{provider, path, source,
  field, value, age_ms, stale, kind}`.
- **Invalidation strategy** — how a Source decides when to re-execute
  (poll-based, watch-based, once-only). See `src/scheduler.rs`.

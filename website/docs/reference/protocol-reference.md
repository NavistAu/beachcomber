---
sidebar_position: 4
title: Protocol Reference
---

# Protocol Reference

beachcomber uses a simple newline-delimited JSON protocol over a Unix socket. Any language that can open a Unix socket and read/write JSON can be a client — no client library required. This page is the wire contract for SDK and client implementers. Every request, response and payload on it is a type in `libbeachcomber::protocol`: the daemon writes them, and `bc_status` and `bc_introspect` hand them to the SDKs as written.

## Connection

Socket path resolution order:
1. `daemon.socket_path` in config, if set (the CLI and daemon only; the SDKs skip this step)
2. `BEACHCOMBER_SOCKET` env var, if non-empty
3. `/tmp/beachcomber-<uid>/sock`

Connect with `AF_UNIX`, `SOCK_STREAM`. Each request is one line: a JSON object followed by `\n`. Each response is one line the same way. There is no framing beyond the newline.

1. **Connect.**
2. **Handshake (recommended):** send `hello` and check that the returned `protocol_version` is compatible with the one the client was built against ([Versioning](#versioning)). The daemon does not require it; a client that skips it has no signal when the daemon advertises a breaking change.
3. **Interact:** send one or more ops, read one response per op, in any order.
4. **Close:** close the socket. The daemon sends no goodbye.

A connection holds no state of its own except a `watch` stream, which holds it until the client disconnects. A connection has no directory: every request carries its `path`.

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
| `key` | string | Provider name (`git`) or field path (`git.branch`); see [Keys](#keys) |
| `path` | string | Absolute directory the request addresses |

Other fields vary per op. A field the op does not take is refused, never ignored, on every op including `status` and `hello`: `{"ok":false,"error":"invalid request: unknown field `format`, expected …"}`.

**`path`.** Omitting it, or giving `$HOME` (the same address), addresses the **global entry**. A `path` is absolute on every op that takes one: the client makes a relative or `~` path absolute against the caller's own directory and home (`libbeachcomber::path::normalise`), which the daemon cannot see. A path that is not absolute (`""`, `"."`, `"rel/x"`, `"~/x"`) is refused, `path must be absolute: "<path>"`, never joined to the daemon's working directory.

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
| `age_ms` | int | Milliseconds since the record that answered was last computed ([Freshness](#freshness)); `0` on `hello`, `status` and `introspect` |
| `error` | string | Error message when `ok` is false. Human-readable; not stable enough to parse |
| `template` | any | `get` and `watch`: the [template half](#template-half) |
| `rest` | string | `get` and `watch`: the part of the key the template walk could not consume |
| `memo` | any | `get` and `watch`: the [memo half](#memo-half), beside the template half |
| `var` | object | `get` and `watch`: the provider's [vars](#var-half), beside the template half; omitted when it has none |
| `var_memo` | object | `get` and `watch`: the vars' memos, by name, beside `var`; omitted when no var has one |
| `log` | array | `get` with a `log` level: the log lines behind the answer, each `{"level", "msg", "origin", "count", "path", "key"}` — `origin` is `source(<name>)`; `count`, present when more than 1, is how many times the line came; `path` the directory of the Source instance that raised it (omitted for the global entry); `key` the leaf a client's render raised it for (omitted here). Repeats are one line; a line's text is cut at 512 bytes; past 16 lines one line, `… more lines`, counts the rest at the highest level among them. Omitted when there are none |

A field with no value is omitted, never set to `null`. A response field a client does not know should be ignored: a newer daemon may add one. JSON key order is not part of the contract, in requests, responses or payloads.

## Keys

A key is a **head** and an **address** below it. The head is `<provider>` or a facet address, `<provider>:<facet>`, and runs to the first `.` or `[`. The address is any run of segments, each written one of three ways — jinja's own spellings, so a key reads the same here as in a template:

- `.seg` — any characters but `.`, `[` and `]`: `git.branch`, `mise.cargo:cargo-nextest`, `mise.npm:@scope/pkg`;
- `[n]` — an array index, the same segment as `.n`: `p.list[0]` is `p.list.0`; a negative index counts from the end (`p.list[-1]`, `p.list.-1`);
- `['…']` or `["…"]` — a quoted segment, where a backslash escapes only the quote and the backslash: `mise['go:golang.org/x/tools/gopls']`.

Only a colon in the head names a facet; below it a colon is an ordinary character, so `git.branch:age` is an ordinary miss. The facets are a closed set — `template`, `macro`, `memo`, `put`, `source` and `cache` — and any other name after the colon, a second colon in the head or an empty provider before it is `invalid facet key: <head> (a facet is template, macro, memo, put, source or cache)`, on every op. Anything else the grammar cannot read is `invalid key: <key>`: an empty head or segment (`p..a`, a trailing `.`), a head starting with `/` or `~`, a stray `]`, an unterminated bracket or quote, an index that is not a canonical decimal (`[01]`).

Every address the daemon or a client prints is in **canonical form**, so it can be sent back as a key: bare segments joined by `.`, and a segment that is empty or holds `.`, `[` or `]` written `['…']`.

### Layers and facets

Every facet has the provider's shape: `get git` returns that part of the tree, and so does `get git:cache`. An unknown provider is a miss, and so is a facet nothing has written. A field's value is the first layer that has it: the effective template (rendered by the client), then `put`, then the Source. Each layer is readable on its own:

- `<p>:template` — the templates: data the client renders. The daemon never evaluates one, but a `put` of one that does not compile is refused, naming the leaf, and so is a `put` of a leaf that is not a string.
- `<p>:macro` — `<p>`'s macro libraries, `<p>:macro.<name>` each a text of `{% macro %}` definitions. Read like `:template`, with no template half; the nearest directory's library wins. A `put` is checked as a configured library is: a string under a name, at `<p>:macro.<name>` or as a table of them at `<p>:macro`, that compiles and prints nothing outside its macros.
- `<p>:memo` — the values `| cache()` stored for `<p>`'s template leaves, each at its leaf's address ([Memo half](#memo-half)). Read like `:template` (`get kubecontext:memo.context`); each value's `age_ms` is its memo's, and a memo of absence holds no value here. Written only by the `memo` op: a `put` to it is `<p>:memo is written by cache(); edit <p>:template.<f>, or refresh <p>`.
- `<p>:put` — what `put` and `config:<file>` seeds stored.
- `<p>:source` — what the provider's Sources produced. A `put` does not hide it here.
- `<p>:cache` — the daemon's metadata about `<p>`, below.

`<p>:put` and `<p>:source` are read-only: a `put` to either is `<p>:put and <p>:source are read-only; write <p>.<node>`, and they never warm a Source or start a lifecycle. `null` means absent, never `""`: a Source or put that has no value for a field has no node for it.

### The `:cache` facet

`<p>:cache.<field>` holds the daemon's metadata about `<p>.<field>` at the same path and for the same record that a read of `<p>.<field>` resolves to, so a value cannot be reported with another record's metadata. It mirrors `<p>.<path>` for any node: a leaf answers with an object, and an object node answers with the tree of its leaves' answers.

```json
{"age_ms": 1200, "source": "source(head)", "executing": false,
 "watch": [".git"], "hash": "9c6001d328a94690", "decay": 4}
```

- `age_ms` and `source` are derived at the read. `source` is `put`, `template`, `memo`, `config(<file>)` for a configuration seed, `computed(result)` / `computed(return)` for a leaf a provider line computed, or `source(<name>)` for a Source. A value served by a template with no memo has `source` `template` only. A template leaf with a memo answers for the memo: `source` `memo`, the memo's `age_ms`, and its stored keys.
- `executing` is derived too, for a Source's record: `true` while a run of the record's instance is in flight, whoever started it (a read that stopped waiting for it, a poll, a file event, a refresh). A template can read it (`comb['git:cache'].branch.executing`, for a spinner).
- `poll` (`{interval, count}`), `watch` and `decay` are stored: written when a Source registers or a `put` carries a `ttl`. `watch` is an array of patterns.
- `persist` is `true` for a put made with `persist` (on a root put, at each top-level key) while the daemon keeps it; absent otherwise.
- `hash` is stored for a Source's record with stat inputs: the validity hash of those inputs as its run stamped them (FNV-1a 64 over each input's path and stamp, `libbeachcomber::validity`), 16 lowercase hex digits. A read checks the record by stamping the inputs again. A Source that only polls has none. A memo's `hash` is the one its `memo` op carried, until an invalidation drops it; its `watch` is the absolute paths the op named.
- `decay` is the number of decay steps before eviction. It is data: a Source takes the nearest `_` prototype, looking at each directory from the requested one up to `$HOME` for `<p>:cache._.decay` and then `*:cache._.decay` (every provider), and 4 when there is none; so does a memo without a `ttl`. A prototype is put like any value (`put git:cache._.decay 6`, `put '*:cache._.decay' 3`, with a `path` for a directory) and read back with `get git:cache._.decay`; a lifecycle fixes its depth when it registers.
- The only `:cache` key a `put` may write is `_.decay`, a positive integer. A derived key is `<key> is derived and cannot be written`; any other key is `<key> is written by the daemon`.
- A `:cache` key cannot be refreshed, and a watch on it wakes on a data write to its namespace.

## Operations

### `hello`

Version negotiation. Clients should send this as the first op on any new connection.

```json
{"op":"hello"}
```

Response:

```json
{"ok":true,"data":{"daemon_version":"0.10.1","protocol_version":"2.0"},"age_ms":0}
```

`protocol_version` follows semver (major.minor) and is independent of `daemon_version`, the daemon binary's build version. It is `2.0` from comb 0.11.0: `1.0` had not moved since `hello` was added, though ops were added and the wire broke. A client should treat another major as a daemon it cannot talk to. The shipped clients do not yet: they compare `daemon_version` only, and a mismatch is reported, not refused.

### `get`

Read a value.

```json
{"op": "get", "key": "git.branch", "path": "/absolute/path", "force": false, "wait": "50ms"}
```

- `key` (required) — `provider` or an address below it; see [Keys](#keys).
- `path` (optional) — the directory the read is addressed at. Static records (source `put` or `config:<file>`) are collected from that path and every ancestor up to `$HOME` (or `/` for a path outside `$HOME`) and then the global entry, the nearest winning. Records a Source produces live at the instances of that Source and are read [instance by instance](#template-half).
- `force` (optional, default `false`) — re-execute every Source the key demands, at every instance the read reaches, whether or not it was stale, past the failback backoff. Nothing is evicted first: a run that fails leaves the last good value, which the read answers. The client renders a cached template past its memo on a forced read, so a read whose answer has a template half is answered on a namespace with no Source too; a static namespace (a facet included) with no template half is an error: `cannot --force static namespace '<name>': no source to re-execute from`.
- `wait` (optional) — how long the read waits for a run it needs: a duration (`"0"`, `"50ms"`, `"2s"`, `"1m30s"`; a bare number is seconds) or `"forever"`. Absent (or `null`), the read waits its **budget**. Anything else, a boolean included, is an invalid request (``invalid wait `soon`: …``).
- `log` (optional) — the least level of log line the answer carries: `"debug"`, `"info"`, `"warn"`, `"error"` or `"fatal"`. Absent, it carries none, at no cost; this is what `comb get -v` sends (`-v` `warn`, `-vv` `info`, `-vvv` `debug`). The response's `log` then holds, for each Source instance the read reached and each one its provider's lines call: the lines its latest stored run raised (`_.log.debug/info/warn`), and, while the instance is failing, an `error` with its last failure (the read is served the last good value) or a `fatal` when `_.log.fatal` stopped it. A run keeps `info` and `warn` lines; it keeps `debug` lines while a read with `log: "debug"` has asked for that provider within the last 12 minutes (an instance's default Active window), so the run after that read has them. An evicted instance's lines go with its record. A failed run leaves the stored run's lines as they were.

A record is valid while it is within its lifetime and its stat inputs (the files its Source declares, stat'd on the read) are as its run found them, and a valid record is served as is, as is an instance in failback backoff. Otherwise the read runs the instance's Source (or takes the run in flight, when that run sees the inputs as they are) and waits:

| The read finds | With no `wait` it waits | With a `wait` |
|---|---|---|
| a record past its lifetime, or whose inputs changed | the Source's budget (`budget(...)`, else `[daemon] budget`, default `50ms`) | that long (`"forever"`: the run's end) |
| nothing stored | the run's end, which its `timeout` bounds | as above |
| anything, with `force` | the run's end | as above |

Each wait counts from the read's start. Answered in time, the read has the fresh value; late, it has what is stored (a miss when nothing is), `:cache.<field>.executing` is `true` while the run goes on, and the run records itself for the next read and for any `watch` of the key. A namespace read starts the runs of every Source it needs before it waits for any. A client's read of the answer may take its own timeout plus the `wait`; `"forever"` has no deadline.

```json
{"ok": true, "data": {"branch": "main", "dirty": false}, "age_ms": 42}
{"ok": true}
```

`ok: true` with no `data` is a miss: nothing answers that address (a rooted provider queried outside a matching directory, an unknown provider, a sub-path into an object that has no such key or past a scalar). There is no `unknown field` error.

#### Freshness

`age_ms` describes the *record* that answered, not the cache entry it lives in:

- a leaf reports how long since its own record's Source last refreshed;
- an object assembled from several records reports the largest of their ages;
- a static record (`put` or `config:<file>`) reports the time since it was written;
- a rendered template value is a lookup, not a stored record: age 0;
- a response with no `data` carries no `age_ms`.

The same number, with the record's source and lifecycle, is readable through the [`:cache` facet](#the-cache-facet).

#### Template half

A `get` on a key whose head names no facet does two lookups: the data at the addressed node, and the same node in the merged tree of the namespace's `template` facet (`<provider>:template`). Whichever exist come back, so a response may carry the template half with no `data` at all: a namespace holding only templates is a hit.

The two halves are addressed from different places. The data half sits at the instances of the Source that owns the key. A Source answers `root(dir)` — the directory whose instance serves `dir`, or none for the one instance at `~` — and a read asks the nearest instance first (`R1 = root(dir)`). If it lacks the key, the read moves to the parent of that root and asks again (`root(parent(R1))`), ending at the instance at `~` and never past it. A namespace read takes every instance on the way and merges them, the nearer winning per leaf: that is a **cascade**, the default. A Source marked `nearest` is answered by `R1` alone: a key it lacks is absent, and no root found means no value (git works this way: a nested repository never shows its superproject's values). A template is a static record, so the template half is read from the requested directory itself and resolves up the chain from there: `get git.branch` with `path` `<repo>/sub` reads git's records at `<repo>` and its templates from `<repo>/sub`.

`template` is a JSON tree whose string leaves are value expressions the **client** renders and whose objects are groups; the daemon never renders one. The client is `libbeachcomber::resolve`: `comb get`, `bc_get` and every SDK's `get` answer with the rendered value, and none of them hands back this two-halved response. A caller that wants the template unrendered reads it as a facet, `<provider>:template.<field>`. Walking the key through the tree:

- landing on an object or a string with the key consumed → that node is `template`;
- landing on a string with segments left over → that string is `template` and the remainder, rejoined in canonical form, is `rest`;
- a miss, or a non-string scalar in the way or at the address → neither field is attached.

A `get` on a facet key (`<provider>:<facet>…`) is already inside a facet and never carries a template half; it reads the facet's own records as `data`.

```json
{"ok": true, "template": "{{ {'cpu': 40, 'mem': 12} }}", "rest": "cpu"}
```

#### Memo half

A template leaf whose expression pipes through `| cache()` has its value stored by the client after it renders (the [`memo`](#memo) op): its **memo**, at the leaf's address in the `<p>:memo` facet, with its record in `<p>:cache`. A `get` that attaches a template half attaches the memo half beside it, `memo`, when a leaf of that template has a memo: for a string template one object, the memo of that leaf; for a group a tree shaped like the template, with that object at each leaf that has a memo.

```json
{"ok": true, "template": "{{ 'kubectl config current-context' | cache() | exec() }}",
 "memo": {"hash": "9c6001d328a94690", "stale": false, "age_ms": 5200, "data": "prod"}}
```

- `hash` — the hash the client computed when it stored the memo, 16 lowercase hex digits. Absent once an invalidation has dropped it: a memo with no hash is a miss for every reader.
- `stale` — whether the memo is past its `ttl`; `age_ms` — how long since it was stored.
- `data` — the leaf's value. Absent for a **memo of absence**: a leaf that rendered nothing is remembered as having rendered nothing, and `null` is never stored.
- `log` — the lines the render that stored the memo raised (as a `get`'s [`log`](#response-format) lines), sent only to a `get` that asked for a `log`; omitted when there are none. A client that takes the memo replays them as the read's own, with `origin` `memo`.

A memo is found as a static record is: up the chain from the requested path, nearest first, so a memo stored at a directory serves the directories below it. It is read only through its template: with no template at an address there is no memo half. The daemon does not decide a hit: the client compares the hash with its own and takes the memo only when they are equal and the memo is not stale.

The daemon drops a memo's hash, and notifies the namespace's watchers at the memo's directory, when the field is invalidated: a `put` or a configuration reload of the template at or above the leaf, a data write at or under the leaf (a `put` to `<p>`, its removal or expiry, a configuration seed, or a Source's run storing the field), a `refresh` of the key, the memo's `ttl` running out, and a change to one of its `watch` paths while it is Active.

### Var half

A provider's `var.<name>` bindings ([configuration](./configuration-reference.md#vars)) are seeded at `<p>:template.var.<name>`. They are no field: the template half of a read never holds the `var` node (a read of `<p>.var…` answers data alone), and `<p>:template` shows it as configuration. A response that carries a template half of a provider with vars carries them beside it, so the client renders a leaf that reads one without another request:

```json
{"ok": true, "template": "{{ listed.exit_code != 2 }}",
 "var": {"listed": "{{ 'ssh-add -l' | ttl('30s') | cache(env.SSH_AUTH_SOCK) | execa(ok=[0, 1, 2]) }}"},
 "var_memo": {"listed": {"hash": "9c6001d328a94690", "stale": false, "age_ms": 5200, "data": {"exit_code": 0, "stdout": "…"}}}}
```

- `var` — every var of the provider, by name, each a value expression; the prototype's (`[client.providers._]`) already merged beneath the provider's own. Omitted when the provider has none, and on a response with no template half.
- `var_memo` — each cached var's memo, by name, shaped as a leaf's [memo half](#memo-half): a var memoises at `<p>:memo.var.<name>`, written by the `memo` op with key `<p>.var.<name>`. Omitted when no var has one.

Both are additive: a client that does not know them ignores them, and renders a template that reads a var with the var absent.

### `refresh`

Re-execute the Sources the key demands — every Source of the namespace for a bare `provider`, only the one owning the field for `provider.field` — each at the instance that answers for the requested directory (the root its `root` names for it, or the one instance at `~` for a Source with no root), out-of-band. It does not return the new value: it answers `{"ok": true}` once acknowledged, and later `get` calls return the refreshed value once the run completes.

```json
{"op": "refresh", "key": "git", "path": "/some/path"}
```

A `refresh` also drops the hash of every memo at or under the key, at every directory, so the next read of a cached template renders it afresh. A key that names no Source but holds memos answers `{"ok": true}` for that alone.

Errors:

- a key naming no Source and no memo — a static namespace, a `:template`/`:macro`/`:memo` key, an unknown name, or a field no Source owns: `nothing to refresh: <key> has no sources`;
- a `nearest` Source with no instance at the path: `nothing to refresh: <key> has no instance at <path>`;
- a disabled provider: `nothing to refresh: <p> is disabled`;
- a `:put`, `:source` or `:cache` key: `cannot refresh <key>: a layer facet is read-only`.

Reading configuration afresh is not a refresh: the config watcher does that.

### `put`

Write static data at any node. `key` is `namespace` or `namespace.node…`, the same address `get` takes, and `namespace` may be a facet address; of the facets, `put` writes only `template`, `macro` and a `cache` `_.decay` (`memo` is the `memo` op's). `data` is required: a request without it is `invalid request: missing field `data``.

```json
{"op":"put","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}
{"op":"put","key":"myapp.deploy.count","data":3}
{"op":"put","key":"myapp","data":{"status":"ok"},"ttl":"30s","path":"/project"}
{"op":"put","key":"myapp","data":{"status":"ok"},"persist":true}
{"op":"put","key":"myapp","data":null}
```

Response: `{"ok":true}`, with a `log` of `warn` lines (origin `put`) when the put replaced what was put below it at that directory (`<key>: replaced, so these went: <keys>`) or, without `persist`, stopped keeping persisted puts (`<key>: not --persist, so no longer kept across a restart: <keys>`).

- **Replaces.** A put replaces the node it addresses; nothing merges implicitly, so `put my {"b":2}` states all of `my` at that directory, and `put my.b 2` replaces only `my.b`.
- **Absent.** `"data": null` removes the put layer at the node: the node and every put address at or below it. The layer below shows through. A `null` inside `data` is an absent leaf, never `""`.
- **Shape.** `data` at a namespace root must be an object; below the root, any JSON. An object key may hold a `.`: it is one name, read back as the quoted segment `['a.b']`. The node part of `key` is capped at ten segments.
- **Arrays.** An array is stored and returned as an array, and is a leaf: a put of an array replaces whatever is there whole, and a nearer layer's array replaces a farther one's whole. A `null` element is absent and the array closes up behind it (`[1, null, 3]` is `[1, 3]`). A put at an index inside the put's own array (`my.list.0`, `my.list[0]`) replaces that element, at one past its end appends, and `null` there removes it; a put at any other index, or into an array another layer holds, writes the address as an object.
- **Layering.** A put sits above the Source in the layer order, so it hides Source and seed values at its node. The Source keeps refreshing its own layer beneath it, and removing the put shows that layer at once.
- **`path`** addresses the entry the record is written to: omitted, or `$HOME`, writes the global entry, which every path's chain ends at.
- **`ttl`** (`"30s"`, `"5m"`, at least `1s`) makes the put a lifecycle entry of its own: live for the ttl, then stale (still served) for as long again, then evicted at about twice the ttl. Reads do not renew it; the next put does. Each put address (key and directory) has its own lifecycle, so `build.status` and `build.eta` put with different ttls expire independently, and a re-put without a `ttl` drops the expiry. `status` shows such a row as a lifecycle entry, source `put`; `<p>:cache.<node>` reports its `poll` and `decay`.
- **`persist: true`** keeps the put across a daemon restart, best effort. The daemon keeps one entry per put address in `$XDG_CACHE_HOME/beachcomber/puts.json` (default `~/.cache/beachcomber/puts.json`): the key, the directory, the `data`, and for a put with a `ttl` the wall-clock expiry (`expires`, milliseconds since the Unix epoch). The file is written whole, through a temporary file and a rename, after each change. Before it serves, the daemon puts each entry again, with the ttl it has left; an entry whose expiry has passed is dropped. A missing file is nothing to replay; an unreadable or invalid one is a logged warning, ignored, and replaced on the next persisted write; an entry the daemon now refuses is logged and dropped. The latest put to an address decides: a put without `persist` there, a `null` put, or the ttl's eviction drops its entry, and a put at an address above it drops it as the put replaces the node. `<p>:cache.<node>.persist` is `true` while it is kept. It is not a guarantee: the cache directory may be cleared by the system or the user, so use it for convenience, not for data you cannot recreate.

Errors:

- an invalid facet: `invalid facet key: <head> (…)`; a key the grammar cannot read: `invalid key: <key>`
- a write to a layer facet: `<p>:put and <p>:source are read-only; write <p>.<node>`
- a write to `<p>:memo`: `<p>:memo is written by cache(); edit <p>:template.<f>, or refresh <p>`
- a field named `_`, in `key` or anywhere in `data` (outside `:cache`, where `_.decay` is the prototype): `invalid field in <key>: '_' is reserved: …`
- a write at `<p>:template.var`, or of a `<p>:template` tree holding `var`: `<key>: var is the vars' node, set in [client.providers.<p>] var.<name>; a put does not write a var`
- a write to `<p>:cache` other than `_.decay`, or a `_.decay` that is not a positive integer: see [The `:cache` facet](#the-cache-facet)
- a `:template` leaf that is not a string, or a template that does not compile (naming the leaf)
- a `ttl` of `0s`: `ttl: must be at least 1s`; one finer than a second: `ttl: must be a whole number of seconds`; an unparsable one: ``ttl: invalid duration `<ttl>`: <the grammar>``; a `ttl` with `data: null`: `a ttl on an absent value has no meaning`
- non-object `data` at a namespace root: `put data at the root must be an object`
- more than ten node segments: `put key is nested too deeply (limit 10 segments)`

### `memo`

Store what a template leaf's `| cache()` computed. The client's resolver sends it after a render that missed, once the value has been handed to its caller; no SDK binds it.

```json
{"op":"memo","key":"kubecontext.context","path":"/project","hash":"9c6001d328a94690","data":"prod","watch":["/home/me/.kube/config"],"ttl":"10m"}
```

- `key` (required) — the template leaf, `provider.field`.
- `path` (optional) — the reader's directory; omitted (or `$HOME`) is the global entry. A memo is stored at the reader's directory and serves the directories below it.
- `hash` (required) — the client's hash of the leaf's inputs, sixteen lowercase hex digits.
- `data` (optional) — the leaf's value. Absent (or `null`) stores a memo of absence.
- `watch` (optional) — the absolute paths whose change invalidates the memo.
- `ttl` (optional) — how long the memo is valid, whole seconds and at least `1s`, as a `put`'s.
- `log` (optional) — the lines the leaf's render raised, as a `get`'s `log` lines: `info` and above, `debug` too when the read asked for it. Kept within a log's bounds (16 lines of 512 bytes) and sent back in the memo half to a `get` that asks for a `log`; not part of `<p>:cache.<field>`.

The write replaces the memo at that address and directory — its value, its stored keys and its age together — and any memo at an address above or below it there, and notifies the namespace's watchers at `path`. A memo's `hash`, `watch`, `poll` and `decay` are read back through `<p>:cache.<field>`, its value through `<p>:memo.<field>` and the memo half. Response: `{"ok": true}`.

**Lifecycle.** The write starts the memo's lifecycle at its address, afresh for a memo written again, and nothing else renews it: a read of the leaf does not. With a `ttl` the memo is live for that long, when its hash is dropped (it is stale from then on), stale for as long again, and evicted at about twice the ttl, its value and record with it. Its `watch` paths are watched while it is Active, each one not there yet through the nearest directory above it that is, and a change to one drops its hash; entering decay lets go of them. With no `ttl` the memo is Active for the default window (12 minutes), then decays at its provider's `_.decay` (default 4 steps) and is evicted at the end of that ladder, about six hours after its write. `introspect demand`, `lifecycle` and `timers` list it with `"source": "memo"` and `"node": "<field>"`.

Errors:

- a facet key: `memo takes a provider.field key, not a facet: <key>`; a bare provider: `memo takes a provider.field key: <key> names no field`
- a hash that is not sixteen lowercase hex digits: `invalid memo hash: <hash> (sixteen lowercase hex digits)`
- a relative watch path: `a memo's watch paths are absolute: <path>`
- a `ttl` of `0s`: `ttl: must be at least 1s`; one finer than a second: `ttl: must be a whole number of seconds`; an unparsable one: ``ttl: invalid duration `<ttl>`: <the grammar>``
- more than ten node segments, an invalid key or provider name: as for `put`

### `status`

Returns the warm cache as an array of row objects, one per (provider, path, source, field). This is the data that `comb s` renders as a table. For daemon health (pid, uptime, …), use `{"op":"introspect","subject":"daemon"}`.

```json
{"op":"status"}
```

Response:

```json
{"ok":true,"data":[
  {"provider":"git","path":"/project","source":"source(head)","field":"branch","value":"main","age_ms":234,
   "kind":{"kind":"lifecycle","decay":0,"depth":4,"watches_files":true}},
  {"provider":"myapp","path":null,"source":"put","field":"status","value":"ok","age_ms":4200,"kind":{"kind":"static"}}
],"age_ms":0}
```

- Rows are sorted by `provider`, then `path` (the global entry first), then `source` as presented, then `field`.
- `path` is the directory the row is stored at, `null` for the global entry.
- `source` is always present, presented as a `:cache` answer's is: `source(<name>)` for a Source of the provider, `put`, `config(<file>)`, `computed(<line>)`, or `memo` for a `<p>:memo` row (one per memo that holds a value, aged by its memo).
- `kind` is always present, an object tagged by its own `kind`: `{"kind": "lifecycle", "decay": N, "depth": D, "watches_files": bool}` for a lifecycle entry (`decay` 0 is Active), or `{"kind": "once"}`, `{"kind": "static"}`, `{"kind": "transient"}`.
- Optional fields, omitted when they have no value: `age_ms`, `poll_interval_secs`, `poll_count`, `polls_elapsed`, `next_poll_in_secs`, and `failure` (`{consecutive_failures, suppressed_until_unix_ms, last_error}`, where `last_error` is the latest failure's whole message), omitted when there is none, and `warn`, the latest `warn` (or worse) line a Source's stored run raised (`_.log.warn`).

### `watch`

Stream a key's value. The connection switches to streaming: the daemon emits one frame with the current value, then one on each change, until the client disconnects. A watch may address any key, including one nothing answers yet: the first frame is then a miss, and a later write wakes it. A watch on a layer facet or `:cache` key warms no Source.

```json
{"op":"watch","key":"git.branch","path":"/project"}
```

```json
{"ok":true,"data":"main","age_ms":0}
{"ok":true,"data":"feature/foo","age_ms":0}
```

- Every frame is the same chain-resolved read `get` returns, with every half (`data`, `template`, `memo`); each frame's `age_ms` is what `get` would report.
- The watch fires on a change to any entry on either chain: a `put` at an ancestor, or at the global entry, reaches a watch opened at a project path. Any write to a namespace or one of its facets wakes the namespace's watchers, so a template edited on its own delivers a frame with the new `template` and no new `data`.
- A frame is sent only when its `data`, `template` or `memo` half changed; ages are not compared, so a memo whose hash was dropped is a frame, and one that only grew older is not.
- The connection is single-purpose: after the watch request the client must not write again. Any further bytes — a pipelined line, a stray newline — end the watch just as a close does. The daemon drops the subscription as soon as the client closes the connection.

*Resolved* watching — a value re-resolved over every key its resolution reads, emitted only when that value changes — is client-side: `libbeachcomber::watch` (`ResolvedWatch`) opens several `watch` subscriptions and re-resolves on their frames, and `comb watch`, `bc_watch_open` and every SDK's `watch` are clients of it. The wire op answers about exactly one address.

### `introspect`

Diagnostic queries into daemon internals. `subject` selects what to inspect: `daemon`, `providers`, `config`, `cache`, `lifecycle`, `watches`, `timers`, `demand`, `procs`; any other is `unknown introspect subject: <name>`. `procs` takes an optional `duration_secs` integer for profiling, and its answer may take that much longer. Every subject's `data` carries `verdicts`, a list of `{level, message}` with `level` `PASS`, `WARN` or `FAIL`.

```json
{"op":"introspect","subject":"daemon"}
```

**`daemon`:**

```json
{"ok":true,"data":{
  "pid": 12345, "version": "0.10.1", "uptime_secs": 3600,
  "socket_path": "/tmp/beachcomber-501/sock",
  "config_path": "/home/me/.config/beachcomber/config.toml",
  "requests_total": 10293, "in_flight": 0, "active_watchers": 3, "cache_entries": 42,
  "watch_backend": "native",
  "reaper": {"armed": true, "visibility": "system-wide", "sweeps": 12, "reaped": 0, "kill_denied": 0},
  "verdicts": [{"level": "PASS", "message": "daemon responsive"}]
},"age_ms":0}
```

`config_path` is omitted when no configuration file was loaded, and `reaper` on an embedded server that has none. `watch_backend` is `native`, `polling`, `disabled` or `unknown`; `reaper.visibility` is `system-wide` or `confined`. `in_flight` counts the runs of Source instances in flight, whoever started them. `active_watchers` counts open `watch` connections, not distinct keys or clients: a resolved `comb watch` opens one per dependency its key's resolution reads. A watch whose registration failed adds a `WARN` verdict.

**`watches`:** `data.watches` is every path watched for a Source, with where its registration stands:

```json
{"watches": [
  {"path": "/home/me/ws/project", "state": "active", "asked_secs_ago": 310},
  {"path": "/home/me/.config/tool/state.json", "state": "active", "via": "/home/me/.config", "asked_secs_ago": 310},
  {"path": "/home/me/ws/big", "state": "pending", "asked_secs_ago": 2},
  {"path": "/srv/locked/state.json", "state": "failed", "error": "Permission denied (os error 13)", "asked_secs_ago": 40}
 ],
 "verdicts": [{"level": "PASS", "message": "watching 4 paths"}]}
```

Watches exist only while an instance is Active, so a daemon with no Active instance lists none and says `PASS` `idle: no instance is Active, so no path is watched`. `asked_secs_ago` is the whole seconds since the path was last asked for. A registration is made off the scheduler's loop, so a path is `pending` until it is made, and its changes are not seen until then. `active` is registered: on the path and everything below it or, with `via`, while the path does not exist, on the entries of the nearest directory above it that does. `failed` carries the error, adds a `WARN` verdict (as does a path still `pending` after 30 seconds), and is asked for again by the next read of a Source that watches it.

**`providers`:** `data.providers` is one object per provider, sorted by name:

```json
{"name": "git", "source": "source",
 "sources": [{"name": "head", "fields": [{"name": "branch"}], "invalidation": "watch .git/HEAD"}],
 "invalidation": "watch .git/HEAD", "in_backoff": null, "disabled": false, "failing": null}
```

`source` is `static` (a namespace with no Sources), `source` or `unknown`. A provider carries no `scope`: whether a Source has a root is a question asked of a directory. `disabled` is true for a provider switched off with `[daemon.providers.<p>] enabled = false`; its `invalidation`, and each Source's, then reads `disabled`. `failing` is `null`, or `{sources, consecutive_failures}`: the Sources whose last runs failed and the longest streak, each also a `WARN` verdict. `data.verdicts` leads with the provider count.

**`config`:** `data` is `{path, parsed, errors, config_source_count, verdicts}`. `path` is the `config.toml` that was loaded, or null when the file does not exist. `config_source_count` is the number of `[daemon.providers.<p>.sources.<s>]` tables that declare a config Source (prototypes are not counted).

**`demand`, `lifecycle`, `timers`:** the scheduler's entries (Active ones, decaying ones, and those with a poll) by `provider`, `path` and `source`, the Source's name or `put` or `memo` for a static address, which also has its `node`: `{"provider":"kubecontext","path":"/project","source":"memo","node":"context",...}`.

The shapes of `cache` and `procs` are diagnostic and not yet part of this contract; `src/server/introspect.rs` builds them.

### `layers`

The stack of layers a read of `key` from `path` consults, per leaf: the data behind `comb status --tree`. It is CLI-only: no SDK or C ABI function binds it.

```json
{"op": "layers", "key": "aws.region", "path": "/home/me/ws/project"}
```

`key` is a provider or a field (`p`, `p.f`, `p.group.f`); a facet key is `layers takes a provider or field key, not a facet: <key>`. Like a `:cache` read, it finds the instances a read would reach but warms none of them and signals no demand: a Source with nothing stored adds no layer, and a namespace with nothing anywhere is a miss.

`data` is a tree shaped like the value with an ordered array at each leaf (a leaf key answers with its array alone):

```json
[{"label": "config(/home/me/.config/beachcomber/defaults/aws.toml)",
  "facet": "template", "path": null,
  "value": "{{ env.AWS_REGION or … }}", "age_ms": 72000,
  "answered": true},
 {"label": "put", "path": "/home/me/ws/project", "value": "eu-west-1",
  "age_ms": 3000}]
```

The order is the read's: the memo records for a template leaf first (`<p>:memo` up the chain from `path`, nearest first, label `memo`, `facet: "memo"`, `value` the memo's value, `null` for a memo of absence), then the template records for the leaf (nearest first, `facet: "template"`, `value` the raw expression), then a leaf the provider's `result` or `return` line computed (`computed(<line>)`), then, for each directory of the read's chain nearest first, its `put`, its `config(<file>)` seeds (every file that sets the leaf, the last in composition order first), and the records of each Source instance the read reaches there (`source(<name>)`; cascade and `nearest` as for `get`). A leaf's list also holds a record that stands as a scalar on an ancestor of it, which hides the leaf in that layer. `label` is presented as a `:cache` answer's `source` is; `path` is the directory the record is stored at (`null`: the global entry). `answered` is present, and true, on the one layer the read takes: when a template stands at the leaf, the nearest memo while it is live (it has a `hash` and is not stale), else the first template record; otherwise the record the merged read resolves to.

## Text and Shell Rendering

The wire protocol is JSON-only — there is no `format` field, and a request that sets one is refused. Text (`-t`) and shell (`-s`) rendering are performed client-side by the CLI and SDKs after receiving the JSON response. Errors are carried by `ok: false`, and values are escaped JSON strings, so a value that looks like an error or holds blank lines cannot be mistaken for anything else.

## Versioning

`protocol_version` uses two-part semver (`MAJOR.MINOR`):

| Change | Bump |
|---|---|
| New op | Minor |
| New optional request field (default preserves prior behaviour) | Minor |
| New optional response field | Minor |
| Remove or rename an op | Major |
| Remove a response field | Major |
| Change an existing field's type or semantics | Major |
| Change wire encoding | Major |

A test holds the number to the wire: `libbeachcomber/tests/wire_shape.json` is the shape of every request and of the response structs, taken at the version it names, and the suite fails when the shape changes while `PROTOCOL_VERSION` does not. Bump the version as the table says, then refresh the snapshot with `UPDATE_WIRE_SHAPE=1 cargo nextest run -p libbeachcomber -E 'test(wire_shape)'`.

The [versioning policy](../about/versioning-policy.md) covers the rest of the public surface. The `hello` handshake exists so clients can detect incompatibility at connection time rather than at first-op failure.

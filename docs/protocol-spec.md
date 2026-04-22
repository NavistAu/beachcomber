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

1. **Discover socket path** — environment-dependent:
   - `$XDG_RUNTIME_DIR/beachcomber/sock` if set
   - `$TMPDIR/beachcomber-<uid>/sock` otherwise
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
  cache entry was last refreshed.
- `stale` (bool, optional) — for `Get` responses, whether the entry is older
  than the provider's expected refresh interval.
- `error` (string, optional) — present when `ok=false`. A human-readable
  message; not stable enough to parse.

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
  "format": "json",
  "force": false,
  "wait": false
}
```

- `key` (string, required) — `provider` or `provider.field`. See "Keys" below.
- `path` (string, optional) — directory context for path-scoped providers.
- `format` (enum, optional, default `json`) — `json` returns structured data;
  `text` returns a rendered plain-text form; `sh` returns shell-export form.
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

Store data in a virtual provider. Virtual providers have no source code
and are pure data containers. Namespace hierarchy (builtin > script > virtual)
prevents accidentally shadowing a real provider.

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

**Request (clear):**
```json
{"op": "put", "key": "myapp"}
```

(Omit `data` to clear the cache entry while keeping the registration.)

**Response:**
```json
{"ok": true}
```

**Error cases:**
- Non-object `data`: `{"ok": false, "error": "put data must be a JSON object"}`
- Name collision with builtin/script provider: `{"ok": false, "error": "cannot store under '<name>': name is used by a builtin or script provider"}`

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
{"op": "watch", "key": "git.branch", "path": "/repo", "format": "json"}
```

**Response (initial + per-event):**
```json
{"ok": true, "data": {"branch": "main"}, "age_ms": 0, "stale": false}
```

The daemon emits one initial event with the current value, then one
response per subsequent change.

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

## Glossary

- **Provider** — a source of data (e.g. `git`, `hostname`, `mise`). Produces
  a `ProviderResult` — a map of field name → `Value`.
- **Field** — a named element of a provider's output (e.g. `branch` on `git`).
- **Virtual provider** — a provider whose data is entirely injected via
  `Put`. No execution.
- **Cache row** — one line of `Status` output: `{provider, field, path, value, age_ms, stale}`.
- **Invalidation strategy** — how a provider decides when to re-execute
  (poll-based, watch-based, once-only). See `src/scheduler.rs`.

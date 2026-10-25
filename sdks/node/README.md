# beachcomber Node.js SDK

Node.js/TypeScript client for the [beachcomber](https://github.com/NavistAu/beachcomber) shell state daemon.

A binding over the beachcomber C ABI (`libbeachcomber.{so,dylib}`), not a
hand-rolled wire-protocol client: `koffi` (an **optional peer dependency**)
gives direct FFI access to the native library when installed; without it,
the SDK falls back to shelling out to `comb` (~5ms per call, against
~0.3ms via FFI — a documented slow tier, not silent). Check
`client.transport()` to see which one is active.

It follows the one SDK API table every beachcomber SDK follows,
[`docs/sdk-api.md`](../../docs/sdk-api.md): the same operations, names,
value shapes and error kinds, in camelCase. Its test suite checks this
surface against the table's machine-readable form, `sdks/api.json`.

## Requirements

- Node.js 18+
- A running `comb` daemon
- `libbeachcomber.{so,dylib}`, shipped alongside `comb` in the same release
  package — discovered automatically (see "Library discovery" below)

## Installation

```sh
npm install libbeachcomber
npm install koffi   # optional — direct FFI; omit to use the comb subprocess fallback
```

## Quick Start

```typescript
import { Client } from 'libbeachcomber';

const client = new Client();
console.log(client.transport()); // "ffi" or "subprocess"

// What `comb get git.branch` prints there; null on a miss.
const branch = await client.get('git.branch', { cwd: '/path/to/repo' });
console.log(branch); // e.g. "main"
```

## API

### `Client`

```typescript
const client = new Client();
const client = new Client({ socketPath: '/custom/path' });
const client = new Client({ socketPath: '/custom/path', timeoutMs: 2000, connectTimeoutMs: 0 });
const client = new Client({ exec: true }); // templates this client renders may call exec()
```

Each option is passed to the library's `bc_client_new` as it is given; the
SDK has no defaults of its own, so an option left unset is the library's
default.

| Option | Library default | Meaning |
|---|---|---|
| `socketPath` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeoutMs` | 100 | How long one read or write on the socket may take. 0 is refused. |
| `connectTimeoutMs` | 1500 | How long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. |
| `autostart` | on | Start a daemon when none serves the auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`; a template that calls it without the option fails with `exec() is disabled in this context`. |

Over the subprocess transport `timeoutMs` bounds each spawned `comb`, and
`connectTimeoutMs` is refused with `UnsupportedError`: `comb` has
no flag to carry it.

#### `client.transport()`

Returns `"ffi"` or `"subprocess"` — which transport this client is using.

#### `client.get(key, opts?)`

The value `comb get` prints for this key, cwd and env: the daemon holds the
templates (`<provider>:template`, seeded from configuration or `put`) and
the client renders them. A plain field is its cached value, a templated one
is rendered, and a bare provider name is the whole merged namespace; a miss
is `null`.

```typescript
const branch = await client.get('git.branch', { cwd: '/some/repo' });
const host   = await client.get('hostname.short');        // no cwd: the global entry
const git    = await client.get('git', { cwd: '/some/repo' }); // full provider (an object)
const fresh  = await client.get('git.branch', { cwd: '/some/repo', force: true, wait: true });
```

`opts` is `{ cwd?, env?, force?, wait? }`. `cwd` is the directory the read
addresses (`~` is home, a relative path is against this process's working
directory); omitted, it is the global entry. `env` is the caller's
environment for `env.*` refs; omitted, every `env.*` is absent. `force`
re-executes every Source the read demands first; `wait` re-executes a
stale entry inline before answering.

The unrendered halves are facets, read like any key:

```typescript
await client.get('git:template.branch'); // the template, as stored
await client.get('myapp:put.workspace'); // the value a put stored
await client.get('git:source.branch');   // the value a Source produced
```

#### `client.refresh(key, path?)`

Re-execute the Sources `key` demands, each at the instance that answers for `path`. A key naming no
Source (a static namespace, a facet, an unknown name, a field no Source
owns) rejects with `nothing to refresh: <key> has no sources`. Works over
both transports — see "Transports and their limits" below.

#### `client.put(key, data, opts?)`

Store data as static data. `data` is required; `put(key, null)` removes the node, and the entry itself is dropped once no records, static or provider-produced, remain at it.

#### `client.status()`

Return the cache rows: `CacheRow[]`, each with `provider`, `path`,
`source`, `field`, `value`, `ageMs`, `stale`, `kind`, `pollIntervalSecs`,
`pollCount`, `fseventsReinstate`, `pollsElapsed`, `nextPollInSecs` and
`failure`, as the daemon writes them (a field with no value is undefined).
Status data that is not an array is a `ParseError`.

#### `client.hello()` / `client.introspect(subject, opts?)`

Daemon protocol/version info (`{ protocolVersion, daemonVersion }`), and
internal daemon introspection (`"daemon" | "providers" | "config" | "cache"
| "lifecycle" | "watches" | "timers" | "demand" | "procs"`). An introspect
response is `{ subject, daemon, other }`: `daemon` is the typed
`DaemonHealth` (with its `reaper` status, `null` when the daemon has none,
and its `verdicts`) for the `daemon` subject and `null` otherwise; `other`
is any other subject's JSON, and `null` for `daemon`. **FFI transport
only**.

#### `client.eval(templateStr, opts?)`

Render a template that is stored nowhere — the same value `comb eval`
prints. Every ref resolves exactly as it would inside a stored template, so
`eval('{{ p.f }}')` and `get('p.f')` agree. `opts` is `{ cwd?, env? }`,
read as `get` reads them: no `cwd` is the global entry. **FFI transport
only**.

`templateStr` accepts a single `{{ expr }}` tag (keeps the expression's
natural type), or literal text/several tags (always a string — a source
with no tags at all is its literal text).

```typescript
await client.put('myapp:template', { workspace: '{{ env.MYAPP_ENV or comb.myappcache.workspace }}' });
const workspace = await client.get('myapp.workspace', {
  cwd: '/some/repo',
  env: { MYAPP_ENV: 'prod' },
});
const same = await client.eval('{{ myapp.workspace }}', { cwd: '/some/repo', env: { MYAPP_ENV: 'prod' } });
```

#### `client.session()`

Open a persistent connection (FFI) or a lightweight context-remembering
wrapper (subprocess — there is no real connection to share there). More
efficient when querying multiple keys in sequence.

```typescript
const session = await client.session();
await session.setContext('/some/repo');   // optional — sets the session's directory
const branch = await session.get('git.branch');
const dirty  = await session.get('git.dirty');
session.close();
```

`setContext` sets the session's directory: every later `get` that names no
`cwd`, and `put` and `refresh` that name no `path`, address it. `session.get`
takes the same `opts` as `client.get`. A session has `setContext`, `get`,
`put`, `refresh` and `close`; `status`, `hello` and `introspect` address no
directory, so they are the client's alone.

#### `client.watch(key, opts?)`

Watch a key's *value* — what `get` answers for it — and receive it
again whenever it changes, so a template edited under the watch fires it
just as a changed cache record does. `opts` is `{ cwd?, env? }`, read as
`get` reads them: no `cwd` watches the global entry.

```typescript
const stream = await client.watch('git.branch', { cwd: '/some/repo' });
for await (const event of stream) {
  console.log(event.data); // the value, null for a miss
}

const next = await stream.nextEvent(500); // a WatchEvent, or null once the stream has ended
```

`stream.nextEvent(timeoutMs?)` resolves the next `WatchEvent`, or `null`
once the stream has ended (the daemon closed it, or it was cancelled); an
event whose `data` is `null` is a miss, not the end. Omitted, `timeoutMs`
waits indefinitely; a timeout that runs out with no event rejects with
`TimeoutError`. `stream.cancel()` ends the stream (a `nextEvent` waiting
now answers `null`); `stream.close()` ends it and releases it. Iterating
with `for await` ends where `nextEvent` answers `null`, and closes the
stream.

#### `client.close()`

Release the underlying client handle (FFI only; a no-op under subprocess).

### Errors

Every error is a `CombError` (extends `Error`), one class per kind of
[`docs/sdk-api.md`](../../docs/sdk-api.md), carrying `kind` (a stable slug —
check it, or the class, not the message), `message` (the library's words,
unchanged) and `libraryVersion` (the `bc_version()` of the library that
raised it; the subprocess tier reports `subprocess:<comb>`). An unrecognised
kind from a newer library is a bare `CombError` with the kind kept.

| Class | `kind` | When thrown |
|---|---|---|
| `DaemonNotRunningError` | `daemon_not_running` | No daemon answers, and none was started |
| `ConnectionFailedError` | `connection_failed` | An explicit `socketPath` could not be connected to |
| `IoError` | `io_error` | A read or write on the socket failed |
| `ParseError` | `parse_error` | A request, a response or a template could not be read |
| `ServerError` | `server_error` | The daemon refused the request, or a template failed to render |
| `TimeoutError` | `timeout` | A read or write ran out `timeoutMs`, or `nextEvent` ran out its timeout |
| `BadFlagsError` | `bad_flags` | A flag bit the ABI does not define was set |
| `BusyError` | `busy` | A session or watch is in use by another caller |
| `PanicError` | `panic` | The native library panicked (caught at the FFI boundary) |
| `VersionSkewError` | `version_skew` | The daemon is a different version from the library |
| `LibraryNotFoundError` | `library_not_found` | No `libbeachcomber` was found (FFI), or no `comb` to spawn (subprocess) |
| `LibrarySymbolError` | `library_symbol` | The loaded library lacks a required `bc_*` function |
| `UnsupportedError` | `unsupported` | The subprocess transport cannot do this operation |

## Transports and their limits

**FFI (`koffi`)** implements the full ABI: `get`, `put`,
`refresh`, `status`, `hello`, `introspect`, `eval`, sessions,
and watch — all with exact envelope fidelity.

**Subprocess (`comb` CLI fallback)** covers `get`, `put`,
`refresh`, `status`, and session context. `timeoutMs` bounds each spawned
`comb`, with no default of its own; `connectTimeoutMs` is refused. `get` is one `comb get -f json
<key> [--path P | --global] [--force] [--wait]`: the value `comb get`
prints, the same value the FFI tier answers — the directory chain up to
`$HOME`, `$HOME` as the global entry, `force`/`wait`, the daemon's
refusals. Two things cannot cross the spawn. A spawned `comb` renders with
its own environment, so a `get` that passes `env` throws
`UnsupportedError`; with no `env` it renders with this process's
environment, where the FFI tier renders with none. And the CLI renders
with `exec()` on, so the client's `exec` option has no effect here. A
caller that needs either needs the FFI transport. `refresh` is one `comb
refresh <key> [--path P | --global]`, the daemon's op itself; its refusal
(`nothing to refresh: <key> has no sources`) is the `ServerError`'s `message`,
the daemon's own words — the same text the FFI transport reports. The tier
does **not** implement `hello`, `introspect`, `eval`, or `watch`: `comb`
has no CLI surface for `hello` or `introspect`, and `comb eval` and `comb
watch` read their environment from their own process, so the `env` a
caller passes could not be honoured.
Those calls throw `UnsupportedError` rather than silently
returning wrong data. An omitted `cwd` or `path` means the global entry,
as under FFI (the transport passes `--global`).

## Library discovery (FFI transport)

1. `$BEACHCOMBER_LIB`, if set — an explicit absolute path.
2. `../lib/<library name>` relative to `comb` resolved on `$PATH` — the
   library and binary ship together, so this comes before the system path
   deliberately.
3. The platform default dynamic-linker search path.

If none resolve, `LibraryNotFoundError` names every location tried. A
loaded library missing a required symbol raises `LibrarySymbolError`
naming the symbol, with the library's own `bc_version()` as its
`libraryVersion` — this is checked
at load, not on first use, and is never a silent fallback to the
subprocess tier: a missing/broken library is an install bug, whereas
`koffi` being absent is a known configuration.

## Socket discovery

The socket path is resolved in this order:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path: a single stable path per user, so singleton
enforcement is per-user rather than per-session. No session-scoped environment
(`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted. Override with the `socketPath` option.

## Development

```sh
npm install
npm run build   # compile to dist/
npm test        # run tests with node:test (integration tests need a built comb + libbeachcomber)
```

# beachcomber Node.js SDK

Node.js/TypeScript client for the [beachcomber](https://github.com/NavistAu/beachcomber) shell state daemon.

A binding over the beachcomber C ABI (`libbeachcomber.{so,dylib}`), not a
hand-rolled wire-protocol client: `koffi` (an **optional peer dependency**)
gives direct FFI access to the native library when installed; without it,
the SDK falls back to shelling out to `comb` (~5ms per call, against
~0.3ms via FFI — a documented slow tier, not silent). Check
`client.transport()` to see which one is active.

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
`connectTimeoutMs` is refused with `UnsupportedTransportError`: `comb` has
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

Return cache rows from the daemon.

#### `client.hello()` / `client.introspect(subject, opts?)`

Daemon protocol/version info, and internal daemon introspection
(`"daemon" | "providers" | "config" | "cache" | "lifecycle" | "watches" |
"timers" | "demand" | "procs"`). **FFI transport only**.

#### `client.eval(templateStr, opts)`

Render a template that is stored nowhere — the same value `comb eval`
prints. Every ref resolves exactly as it would inside a stored template, so
`eval('{{ p.f }}')` and `get('p.f')` agree. `opts.cwd` is **required**, and
read as `get` reads it; `opts.env` is optional. **FFI transport only**.

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
takes the same `opts` as `client.get`.

#### `client.watch(key, { cwd, env? })`

Watch a key's *value* — what `get` answers for it — and receive it
again whenever it changes, so a template edited under the watch fires it
just as a changed cache record does. `cwd` and `env` mean what they mean
for `get`, except that `cwd` is required here. The stream is an `AsyncIterable<WatchEvent>`; call
`stream.close()` to stop watching.

```typescript
const stream = await client.watch('git.branch', { cwd: '/some/repo' });
for await (const event of stream) {
  console.log(event.data); // the value, null for a miss
}
```

#### `client.close()`

Release the underlying client handle (FFI only; a no-op under subprocess).

### Errors

Every error is a `CombError` (extends `Error`) carrying a stable,
machine-readable `kind` — check `err.kind`, not `err.message`, for
programmatic handling.

| Class | `kind` | When thrown |
|---|---|---|
| `DaemonNotRunning` | `daemon_not_running` | Socket unreachable |
| `ServerError` | `server_error` | Daemon responded `ok: false` |
| `ParseError` | `parse_error` | A response/envelope was not valid JSON |
| `BadFlagsError` | `bad_flags` | A reserved `get` flag bit was set |
| `BusyError` | `busy` | A session/watch handle is in use by another caller |
| `PanicError` | `panic` | The native library panicked (caught at the FFI boundary) |
| `VersionSkewError` | `version_skew` | Daemon version doesn't match the loaded library |
| `ConnectionFailedError` | `connection_failed` | Low-level connection failure |
| `IoErrorError` | `io_error` | Low-level I/O failure |
| `TimeoutError` | `timeout` | Call did not complete in time |
| `LibraryDiscoveryError` | `library_discovery_failed` | No native library candidate could be loaded (FFI transport) |
| `MissingSymbolError` | `missing_symbol` | A loaded library is missing a required `bc_*` symbol |
| `UnsupportedTransportError` | `unsupported_transport` | This op has no faithful implementation over the subprocess transport |

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
`UnsupportedTransportError`; with no `env` it renders with this process's
environment, where the FFI tier renders with none. And the CLI renders
with `exec()` on, so the client's `exec` option has no effect here. A
caller that needs either needs the FFI transport. `refresh` is one `comb
refresh <key> [--path P | --global]`, the daemon's op itself; its refusal
(`nothing to refresh: <key> has no sources`) is `ServerError.serverMessage`,
the daemon's own words — the same text the FFI transport reports. The tier
does **not** implement `hello`, `introspect`, `eval`, or `watch`: `comb`
has no CLI surface for `hello` or `introspect`, and `comb eval` and `comb
watch` read their environment from their own process, so the `env` a
caller passes could not be honoured.
Those calls throw `UnsupportedTransportError` rather than silently
returning wrong data. An omitted `cwd` or `path` means the global entry,
as under FFI (the transport passes `--global`).

## Library discovery (FFI transport)

1. `$BEACHCOMBER_LIB`, if set — an explicit absolute path.
2. `../lib/<library name>` relative to `comb` resolved on `$PATH` — the
   library and binary ship together, so this comes before the system path
   deliberately.
3. The platform default dynamic-linker search path.

If none resolve, `LibraryDiscoveryError` names every location tried. A
loaded library missing a required symbol raises `MissingSymbolError`
naming the symbol and the library's own `bc_version()` — this is checked
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

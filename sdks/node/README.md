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

const result = await client.get('git.branch', '/path/to/repo');
if (result.isHit) {
  console.log(result.getString());  // e.g. "main"
  console.log(result.ageMs);        // e.g. 1234
  console.log(result.stale);        // false
}
```

## API

### `Client`

```typescript
const client = new Client();
const client = new Client({ socketPath: '/custom/path' });
const client = new Client({ socketPath: '/custom/path', timeoutMs: 2000, autostart: true });
const client = new Client({ exec: true }); // templates this client resolves may call exec()
```

`exec` is off by default: a template that calls `exec()` on a client
without it fails with `exec() is disabled in this context`.

#### `client.transport()`

Returns `"ffi"` or `"subprocess"` — which transport this client is using.

#### `client.get(key, path?)`

Read a node raw — both halves, nothing rendered.

```typescript
const result = await client.get('git.branch', '/some/repo');
const result = await client.get('hostname');          // global provider, no path needed
const result = await client.get('git', '/some/repo'); // full provider (returns object)
```

Returns a `CombResult` (see below).

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

#### `client.resolve(key, opts)` / `client.eval(templateStr, opts)`

`resolve` returns the same value `comb get` returns for this key, cwd and
env: the daemon holds the templates (`<provider>:template`, seeded from
configuration or `put`) and the client renders them. A plain field is its
cached value, a templated one is rendered, and a bare provider name is the
whole merged namespace; a miss is `null`. `eval` renders a template that
is stored nowhere — the same value `comb eval` prints. `opts.cwd` is
**required**: this library never falls back to the process's own working
directory, and every fetch the resolution makes addresses it. `opts.env`
is optional. **FFI transport only**.

`templateStr` accepts a single `{{ expr }}` tag (keeps the expression's
natural type), or literal text/several tags (always a string — a source
with no tags at all is its literal text).

```typescript
await client.put('myapp:template', { workspace: '{{ env.MYAPP_ENV or comb.myappcache.workspace }}' });
const value = await client.resolve('myapp.workspace', {
  cwd: '/some/repo',
  env: { MYAPP_ENV: 'prod' },
});
```

#### `client.session()`

Open a persistent connection (FFI) or a lightweight context-remembering
wrapper (subprocess — there is no real connection to share there). More
efficient when querying multiple keys in sequence.

```typescript
const session = await client.session();
await session.setContext('/some/repo');   // optional — sets default path
const branch = await session.get('git.branch');
const dirty  = await session.get('git.dirty');
session.close();
```

`setContext` sets the session's directory: every later `get`, `put` and
`refresh` that names no path addresses it.

#### `client.watch(key, { cwd, env? })`

Watch a key's *value* — what `resolve` answers for it — and receive it
again whenever it changes, so a template edited under the watch fires it
just as a changed cache record does. `cwd` and `env` mean what they mean
for `resolve`. The stream is an `AsyncIterable<WatchEvent>`; call
`stream.close()` to stop watching.

```typescript
const stream = await client.watch('git.branch', { cwd: '/some/repo' });
for await (const event of stream) {
  console.log(event.data); // the resolved value, null for a miss
}
```

#### `client.close()`

Release the underlying client handle (FFI only; a no-op under subprocess).

### `CombResult`

`get` returns the node as the daemon holds it — both halves, nothing
rendered. A node may hold a cached value (`data`), a template (`template`),
or both. A template-only result is neither a hit nor a miss: it is an
address whose value `resolve` renders — `get` is the raw half,
`resolve(key, { cwd, env })` is how you get the value.

| Property / method | Type | Description |
|---|---|---|
| `isHit` | `boolean` | `true` when the cache had a value |
| `isMiss` | `boolean` | `true` when nothing is stored at the address: no value **and** no template |
| `data` | `unknown` | Raw data (undefined when nothing is cached) |
| `ageMs` | `number` | Cache age in milliseconds (0 when nothing is cached) |
| `stale` | `boolean` | Whether the value is stale (false when nothing is cached) |
| `template` | `unknown` | The template in effect at the address, unrendered: a string for a leaf, an object for a group (undefined when none is in effect) |
| `rest` | `string \| undefined` | The address left over when the template walk stopped at a string — `get('p.t.x')` where `p.t` is a template string gives the template of `p.t` and `rest === 'x'` |
| `getString(field?)` | `string \| undefined` | Data as a string; picks a named field from object results |
| `getNumber(field?)` | `number \| undefined` | Data as a number |
| `getBool(field?)` | `boolean \| undefined` | Data as a boolean |

For full provider queries (e.g. `key = "git"`), the data is an object.
Use the `field` argument to pick a field:

```typescript
const result = await client.get('git', '/repo');
result.getString('branch');   // "main"
result.getBool('dirty');      // false
```

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
`refresh`, `status`, `hello`, `introspect`, `resolve`, `eval`, sessions,
and watch — all with exact envelope fidelity.

**Subprocess (`comb` CLI fallback)** covers `get`, `put`,
`refresh`, `status`, and session context. `get` is one `comb get
-f json <key> <p>:cache.<rest>`, so its answer is the resolver's — the
directory chain up to `$HOME`, `$HOME` as the global entry, the `:cache`
facet's `age_ms` and `stale`, `force`/`wait`, the daemon's refusals — with one divergence:
`comb get` *resolves*, so a template-backed key returns its **rendered**
value where the FFI tier returns the raw template half. A result from
this tier never carries `template` or `rest`; a template-only node is a
hit holding the rendered value (age 0, not stale — it has no cached
freshness of its own). The CLI renders with `exec()` on, so the client's
`exec` option has no effect here — a caller that must not spawn processes
from a template, or that needs the raw template half, needs the FFI
transport. `refresh` is one `comb refresh <key> [--path P | --global]`,
the daemon's op itself; its refusal (`nothing to refresh: <key> has no
sources`) is `ServerError.serverMessage`, the daemon's own words — the
same text the FFI transport reports. The tier does
**not** implement `hello`, `introspect`, `resolve`, `eval`, or `watch`:
`comb` has no CLI surface for `hello` or `introspect`, and `resolve`/`eval`
are not wired over this tier (`comb get`/`comb eval` resolve through the
same library, so a shell-out would be faithful — nobody has needed it).
Those calls throw `UnsupportedTransportError` rather than silently
returning wrong data. An omitted `path` means the global entry, as under
FFI (the transport passes `--global`).

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

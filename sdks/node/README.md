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
npm install beachcomber
npm install koffi   # optional — direct FFI; omit to use the comb subprocess fallback
```

## Quick Start

```typescript
import { Client } from 'beachcomber';

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
```

#### `client.transport()`

Returns `"ffi"` or `"subprocess"` — which transport this client is using.

#### `client.get(key, path?)`

Read a cached value.

```typescript
const result = await client.get('git.branch', '/some/repo');
const result = await client.get('hostname');          // global provider, no path needed
const result = await client.get('git', '/some/repo'); // full provider (returns object)
```

Returns a `CombResult`.

#### `client.refresh(key, path?)`

Force the daemon to recompute a provider. **FFI transport only** — see
"Transports and their limits" below.

#### `client.put(key, data?, opts?)` / `client.putNull(key, path?)`

Store data in a virtual provider, or clear its cached entry (keeping the
registry entry) with `putNull`.

#### `client.status()`

Return cache rows from the daemon.

#### `client.hello()` / `client.introspect(subject, opts?)`

Daemon protocol/version info, and internal daemon introspection
(`"daemon" | "providers" | "config" | "cache" | "lifecycle" | "watches" |
"timers" | "demand" | "procs"`). **FFI transport only**.

#### `client.resolve(key, opts)` / `client.eval(templateStr, opts)`

Client-side field resolution — the same evaluator `comb get`'s resolution
layer uses, run entirely in this process (no daemon round-trip beyond
fetching any `cache.*` refs the expression needs). `opts.cwd` is
**required**: this library never falls back to the process's own working
directory. `opts.env` and `opts.overrides` are optional. **FFI transport
only**.

```typescript
const value = await client.resolve('myapp.workspace', {
  cwd: '/some/repo',
  env: { MYAPP_ENV: 'prod' },
  overrides: { 'myapp.workspace': 'env.MYAPP_ENV or cache.myappcache.workspace' },
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

Note: the daemon's `put` op does not consult connection context (only
`get`/`refresh` do) — pass an explicit `path` to `session.put()` regardless
of `setContext`.

#### `client.watch(key, path?)`

Open a watch stream. The stream is an `AsyncIterable<WatchEvent>`; call
`stream.close()` to stop watching.

```typescript
const stream = await client.watch('git.branch', '/some/repo');
for await (const event of stream) {
  console.log(event.data);
}
```

#### `client.close()`

Release the underlying client handle (FFI only; a no-op under subprocess).

### `CombResult`

| Property / method | Type | Description |
|---|---|---|
| `isHit` | `boolean` | `true` when the cache had a value |
| `isMiss` | `boolean` | `true` when the cache had no value |
| `data` | `unknown` | Raw data (undefined on miss) |
| `ageMs` | `number` | Cache age in milliseconds (0 on miss) |
| `stale` | `boolean` | Whether the value is stale (false on miss) |
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

**FFI (`koffi`)** implements the full ABI: `get`, `put`, `putNull`,
`refresh`, `status`, `hello`, `introspect`, `resolve`, `eval`, sessions,
and watch — all with exact envelope fidelity.

**Subprocess (`comb` CLI fallback)** covers `get`, `put`, `putNull`,
`status`, `watch`, and session context. It does **not** implement `hello`,
`introspect`, `refresh`, `resolve`, or `eval` — `comb`'s CLI has no
faithful JSON equivalent for them (see `src/subprocess_transport.ts` for
why each one specifically can't be approximated safely), and those calls
throw `UnsupportedTransportError` rather than silently returning wrong
data. It is also asymmetric on omitted paths: `comb get` without `--path`
defaults to the CLI process's own working directory, while `comb put`
without `--path` means "global" — pass an explicit `path` for global
(pathless) providers under this transport.

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

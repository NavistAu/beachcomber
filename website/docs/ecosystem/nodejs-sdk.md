---
sidebar_position: 4
---

# Node.js SDK

Node.js/TypeScript client for the beachcomber shell state daemon. A binding over the shared `libbeachcomber` C ABI, not a hand-rolled wire-protocol client: `koffi` (an optional peer dependency) gives direct FFI access to the native library when installed; without it, the SDK falls back to shelling out to `comb` (~5ms per call vs ~0.3ms via FFI). Published on [npm](https://www.npmjs.com/package/libbeachcomber).

It follows the one SDK API table every beachcomber SDK follows ([`docs/sdk-api.md`](https://github.com/NavistAu/beachcomber/blob/develop/docs/sdk-api.md)): the same operations, names, value shapes and error kinds, in camelCase.

## Requirements

- Node.js 18+
- A running `comb` daemon

## Installation

```sh
npm install libbeachcomber
```

## Quick start

```typescript
import { Client } from 'libbeachcomber';

const client = new Client();

// What `comb get git.branch` prints there; null on a miss.
const branch = await client.get('git.branch', { cwd: '/path/to/repo' });
console.log(branch); // e.g. "main"
```

## API reference

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
| `timeoutMs` | 250 | How long one read or write on the socket may take. 0 is refused. |
| `connectTimeoutMs` | 1500 | How long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. |
| `autostart` | on | Start a daemon when none serves the auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`; a template that calls it without the option fails with `exec() is disabled in this context`. |

Over the subprocess transport `timeoutMs` bounds each spawned `comb`, and
`connectTimeoutMs` is refused with `UnsupportedError`: `comb` has
no flag to carry it.

#### `client.get(key, opts?)`

The value `comb get` prints for this key, cwd and env: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace; a miss is `null`.

`opts` is `{ cwd?, env?, force?, wait? }`. `cwd` is the directory the read addresses (`~` is home, a relative path is against this process's working directory); omitted, it is the global entry. `env` is the caller's environment for `env.*` refs; omitted, the calling process's environment, as `comb` renders. `force` re-executes every Source the read demands first, as `comb get --force`; `wait` is how long to wait for a run the read needs before answering what is stored, as `comb get --wait`: a duration (`'0'`, `'50ms'`, `'2s'`) or `'forever'`, anything else a `ParseError`.

```typescript
const branch = await client.get('git.branch', { cwd: '/some/repo' });
const host   = await client.get('hostname.short');              // no cwd: the global entry
const git    = await client.get('git', { cwd: '/some/repo' });  // full provider (an object)
await client.put('myapp:template', { workspace: '{{ env.MYAPP_ENV or comb.myappcache.workspace }}' });
const workspace = await client.get('myapp.workspace', { cwd: '/some/repo', env: { MYAPP_ENV: 'prod' } });
```

The unrendered halves are facets, read like any key: `git:template.branch` is the template as stored, `myapp:put.workspace` and `git:source.branch` those layers' values.

Over the subprocess fallback a `get` is one `comb get -f json`, an `eval` one `comb eval -f json`, and a `watch` one long-lived `comb watch -f json`. The call's `env` map is the spawned `comb`'s whole environment (with no map, this process's), `--socket` carries `socketPath` or `$BEACHCOMBER_SOCKET`, `--color` follows `NO_COLOR` in the render's environment, and `--no-exec` goes unless the client was made with `exec: true`, so it renders as the FFI transport does.

#### `client.refresh(key, path?)`

Force the daemon to recompute a provider.

```typescript
await client.refresh('git', '/some/repo');
```

#### `client.status()`

Return typed cache rows as `CacheRow[]` (one per warm cache entry): `provider`, `path`, `source`, `field`, `value`, `ageMs`, `kind`, `pollIntervalSecs`, `pollCount`, `pollsElapsed`, `nextPollInSecs` and `failure`, a field with no value undefined. Status data that is not an array is a `ParseError`.

#### `client.hello()`

Handshake — returns a `HelloInfo` with daemon and protocol version strings.

#### `client.put(key, data?, opts?)`

Write a value into the cache as static data. Omit `data` or pass `null` to clear the entry.

#### `client.introspect(subject, opts?)`

Inspect a daemon subsystem (`"daemon"` | `"providers"` | `"config"` | `"cache"` | `"lifecycle"` | `"watches"` | `"timers"` | `"demand"` | `"procs"`). The response is `{ subject, daemon, other }`: `daemon` is the typed `DaemonHealth` (its `reaper` `null` when the daemon has none) for the `daemon` subject and `null` otherwise; `other` is any other subject's JSON, `null` for `daemon`.

#### `client.watch(key, opts?)`

Subscribe to a key's value — what `get(key, opts)` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `opts` is `{ cwd?, env? }`, read as `get` reads them: no `cwd` watches the global entry. Returns a `WatchStream`: `nextEvent(timeoutMs?)` resolves the next `WatchEvent` (whose `data` is the value, `null` for a miss), or `null` once the stream has ended; a `timeoutMs` that runs out with no event rejects with `TimeoutError`, and omitted it waits indefinitely. `cancel()` ends the stream, `close()` ends and releases it, and `for await (const event of stream)` iterates until the end. Over the subprocess fallback a watch is one long-lived `comb watch -f json`, spawned as `get` is, whose child `cancel()` and `close()` stop.

#### `client.eval(templateStr, opts?)`

Render a template that is stored nowhere — the same value `comb eval` prints. Every ref resolves exactly as it would inside a stored template, so `eval('{{ p.f }}')` and `get('p.f')` agree. `opts` is `{ cwd?, env? }`, read as `get` reads them: no `cwd` is the global entry. `templateStr` accepts a single `{{ expr }}` tag (keeps the expression's natural type), or literal text/several tags (always a string — a source with no tags at all is its literal text). FFI transport only.

```typescript
const value = await client.eval('{{ myapp.workspace }}', {
  cwd: '/some/repo',
  env: { MYAPP_ENV: 'prod' },
});
```

#### `client.session()`

Open a persistent connection. More efficient when querying multiple keys in sequence (one TCP round-trip per query instead of one connection per query).

```typescript
const session = await client.session();
await session.setContext('/some/repo');   // optional — sets the session's directory
const branch = await session.get('git.branch');
const dirty  = await session.get('git.dirty');
await session.refresh('git');
session.close();
```

`setContext` sets the session's directory: every later `get` that names no `cwd`, and `put` and `refresh` that name no `path`, address it. `session.get` takes the same `opts` as `client.get`. A session has `setContext`, `get`, `put`, `refresh` and `close`; `status`, `hello` and `introspect` are the client's alone.

## Errors

One class per error kind, each extending `CombError` (which extends `Error`) and carrying `kind` (the slug), `message` (the library's words, unchanged) and `libraryVersion` (the library that raised it).

| Class | `kind` | When thrown |
|---|---|---|
| `DaemonNotRunningError` | `daemon_not_running` | No daemon answers, and none was started |
| `ConnectionFailedError` | `connection_failed` | An explicit `socketPath` could not be connected to |
| `IoError` | `io_error` | A read or write on the socket failed |
| `ParseError` | `parse_error` | A request, a response or a template could not be read |
| `ServerError` | `server_error` | The daemon refused the request, or a template failed to render |
| `TimeoutError` | `timeout` | A read or write ran out `timeoutMs`, or `nextEvent` ran out its timeout |
| `BusyError` | `busy` | A session or watch is in use by another caller |
| `PanicError` | `panic` | The native library panicked (caught at the FFI boundary) |
| `LibraryNotFoundError` | `library_not_found` | No `libbeachcomber` was found (FFI), or no `comb` to spawn (subprocess) |
| `LibrarySymbolError` | `library_symbol` | The loaded library lacks a required `bc_*` function |
| `UnsupportedError` | `unsupported` | The subprocess transport cannot do this operation |

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Library discovery

The library is `libbeachcomber.dylib` on macOS and `libbeachcomber.so` on
every other platform. It is located by the one rule every SDK follows
([docs/sdk-api.md](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md#library-discovery),
checked against the shared fixture `sdks/discovery.json`), trying in order:

1. `$BEACHCOMBER_LIB`, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library.
2. Beside `comb`: `<dir>/../lib/<file>`, where `<dir>` is the directory of
   the first executable `comb` on `$PATH`, its symlinks resolved. A Homebrew
   `comb` is a symlink into the Cellar, and the library sits in the Cellar
   beside it; a `.deb` or `.rpm` puts them in `/usr/bin` and `/usr/lib`.
3. The system loader, given the bare file name.

If none loads, the FFI transport raises `LibraryNotFoundError`, naming every location tried and why each failed.

## Socket discovery

The socket path is resolved in this order:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

Override with the `socketPath` option.

---
sidebar_position: 4
---

# Node.js SDK

Node.js/TypeScript client for the beachcomber shell state daemon. A binding over the shared `libbeachcomber` C ABI, not a hand-rolled wire-protocol client: `koffi` (an optional peer dependency) gives direct FFI access to the native library when installed; without it, the SDK falls back to shelling out to `comb` (~5ms per call vs ~0.3ms via FFI). Published on [npm](https://www.npmjs.com/package/libbeachcomber).

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
| `timeoutMs` | 100 | How long one read or write on the socket may take. 0 is refused. |
| `connectTimeoutMs` | 1500 | How long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. |
| `autostart` | on | Start a daemon when none serves the auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`; a template that calls it without the option fails with `exec() is disabled in this context`. |

Over the subprocess transport `timeoutMs` bounds each spawned `comb`, and
`connectTimeoutMs` is refused with `UnsupportedTransportError`: `comb` has
no flag to carry it.

#### `client.get(key, opts?)`

The value `comb get` prints for this key, cwd and env: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace; a miss is `null`.

`opts` is `{ cwd?, env?, force?, wait? }`. `cwd` is the directory the read addresses (`~` is home, a relative path is against this process's working directory); omitted, it is the global entry. `env` is the caller's environment for `env.*` refs; omitted, every `env.*` is absent. `force` re-executes every Source the read demands first; `wait` re-executes a stale entry inline before answering.

```typescript
const branch = await client.get('git.branch', { cwd: '/some/repo' });
const host   = await client.get('hostname.short');              // no cwd: the global entry
const git    = await client.get('git', { cwd: '/some/repo' });  // full provider (an object)
await client.put('myapp:template', { workspace: '{{ env.MYAPP_ENV or comb.myappcache.workspace }}' });
const workspace = await client.get('myapp.workspace', { cwd: '/some/repo', env: { MYAPP_ENV: 'prod' } });
```

The unrendered halves are facets, read like any key: `git:template.branch` is the template as stored, `myapp:put.workspace` and `git:source.branch` those layers' values.

Over the subprocess fallback a `get` is one `comb get -f json`, which renders with that process's own environment: a `get` that passes `env` throws `UnsupportedTransportError`, and one without renders with this process's environment, with `exec()` on.

#### `client.refresh(key, path?)`

Force the daemon to recompute a provider.

```typescript
await client.refresh('git', '/some/repo');
```

#### `client.status()`

Return typed cache rows as `CacheRow[]` (one per warm cache entry).

#### `client.hello()`

Handshake — returns a `HelloInfo` with daemon and protocol version strings.

#### `client.put(key, data?, opts?)`

Write a value into the cache as static data. Omit `data` or pass `null` to clear the entry.

#### `client.introspect(subject, opts?)`

Inspect a daemon subsystem (`"daemon"` | `"providers"` | `"config"` | `"cache"` | `"lifecycle"` | `"watches"` | `"timers"` | `"demand"` | `"procs"`).

#### `client.watch(key, opts)`

Subscribe to a key's value — what `get(key, opts)` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `opts.cwd` and `opts.env` mean what they mean for `get`, except that `opts.cwd` is required. Returns a `WatchStream` (async iterable) yielding `WatchEvent` values, whose `data` is the value — `null` for a miss. FFI transport only: the subprocess fallback tier does not implement `watch`, joining `eval` there.

#### `client.eval(templateStr, opts)`

Render a template that is stored nowhere — the same value `comb eval` prints. Every ref resolves exactly as it would inside a stored template, so `eval('{{ p.f }}')` and `get('p.f')` agree. `opts.cwd` is **required**, read as `get` reads it; `opts.env` is optional. `templateStr` accepts a single `{{ expr }}` tag (keeps the expression's natural type), or literal text/several tags (always a string — a source with no tags at all is its literal text). FFI transport only.

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

`setContext` sets the session's directory: every later `get` that names no `cwd`, and `put` and `refresh` that name no `path`, address it. `session.get` takes the same `opts` as `client.get`.

## Errors

| Class | When thrown |
|---|---|
| `DaemonNotRunning` | Socket does not exist or connection was refused |
| `ServerError` | Daemon responded with `{"ok":false,"error":"..."}` |
| `ParseError` | Response was not valid JSON or not an object |

All extend `CombError` which extends `Error`.

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Socket discovery

The socket path is resolved in this order:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

Override with the `socketPath` option.

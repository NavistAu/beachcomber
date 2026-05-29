---
sidebar_position: 4
---

# Node.js SDK

Node.js/TypeScript client for the beachcomber shell state daemon. No external runtime dependencies — pure Node.js stdlib (`net`, `fs`, `os`, `path`). Published on [npm](https://www.npmjs.com/package/libbeachcomber).

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

const result = await client.get('git.branch', '/path/to/repo');
if (result.isHit) {
  console.log(result.getString());  // e.g. "main"
  console.log(result.ageMs);        // e.g. 1234
  console.log(result.stale);        // false
}
```

## API reference

### `Client`

```typescript
const client = new Client();
const client = new Client({ socketPath: '/custom/path' });
const client = new Client({ socketPath: '/custom/path', timeoutMs: 2000 });
```

#### `client.get(key, path?)`

Read a cached value. Returns a `CombResult`.

```typescript
const result = await client.get('git.branch', '/some/repo');
const result = await client.get('hostname');           // global provider, no path needed
const result = await client.get('git', '/some/repo');  // full provider (returns object)
```

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

Write a value into the cache as a virtual provider. Omit `data` or pass `null` to clear the entry.

#### `client.introspect(subject, opts?)`

Inspect a daemon subsystem (`"daemon"` | `"providers"` | `"config"` | `"cache"` | `"lifecycle"` | `"watches"`).

#### `client.watch(key, path?)`

Subscribe to live cache updates. Returns a `WatchStream` (async iterable) yielding `WatchEvent` values.

#### `client.getWithFlags(key, path?, opts?)`

Read a cached value with optional `force` (recompute) or `wait` (block for fresh value) flags.

#### `client.session()`

Open a persistent connection. More efficient when querying multiple keys in sequence (one TCP round-trip per query instead of one connection per query).

```typescript
const session = await client.session();
await session.setContext('/some/repo');   // optional — sets default path
const branch = await session.get('git.branch');
const dirty  = await session.get('git.dirty');
await session.refresh('git');
session.close();
```

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

For full provider queries (e.g. `key = "git"`), the data is an object. Use the `field` argument to pick a field:

```typescript
const result = await client.get('git', '/repo');
result.getString('branch');   // "main"
result.getBool('dirty');      // false
```

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

1. Config file override (if set in `~/.config/beachcomber/config.toml`)
2. `$BEACHCOMBER_SOCKET` environment variable
3. `$XDG_RUNTIME_DIR/beachcomber/sock`
4. `/tmp/beachcomber-<uid>/sock`

Override with the `socketPath` option.

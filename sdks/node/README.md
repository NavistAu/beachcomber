# beachcomber Node.js SDK

Node.js/TypeScript client for the [beachcomber](https://github.com/NavistAu/beachcomber) shell state daemon.

**No external runtime dependencies** — pure Node.js stdlib (`net`, `fs`, `os`, `path`).

## Requirements

- Node.js 18+
- A running `comb` daemon

## Installation

```sh
npm install beachcomber
```

## Quick Start

```typescript
import { Client } from 'beachcomber';

const client = new Client();

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
const client = new Client({ socketPath: '/custom/path', timeoutMs: 2000 });
```

#### `client.get(key, path?)`

Read a cached value.

```typescript
const result = await client.get('git.branch', '/some/repo');
const result = await client.get('hostname');          // global provider, no path needed
const result = await client.get('git', '/some/repo'); // full provider (returns object)
```

Returns a `CombResult`.

#### `client.poke(key, path?)`

Force the daemon to recompute a provider.

```typescript
await client.poke('git', '/some/repo');
```

#### `client.list()`

List available providers.

```typescript
const providers = await client.list();
// [{ name: 'git', global: false, fields: ['branch', 'dirty', ...] }, ...]
```

#### `client.status()`

Return daemon status information.

```typescript
const status = await client.status();
```

#### `client.session()`

Open a persistent connection.  More efficient when querying multiple keys
in sequence (one TCP round-trip per query instead of one connection per
query).

```typescript
const session = await client.session();
await session.setContext('/some/repo');   // optional — sets default path
const branch = await session.get('git.branch');
const dirty  = await session.get('git.dirty');
await session.poke('git');
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

For full provider queries (e.g. `key = "git"`), the data is an object.
Use the `field` argument to pick a field:

```typescript
const result = await client.get('git', '/repo');
result.getString('branch');   // "main"
result.getBool('dirty');      // false
```

### Errors

| Class | When thrown |
|---|---|
| `DaemonNotRunning` | Socket does not exist or connection was refused |
| `ServerError` | Daemon responded with `{"ok":false,"error":"..."}` |
| `ParseError` | Response was not valid JSON or not an object |

All extend `CombError` which extends `Error`.

## Socket discovery

The socket path is resolved in this order:

1. `$XDG_RUNTIME_DIR/beachcomber/sock`
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `<os.tmpdir()>/beachcomber-<uid>/sock`

Override with the `socketPath` option.

## Development

```sh
npm install
npm run build   # compile to dist/
npm test        # run tests with node:test
```

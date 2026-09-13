---
sidebar_position: 7
---

# Ruby SDK

Ruby client for the beachcomber daemon. Binds via `fiddle` to the shared C library (`libbeachcomber.{so,dylib}`) — no external dependencies, stdlib only (`fiddle`, `json`, `etc`, `rbconfig`). Ruby 3.0+. Published on [RubyGems](https://rubygems.org/gems/libbeachcomber).

## Installation

```sh
gem install libbeachcomber
```

## Quick start

```ruby
require 'beachcomber'

client = Beachcomber::Client.new          # auto-discovers socket
# client = Beachcomber::Client.new(socket_path: '/custom/path')
# client = Beachcomber::Client.new(timeout: 0.5)  # 500 ms
# client = Beachcomber::Client.new(exec: true)    # templates this client resolves may call exec()

result = client.get('git.branch', path: '/path/to/repo')
if result.hit?
  puts result.data    # "main"
  puts result.age_ms  # 42
  puts result.stale?  # false
end

# Hash data: full provider query
result = client.get('git', path: '/path/to/repo')
puts result['branch'] if result.hit?
```

## Sessions

Persistent connections avoid per-call socket overhead when making multiple queries:

```ruby
client.session do |s|
  s.set_context('/path/to/repo')   # sets default path for this connection
  r1 = s.get('git.branch')
  r2 = s.get('git.dirty')
  s.refresh('git')
end
# connection closed automatically
```

## API reference

### `Beachcomber::Client`

Opens a fresh socket connection for each call. Simple and stateless.

| Method | Description |
|--------|-------------|
| `get(key, path: nil)` | Read a node raw — both halves, nothing rendered. Returns a `Result`. |
| `get_with_flags(key, path: nil, force: false, wait: false)` | Read with protocol flags. Returns a `Result`. |
| `refresh(key, path: nil)` | Force recomputation. Returns `nil`. |
| `hello` | Handshake. Returns a `HelloInfo`. |
| `put(key, data = nil, ttl: nil, path: nil)` | Write a value into the cache. Returns `nil`. |
| `introspect(subject, duration_secs: nil)` | Inspect a daemon subsystem. Returns an `IntrospectResponse`. |
| `watch(key, path: nil)` | Subscribe to live updates. Returns a `WatchStream` (Enumerable). |
| `status` | Daemon cache status as typed rows. Returns `Array<CacheRow>`. |
| `session { \|s\| }` | Open a persistent connection (see above). |
| `resolve(key, cwd:, env: nil)` | The same value `comb get` returns for this key, cwd and env — the daemon holds the templates, the client renders them; a bare provider name is the whole merged namespace. |
| `eval_expression(template_str, cwd:, env: nil)` | Render a template stored nowhere — the same value `comb eval` prints. |

`resolve` and `eval_expression` require `cwd`. `template_str` (and each stored template) accepts a single `{{ expr }}` tag, or literal text/several tags — the former keeps the expression's natural type, the latter is always a string (a source with no tags at all is its literal text). `exec()` inside a template runs only on a client created with `Beachcomber::Client.new(exec: true)`; it is off by default, and a template that calls it then fails with `exec() is disabled in this context`.

### `Beachcomber::Session`

| Method | Description |
|--------|-------------|
| `set_context(path)` | Set default path for subsequent queries. |
| `get(key, path: nil)` | Read a cached value. |
| `get_with_flags(key, path: nil, force: false, wait: false)` | Read with protocol flags. |
| `refresh(key, path: nil)` | Force recomputation. |
| `hello` | Handshake. Returns a `HelloInfo`. |
| `put(key, data = nil, ttl: nil, path: nil)` | Write a value into the cache. |
| `introspect(subject, duration_secs: nil)` | Inspect a daemon subsystem. Returns an `IntrospectResponse`. |
| `status` | Daemon cache status as typed rows. Returns `Array<CacheRow>`. |
| `close` | Close the connection (called automatically by `Client#session`). |

### `Beachcomber::Result`

`get` returns the node as the daemon holds it — both halves, nothing rendered. A node may hold a cached value (`data`), a template (`template`), or both. A template-only result is neither a hit nor a miss: it is an address whose value `resolve` renders — `get` is the raw half, `resolve(key, cwd:, env:)` is how you get the value.

| Method | Returns | Description |
|--------|---------|-------------|
| `ok?` | Boolean | Daemon reported success. |
| `hit?` | Boolean | Success and data is present. |
| `miss?` | Boolean | Success and neither data nor a template is present. |
| `stale?` | Boolean | Data exists but is stale. |
| `data` | Object/nil | Decoded payload (String, Hash, Array, …). |
| `age_ms` | Integer | Age of the cached value in milliseconds. |
| `template` | String/Hash/nil | The template in effect at the address, unrendered: a String for a leaf, a Hash for a group. `nil` when none is in effect. |
| `rest` | String/nil | The address left over when the template walk stopped at a string — `get('p.t.x')` where `p.t` is a template string gives the template of `p.t` and `rest == 'x'`. |
| `error` | String/nil | Error message when `ok?` is false. |
| `[](key)` | Object/nil | Delegates to `data` hash. Raises `TypeError` if data is not a Hash. |

### Key format

- `"git"` — full provider, returns a Hash of all fields
- `"git.branch"` — single field, returns a scalar

## Errors

| Class | When raised |
|-------|-------------|
| `Beachcomber::DaemonNotRunning` | Socket unreachable (daemon not started). |
| `Beachcomber::ServerError` | Daemon returns `ok: false`. |
| `Beachcomber::ProtocolError` | Response is not valid JSON or unexpected format. |

All inherit from `Beachcomber::Error < StandardError`.

## Socket discovery

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

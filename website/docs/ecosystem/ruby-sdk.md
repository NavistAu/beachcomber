---
sidebar_position: 7
---

# Ruby SDK

Ruby client for the beachcomber daemon. Communicates over a Unix domain socket using newline-delimited JSON. No external dependencies — stdlib only (`socket`, `json`, `etc`). Ruby 3.0+. Published on [RubyGems](https://rubygems.org/gems/libbeachcomber).

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
| `get(key, path: nil)` | Read a cached value. Returns a `Result`. |
| `get_with_flags(key, path: nil, force: false, wait: false)` | Read with protocol flags. Returns a `Result`. |
| `refresh(key, path: nil)` | Force recomputation. Returns `nil`. |
| `hello` | Handshake. Returns a `HelloInfo`. |
| `put(key, data = nil, ttl: nil, path: nil)` | Write a value into the cache. Returns `nil`. |
| `introspect(subject, duration_secs: nil)` | Inspect a daemon subsystem. Returns an `IntrospectResponse`. |
| `watch(key, path: nil)` | Subscribe to live updates. Returns a `WatchStream` (Enumerable). |
| `status` | Daemon cache status as typed rows. Returns `Array<CacheRow>`. |
| `session { \|s\| }` | Open a persistent connection (see above). |

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

| Method | Returns | Description |
|--------|---------|-------------|
| `ok?` | Boolean | Daemon reported success. |
| `hit?` | Boolean | Success and data is present. |
| `miss?` | Boolean | Success but no data (cache miss). |
| `stale?` | Boolean | Data exists but is stale. |
| `data` | Object/nil | Decoded payload (String, Hash, Array, …). |
| `age_ms` | Integer | Age of the cached value in milliseconds. |
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

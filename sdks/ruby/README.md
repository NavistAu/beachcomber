# beachcomber Ruby SDK

Ruby client for the [beachcomber](https://github.com/NavistAu/beachcomber) daemon. Binds via `fiddle` to the shared C library (`libbeachcomber.{so,dylib}`).

**No external dependencies** — stdlib only (`fiddle`, `json`, `etc`, `rbconfig`). Ruby 3.0+.

## Installation

Copy the `lib/` directory into your project or install as a gem:

```sh
gem build libbeachcomber.gemspec
gem install libbeachcomber-0.9.1.gem
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

## API

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
| `session { \|s\| }` | Open a persistent connection (see below). |
| `resolve(key, cwd:, env: nil, overrides: nil)` | Client-side field resolution — the same evaluator `comb get`'s resolution layer uses. |
| `eval_expression(template_str, cwd:, env: nil, overrides: nil)` | Evaluate a value expression in any of the three forms — bare, one `{{ }}` tag, or a template — same evaluator as `resolve`. |

`resolve` and `eval_expression` require `cwd`. `template_str` (and each
field's own expression) accepts a bare expression, a single `{{ expr }}`
tag, or literal text/several tags — the first two keep the expression's
natural type, the third is always a string.

### `Beachcomber::Session`

Persistent connection. Use when making multiple queries per invocation to avoid per-call socket overhead.

```ruby
client.session do |s|
  s.set_context('/path/to/repo')   # sets default path for this connection
  r1 = s.get('git.branch')
  r2 = s.get('git.dirty')
  s.refresh('git')
end
# connection closed automatically
```

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

### Errors

| Class | When raised |
|-------|-------------|
| `Beachcomber::DaemonNotRunning` | Socket unreachable (daemon not started). |
| `Beachcomber::ServerError` | Daemon returns `ok: false`. |
| `Beachcomber::ProtocolError` | Response is not valid JSON or unexpected format. |

All inherit from `Beachcomber::Error < StandardError`.

## Socket discovery

1. `$BEACHCOMBER_SOCKET` — if set and non-empty
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path. No session-scoped environment is consulted (`$TMPDIR`, `$XDG_RUNTIME_DIR`); non-standard setups point clients at the daemon via `$BEACHCOMBER_SOCKET`.

## Running tests

```sh
ruby -Ilib -Itest test/test_result.rb test/test_discovery.rb test/test_client.rb
# or
rake test
```

Tests use only Ruby's built-in `minitest` and `UNIXServer` — no external gems required.

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

# What `comb get git.branch` prints in that directory; nil on a miss
branch = client.get('git.branch', cwd: '/path/to/repo')   # "main"

# A full provider (a Hash)
git = client.get('git', cwd: '/path/to/repo')
puts git['branch']

# The caller's environment for env.* refs, and the force/wait flags
client.get('aws.profile', cwd: '/path/to/repo', env: ENV.to_h)
client.get('git.branch', cwd: '/path/to/repo', force: true, wait: true)

# The template itself, unrendered: a facet, read like any key
client.get('aws:template.profile')
```

## API

### `Beachcomber::Client`

Opens a fresh socket connection for each call. Simple and stateless.

| Method | Description |
|--------|-------------|
| `get(key, cwd: nil, env: nil, force: false, wait: false)` | The value `comb get` prints for this key, cwd and env; `nil` on a miss. |
| `refresh(key, path: nil)` | Force recomputation. Returns `nil`. |
| `hello` | Handshake. Returns a `HelloInfo`. |
| `put(key, data, ttl: nil, path: nil)` | Write a value into the cache; `nil` removes the node. Returns `nil`. |
| `introspect(subject, duration_secs: nil)` | Inspect a daemon subsystem. Returns an `IntrospectResponse`. |
| `watch(key, cwd:, env: nil)` | Subscribe to a key's value — what `get` answers for it. Returns a `WatchStream` (Enumerable). |
| `status` | Daemon cache status as typed rows. Returns `Array<CacheRow>`. |
| `session { \|s\| }` | Open a persistent connection (see below). |
| `eval_expression(template_str, cwd:, env: nil)` | Render a template stored nowhere — the same value `comb eval` prints. |

`get` returns the bare value. The daemon holds the templates
(`<provider>:template`, seeded from configuration or `put`) and the client
renders them: a plain field is its cached value, a templated one is
rendered, and a bare provider name is the whole merged namespace.
`cwd: nil` is the global entry. The unrendered halves are facets, read like
any key: `p:template.x` is the template as stored, `p:put.x` and
`p:source.x` those layers' values.

`eval_expression` and `watch` require `cwd`. `template_str` (and each
stored template) accepts a single `{{ expr }}` tag, or literal
text/several tags — the former keeps the expression's natural type, the
latter is always a string (a source with no tags at all is its literal
text).

`exec()` inside a template runs only on a client created with
`Beachcomber::Client.new(exec: true)`; it is off by default, and a
template that calls it then fails with `exec() is disabled in this
context`.

### `Beachcomber::Session`

Persistent connection. Use when making multiple queries per invocation to avoid per-call socket overhead.

```ruby
client.session do |s|
  s.set_context('/path/to/repo')   # this session's directory
  branch = s.get('git.branch')     # reads there
  dirty = s.get('git.dirty')
  s.refresh('git')
end
# connection closed automatically
```

| Method | Description |
|--------|-------------|
| `set_context(path)` | Set this session's directory: a `get` with no `cwd`, and a `put` or `refresh` with no `path`, address it. |
| `get(key, cwd: nil, env: nil, force: false, wait: false)` | `Client#get` on this connection; `cwd: nil` is the session's directory, else the global entry. |
| `refresh(key, path: nil)` | Force recomputation. |
| `hello` | Handshake. Returns a `HelloInfo`. |
| `put(key, data, ttl: nil, path: nil)` | Write a value into the cache; `nil` removes the node. |
| `introspect(subject, duration_secs: nil)` | Inspect a daemon subsystem. Returns an `IntrospectResponse`. |
| `status` | Daemon cache status as typed rows. Returns `Array<CacheRow>`. |
| `close` | Close the connection (called automatically by `Client#session`). |

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
rake test
COMB_BIN=/path/to/comb ruby conformance_runner.rb   # the shared fixtures under tests/conformance/
```

Tests use only Ruby's built-in `minitest` and `UNIXServer` — no external gems required.

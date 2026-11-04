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
# client = Beachcomber::Client.new(timeout_ms: 500)

# What `comb get git.branch` prints in that directory; nil on a miss
branch = client.get('git.branch', cwd: '/path/to/repo')   # "main"

# A full provider (a Hash)
git = client.get('git', cwd: '/path/to/repo')
puts git['branch']

# The caller's environment for env.* refs; force a run, and wait for it
client.get('aws.profile', cwd: '/path/to/repo', env: ENV.to_h)
client.get('git.branch', cwd: '/path/to/repo', force: true, wait: 'forever')
# Answer at once with what is stored, however stale
client.get('git.branch', cwd: '/path/to/repo', wait: '0')

# The template itself, unrendered: a facet, read like any key
client.get('aws:template.profile')
```

## Client options

```ruby
client = Beachcomber::Client.new(socket_path: '/custom/path', timeout_ms: 500, connect_timeout_ms: 0)
```

| Option | Default | Meaning |
|---|---|---|
| `socket_path:` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeout_ms:` | 250 | How long one read or write on the socket may take; at least 1. |
| `connect_timeout_ms:` | 1500 | How long connecting keeps retrying before it gives up; 0 is one attempt. |
| `autostart:` | on | Start a daemon when none answers an auto-discovered socket. |
| `exec:` | off | Whether a template this client renders may call `exec()`. |

The defaults are the library's (`bc_client_new`): this SDK passes on the
options you give and adds none of its own.

## API

This SDK follows the SDK API table, [`docs/sdk-api.md`](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md):
the same methods, arguments, value shapes and error kinds as every other
SDK, checked against `sdks/api.json` by `test/test_api_surface.rb`.

### `Beachcomber::Client`

Opens a fresh socket connection for each call. Simple and stateless.

| Method | Description |
|--------|-------------|
| `get(key, cwd: nil, env: nil, force: false, wait: nil)` | The value `comb get` prints for this key, cwd and env; `nil` on a miss. `force: true` re-executes every Source the read demands, as `comb get --force`; `wait:` is how long to wait for a run the read needs before answering what is stored, as `comb get --wait`: a duration (`'0'`, `'50ms'`, `'2s'`) or `'forever'` (anything else is a `ParseError`). Left out, a stored record waits its Source's budget and a cold read waits for its run; the read may take `timeout_ms` plus the wait. |
| `eval(template, cwd: nil, env: nil)` | Render a template stored nowhere — the same value `comb eval` prints. |
| `watch(key, cwd: nil, env: nil)` | Subscribe to a key's value — what `get` answers for it — and receive it again whenever it changes. Returns a `WatchStream` (Enumerable). |
| `put(key, data, ttl: nil, path: nil)` | Write a value into the cache; `nil` removes the node. Returns `nil`. |
| `refresh(key, path: nil)` | Force recomputation. Returns `nil`. |
| `status` | Daemon cache status as typed rows. Returns `Array<CacheRow>`. |
| `hello` | Handshake. Returns a `HelloInfo`. |
| `introspect(subject, duration_secs: nil)` | Inspect a daemon subsystem. Returns an `IntrospectResponse`. |
| `session { \|s\| }` | Open a persistent connection (see below). |
| `close` | Free the client's handle. Idempotent; an operation on a closed client raises `IOError`. |

`get` returns the bare value. The daemon holds the templates
(`<provider>:template`, seeded from configuration or `put`) and the client
renders them: a plain field is its cached value, a templated one is
rendered, and a bare provider name is the whole merged namespace.
`cwd: nil` is the global entry, for `get`, `eval` and `watch` alike. The
unrendered halves are facets, read like any key: `p:template.x` is the
template as stored, `p:put.x` and `p:source.x` those layers' values.

`template` (and each stored template) accepts a single `{{ expr }}` tag, or
literal text/several tags — the former keeps the expression's natural type,
the latter is always a string (a source with no tags at all is its literal
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
| `get(key, cwd: nil, env: nil, force: false, wait: nil)` | `Client#get` on this connection; `cwd: nil` is the session's directory, else the global entry. |
| `put(key, data, ttl: nil, path: nil)` | `Client#put` on this connection; `path: nil` is the session's directory. |
| `refresh(key, path: nil)` | `Client#refresh` on this connection; `path: nil` is the session's directory. |
| `close` | Close the connection (called automatically by `Client#session`). |

`status`, `hello` and `introspect` address no directory and need no
connection of their own, so they are the client's alone.

### `Beachcomber::WatchStream`

| Method | Description |
|--------|-------------|
| `next_event(timeout_ms = nil)` | The next `WatchEvent`, waiting indefinitely when `timeout_ms` is nil (0 polls once). `nil` once the stream has ended; a wait that runs out raises `Beachcomber::TimeoutError`, and the stream stays open. |
| `cancel` | End the stream; safe from another thread while one waits in `next_event`. |
| `close` | Cancel and free the stream. Idempotent. |
| `each { \|event\| }` | Yield each event until the stream ends. |

An event's `data` is the value `get` answers at that moment; a `nil` `data`
is a miss, not the end of the stream.

### Typed shapes

`CacheRow`, `HelloInfo`, `DaemonHealth` (with `ReaperStatus` and `Verdict`),
`IntrospectResponse` and `WatchEvent` are Structs with exactly the fields
`libbeachcomber::protocol` defines; a field the library leaves out is `nil`.
A row's `kind` and `failure`, and an introspect subject other than `daemon`,
are the JSON the library hands on.

### Key format

- `"git"` — full provider, returns a Hash of all fields
- `"git.branch"` — single field, returns a scalar

### Errors

Every error is a `Beachcomber::CombError < StandardError`, one subclass per
kind, with `kind` (the slug), `message` (the library's message, unchanged)
and `library_version` (the loaded library's version, `nil` when none was
loaded).

| Class | `kind` | When raised |
|-------|--------|-------------|
| `DaemonNotRunningError` | `daemon_not_running` | No daemon answers, and none was started. |
| `ConnectionFailedError` | `connection_failed` | An explicit `socket_path` could not be connected to. |
| `IoError` | `io_error` | A read or write on the socket failed. |
| `ParseError` | `parse_error` | A request, a response or a template could not be read. |
| `ServerError` | `server_error` | The daemon refused the request, or a template failed to render. |
| `TimeoutError` | `timeout` | A read or write ran out `timeout_ms`, or `next_event` ran out its `timeout_ms`. |
| `BusyError` | `busy` | A session or watch is in use by another caller. |
| `PanicError` | `panic` | The library panicked; the panic was caught at the boundary. |
| `VersionSkewError` | `version_skew` | The daemon is a different version from the library. |
| `LibraryNotFoundError` | `library_not_found` | No `libbeachcomber` was found where discovery looks. |
| `LibrarySymbolError` | `library_symbol` | The library found lacks a function this SDK calls. |
| `UnsupportedError` | `unsupported` | An operation this SDK cannot do. |

## Library discovery

The library is found by the one rule every SDK follows
([`docs/sdk-api.md`, "Library discovery"](../../docs/sdk-api.md#library-discovery)). The file is
`libbeachcomber.dylib` on macOS and `libbeachcomber.so` on every other
platform, and the SDK tries, in order:

1. `$BEACHCOMBER_LIB`, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library.
2. Beside `comb`: `<dir>/../lib/<file>`, where `<dir>` is the directory of
   the first executable `comb` on `$PATH`, its symlinks resolved. A Homebrew
   `comb` is a symlink into the Cellar, and the library sits in the Cellar
   beside it.
3. The system loader, given the bare file name.

When none loads, `LibraryNotFoundError` names every candidate and why each
failed; there is no fallback to spawning `comb` as a subprocess. The tests
check this against the shared fixture, `sdks/discovery.json`.

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

---
sidebar_position: 5
---

# Go SDK

Go client for the beachcomber shell-state daemon. A binding over the shared `libbeachcomber` C ABI, using [`purego`](https://github.com/ebitengine/purego) (vendored) for pure-Go dynamic library loading — no cgo, no wire-protocol code in this SDK. Idiomatic Go with standard error returns. It follows the [SDK API table](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md) every beachcomber SDK follows: the same operations, arguments, value shapes and error kinds, in Go's casing.

## Requirements

- Go 1.21+
- A running `comb` daemon

## Installation

```sh
go get github.com/NavistAu/beachcomber/sdks/go
```

## Quick start

```go
package main

import (
    "fmt"
    "log"

    beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

func main() {
    c, err := beachcomber.NewClient()
    if err != nil {
        log.Fatal(err)
    }
    defer c.Close()

    branch, err := c.Get("git.branch", "/path/to/repo", nil) // what `comb get git.branch` prints there; nil on a miss
    if err != nil {
        log.Fatal(err)
    }
    if s, ok := branch.(string); ok {
        fmt.Printf("branch: %s\n", s)
    }
}
```

## Sessions

Open a persistent connection for multiple queries to avoid per-call socket overhead:

```go
sess, err := c.Session()
if err != nil {
    log.Fatal(err)
}
defer sess.Close()

sess.SetContext("/path/to/repo")

branch, err := sess.Get("git.branch", "", nil)
dirty, err  := sess.Get("git.dirty", "", nil)
```

## API reference

### `Client`

```go
c, err := beachcomber.NewClient()                                       // auto-discover socket
c, err := beachcomber.NewClient(beachcomber.WithSocketPath("/custom/path/sock")) // explicit socket path
c, err := beachcomber.NewClient(beachcomber.WithExec(true))             // templates this client renders may call exec()
c, err := beachcomber.NewClient(beachcomber.WithTimeoutMs(500), beachcomber.WithConnectTimeoutMs(0))
```

Each option is passed to the library's `bc_client_new` as it is given; the SDK has no defaults of its own, so an option left out is the library's default. `NewClient` locates and validates `libbeachcomber`, so a broken install is reported there (`ErrLibraryNotFound`, `ErrLibrarySymbol`). `c.Close()` frees the client; a call after it fails with `ErrIo`.

| Option | Library default | Meaning |
|---|---|---|
| `WithSocketPath(path string)` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `WithTimeoutMs(ms uint64)` | 250 | How long one read or write on the socket may take. 0 is refused (`parse_error` on the first call). |
| `WithConnectTimeoutMs(ms uint64)` | 1500 | How long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. |
| `WithAutostart(enabled bool)` | on | Start a daemon when none serves the auto-discovered socket. |
| `WithExec(enabled bool)` | off | Whether a template this client renders may call `exec()`. |

#### `c.Get(key, cwd string, env map[string]string, options ...GetOption) (any, error)`

The value `comb get` prints for this key, cwd and env, as `encoding/json` decodes it (a `string`, `float64`, `bool`, `map[string]interface{}` or `[]interface{}`) — the daemon holds the templates, the client renders them; a plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. A miss is `nil`. `cwd` is the directory the read addresses (`~` is home, a relative path is against this process's working directory); `""` is the global entry. `env` may be `nil`, which renders with the calling process's environment, as `comb` does. `options` are `beachcomber.Force` (re-execute every Source the read demands first, as `comb get --force`) and `beachcomber.Wait(d)` (how long to wait for a run the read needs before answering what is stored, as `comb get --wait`: a duration (`"0"`, `"50ms"`, `"2s"`) or `"forever"`; anything else is `ErrParse`). The unrendered halves are facets, read like any key: `p:template.x` is the template as stored, `p:put.x` and `p:source.x` those layers' values. `exec()` inside a template runs only on a client constructed with `beachcomber.WithExec(true)`; it is off by default, and a template that calls it then fails with `exec() is disabled in this context`.

```go
v, err := c.Get("git.branch", "/path/to/repo", nil)
v, err := c.Get("hostname", "", nil)                 // the global entry
v, err := c.Get("git", "/path/to/repo", nil)         // full provider object: a map[string]interface{}
v, err := c.Get("git.branch", "/path/to/repo", nil, beachcomber.Force)
v, err := c.Get("git:template.branch", "", nil)      // the template, unrendered
```

#### `c.Refresh(key, path) error`

Force the daemon to recompute a provider.

#### `c.Status() ([]CacheRow, error)`

Return cache rows as typed structs (one per warm cache entry).

#### `c.Hello() (*HelloInfo, error)`

Handshake — returns daemon version, protocol version, and build info.

#### `c.Put(key string, data interface{}, ttl, path string, persist bool) error`

Write a value into the cache as static data. `persist`: keep the put across a daemon restart, best effort.

#### `c.Introspect(subject IntrospectSubject, durationSecs ...uint64) (*IntrospectResponse, error)`

Inspect a daemon subsystem (`"daemon"`, `"providers"`, `"config"`, `"cache"`, `"lifecycle"`, `"watches"`, `"timers"`, `"demand"`, `"procs"`). A duration, which `"procs"` reads, is sent as given, 0 included; left out, the daemon's own default applies.

#### `c.Watch(key, cwd string, env map[string]string) (*WatchStream, error)`

Subscribe to a key's value — what `Get` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `cwd` and `env` mean what they mean for `Get` (`""` is the global entry). Returns a `WatchStream`: `NextEvent()` waits indefinitely and `NextEvent(ms)` at most `ms` milliseconds, returning an `ErrTimeout` error when none arrives; it answers `nil, nil` once the stream has ended. `Cancel()` ends the stream (a `NextEvent` waiting on another goroutine returns `nil, nil`), and `Close()` cancels and frees it. `WatchEvent.Data` is `nil` for a miss, and carries no age or staleness — a value resolved from several cache records has no single one.

#### `c.Eval(templateStr, cwd string, env map[string]string) (any, error)`

`Eval` renders a template stored nowhere — the same value `comb eval` prints, decoded as `Get`'s is, `nil` when it renders nothing — so `Eval("{{ p.f }}", …)` and `Get("p.f", …)` agree. `cwd` and `env` mean what they mean for `Get`. `templateStr` accepts a single `{{ expr }}` tag (keeps the expression's natural type) or literal text/several tags (always a string — a source with no tags at all is its literal text). `exec()` inside a template runs only on a client constructed with `beachcomber.WithExec(true)`; it is off by default, and a template that calls it then fails with `exec() is disabled in this context`.

#### `c.Session() (*Session, error)`

Open a persistent connection.

### `Session`

| Method | Description |
|---|---|
| `Get(key, cwd string, env map[string]string, options ...GetOption) (any, error)` | `c.Get` on this session's connection; `cwd == ""` is the session's directory when `SetContext` has set one, else the global entry |
| `Refresh(key, path string) error` | Force recomputation |
| `SetContext(path string) error` | Set this session's directory: every later `Get` with `cwd == ""`, and `Put` and `Refresh` with `path == ""`, address it |
| `Put(key string, data interface{}, ttl, path string) error` | Write a static-data entry |
| `Close() error` | Close the connection |

`Status`, `Hello` and `Introspect` address no directory and need no connection of their own, so they are the `Client`'s alone.

### Values

`Get` and `Eval` answer the value itself, never a wrapper:

```go
v, _ := c.Get("git", "/repo", nil)
git, _ := v.(map[string]interface{})
branch, _ := git["branch"].(string) // "main"
dirty, _ := git["dirty"].(bool)     // false
```

`CacheRow`, `HelloInfo`, `DaemonHealth` (with `ReaperStatus` and `Verdict`), `IntrospectResponse` and `WatchEvent` are typed.

## Errors

Every error is an `*Error`: `Kind` (the table's slug), `Message` (the library's words unchanged) and `LibraryVersion`; `err.Error()` is the message alone. One sentinel per kind matches it with `errors.Is`:

| Sentinel | Kind |
|---|---|
| `ErrDaemonNotRunning` | `daemon_not_running` |
| `ErrConnectionFailed` | `connection_failed` |
| `ErrIo` | `io_error` |
| `ErrParse` | `parse_error` |
| `ErrServer` | `server_error` |
| `ErrTimeout` | `timeout` |
| `ErrBusy` | `busy` |
| `ErrPanic` | `panic` |
| `ErrLibraryNotFound` | `library_not_found` |
| `ErrLibrarySymbol` | `library_symbol` |
| `ErrUnsupported` | `unsupported` |

```go
if errors.Is(err, beachcomber.ErrConnectionFailed) { … } // an explicit socket nobody serves
```

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

If none loads, `NewClient` returns `ErrLibraryNotFound`, naming every location tried and why each failed.

## Socket discovery

The SDK discovers the daemon socket at:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

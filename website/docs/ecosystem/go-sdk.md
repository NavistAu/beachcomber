---
sidebar_position: 5
---

# Go SDK

Go client for the beachcomber shell-state daemon. A binding over the shared `libbeachcomber` C ABI, using [`purego`](https://github.com/ebitengine/purego) (vendored) for pure-Go dynamic library loading — no cgo, no wire-protocol code in this SDK. Idiomatic Go with standard error returns.

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

    result, err := c.Get("git.branch", "/path/to/repo")
    if err != nil {
        log.Fatal(err)
    }
    if result.IsHit() {
        branch, _ := result.GetString("")
        fmt.Printf("branch: %s (age %dms)\n", branch, result.AgeMs)
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

branchResult, err := sess.Get("git.branch", "")
dirtyResult, err  := sess.Get("git.dirty", "")
```

## API reference

### `Client`

```go
c, err := beachcomber.NewClient()                       // auto-discover socket
c := beachcomber.NewClientWithPath("/custom/path/sock") // explicit socket path
c, err := beachcomber.NewClient(beachcomber.WithExec(true)) // templates this client resolves may call exec()
```

#### `c.Get(key, path) (*Result, error)`

Read a cached value. `path` may be `""` to omit it.

```go
result, err := c.Get("git.branch", "/path/to/repo")
result, err := c.Get("hostname", "")       // global provider
result, err := c.Get("git", "/path/to/repo") // full provider object
```

#### `c.Refresh(key, path) error`

Force the daemon to recompute a provider.

#### `c.Status() ([]CacheRow, error)`

Return cache rows as typed structs (one per warm cache entry).

#### `c.Hello() (*HelloInfo, error)`

Handshake — returns daemon version, protocol version, and build info.

#### `c.Put(key string, data interface{}, ttl, path string) error`

Write a value into the cache as static data.

#### `c.Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error)`

Inspect a daemon subsystem (`"daemon"`, `"providers"`, `"config"`, `"cache"`, `"lifecycle"`, `"watches"`, `"timers"`, `"demand"`, `"procs"`).

#### `c.Watch(key, cwd string, env map[string]string) (*WatchStream, error)`

Subscribe to a key's *resolved* value — what `Resolve` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `cwd` and `env` mean what they mean for `Resolve`, and `cwd` is required. Returns a `WatchStream`; call `NextEvent()` in a loop and `Close()` when done. `WatchEvent.Data` carries no age or staleness — a value resolved from several cache records has no single one.

#### `c.Resolve(key, cwd string, env map[string]string) (*Result, error)` / `c.Eval(templateStr, cwd string, env map[string]string) (*Result, error)`

`Resolve` returns the same value `comb get` returns for this key, cwd and env — the daemon holds the templates, the client renders them; a plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. `Eval` renders a template stored nowhere — the same value `comb eval` prints. Both require `cwd` (every fetch the resolution makes addresses it); `env` may be `nil`, which makes every `env.*` reference `""`. `templateStr` accepts a single `{{ expr }}` tag (keeps the expression's natural type) or literal text/several tags (always a string — a source with no tags at all is its literal text). `exec()` inside a template runs only on a client constructed with `beachcomber.WithExec(true)`; it is off by default, and a template that calls it then fails with `exec() is disabled in this context`.

#### `c.Session() (*Session, error)`

Open a persistent connection.

### `Session`

| Method | Description |
|---|---|
| `Get(key, path string) (*Result, error)` | Read a node raw — both halves, nothing rendered (see `Result`) |
| `Refresh(key, path string) error` | Force recomputation |
| `SetContext(path string) error` | Set default path for subsequent queries |
| `GetWithFlags(key, path string, force, wait bool) (*Result, error)` | Read with force/wait flags |
| `Hello() (*HelloInfo, error)` | Handshake — returns daemon and protocol versions |
| `Put(key string, data interface{}, ttl, path string) error` | Write a static-data entry |
| `Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error)` | Inspect a daemon subsystem |
| `Status() ([]CacheRow, error)` | Return typed cache rows |
| `Close() error` | Close the connection |

### `Result`

`Get` returns the node as the daemon holds it — both halves, nothing rendered. A node may hold a cached value (`Data`), a template (`Template`), or both. A template-only `Result` is neither a hit nor a miss: it is an address whose value `Resolve` renders — `Get` is the raw half, `Resolve(key, cwd, env)` is how you get the value.

| Field / method | Type | Description |
|---|---|---|
| `OK` | `bool` | `true` when the daemon returned success |
| `Data` | `interface{}` | Decoded payload (string, map, etc.); `nil` when nothing is cached |
| `AgeMs` | `uint64` | Cache age in milliseconds |
| `Stale` | `bool` | Whether the value is stale |
| `Template` | `interface{}` | The template in effect at the address, unrendered: a string for a leaf, a map for a group; `nil` when none is in effect |
| `Rest` | `string` | The address left over when the template walk stopped at a string — `Get("p.t.x", "")` where `p.t` is a template string gives the template of `p.t` and `Rest == "x"`; `""` otherwise |
| `IsHit() bool` | — | `true` when `OK && Data != nil` |
| `IsMiss() bool` | — | `true` when `OK && Data == nil && Template == nil` |
| `GetString(field string) (string, bool)` | — | Extract a string (pass `""` for scalar results) |
| `GetInt(field string) (int64, bool)` | — | Extract an integer |
| `GetFloat(field string) (float64, bool)` | — | Extract a float |
| `GetBool(field string) (bool, bool)` | — | Extract a boolean |
| `RawJSON() []byte` | — | Full raw JSON response |

For full provider queries, pass a field name to the typed accessors:

```go
result, _ := c.Get("git", "/repo")
branch, _ := result.GetString("branch")  // "main"
dirty, _  := result.GetBool("dirty")     // false
```

## Errors

| Error | When returned |
|---|---|
| `ErrDaemonNotRunning` | Unix socket cannot be reached |
| `*ServerError` | Daemon responded with `ok: false` |
| `*ProtocolError` | Response could not be parsed |

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Socket discovery

The SDK discovers the daemon socket at:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

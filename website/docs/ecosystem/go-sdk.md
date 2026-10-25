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

    result, err := c.Get("git.branch", "/path/to/repo", nil) // what `comb get git.branch` prints there
    if err != nil {
        log.Fatal(err)
    }
    if result.IsHit() {
        branch, _ := result.GetString("")
        fmt.Printf("branch: %s\n", branch)
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

branchResult, err := sess.Get("git.branch", "", nil)
dirtyResult, err  := sess.Get("git.dirty", "", nil)
```

## API reference

### `Client`

```go
c, err := beachcomber.NewClient()                       // auto-discover socket
c := beachcomber.NewClientWithPath("/custom/path/sock") // explicit socket path
c, err := beachcomber.NewClient(beachcomber.WithExec(true)) // templates this client renders may call exec()
```

#### `c.Get(key, cwd string, env map[string]string, flags ...GetFlag) (*Result, error)`

The value `comb get` prints for this key, cwd and env — the daemon holds the templates, the client renders them; a plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace. A miss is a `Result` with `nil` `Data`. `cwd` is the directory the read addresses (`~` is home, a relative path is against this process's working directory); `""` is the global entry. `env` may be `nil`, which makes every `env.*` reference absent. `flags` are `beachcomber.Force` (re-execute every Source the read demands first, as `comb get --force`) and `beachcomber.Wait` (re-execute a stale entry inline, as `comb get --wait`). The unrendered halves are facets, read like any key: `p:template.x` is the template as stored, `p:put.x` and `p:source.x` those layers' values. `exec()` inside a template runs only on a client constructed with `beachcomber.WithExec(true)`; it is off by default, and a template that calls it then fails with `exec() is disabled in this context`.

```go
result, err := c.Get("git.branch", "/path/to/repo", nil)
result, err := c.Get("hostname", "", nil)                 // the global entry
result, err := c.Get("git", "/path/to/repo", nil)         // full provider object
result, err := c.Get("git.branch", "/path/to/repo", nil, beachcomber.Force)
result, err := c.Get("git:template.branch", "", nil)      // the template, unrendered
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

Subscribe to a key's *resolved* value — what `Get` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `cwd` and `env` mean what they mean for `Get`, and `cwd` is required. Returns a `WatchStream`; call `NextEvent()` in a loop and `Close()` when done. `WatchEvent.Data` carries no age or staleness — a value resolved from several cache records has no single one.

#### `c.Eval(templateStr, cwd string, env map[string]string) (*Result, error)`

`Eval` renders a template stored nowhere — the same value `comb eval` prints, so `Eval("{{ p.f }}", …)` and `Get("p.f", …)` agree. It requires `cwd` (every fetch the render makes addresses it); `env` means what it means for `Get`. `templateStr` accepts a single `{{ expr }}` tag (keeps the expression's natural type) or literal text/several tags (always a string — a source with no tags at all is its literal text). `exec()` inside a template runs only on a client constructed with `beachcomber.WithExec(true)`; it is off by default, and a template that calls it then fails with `exec() is disabled in this context`.

#### `c.Session() (*Session, error)`

Open a persistent connection.

### `Session`

| Method | Description |
|---|---|
| `Get(key, cwd string, env map[string]string, flags ...GetFlag) (*Result, error)` | `c.Get` on this session's connection; `cwd == ""` is the session's directory when `SetContext` has set one, else the global entry |
| `Refresh(key, path string) error` | Force recomputation |
| `SetContext(path string) error` | Set this session's directory: every later `Get` with `cwd == ""`, and `Put` and `Refresh` with `path == ""`, address it |
| `Hello() (*HelloInfo, error)` | Handshake — returns daemon and protocol versions |
| `Put(key string, data interface{}, ttl, path string) error` | Write a static-data entry |
| `Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error)` | Inspect a daemon subsystem |
| `Status() ([]CacheRow, error)` | Return typed cache rows |
| `Close() error` | Close the connection |

### `Result`

`Get` and `Eval` return the value in a `Result`: `Data` is the value, or `nil` on a miss.

| Field / method | Type | Description |
|---|---|---|
| `OK` | `bool` | `true` when the daemon returned success |
| `Data` | `interface{}` | The decoded value (string, map, etc.); `nil` on a miss |
| `IsHit() bool` | — | `true` when `OK && Data != nil` |
| `IsMiss() bool` | — | `true` when `OK && Data == nil` |
| `GetString(field string) (string, bool)` | — | Extract a string (pass `""` for scalar results) |
| `GetInt(field string) (int64, bool)` | — | Extract an integer |
| `GetFloat(field string) (float64, bool)` | — | Extract a float |
| `GetBool(field string) (bool, bool)` | — | Extract a boolean |
| `RawJSON() []byte` | — | The JSON value backing this `Result` |

For full provider queries, pass a field name to the typed accessors:

```go
result, _ := c.Get("git", "/repo", nil)
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

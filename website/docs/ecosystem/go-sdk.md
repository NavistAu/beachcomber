---
sidebar_position: 5
---

# Go SDK

Go client for the beachcomber shell-state daemon. Communicates over a Unix domain socket using newline-delimited JSON. Idiomatic Go with standard error returns and no external dependencies.

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

#### `c.List() (*Result, error)`

List available providers.

#### `c.Status() (*Result, error)`

Return daemon scheduler and cache status.

#### `c.Session() (*Session, error)`

Open a persistent connection.

### `Session`

| Method | Description |
|---|---|
| `Get(key, path string) (*Result, error)` | Read a cached value |
| `Refresh(key, path string) error` | Force recomputation |
| `SetContext(path string) error` | Set default path for subsequent queries |
| `Close() error` | Close the connection |

### `Result`

| Field / method | Type | Description |
|---|---|---|
| `OK` | `bool` | `true` when the daemon returned success |
| `Data` | `interface{}` | Decoded payload (string, map, etc.) |
| `AgeMs` | `uint64` | Cache age in milliseconds |
| `Stale` | `bool` | Whether the value is stale |
| `IsHit() bool` | — | `true` when `OK && Data != nil` |
| `IsMiss() bool` | — | `true` when `OK && Data == nil` |
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

## Unsupported operations

The `put` and `watch` protocol operations are not currently exposed in this SDK. Use the CLI (`comb p` for put, `comb w` for watch) or speak the [raw protocol](/docs/reference/protocol-reference) directly over a Unix socket.

## Socket discovery

The SDK discovers the daemon socket at:

1. `$XDG_RUNTIME_DIR/beachcomber/sock` (if `XDG_RUNTIME_DIR` is set and the path exists)
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `/tmp/beachcomber-<uid>/sock`

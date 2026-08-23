# beachcomber Go SDK

Go client for the [beachcomber](https://github.com/NavistAu/beachcomber) (`comb`) shell-state daemon.

A binding over the beachcomber C ABI (`libbeachcomber.{so,dylib}`), not a
hand-rolled wire-protocol client: this SDK holds no socket code and no
NDJSON framing of its own. It loads `libbeachcomber` via
[`purego`](https://github.com/ebitengine/purego) — a pure-Go `dlopen`/`dlsym`
binding, not cgo — and the native library owns the daemon connection,
including socket auto-discovery and the wire protocol. `purego` is this
SDK's only non-stdlib dependency; every other beachcomber SDK is
stdlib-only, but the entire ABI surface here fits `purego`'s supported
shape (pointer/integer arguments, pointer or small-integer returns, no
callbacks into Go), so a cgo binding was never needed.

## Requirements

- Go 1.21+
- A `comb` daemon — one is autostarted on demand when none is running
  (the shared library's default; disable with
  `NewClient(beachcomber.WithAutostart(false))`). Autostart applies only
  to `NewClient`'s auto-discovered socket path: the library never
  autostarts for an explicit path, so `NewClientWithPath` fails its
  first call with `ErrDaemonNotRunning` when nothing serves that socket
- `libbeachcomber.{so,dylib}` discoverable — see "Library discovery" below

## Installation

```sh
go get github.com/NavistAu/beachcomber/sdks/go
```

## Quick start

```go
package main

import (
	"errors"
	"fmt"
	"log"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

func main() {
	client, err := beachcomber.NewClient()
	if err != nil {
		log.Fatal(err)
	}

	result, err := client.Get("git.branch", "/path/to/repo")
	if err != nil {
		if errors.Is(err, beachcomber.ErrDaemonNotRunning) {
			log.Fatal("comb daemon is not running")
		}
		log.Fatal(err)
	}
	if result.IsHit() {
		branch, _ := result.GetString("")
		fmt.Println(branch)      // "main"
		fmt.Println(result.AgeMs) // 234
		fmt.Println(result.Stale) // false
	}

	// Full provider query (returns an object)
	result, err = client.Get("git", "/path/to/repo")
	if err != nil {
		log.Fatal(err)
	}
	if branch, ok := result.GetString("branch"); ok {
		fmt.Println(branch)
	}

	// Force recomputation
	if err := client.Refresh("git", "/path/to/repo"); err != nil {
		log.Fatal(err)
	}
}
```

For multiple queries per invocation, open a persistent [`Session`](#session)
instead of issuing one-shot `Client` calls:

```go
sess, err := client.Session()
if err != nil {
	log.Fatal(err)
}
defer sess.Close()

sess.SetContext("/path/to/repo")
branch, err := sess.Get("git.branch", "")
dirty, err := sess.Get("git.dirty", "")
```

## Custom socket path

```go
client := beachcomber.NewClientWithPath("/tmp/beachcomber-1000/sock")
```

Unlike `NewClient`, `NewClientWithPath` never fails from the caller's point
of view — a library discovery/validation failure is recorded and surfaced
on the client's first operation instead.

## API

### `Client`

Each call opens a fresh daemon connection (`libbeachcomber`'s own
behavior — this SDK does not hold a socket open on the `Client`'s behalf).
For latency-sensitive use, prefer `Session`.

| Method | Description |
|---|---|
| `NewClient() (*Client, error)` | Auto-discovers the daemon socket path. May fail immediately: library discovery/validation happens here. |
| `NewClientWithPath(socketPath string) *Client` | Uses an explicit socket path. Never fails immediately; errors surface on first use. |
| `Get(key, path string) (*Result, error)` | Read a cached value. |
| `GetWithFlags(key, path string, force, wait bool) (*Result, error)` | Read with the ABI's `force`/`wait` flags. |
| `Refresh(key, path string) error` | Force recomputation. |
| `Put(key string, data interface{}, ttl, path string) error` | Write a value into a virtual provider. `data` is marshalled to JSON. |
| `Status() ([]CacheRow, error)` | Cache rows from the daemon. |
| `Hello() (*HelloInfo, error)` | Protocol/daemon version handshake. |
| `Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error)` | Inspect a daemon subsystem. |
| `Resolve(key, cwd string, env, overrides map[string]string) (*Result, error)` | Client-side field resolution — the same evaluator `comb get` uses, run in-process. |
| `Eval(templateStr, cwd string, env, overrides map[string]string) (*Result, error)` | Evaluate an arbitrary expression string, same evaluator as `Resolve`. |
| `Watch(key, path string) (*WatchStream, error)` | Subscribe to live updates. |
| `Session() (*Session, error)` | Open a persistent connection. |

`Resolve` and `Eval` require `cwd`: path expressions select a cache
coordinate over it, so the library never falls back to the process's own
working directory. `env` and `overrides` are optional (`nil` for "not
supplied") — a `nil` `env` makes every `env.*` reference resolve to `""`,
and `nil` `overrides` uses the built-in default expressions.

### `Session`

Obtained via `client.Session()`. Not safe for concurrent use from multiple
goroutines — the underlying library guards it with a mutex and returns a
`busy` `*ServerError` to a concurrent caller rather than blocking or
interleaving requests; create one `Session` per goroutine.

| Method | Description |
|---|---|
| `Get(key, path string) (*Result, error)` | Read on this session's connection. |
| `GetWithFlags(key, path string, force, wait bool) (*Result, error)` | Read with flags, on this session's connection. |
| `SetContext(path string) error` | Set the default path for subsequent queries on this connection. |
| `Put(key string, data interface{}, ttl, path string) error` | Write on this session's connection. |
| `Refresh(key, path string) error` | Delegates to a fresh connection via the parent `Client` — the ABI exposes only `get`/`put`/`set_context` on a session handle. |
| `Hello() (*HelloInfo, error)` | Delegates to the parent `Client`. |
| `Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error)` | Delegates to the parent `Client`. |
| `Status() ([]CacheRow, error)` | Delegates to the parent `Client`. |
| `Close() error` | Close and free the session handle. |

### `Result`

| Method | Description |
|---|---|
| `IsHit() bool` | Successful response carrying data. |
| `IsMiss() bool` | Successful response with no data. |
| `GetString(field string) (string, bool)` | Extract a string; `field == ""` reads `Data` itself when it's a scalar. |
| `GetInt(field string) (int64, bool)` | Extract an integer (truncated from the underlying `float64`). |
| `GetFloat(field string) (float64, bool)` | Extract a numeric field. |
| `GetBool(field string) (bool, bool)` | Extract a boolean. |
| `RawJSON() []byte` | The raw JSON payload backing this `Result`. |

Fields: `OK bool`, `Data interface{}`, `AgeMs uint64`, `Stale bool`.
Construction always succeeds — an `ok:false` envelope becomes a
`*ServerError` instead of a `Result`, so every `Result` you hold represents
success.

### `WatchStream`

```go
stream, err := client.Watch("git.branch", "/some/repo")
if err != nil {
	log.Fatal(err)
}
defer stream.Close()

for {
	ev, err := stream.NextEvent()
	if err != nil {
		log.Fatal(err)
	}
	if ev == nil {
		break // stream ended
	}
	fmt.Println(ev.Data, ev.AgeMs, ev.Stale)
}
```

### Key format

- `"git"` — full provider, `Result.Data` is a `map[string]interface{}`
- `"git.branch"` — single field, `Result.Data` is a scalar

## Errors

Every error this SDK returns is one of three types:

| Type | When returned |
|---|---|
| `*ServerError` | The library reported an `ok:false` envelope — either the daemon rejected the request, or the ABI itself did (bad flags, a busy handle, a caught panic, version skew). Carries `Kind` (the envelope's stable, machine-readable slug), `Message`, and `LibVersion`. |
| `*ProtocolError` | A response could not be parsed — malformed JSON or a response missing fields this SDK requires. Never comes from an `ok:false` envelope. |
| `*LibraryError` | Failure to locate, load, or validate `libbeachcomber` itself — discovery exhausted every candidate, or a loaded library is missing a required symbol. `Message` names every location tried, or the missing symbol. |

`errors.Is(err, beachcomber.ErrDaemonNotRunning)` reports `true` for a
`*ServerError` whose `Kind` is `daemon_not_running` (no override socket
path, none found by auto-discovery) or `connection_failed` (an explicit
socket path, e.g. via `NewClientWithPath`, that nothing is listening on) —
both were a single case in this SDK before the ABI made the distinction.

Check `ServerError.Kind`, not `err.Error()`'s text, for programmatic
handling — it mirrors the C ABI envelope's `error.kind` (see
`libbeachcomber-ffi/src/envelope.rs`).

## Library discovery

At first use, `libbeachcomber` is located, in order:

1. `$BEACHCOMBER_LIB` — an exact path to the shared library.
2. `../lib/<libname>` relative to the `comb` binary resolved on `$PATH`
   (symlinks resolved) — the library and binary ship together, so this
   comes before the system path deliberately.
3. The platform default dynamic-linker search path (a bare filename, left
   for `dlopen` to resolve).

If none resolve, `*LibraryError` names every location tried. Every required
`bc_*` symbol is checked at load time, not on first use; a loaded library
missing one also returns `*LibraryError`, naming the missing symbol(s) and
the library's own `bc_version()`. Discovery and validation happen once per
process (`sync.Once`) and are cached for the lifetime of the program.

## Socket discovery

Once the library is loaded, the daemon socket path is resolved by
`libbeachcomber` itself (not this SDK) for `NewClient`. This SDK also
exposes the same resolution standalone via `DiscoverSocketPath()`:

1. `$BEACHCOMBER_SOCKET` — if set and non-empty.
2. `/tmp/beachcomber-<uid>/sock`.

This mirrors the daemon's bind path: a single stable path per user, so
singleton enforcement is per-user rather than per-session. No
session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted.
Pass an explicit path to `NewClientWithPath` to override it directly.

## Development / testing

```sh
go test -timeout 30s ./...
# or
make test
```

Integration tests spawn a real `comb daemon` and drive it through the real
library; they look for `libbeachcomber.{so,dylib}` and `comb` at their
repo-relative debug build locations (`../../target/debug/`) unless
`BEACHCOMBER_LIB` / `COMB_BIN` are set, and skip (not fail) when those
artifacts aren't built — run `cargo build` first.

`cmd/conformance` is a separate runner that drives this SDK's public API
against the shared cross-binding JSON fixtures under `tests/conformance/`:

```sh
COMB_BIN=/path/to/comb go run ./cmd/conformance
```

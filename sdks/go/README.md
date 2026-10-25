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

It follows the SDK API table every beachcomber SDK follows,
[`docs/sdk-api.md`](../../docs/sdk-api.md): the same operations, arguments,
value shapes and error kinds, in Go's casing. Its own tests check its
public surface against the table's machine-readable form, `../api.json`.

## Requirements

- Go 1.21+
- A `comb` daemon — one is autostarted on demand when none is running
  (the shared library's default; disable with
  `NewClient(beachcomber.WithAutostart(false))`). Autostart applies only
  to an auto-discovered socket path: the library never autostarts for
  `WithSocketPath`, so a client given one fails its first call with
  `ErrConnectionFailed` when nothing serves that socket
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
	defer client.Close()

	// What `comb get git.branch` prints in that directory; nil on a miss.
	branch, err := client.Get("git.branch", "/path/to/repo", nil)
	if err != nil {
		if errors.Is(err, beachcomber.ErrDaemonNotRunning) {
			log.Fatal("comb daemon is not running")
		}
		log.Fatal(err)
	}
	if s, ok := branch.(string); ok {
		fmt.Println(s) // "main"
	}

	// Full provider query: a map[string]interface{}
	git, err := client.Get("git", "/path/to/repo", nil)
	if err != nil {
		log.Fatal(err)
	}
	if m, ok := git.(map[string]interface{}); ok {
		fmt.Println(m["branch"])
	}

	// The template itself, unrendered: a facet, read like any key
	tmpl, err := client.Get("git:template.branch", "", nil)

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
branch, err := sess.Get("git.branch", "", nil)
dirty, err := sess.Get("git.dirty", "", nil)
```

## Client options

Each option is passed to the library's `bc_client_new` as it is given; this
SDK has no defaults of its own, so an option left out is the library's
default.

| Option | Library default | Meaning |
|---|---|---|
| `WithSocketPath(path string)` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `WithTimeoutMs(ms uint64)` | 100 | How long one read or write on the socket may take. 0 is refused (`parse_error` on the first call). |
| `WithConnectTimeoutMs(ms uint64)` | 1500 | How long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. |
| `WithAutostart(enabled bool)` | on | Start a daemon when none serves the auto-discovered socket. |
| `WithExec(enabled bool)` | off | Whether a template this client renders may call `exec()`. |

```go
c, err := beachcomber.NewClient(
	beachcomber.WithSocketPath("/tmp/beachcomber-1000/sock"),
	beachcomber.WithTimeoutMs(500),
	beachcomber.WithConnectTimeoutMs(0),
)
```

`NewClient` locates and validates `libbeachcomber` (see "Library
discovery"), so a broken install is reported here, as `ErrLibraryNotFound`
or `ErrLibrarySymbol`; a malformed option is the library's to refuse, on the
client's first call.

## API

### `Client`

Each call opens a fresh daemon connection (`libbeachcomber`'s own
behavior — this SDK does not hold a socket open on the `Client`'s behalf).
For latency-sensitive use, prefer `Session`.

| Method | Description |
|---|---|
| `NewClient(options ...Option) (*Client, error)` | Make a client (see "Client options"). |
| `Get(key, cwd string, env map[string]string, flags ...GetFlag) (any, error)` | The value `comb get` prints for this key, cwd and env, `nil` on a miss — the daemon holds the templates, the client renders them; a bare provider name is the whole merged namespace. `flags` are `beachcomber.Force` and `beachcomber.Wait`. |
| `Refresh(key, path string) error` | Force recomputation. |
| `Put(key string, data interface{}, ttl, path string) error` | Write a value into the cache as static data. `data` is marshalled to JSON. |
| `Status() ([]CacheRow, error)` | Cache rows from the daemon. |
| `Hello() (*HelloInfo, error)` | Protocol/daemon version handshake. |
| `Introspect(subject IntrospectSubject, durationSecs ...uint64) (*IntrospectResponse, error)` | Inspect a daemon subsystem. A duration, which `SubjectProcs` reads, is sent as given, 0 included; left out, the daemon's default applies. |
| `Eval(templateStr, cwd string, env map[string]string) (any, error)` | Render a template stored nowhere — the same value `comb eval` prints, `nil` when it renders nothing. |
| `Watch(key, cwd string, env map[string]string) (*WatchStream, error)` | Subscribe to a key's value, what `Get` answers. |
| `Session() (*Session, error)` | Open a persistent connection. |
| `Close() error` | Free the client. Idempotent; a call after it fails with `ErrIo`. |

`Get`'s `cwd` is the directory the read addresses: `~` is the home
directory, a relative path is against this process's working directory,
and `""` is the global entry; `Eval` and `Watch` read it the same way. `env` is
optional (`nil` for "not supplied") — a `nil` `env` renders with the calling
process's environment, as `comb` does. The unrendered halves are facets, read like any key:
`Get("p:template.x", …)` is the template as stored, `Get("p:put.x", …)` and
`Get("p:source.x", …)` those layers' values.

`Force` re-executes every Source the read demands before answering, as
`comb get --force`; `Wait` re-executes a stale entry inline, as
`comb get --wait`.

`templateStr` accepts a single `{{ expr }}` tag, or literal text/several
tags — the former keeps the expression's natural type, the latter is
always a string (a source with no tags at all is its literal text).

`exec()` inside a template runs only on a client constructed with
`beachcomber.WithExec(true)`; it is off by default, and a template that
calls it then fails with `exec() is disabled in this context`.

### `Session`

Obtained via `client.Session()`. Not safe for concurrent use from multiple
goroutines — the underlying library guards it with a mutex and returns an
`*Error` of kind `busy` (`ErrBusy`) to a concurrent caller rather than
blocking or interleaving requests; create one `Session` per goroutine.
`Status`, `Hello` and `Introspect` address no directory and need no
connection of their own, so they are the `Client`'s alone.

| Method | Description |
|---|---|
| `Get(key, cwd string, env map[string]string, flags ...GetFlag) (any, error)` | `Client.Get` on this session's connection; `cwd == ""` is the session's directory when `SetContext` has set one, else the global entry. |
| `SetContext(path string) error` | Set this session's directory: every later `Get` with `cwd == ""`, and `Put` and `Refresh` with `path == ""`, address it. |
| `Put(key string, data interface{}, ttl, path string) error` | Write on this session's connection. |
| `Refresh(key, path string) error` | Recompute on this session's connection, at `path` or the session's directory. |
| `Close() error` | Close and free the session handle. Idempotent; a call after it fails with `ErrIo`. |

### Values

`Get` and `Eval` answer the value itself, as `encoding/json` decodes it
into an `interface{}`: a `string`, `float64`, `bool`,
`map[string]interface{}` or `[]interface{}`, and `nil` on a miss. There is
no wrapper. `CacheRow`, `HelloInfo`, `DaemonHealth` (with `ReaperStatus`
and `Verdict`), `IntrospectResponse` and `WatchEvent` are typed, with the
fields the table lists.

### `WatchStream`

```go
stream, err := client.Watch("git.branch", "/some/repo", nil)
if err != nil {
	log.Fatal(err)
}
defer stream.Close()

for {
	ev, err := stream.NextEvent() // NextEvent(ms) waits at most ms
	if err != nil {
		log.Fatal(err)
	}
	if ev == nil {
		break // stream ended
	}
	fmt.Println(ev.Data) // nil is a miss, not the end
}
```

| Method | Description |
|---|---|
| `NextEvent(timeoutMs ...int32) (*WatchEvent, error)` | The next event. No argument waits indefinitely; `NextEvent(ms)` that runs out with no event is an `ErrTimeout` error. `nil, nil` once the stream has ended. |
| `Cancel() error` | End the stream; a `NextEvent` waiting on another goroutine returns `nil, nil`. |
| `Close() error` | Cancel and free the watch handle. |

### Key format

- `"git"` — full provider, `Get` answers a `map[string]interface{}`
- `"git.branch"` — single field, `Get` answers a scalar

## Errors

Every error this SDK returns is an `*Error`: `Kind` is the stable slug the
table names, `Message` the library's words unchanged (or the SDK's own for
the three kinds it raises itself), and `LibraryVersion` the loaded
library's `bc_version()`. `err.Error()` is the message alone.

One sentinel per kind matches an `*Error` of that kind with `errors.Is`:

| Sentinel | Kind | When |
|---|---|---|
| `ErrDaemonNotRunning` | `daemon_not_running` | No daemon answers, and none was started. |
| `ErrConnectionFailed` | `connection_failed` | A `WithSocketPath` socket could not be connected to. |
| `ErrIo` | `io_error` | A read or write on the socket failed, or the client or session is closed. |
| `ErrParse` | `parse_error` | A request, a response or a template could not be read. |
| `ErrServer` | `server_error` | The daemon refused the request, or a template failed to render. |
| `ErrTimeout` | `timeout` | A read or write ran out `timeout_ms`, or `NextEvent(ms)` ran out. |
| `ErrBadFlags` | `bad_flags` | A flag bit the ABI does not define was set. |
| `ErrBusy` | `busy` | A session or watch is in use by another caller. |
| `ErrPanic` | `panic` | The library panicked; the panic was caught at the boundary. |
| `ErrVersionSkew` | `version_skew` | The daemon is a different version from the library. |
| `ErrLibraryNotFound` | `library_not_found` | No `libbeachcomber` was found where discovery looks. |
| `ErrLibrarySymbol` | `library_symbol` | The library found lacks a function this SDK calls. |
| `ErrUnsupported` | `unsupported` | A transport cannot do this operation. This SDK has one transport, which can do every operation. |

```go
if errors.Is(err, beachcomber.ErrTimeout) { … }
var e *beachcomber.Error
if errors.As(err, &e) { log.Printf("%s: %s (library %s)", e.Kind, e.Message, e.LibraryVersion) }
```

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

If none loads, `NewClient` returns `ErrLibraryNotFound`, naming every
location tried and why each failed. Every required `bc_*` symbol is checked at load time, not on
first use; a loaded library missing one returns `ErrLibrarySymbol`, naming
the missing symbol(s), with the library's own `bc_version()` in
`LibraryVersion`. Discovery and validation happen once per
process (`sync.Once`) and are cached for the lifetime of the program.

## Socket discovery

Once the library is loaded, the daemon socket path is resolved by
`libbeachcomber` itself (not this SDK) when no `WithSocketPath` is given
(`bc_socket_path` in the C ABI returns it):

1. `$BEACHCOMBER_SOCKET` — if set and non-empty.
2. `/tmp/beachcomber-<uid>/sock`.

This mirrors the daemon's bind path: a single stable path per user, so
singleton enforcement is per-user rather than per-session. No
session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted.
Pass `WithSocketPath` to override it directly.

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

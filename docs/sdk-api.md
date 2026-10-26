# The SDK API

Every SDK in `sdks/` binds the C ABI (`libbeachcomber-ffi/include/beachcomber.h`)
and follows this one table: the same operations under the same names, the same
arguments, the same value shapes and the same error kinds. The C SDK is the ABI
itself. [`sdks/api.json`](../sdks/api.json) is the table in machine-readable
form, and each SDK's own test suite checks its public surface against it, so an
SDK that drifts fails its tests.

Names are written here in snake_case. Python, Ruby and Lua spell them that way;
Node spells them in camelCase (`set_context` is `setContext`, `timeout_ms` is
`timeoutMs`), and Go in PascalCase (`SetContext`, `WithTimeoutMs`).

## Client

A client is created with these options, all optional. An SDK passes each one
it is given to `bc_client_new` and has no default of its own: an option left
out is the ABI's default.

| Option | ABI default | Meaning |
|---|---|---|
| `socket_path` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeout_ms` | 100 | How long one read or write on the socket may take. |
| `connect_timeout_ms` | 1500 | How long connecting keeps retrying before it gives up; 0 is one attempt. |
| `autostart` | on | Start a daemon when none answers an auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`. |

| Method | ABI | Answers |
|---|---|---|
| `get(key, cwd?, env?, force?, wait?)` | `bc_get` | The value `comb get` prints; absent on a miss. |
| `eval(template, cwd?, env?)` | `bc_eval` | The value `comb eval` prints; absent when it renders nothing. |
| `watch(key, cwd?, env?)` | `bc_watch_open` | A watch stream of `get`'s value. |
| `put(key, data, ttl?, path?)` | `bc_put` | Nothing. `data` absent removes the node. |
| `refresh(key, path?)` | `bc_refresh` | Nothing. |
| `status()` | `bc_status` | A list of cache rows. |
| `hello()` | `bc_hello` | The protocol and daemon versions. |
| `introspect(subject, duration_secs?)` | `bc_introspect` | The subject's report: a typed daemon health for `daemon`, the JSON as it is for the rest. |
| `session()` | `bc_session_open` | A session. |
| `close()` | `bc_client_free` | Nothing. |

`cwd` is the directory a read addresses, read as `comb get --path` reads one:
`~` is the home directory and a relative path is against the caller's working
directory. Leaving it out addresses the global entry. `env` is the caller's
environment for `env.*` references; leaving it out is the calling process's
environment, as `comb` renders with its own.

`force` and `wait` are `comb get --force` and `--wait`, and `bc_get`'s
`options_json` keys (`{"force": true, "wait": "50ms"}`); an SDK sends the ones
its caller gave and no others. `force` re-executes every Source the read
demands. `wait` is how long the read waits for a run it needs before it
answers what is stored: a duration (`"0"`, `"50ms"`, `"2s"`) or `"forever"`.
The library reads it, so a `wait` that is neither is the same `parse_error`
on every SDK. Left out, a stored record waits its Source's budget, and a cold
read or a forced one waits for its run.

A read may take `timeout_ms` plus its `wait`, and one that waits `"forever"`
has no deadline. A cold read with no `wait` waits for its run, so a slow
Source can outlast the default 100 ms `timeout_ms` and report `timeout`; the
run goes on, and the next read has its value.

A template leaf that caches its value (`| cache()`) is served from its memo
when the daemon holds a live one, on every SDK as on `comb get`; `force`
renders past it. A `get` that renders such a leaf writes the new memo back
before it returns: a synchronous call has no "after the caller has the
value", and the write is one more request on the connection the read used.
A write that fails is dropped, and the next read renders again. `eval`
stores nothing of its own text. The library does both, in
`libbeachcomber::resolve`, so no SDK has code of its own for it.

## Session

A session holds one connection and a directory of its own.

| Method | ABI | Answers |
|---|---|---|
| `set_context(path)` | `bc_session_set_context` | Nothing. |
| `get(key, cwd?, env?, force?, wait?)` | `bc_session_get` | As the client's `get`; no `cwd` is the session's directory, else the global entry. |
| `put(key, data, ttl?, path?)` | `bc_session_put` | As the client's `put`; no `path` is the session's directory. |
| `refresh(key, path?)` | `bc_session_refresh` | As the client's `refresh`; no `path` is the session's directory. |
| `close()` | `bc_session_close` | Nothing. |

`status`, `hello` and `introspect` address no directory and need no connection
of their own, so they are the client's alone.

## Watch stream

| Method | ABI | Answers |
|---|---|---|
| `next_event(timeout_ms?)` | `bc_watch_next` | The next event; absent once the stream has ended. A `timeout_ms` that runs out with no event is the `timeout` error. |
| `cancel()` | `bc_watch_cancel` | Nothing. Ends the stream; safe to call while another thread waits in `next_event`. |
| `close()` | `bc_watch_cancel`, `bc_watch_free` | Nothing. |

An event carries `data`, the value `get` answers at that moment; absent `data`
is a miss, not the end of the stream. Each SDK also iterates a stream the way
its language iterates (a Python iterator, a Ruby `each`, a Node async
iterator), ending where `next_event` answers absent.

## A closed handle

`close` is idempotent. After it, any call on that client or session fails
with `io_error` (`the client is closed`, `the session is closed`), raised by
the SDK before the library is reached: the library frees the handle, and a
freed or NULL handle would crash the process. A closed watch stream has
ended, so `next_event` answers absent and `cancel` does nothing.

## Values

`get`, `eval` and an event's `data` are the value itself: a string, number,
boolean, object or array, never a wrapper. Absent is the language's null
(`None`, `nil`, `null`, a nil `any`), and it means a miss: nothing answers at
the address. A cache row, the hello versions, the daemon health (with its
reaper status and verdicts), an introspect response and a watch event are
typed, with exactly the fields `libbeachcomber::protocol` defines, in the
language's casing; `sdks/api.json` lists them under `types`. A row's `kind`
and `failure`, and an introspect subject other than `daemon`, stay the JSON
the ABI hands on.

## Errors

Every failure is one of these kinds. Each SDK has one error type per kind,
named for it, carrying `kind` (the slug below) and `message`. The message is
the ABI's, unchanged; the library's version is a field of its own,
`library_version`, not part of the message.

| Kind | Raised by | When |
|---|---|---|
| `daemon_not_running` | ABI | No daemon answers, and none was started. |
| `connection_failed` | ABI | An explicit `socket_path` could not be connected to. |
| `io_error` | ABI | A read or write on the socket failed. |
| `parse_error` | ABI | A request, a response or a template could not be read. |
| `server_error` | ABI | The daemon refused the request, or a template failed to render. |
| `timeout` | ABI | A read or write ran out `timeout_ms`, or `next_event` ran out its `timeout_ms`. |
| `busy` | ABI | A session or watch is in use by another caller. |
| `panic` | ABI | The library panicked; the panic was caught at the boundary. |
| `version_skew` | ABI | The daemon is a different version from the library. |
| `library_not_found` | SDK | No `libbeachcomber` was found where discovery looks. |
| `library_symbol` | SDK | The library found lacks a function this SDK calls. |
| `unsupported` | SDK | A subprocess tier cannot do this operation. |

| Kind | Python, Ruby, Node | Go | Lua |
|---|---|---|---|
| any | `CombError`, the base of all of them | `*Error` | an `Error` |
| `daemon_not_running` | `DaemonNotRunningError` | `ErrDaemonNotRunning` | `kind == "daemon_not_running"` |
| `connection_failed` | `ConnectionFailedError` | `ErrConnectionFailed` | `kind == "connection_failed"` |
| `io_error` | `IoError` | `ErrIo` | `kind == "io_error"` |
| `parse_error` | `ParseError` | `ErrParse` | `kind == "parse_error"` |
| `server_error` | `ServerError` | `ErrServer` | `kind == "server_error"` |
| `timeout` | `TimeoutError` | `ErrTimeout` | `kind == "timeout"` |
| `busy` | `BusyError` | `ErrBusy` | `kind == "busy"` |
| `panic` | `PanicError` | `ErrPanic` | `kind == "panic"` |
| `version_skew` | `VersionSkewError` | `ErrVersionSkew` | `kind == "version_skew"` |
| `library_not_found` | `LibraryNotFoundError` | `ErrLibraryNotFound` | `kind == "library_not_found"` |
| `library_symbol` | `LibrarySymbolError` | `ErrLibrarySymbol` | `kind == "library_symbol"` |
| `unsupported` | `UnsupportedError` | `ErrUnsupported` | `kind == "unsupported"` |

Go returns one error type, `*Error` (`Kind`, `Message`, `LibraryVersion`),
and the sentinels match it by kind: `errors.Is(err, beachcomber.ErrTimeout)`.
Lua returns `nil, err` from any method that fails, where `err.kind` is the
slug; a miss is `nil` with no error, and a method that answers nothing
returns `true`. An error the SDK raises itself (`library_not_found`,
`library_symbol`, `unsupported`) carries the SDK's own message.

## Library discovery

An SDK has to find `libbeachcomber` before it can call anything, so this rule
is the one part of the API that cannot live in the library. Every SDK follows
it. The file is `libbeachcomber.dylib` on macOS and `libbeachcomber.so` on
every other platform, and the SDK tries, in order:

1. `$BEACHCOMBER_LIB`, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library.
2. Beside `comb`: `<dir>/../lib/<file>`, where `<dir>` is the directory of
   the first executable `comb` on `$PATH`, its symlinks resolved. A Homebrew
   `comb` is a symlink into the Cellar, and the library sits in the Cellar
   beside it; a `.deb` or `.rpm` puts them in `/usr/bin` and `/usr/lib`.
3. The system loader, given the bare file name.

When none loads, the SDK raises `library_not_found`, naming every candidate
it tried and why each failed. [`sdks/discovery.json`](../sdks/discovery.json)
is the rule as a fixture: each SDK's tests build every case's directory tree
and check the SDK's list of candidates against it.

## Language conventions

| | Optional arguments | Session scope | Stream iteration |
|---|---|---|---|
| Python | keyword arguments | `with client.session() as s:` | `for event in stream:` |
| Ruby | keyword arguments | `client.session { \|s\| … }` | `stream.each { \|event\| … }` |
| Node | an options object | `const s = await client.session()` | `for await (const event of stream)` |
| Go | `""` and `nil` are absent; a trailing optional is variadic (`Get`'s `Force` and `Wait("50ms")`, `NextEvent`'s timeout, `Introspect`'s duration) | `s, err := c.Session()` | `ev, err := w.NextEvent()` until `ev` is nil |
| Lua | an options table | `local s = client:session()` | `while true do local ev = w:next_event() … end` |

---
sidebar_position: 8
---

# C SDK

A thin C binding for the beachcomber (`comb`) shell-state daemon, built
directly on `libbeachcomber`'s C ABI (`bc_*`). Unlike the other SDKs, this
one is not a dynamically-loaded binding — it links against the shared
library at build time and resolves it through the platform linker and
run-path, the same way any C program consumes a shared library.

The SDK itself is just `beachcomber.h` — a cbindgen-generated header — plus
build glue (a `Makefile` and a `libbeachcomber.pc.in` pkg-config template).
There is no hand-written client code: every `bc_*` call is a direct entry
point into `libbeachcomber-ffi`, the same C ABI every other SDK binds
through `ctypes`/`fiddle`/FFI. No external dependencies beyond the shared
library and the C standard library.

## Installing

### Debian/Ubuntu

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/libbeachcomber-dev_0.8.0_amd64.deb
sudo dpkg -i libbeachcomber-dev_0.8.0_amd64.deb
```

### Fedora/RHEL

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/libbeachcomber-devel-0.8.0-1.x86_64.rpm
sudo rpm -i libbeachcomber-devel-0.8.0-1.x86_64.rpm
```

### Arch Linux (AUR)

```sh
yay -S libbeachcomber
```

### From source

Available as a source tarball in [GitHub Releases](https://github.com/NavistAu/beachcomber/releases), or build from the repo:

```sh
cd sdks/c
make               # builds libbeachcomber (via `cargo build -p libbeachcomber-ffi`) if needed,
                    # then the conformance runner
make test          # builds and runs the conformance runner against a real daemon
make install       # installs beachcomber.h + libbeachcomber.pc to /usr/local (override with PREFIX=...)
```

`make` builds `libbeachcomber` itself from the workspace root if it isn't
already present at `../../target/debug`. Override `BC_LIB_DIR` /
`BC_INCLUDE_DIR` to build against an installed copy instead.

## Quick start

```c
#include <stdio.h>
#include "beachcomber.h"

int main(void) {
    BcClient *c = bc_client_new(NULL);   /* default socket discovery */

    char *r = bc_get(c, "git.branch", "/path/to/repo", 0);
    printf("%s\n", r);
    /* {"ok":true,"data":{"data":"main","age_ms":12,"stale":false}}
     * or {"ok":true,"data":{"data":null,"age_ms":null,"stale":null}} on miss
     * or {"ok":false,"error":{"kind":"...","message":"..."}} on error */
    bc_string_free(r);

    bc_client_free(c);
    return 0;
}
```

Every `bc_*` call returns a caller-owned, NUL-terminated `char *` JSON
string. There is no typed C struct layer over it — parse the envelope with
your own JSON library. This SDK does not ship one; free every returned
string with `bc_string_free()` (except `bc_version()`, which is static and
must never be freed).

Compile with pkg-config (recommended):

```sh
cc -o myapp myapp.c $(pkg-config --cflags --libs libbeachcomber)
```

Or specify paths directly:

```sh
cc -o myapp myapp.c -I/usr/local/include -L/usr/local/lib -lbeachcomber \
   -Wl,-rpath,/usr/local/lib
```

## The JSON envelope

Every operation returns one of two shapes:

```json
{"ok": true, "data": <op result>}
{"ok": false, "error": {"kind": "...", "message": "..."}}
```

`bc_get` / `bc_session_get` nest the read itself one level deeper —
`data.data` is the value (`null` on a cache miss), alongside `data.age_ms`
and `data.stale`.

`bc_watch_next` uses a five-outcome shape instead of the plain `data` field,
so a binding can tell the cases apart without string matching:

```json
{"ok": true, "outcome": "event", "data": <event>}
{"ok": true, "outcome": "timeout"}
{"ok": true, "outcome": "eof"}
{"ok": true, "outcome": "cancelled"}
{"ok": false, "error": {"kind": "...", "message": "..."}}
```

### Error kinds

| `error.kind` | Meaning |
|---|---|
| `daemon_not_running` | Cannot connect to the socket |
| `connection_failed` | Connection attempt failed |
| `io_error` | Socket read/write failure |
| `parse_error` | Malformed response from the daemon |
| `server_error` | Daemon returned `ok: false` |
| `timeout` | The call exceeded its timeout |
| `bad_flags` | An unrecognised bit was set in a `flags` argument |
| `busy` | The handle's connection is already in use by another caller |
| `version_skew` | The daemon's reported version doesn't match this library's |
| `panic` | The call panicked internally; caught at the FFI boundary |

## API reference

### Version

| Function | Description |
|---|---|
| `const char *bc_version(void)` | Library build version. Static string — never pass to `bc_string_free`. |

### Client lifecycle

| Function | Description |
|---|---|
| `BcClient *bc_client_new(const char *options_json)` | Create a client handle. `options_json` is nullable; recognised keys are `socket_path`, `timeout_ms`, `autostart`. Never returns NULL. |
| `void bc_client_free(BcClient *client)` | Free a client handle. Null-safe. |

### Operations

`BcClient` operations reconnect per call.

| Function | Description |
|---|---|
| `char *bc_get(BcClient *c, const char *key, const char *path, uint32_t flags)` | Read a cached value. `path` is nullable. `flags` is a bitmask of `BC_GET_FORCE` / `BC_GET_WAIT`; any other bit yields `kind: "bad_flags"`. |
| `char *bc_put(BcClient *c, const char *key, const char *json_data, const char *ttl, const char *path)` | Write a value into the cache as a virtual provider. `json_data` must parse as JSON; `ttl` and `path` are nullable. |
| `char *bc_put_null(BcClient *c, const char *key, const char *path)` | Clear the cached entry for a virtual provider key. `path` is nullable. |
| `char *bc_refresh(BcClient *c, const char *key, const char *path)` | Force recomputation of a provider. `path` is nullable. |
| `char *bc_status(BcClient *c)` | List all cache entries currently held by the daemon. |
| `char *bc_introspect(BcClient *c, const char *subject, const char *options_json)` | Inspect a daemon subsystem. `options_json`'s only recognised key, `duration_secs`, is consulted by the `procs` subject only. |
| `char *bc_hello(BcClient *c)` | Handshake — daemon and protocol version info. |

### Resolve / eval

Client-side field resolution — evaluated in-process, never touching the
daemon except to fetch `cache.*` refs the expression itself names.

| Function | Description |
|---|---|
| `char *bc_resolve(BcClient *c, const char *key, const char *cwd, const char *env_json, const char *overrides_json)` | Resolve a virtual field (`key = "provider.field"`) or a path expression (`key` = a bare provider name), exactly as `comb get`'s resolution layer does. `cwd` is required. `env_json` / `overrides_json` are nullable. |
| `char *bc_eval(BcClient *c, const char *template_str, const char *cwd, const char *env_json, const char *overrides_json)` | Evaluate a value expression with the same evaluator, for an expression that need not be registered anywhere: a bare expression, a single `{{ expr }}` tag (keeps the expression's natural type), or literal text/several tags (always a string). `cwd` is required, and every daemon ref the expression names is fetched scoped to it. |

### Sessions

`BcSession` holds one persistent connection — use it when a `set_context`
needs to be visible to later calls. The connection is guarded by an
internal mutex: a concurrent caller gets `kind: "busy"` rather than blocking.

| Function | Description |
|---|---|
| `BcSession *bc_session_open(BcClient *client)` | Open a persistent session on `client`'s connection. Never returns NULL. |
| `void bc_session_close(BcSession *session)` | Close and free a session handle. Null-safe. |
| `char *bc_session_get(BcSession *s, const char *key, const char *path, uint32_t flags)` | Same as `bc_get`, on the session's persistent connection. |
| `char *bc_session_put(BcSession *s, const char *key, const char *json_data, const char *ttl, const char *path)` | Same as `bc_put`, on the session's persistent connection. |
| `char *bc_session_set_context(BcSession *s, const char *path)` | Set a default path so subsequent queries on this session don't need an explicit `path`. |

```c
BcSession *s = bc_session_open(c);
char *r = bc_session_set_context(s, "/path/to/repo");
bc_string_free(r);
r = bc_session_get(s, "git.branch", NULL, 0);   /* uses the context path */
bc_string_free(r);
bc_session_close(s);
```

### Watch

Blocking poll with five machine-readable outcomes (see [The JSON
envelope](#the-json-envelope)). `bc_watch_cancel` is the one call in this
SDK documented as safe to invoke from another thread while a
`bc_watch_next` is in flight.

| Function | Description |
|---|---|
| `BcWatch *bc_watch_open(BcClient *client, const char *key, const char *path)` | Open a watch on `key`. `path` is nullable. Returns NULL only on allocation failure. |
| `char *bc_watch_next(BcWatch *w, int32_t timeout_ms)` | Wait for the next event. `timeout_ms`: `-1` blocks indefinitely, `0` polls, `>0` waits that long. |
| `void bc_watch_cancel(BcWatch *w)` | Cancel a pending or future `bc_watch_next` call. Null-safe. Safe to call from another thread. |
| `void bc_watch_free(BcWatch *w)` | Free a watch handle. Null-safe. |

```c
BcWatch *w = bc_watch_open(c, "git.branch", "/path/to/repo");
char *r = bc_watch_next(w, 5000);   /* ms; -1 blocks, 0 polls */
printf("%s\n", r);   /* {"ok":true,"outcome":"event"|"timeout"|"eof"|"cancelled",...} */
bc_string_free(r);
bc_watch_free(w);
```

### Memory

| Function | Description |
|---|---|
| `void bc_string_free(char *ptr)` | Free a string returned by any `bc_*` function that documents its result as caller-owned. Null-safe. Never call on `bc_version()`'s return value. |

## Key format

- `"git"` — full provider, `data.data` is an object with all fields
- `"git.branch"` — single field, `data.data` is a scalar string

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Socket discovery

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment
(`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user
reaches the same daemon.

## Files

| File | Purpose |
|---|---|
| `beachcomber.h` | Passthrough to the cbindgen-generated ABI header |
| `runner_json.h` / `runner_json.c` | JSON parser used only by `conformance_runner.c` — not part of the public API |
| `conformance_runner.c` | Protocol conformance runner, driven over the ABI |
| `libbeachcomber.pc.in` | pkg-config template for an installed `libbeachcomber` |
| `Makefile` | Build system |

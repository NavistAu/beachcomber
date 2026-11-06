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
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/libbeachcomber-dev_0.9.1_amd64.deb
sudo dpkg -i libbeachcomber-dev_0.9.1_amd64.deb
```

### Fedora/RHEL

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/libbeachcomber-devel-0.9.1-1.x86_64.rpm
sudo rpm -i libbeachcomber-devel-0.9.1-1.x86_64.rpm
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

    /* What `comb get git.branch` prints there; NULL env_json is the calling process's environment */
    char *r = bc_get(c, "git.branch", "/path/to/repo", NULL, NULL);
    printf("%s\n", r);
    /* {"ok":true,"data":"main"}
     * or {"ok":true,"data":null} on miss
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

`bc_get` / `bc_session_get`'s `data` is the value itself — what `comb get`
prints — or `null` on a miss.

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
| `timeout` | The call exceeded its timeout — any op, on a client or a session |
| `busy` | The handle's connection is already in use by another caller |
| `panic` | The call panicked internally; caught at the FFI boundary |

## API reference

### Version

| Function | Description |
|---|---|
| `const char *bc_version(void)` | Library build version. Static string — never pass to `bc_string_free`. |

### Client lifecycle

| Function | Description |
|---|---|
| `BcClient *bc_client_new(const char *options_json)` | Create a client handle. `options_json` is nullable; its keys, each optional, are `socket_path`, `timeout_ms` (default 250, at least 1: how long one read or write may take), `connect_timeout_ms` (default 1500, 0 is one attempt: how long connecting keeps retrying), `autostart` (default on) and `exec` (`{"exec":true}` lets templates this client renders call `exec()`; off by default). Any other key is refused, as `parse_error` on the handle's first op. Never returns NULL. |
| `void bc_client_free(BcClient *client)` | Free a client handle. Null-safe. |

### Operations

`BcClient` operations reconnect per call.

| Function | Description |
|---|---|
| `char *bc_put(BcClient *c, const char *key, const char *json_data, const char *ttl, const char *path)` | Write a value into the cache as static data. `json_data` must parse as JSON; the JSON value `"null"` removes the node. `ttl` and `path` are nullable. |
| `char *bc_refresh(BcClient *c, const char *key, const char *path)` | Re-execute the Sources `key` demands, each at the instance that answers for `path`. `path` is nullable. A key naming no Source (a static namespace, a facet, an unknown name, a field no Source owns) errors: `nothing to refresh: <key> has no sources`. |
| `char *bc_status(BcClient *c)` | List all cache entries currently held by the daemon. |
| `char *bc_introspect(BcClient *c, const char *subject, const char *options_json)` | Inspect a daemon subsystem. `options_json`'s only recognised key, `duration_secs`, is consulted by the `procs` subject only. |
| `char *bc_hello(BcClient *c)` | Handshake — daemon and protocol version info. |

### Get / eval

Both go through `libbeachcomber::resolve`, the same function behind
`comb get` and `comb eval`: the daemon holds the templates
(`<provider>:template`, seeded from configuration or `bc_put`) and the
client renders them in-process with the `cwd` and `env_json` you pass.

| Function | Description |
|---|---|
| `char *bc_get(BcClient *c, const char *key, const char *cwd, const char *env_json, const char *options_json)` | The value `comb get` prints for this key, cwd and env. A plain field is its cached value, a templated one is rendered, and a bare provider name is the whole merged namespace; a miss is `data: null`. `cwd` is the directory every fetch the read makes addresses, read as `comb get --path` reads one (`~` is home, a relative path is against the process's working directory); NULL is the global entry. `env_json` is nullable (NULL renders with the calling process's environment, as `comb` does). `options_json` is nullable; its keys, each optional, are `force` (`true` re-executes every Source the read demands, as `comb get --force`) and `wait` (how long to wait for a run the read needs before answering what is stored, as `comb get --wait`: a duration such as `"0"` or `"50ms"`, or `"forever"`); any other key, or a `wait` that is neither, is a `parse_error`. The read may take `timeout_ms` plus the wait. |
| `char *bc_eval(BcClient *c, const char *template_str, const char *cwd, const char *env_json)` | Render a template that is stored nowhere — the same value `comb eval` prints. `template_str` accepts a single `{{ expr }}` tag (keeps the expression's natural type), or literal text/several tags (always a string — a source with no tags at all is its literal text). `cwd` is as for `bc_get`, and every daemon ref the expression names is fetched scoped to it. |

The unrendered halves are facets, read with `bc_get` like any key:
`p:template.x` is the template as stored, `p:put.x` and `p:source.x` those
layers' values. There is no raw read.

`exec()` inside a template runs only on a client created with
`bc_client_new("{\"exec\":true}")`; it is off by default, and a template
that calls it then fails with `exec() is disabled in this context`. A
template that does not compile, or cycles, is `kind: "parse_error"`; a
runtime failure (a failed or disabled `exec()`) is `kind: "server_error"`.

### Sessions

`BcSession` holds one persistent connection — use it when a `set_context`
needs to be visible to later calls. The connection is guarded by an
internal mutex: a concurrent caller gets `kind: "busy"` rather than blocking.

| Function | Description |
|---|---|
| `BcSession *bc_session_open(BcClient *client)` | Open a persistent session on `client`'s connection. Never returns NULL. |
| `void bc_session_close(BcSession *session)` | Close and free a session handle. Null-safe. |
| `char *bc_session_get(BcSession *s, const char *key, const char *cwd, const char *env_json, const char *options_json)` | Same as `bc_get`, on the session's persistent connection; a NULL `cwd` is the session's directory when it has one. |
| `char *bc_session_put(BcSession *s, const char *key, const char *json_data, const char *ttl, const char *path)` | Same as `bc_put`, on the session's persistent connection. |
| `char *bc_session_set_context(BcSession *s, const char *path)` | Set a default path so subsequent queries on this session don't need an explicit `path`. |

```c
BcSession *s = bc_session_open(c);
char *r = bc_session_set_context(s, "/path/to/repo");
bc_string_free(r);
r = bc_session_get(s, "git.branch", NULL, NULL, 0);   /* reads at the context path */
bc_string_free(r);
bc_session_close(s);
```

### Watch

Opens a watch on the *value* of a key — what `bc_get(key, cwd, env_json, NULL)`
answers — and emits it again whenever it changes: the fan-in underneath
subscribes to every key the resolution reads, and a write to a namespace or
any of its facets wakes its watchers, so a template edited under the watch
fires it exactly as a changed cache record does. An event carries no `age_ms` — a
value resolved from several cache records has no single age or staleness.

Blocking poll with five machine-readable outcomes (see [The JSON
envelope](#the-json-envelope)). `bc_watch_cancel` is the one call in this
SDK documented as safe to invoke from another thread while a
`bc_watch_next` is in flight.

| Function | Description |
|---|---|
| `BcWatch *bc_watch_open(BcClient *client, const char *key, const char *cwd, const char *env_json)` | Open a watch on `key`, resolved at `cwd` with `env_json`'s environment — the same two inputs `bc_get` takes. `cwd` is as for `bc_get` (NULL watches the global entry); `env_json` is nullable (NULL renders with the calling process's environment, as `comb` does). Returns NULL only when the process is out of memory or file descriptors. |
| `char *bc_watch_next(BcWatch *w, int32_t timeout_ms)` | Wait for the next event. `timeout_ms`: `-1` blocks indefinitely, `0` polls, `>0` waits that long. |
| `void bc_watch_cancel(BcWatch *w)` | Cancel a pending or future `bc_watch_next` call: one in flight returns `cancelled` at once. Null-safe. Safe to call from another thread. |
| `void bc_watch_free(BcWatch *w)` | Free a watch handle. Null-safe. |

```c
BcWatch *w = bc_watch_open(c, "git.branch", "/path/to/repo", NULL);
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

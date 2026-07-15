---
sidebar_position: 8
---

# C SDK

A minimal C client library for the beachcomber shell-state daemon. Uses only POSIX (sockets, unistd) and the C standard library — no external dependencies.

## Installing

### Debian/Ubuntu

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/libbeachcomber-dev_0.6.0_amd64.deb
sudo dpkg -i libbeachcomber-dev_0.6.0_amd64.deb
```

### Fedora/RHEL

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/libbeachcomber-devel-0.6.0-1.x86_64.rpm
sudo rpm -i libbeachcomber-devel-0.6.0-1.x86_64.rpm
```

### Arch Linux (AUR)

```sh
yay -S libbeachcomber
```

### From source

Available as a source tarball in [GitHub Releases](https://github.com/NavistAu/beachcomber/releases), or build from the repo:

## Building

```sh
make          # builds libbeachcomber.dylib (macOS) or libbeachcomber.so (Linux) + libbeachcomber.a
make test     # builds and runs the test suite
make install  # installs to /usr/local (override with PREFIX=...)
```

## Quick start

```c
#include <stdio.h>
#include "beachcomber.h"

int main(void) {
    comb_client_t *c = comb_connect();
    if (!c) {
        fprintf(stderr, "beachcomber daemon not running\n");
        return 1;
    }

    /* Read a single scalar field */
    comb_result_t *r = comb_get(c, "git.branch", "/path/to/repo");
    if (comb_result_ok(r) && comb_result_is_hit(r)) {
        printf("branch: %s (age %llums)\n",
               comb_result_get_str(r, NULL),
               (unsigned long long)comb_result_age_ms(r));
    } else if (comb_result_ok(r)) {
        printf("cache miss — try again shortly\n");
    } else {
        printf("error: %s\n", comb_result_error(r));
    }
    comb_result_free(r);

    /* Read a full provider object */
    r = comb_get(c, "git", "/path/to/repo");
    if (comb_result_ok(r) && comb_result_is_hit(r)) {
        int64_t staged = 0;
        int dirty = 0;
        comb_result_get_int(r, "staged", &staged);
        comb_result_get_bool(r, "dirty", &dirty);
        printf("staged=%lld dirty=%d\n", (long long)staged, dirty);
    }
    comb_result_free(r);

    /* Set a connection-level path context */
    comb_set_context(c, "/path/to/repo");
    r = comb_get(c, "git.branch", NULL);   /* uses context path */
    comb_result_free(r);

    /* Force recomputation */
    comb_refresh(c, "git", "/path/to/repo");

    /* Daemon status — iterate typed cache rows */
    comb_cache_row_t *rows = NULL;
    size_t n = 0;
    if (comb_status(c, &rows, &n) == 0) {
        for (size_t i = 0; i < n; i++) {
            printf("%s age=%llums stale=%d\n",
                   rows[i].provider,
                   (unsigned long long)rows[i].age_ms,
                   (int)rows[i].stale);
        }
        comb_free_cache_rows(rows, n);
    }

    comb_disconnect(c);
    return 0;
}
```

Compile with pkg-config (recommended):

```sh
cc -o myapp myapp.c $(pkg-config --cflags --libs libbeachcomber)
```

Or specify paths directly:

```sh
cc -o myapp myapp.c -I/usr/local/include -L/usr/local/lib -lbeachcomber
```

## API reference

### Connection

| Function | Description |
|---|---|
| `comb_connect()` | Auto-discover socket and connect |
| `comb_connect_path(path)` | Connect to an explicit socket path |
| `comb_disconnect(c)` | Close connection and free client |

### Operations

| Function | Description |
|---|---|
| `comb_get(c, key, path)` | Read a cached value (`path` may be NULL) |
| `comb_refresh(c, key, path)` | Force recomputation |
| `comb_set_context(c, path)` | Set default path for this connection |
| `comb_status(c, rows_out, n_out)` | Return typed cache rows; caller frees with `comb_free_cache_rows()` |
| `comb_hello(c, out)` | Handshake — fills a `comb_hello_info_t` with version info |
| `comb_put(c, key, data, ttl, path)` | Write a value into the cache as a virtual provider |
| `comb_put_null(c, key, path)` | Clear a virtual provider entry |
| `comb_introspect(c, subject, dur_secs)` | Inspect a daemon subsystem (returns raw result) |
| `comb_introspect_daemon(c, out)` | Inspect daemon health into a typed `comb_daemon_health_t` |
| `comb_watch(c, key, path)` | Subscribe to live cache updates; returns a `comb_watch_handle_t *` |
| `comb_watch_next(handle, event, timeout_ms)` | Receive the next watch event into `comb_watch_event_t` |
| `comb_watch_free(handle)` | Release a watch handle and close the connection |

### Result accessors

All operations that return data return a `comb_result_t *`. Always free it with `comb_result_free()` when done.

| Function | Returns |
|---|---|
| `comb_result_ok(r)` | 1 if server returned `ok:true` |
| `comb_result_is_hit(r)` | 1 if data was present (cache hit) |
| `comb_result_error(r)` | Error string, or NULL |
| `comb_result_get_str(r, field)` | String value (field=NULL for scalar results) |
| `comb_result_get_int(r, field, &out)` | Integer value; returns 1 on success |
| `comb_result_get_float(r, field, &out)` | Float value; returns 1 on success |
| `comb_result_get_bool(r, field, &out)` | Boolean (0/1); returns 1 on success |
| `comb_result_age_ms(r)` | Cache age in milliseconds |
| `comb_result_stale(r)` | 1 if data is stale |
| `comb_result_raw_json(r)` | Full raw JSON response string |
| `comb_result_free(r)` | Free the result (safe to call with NULL) |

## Key format

- `"git"` — full provider, data is an object with all fields
- `"git.branch"` — single field, data is a scalar string

For scalar results, pass `NULL` (or any field name) to `comb_result_get_str`. For object results, pass the field name to select a member.

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Socket discovery

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

## Files

| File | Purpose |
|---|---|
| `beachcomber.h` | Public API header |
| `beachcomber.c` | Library implementation |
| `json.h` | Minimal JSON parser header |
| `json.c` | Minimal JSON parser implementation |
| `test_beachcomber.c` | Unit + integration test suite |
| `Makefile` | Build system |
| `libbeachcomber.pc.in` | pkg-config template |

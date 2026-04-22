# beachcomber C SDK

A minimal C client library for the [beachcomber](https://github.com/NavistAu/beachcomber) shell-state daemon.

## Building

```sh
make            # builds libbeachcomber.dylib (macOS) or libbeachcomber.so (Linux) + libbeachcomber.a
make test       # builds and runs the test suite
make conformance  # builds the protocol conformance runner (sdks/c/conformance_runner)
make install    # installs to /usr/local (override with PREFIX=...)
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

    /* Force recompute then wait for the fresh value */
    r = comb_get_with_flags(c, "git.branch", "/path/to/repo",
                            1 /* force */, 1 /* wait */);
    comb_result_free(r);

    /* Set a connection-level path context */
    comb_set_context(c, "/path/to/repo");
    r = comb_get(c, "git.branch", NULL);   /* uses context path */
    comb_result_free(r);

    /* Force recomputation */
    comb_refresh(c, "git", "/path/to/repo");

    /* Hello — protocol and daemon version */
    comb_hello_info_t hello;
    if (comb_hello(c, &hello) == 0) {
        printf("protocol: %s  daemon: %s\n",
               hello.protocol_version, hello.daemon_version);
    }

    /* Put a virtual provider entry */
    comb_put(c, "myapp", "{\"theme\":\"dark\",\"version\":3}", NULL, NULL);

    /* Clear a virtual provider entry */
    comb_put_null(c, "myapp", NULL);

    /* Typed daemon introspect */
    comb_daemon_health_t health;
    if (comb_introspect_daemon(c, &health) == 0) {
        printf("pid=%lld uptime=%llus cache=%llu\n",
               (long long)health.pid,
               (unsigned long long)health.uptime_secs,
               (unsigned long long)health.cache_entries);
    }

    /* Generic introspect — raw result */
    comb_result_t *ir = comb_introspect(c, COMB_INTROSPECT_CACHE, 0);
    printf("cache introspect: %s\n", comb_result_raw_json(ir));
    comb_result_free(ir);

    /* Typed status rows */
    comb_cache_row_t rows[64];
    int n = comb_status_rows(c, rows, 64);
    for (int i = 0; i < n; i++) {
        printf("  %s.%s age=%llums stale=%d\n",
               rows[i].provider, rows[i].field,
               (unsigned long long)rows[i].age_ms, rows[i].stale);
    }

    /* Watch — blocking poll */
    comb_watch_handle_t *wh = comb_watch(c, "git.branch", "/path/to/repo");
    if (wh) {
        comb_watch_event_t ev;
        int ret = comb_watch_next(wh, &ev, 5000 /* ms */);
        if (ret == 1) {
            printf("watch event: %s (stale=%d)\n", ev.data_json, ev.stale);
        }
        comb_watch_free(wh);
    }

    /* Raw status */
    r = comb_status(c);
    printf("raw status: %s\n", comb_result_raw_json(r));
    comb_result_free(r);

    comb_disconnect(c);
    return 0;
}
```

Compile against the static library:

```sh
cc -o myapp myapp.c -I/usr/local/include -L/usr/local/lib -lbeachcomber
```

Or against the shared library:

```sh
cc -o myapp myapp.c -I/usr/local/include -L/usr/local/lib -lbeachcomber \
   -Wl,-rpath,/usr/local/lib
```

## API overview

### Connection

| Function | Description |
|---|---|
| `comb_connect()` | Auto-discover socket and connect |
| `comb_connect_path(path)` | Connect to an explicit socket path |
| `comb_disconnect(c)` | Close connection and free client |

Socket discovery order:
1. `$XDG_RUNTIME_DIR/beachcomber/sock`
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `/tmp/beachcomber-<uid>/sock`

### Operations

| Function | Description |
|---|---|
| `comb_get(c, key, path)` | Read a cached value (`path` may be NULL) |
| `comb_get_with_flags(c, key, path, force, wait)` | Get with force/wait flags |
| `comb_refresh(c, key, path)` | Force recomputation |
| `comb_set_context(c, path)` | Set default path for this connection |
| `comb_hello(c, &out)` | Query protocol/daemon version; fills `comb_hello_info_t` |
| `comb_put(c, key, data_json, ttl, path)` | Store a virtual provider entry |
| `comb_put_null(c, key, path)` | Clear a virtual provider entry |
| `comb_introspect_daemon(c, &out)` | Typed daemon health; fills `comb_daemon_health_t` |
| `comb_introspect(c, subject, duration_secs)` | Generic introspect; returns raw result |
| `comb_status(c)` | Query daemon status (raw result) |
| `comb_status_rows(c, rows, cap)` | Typed status; fills `comb_cache_row_t[]` |

### Watch (blocking poll)

`comb_watch()` opens a persistent connection and returns a handle. Call
`comb_watch_next()` in a loop to block up to `timeout_ms` for the next event.
`comb_watch_free()` closes the stream. No threads, no callbacks.

| Function | Description |
|---|---|
| `comb_watch(c, key, path)` | Open watch stream; returns handle |
| `comb_watch_next(handle, &event, timeout_ms)` | Block for next event: 1=event, 0=timeout, -1=error |
| `comb_watch_free(handle)` | Close stream and free handle (safe with NULL) |

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

### Typed shapes

| Type | Description |
|---|---|
| `comb_hello_info_t` | `protocol_version[32]`, `daemon_version[32]` |
| `comb_daemon_health_t` | `pid`, `version`, `uptime_secs`, `socket_path`, `config_path`, `requests_total`, `in_flight`, `active_watchers`, `cache_entries` |
| `comb_cache_row_t` | `provider[64]`, `field[64]`, `path[256]`, `value_json[1024]`, `age_ms`, `stale` |
| `comb_watch_event_t` | `data_json[1024]`, `age_ms`, `stale` |

### Introspect subjects

`comb_introspect_subject_t` values: `COMB_INTROSPECT_DAEMON`, `COMB_INTROSPECT_PROVIDERS`, `COMB_INTROSPECT_CONFIG`, `COMB_INTROSPECT_CACHE`, `COMB_INTROSPECT_BACKOFF`, `COMB_INTROSPECT_WATCHES`, `COMB_INTROSPECT_TIMERS`, `COMB_INTROSPECT_DEMAND`, `COMB_INTROSPECT_PROCS`.

## Key format

- `"git"` — full provider, data is an object with all fields
- `"git.branch"` — single field, data is a scalar string

For scalar results, pass `NULL` (or any field name) to `comb_result_get_str`.
For object results, pass the field name to select a member.

## Conformance runner

```sh
make conformance
COMB_BIN=/path/to/comb ./conformance_runner [conformance_dir]
# conformance_dir defaults to ../../tests/conformance (repo root relative to sdks/c/)
# Override with CONFORMANCE_DIR env var.
```

## Dependencies

None. The library uses only POSIX (sockets, unistd, poll) and the C standard library.
It includes a minimal JSON parser (`json.c` / `json.h`) with no external dependencies.

## Files

| File | Purpose |
|---|---|
| `beachcomber.h` | Public API header |
| `beachcomber.c` | Library implementation |
| `json.h` | Minimal JSON parser header |
| `json.c` | Minimal JSON parser implementation |
| `test_beachcomber.c` | Unit + integration test suite |
| `conformance_runner.c` | Protocol conformance runner |
| `Makefile` | Build system |

---
sidebar_position: 8
---

# C SDK

A minimal C client library for the beachcomber shell-state daemon. Available as a source tarball in [GitHub Releases](https://github.com/NavistAu/beachcomber/releases). Uses only POSIX (sockets, unistd) and the C standard library — no external dependencies.

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
    comb_poke(c, "git", "/path/to/repo");

    /* List providers */
    r = comb_list(c);
    printf("raw list response: %s\n", comb_result_raw_json(r));
    comb_result_free(r);

    /* Daemon status */
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
| `comb_poke(c, key, path)` | Force recomputation |
| `comb_set_context(c, path)` | Set default path for this connection |
| `comb_list(c)` | List available providers |
| `comb_status(c)` | Query daemon status |

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

## Socket discovery

1. `$XDG_RUNTIME_DIR/beachcomber/sock`
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `/tmp/beachcomber-<uid>/sock`

## Files

| File | Purpose |
|---|---|
| `beachcomber.h` | Public API header |
| `beachcomber.c` | Library implementation |
| `json.h` | Minimal JSON parser header |
| `json.c` | Minimal JSON parser implementation |
| `test_beachcomber.c` | Unit + integration test suite |
| `Makefile` | Build system |

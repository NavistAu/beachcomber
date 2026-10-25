# beachcomber C SDK

A C binding for the [beachcomber](https://github.com/NavistAu/beachcomber)
shell-state daemon, built directly on `libbeachcomber`'s C ABI.

Unlike the other five SDKs, this one is not a dynamically-loaded binding —
it links against the cdylib at build time and resolves through the
platform linker and run-path, the same way any C program consumes a shared
library.

## Building

```sh
make               # builds the conformance runner (default target)
make conformance   # same, explicitly
make test          # builds and runs the conformance runner
make install       # installs beachcomber.h + libbeachcomber.pc to /usr/local (override with PREFIX=...)
```

`make` builds `libbeachcomber` itself (via `cargo build -p libbeachcomber-ffi`
in the workspace root) if it isn't already present at `../../target/debug`.
Override `BC_LIB_DIR` / `BC_INCLUDE_DIR` to build against an installed copy
instead.

## Quick start

```c
#include <stdio.h>
#include "beachcomber.h"

int main(void) {
    BcClient *c = bc_client_new(NULL); /* default socket discovery */
    /* Options JSON keys, each optional: socket_path, timeout_ms (default
     * 100, at least 1), connect_timeout_ms (default 1500, 0 is one attempt),
     * autostart (default on) and exec (default off) —
     * bc_client_new("{\"exec\":true}") lets templates this client renders
     * call exec(). Any other key is refused (parse_error on the first op). */

    /* The value `comb get git.branch` prints at that directory. NULL cwd is
     * the global entry; env_json is the caller's environment for env.*. */
    char *r = bc_get(c, "git.branch", "/path/to/repo", NULL, 0);
    printf("%s\n", r);
    /* {"ok":true,"data":"main"}
     * or {"ok":true,"data":null} on miss
     * or {"ok":false,"error":{"kind":"...","message":"..."}} on error */
    bc_string_free(r);

    /* Re-execute what the read demands, waiting for the fresh value */
    r = bc_get(c, "git.branch", "/path/to/repo", NULL, BC_GET_FORCE | BC_GET_WAIT);
    bc_string_free(r);

    /* The template itself, unrendered: a facet, read like any key */
    r = bc_get(c, "myapp:template.theme", NULL, NULL, 0);
    bc_string_free(r);

    /* Put / remove a static-data entry ("null" removes) */
    r = bc_put(c, "myapp", "{\"theme\":\"dark\",\"version\":3}", NULL, NULL);
    bc_string_free(r);
    r = bc_put(c, "myapp", "null", NULL, NULL);
    bc_string_free(r);

    /* A persistent connection, for a context set once and reused */
    BcSession *s = bc_session_open(c);
    r = bc_session_set_context(s, "/path/to/repo");
    bc_string_free(r);
    r = bc_session_get(s, "git.branch", NULL, NULL, 0); /* reads at the context path */
    bc_string_free(r);
    bc_session_close(s);

    /* Watch — blocking poll */
    BcWatch *w = bc_watch_open(c, "git.branch", "/path/to/repo", NULL);
    r = bc_watch_next(w, 5000 /* ms; -1 blocks, 0 polls */);
    printf("%s\n", r); /* {"ok":true,"outcome":"event"|"timeout"|"eof"|"cancelled",...} */
    bc_string_free(r);
    bc_watch_free(w);

    bc_client_free(c);
    return 0;
}
```

```sh
cc -o myapp myapp.c -I/usr/local/include -L/usr/local/lib -lbeachcomber \
   -Wl,-rpath,/usr/local/lib
```

## API surface

`beachcomber.h` is a one-line passthrough to the cbindgen-generated
`libbeachcomber-ffi/include/beachcomber.h` — see that file's doc comments
for the full bc_* contract (ownership, NULL-safety, flag bits). In short:

- Every call returns a caller-owned, NUL-terminated `char *` JSON envelope
  — `{"ok":true,"data":...}` or `{"ok":false,"error":{"kind":...,"message":...}}`
  — except `bc_version()` (static, never freed) and the handle constructors
  (`bc_client_new`, `bc_session_open`, `bc_watch_open`). `data` is always
  present, and for `bc_get`, `bc_session_get` and `bc_eval` a `null` is a
  miss. The `kind` slugs, and the one API every other
  SDK builds on these functions, are in `docs/sdk-api.md`.
- Free every other returned string with `bc_string_free()`.
- `BcClient` (`bc_get`/`bc_put`/`bc_refresh`/`bc_status`/
  `bc_introspect`/`bc_hello`/`bc_eval`/`bc_watch_open`)
  reconnects per call. `BcSession` (`bc_session_open` on a `BcClient`, then
  `bc_session_get`/`put`/`refresh`/`set_context`) holds one persistent
  connection, and a directory of its own that a NULL `cwd` or `path` sends —
  use it when a `set_context` needs to be visible to later calls.
- `bc_get(client, key, cwd, env_json, flags)` / `bc_session_get(session, …)`
  return the same value `comb get` prints for this key, cwd and env: the
  daemon holds the templates (`<provider>:template`, seeded from
  configuration or `bc_put`) and the client renders them. A plain field is
  its cached value, a templated one is rendered, and a bare provider name is
  the whole merged namespace; a miss is `data: null`. `cwd` is the directory
  the read addresses, read as `comb get --path` reads one (`~` is the home
  directory, a relative path is against the process's working directory);
  NULL is the global entry. `env_json` is nullable ("no environment": every
  `env.*` is absent). `flags` is a bitmask: `BC_GET_FORCE`, `BC_GET_WAIT`;
  any other bit set is rejected (`kind: "bad_flags"`).
- The unrendered halves are facets, read with `bc_get` like any key:
  `p:template.x` is the template as stored, `p:put.x` and `p:source.x`
  those layers' values. There is no raw read.
- `bc_eval(client, template_str, cwd, env_json)` renders a template that is
  stored nowhere — the same value `comb eval` prints. `template_str`
  accepts a single `{{ expr }}` tag, or literal text/several tags — the
  former keeps the expression's natural type, the latter is always a
  string (a source with no tags at all is its literal text).
- `exec()` inside a template runs only on a client created with
  `bc_client_new("{\"exec\":true}")`; it is off by default, and a template
  that calls it then fails with `exec() is disabled in this context`.
- `bc_watch_open(c, key, cwd, env_json)` + `bc_watch_next(w, timeout_ms)` +
  `bc_watch_cancel` /
  `bc_watch_free`: blocking poll, five machine-readable outcomes
  (`event`/`timeout`/`eof`/`cancelled`/the ordinary `ok:false` error
  envelope). `bc_watch_cancel` is the one call safe to invoke from another
  thread while a `bc_watch_next` is in flight.

There is no typed C struct layer over the envelope — parse the returned
JSON with your own JSON library. This SDK does not ship one (see below).

## Conformance runner

```sh
make conformance
COMB_BIN=/path/to/comb ./conformance_runner [conformance_dir]
# conformance_dir defaults to ../../tests/conformance (repo root relative to sdks/c/)
# Override with CONFORMANCE_DIR env var.
```

## Dependencies

The SDK itself (`beachcomber.h`) has none beyond `libbeachcomber` — no
protocol code, no JSON parser, nothing to link but the cdylib.

`conformance_runner.c` is a JSON *consumer* — it builds request payloads
from fixture JSON and reads the bc_* envelope strings it gets back — so it
carries its own minimal parser, `runner_json.c` / `runner_json.h`. That
parser is scoped to the runner alone: it is not installed, not part of the
public API, and not what a program linking this SDK needs to bring in.

## Files

| File | Purpose |
|---|---|
| `beachcomber.h` | Passthrough to the generated ABI header |
| `runner_json.h` / `runner_json.c` | JSON parser used only by `conformance_runner.c` |
| `conformance_runner.c` | Protocol conformance runner, driven over the ABI |
| `libbeachcomber.pc.in` | pkg-config template for an installed `libbeachcomber` |
| `Makefile` | Build system |

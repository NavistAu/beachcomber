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
    /* Options JSON keys: socket_path, timeout_ms, autostart, exec —
     * bc_client_new("{\"exec\":true}") lets templates this client resolves
     * call exec(); it is off by default. */

    char *r = bc_get(c, "git.branch", "/path/to/repo", 0);
    printf("%s\n", r);
    /* {"ok":true,"data":{"data":"main","age_ms":12,"stale":false}}
     * or {"ok":true,"data":{"data":null,"age_ms":null,"stale":null,"template":"{{ … }}"}}
     *    on a template-only node (see "The get envelope" below)
     * or {"ok":true,"data":{"data":null,"age_ms":null,"stale":null}} on miss
     * or {"ok":false,"error":{"kind":"...","message":"..."}} on error */
    bc_string_free(r);

    /* Force recompute then wait for the fresh value */
    r = bc_get(c, "git.branch", "/path/to/repo", BC_GET_FORCE | BC_GET_WAIT);
    bc_string_free(r);

    /* Put / remove a static-data entry ("null" removes) */
    r = bc_put(c, "myapp", "{\"theme\":\"dark\",\"version\":3}", NULL, NULL);
    bc_string_free(r);
    r = bc_put(c, "myapp", "null", NULL, NULL);
    bc_string_free(r);

    /* The same value `comb get myapp.theme` prints at that cwd */
    r = bc_resolve(c, "myapp.theme", "/path/to/repo", NULL);
    bc_string_free(r);

    /* A persistent connection, for a context set once and reused */
    BcSession *s = bc_session_open(c);
    r = bc_session_set_context(s, "/path/to/repo");
    bc_string_free(r);
    r = bc_session_get(s, "git.branch", NULL, 0); /* uses the context path */
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
  (`bc_client_new`, `bc_session_open`, `bc_watch_open`).
- Free every other returned string with `bc_string_free()`.
- `BcClient` (`bc_get`/`bc_put`/`bc_refresh`/`bc_status`/
  `bc_introspect`/`bc_hello`/`bc_resolve`/`bc_eval`/`bc_watch_open`)
  reconnects per call. `BcSession` (`bc_session_open` on a `BcClient`, then
  `bc_session_get`/`put`/`set_context`) holds one persistent connection —
  use it when a `set_context` needs to be visible to later calls.
- `bc_get` / `bc_session_get` take a `flags` bitmask: `BC_GET_FORCE`,
  `BC_GET_WAIT`. Any other bit set is rejected (`kind: "bad_flags"`). The
  envelope's `data` object is the node as the daemon holds it — both
  halves, nothing rendered; see "The `get` envelope" below.
- `bc_resolve(client, key, cwd, env_json)` returns the same value `comb get`
  returns for this key, cwd and env: the daemon holds the templates
  (`<provider>:template`, seeded from configuration or `bc_put`) and the
  client renders them. A plain field is its cached value, a templated one
  is rendered, and a bare provider name is the whole merged namespace; a
  miss is `data: null`. `cwd` is required; `env_json` is nullable ("no
  environment": every `env.*` is `""`). Every fetch the resolution makes
  addresses `cwd`.
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

## The `get` envelope

`bc_get` / `bc_session_get` return the node as the daemon holds it — both
halves, nothing rendered. The inner `data` object has five keys, the last
two present only when set:

| node | `data` | `age_ms` / `stale` | `template` | `rest` |
|---|---|---|---|---|
| value (a hit) | the value | set | only if a template is in effect | only if set |
| template only | `null` | `null` | a string (leaf) or object (group) | only if set |
| nothing (a miss) | `null` | `null` | absent | absent |

```json
{"data": 1, "age_ms": 12, "stale": false, "template": "{{ 2 }}", "rest": "x"}
{"data": null, "age_ms": null, "stale": null, "template": "{{ env.A }}"}
{"data": null, "age_ms": null, "stale": null}
```

`template` is the template in effect at the address, unrendered. `rest` is
the address left over when the template walk stopped at a string:
`bc_get(c, "p.t.x", …)` where `p.t` is a template string gives `template`
of `p.t` and `"rest": "x"`. A template-only node is not a miss — something
is stored there — and not a hit — it has no value until it is rendered. It
is an address whose value `bc_resolve` renders: `bc_get` is the raw half,
`bc_resolve(c, key, cwd, env_json)` is how you get the value.

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

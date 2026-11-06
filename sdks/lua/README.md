# beachcomber — Lua SDK

Lua client for the [beachcomber](https://github.com/NavistAu/beachcomber)
daemon. A binding over `libbeachcomber`'s C ABI
(`libbeachcomber-ffi/include/beachcomber.h`), not a hand-rolled socket
protocol. It follows the SDK API table in
[`docs/sdk-api.md`](../../docs/sdk-api.md) — the same operations, names,
value shapes and error kinds as every other SDK — and its test suite checks
its surface against the table's machine-readable form, `sdks/api.json`.

---

## Transport

Two transports, selected automatically by `comb.connect()`:

| Transport | Interpreter | Mechanism | Cost/call | `transport()` |
|---|---|---|---|---|
| `ffi` | LuaJIT (incl. Neovim, which always ships LuaJIT) | `ffi` calls straight into the cdylib | ~0.3ms | `"ffi"` |
| `subprocess` | PUC Lua 5.1–5.4 | shells out to the `comb` binary | ~5ms | `"subprocess"` |

**Why PUC Lua gets a subprocess fallback instead of a C shim.** PUC Lua has
no `ffi` and cannot call into a cdylib without one — a C shim compiled per
platform is the only other way to bridge that gap, and it would be the sole
binding artifact across every SDK in this project needing a build toolchain
at install time. Given that trade-off, PUC Lua is supported only through
the subprocess fallback, which needs nothing beyond a `comb` binary on
`$PATH`.

The fallback is a *sanctioned* fallback, not a silent recovery path: it's
selected only when `ffi` is unavailable *by design* (checked once, at
`comb.connect()` time). A broken or missing library under LuaJIT is a loud
error naming every path tried — it never silently downgrades to
`subprocess`, so a broken install reads as a broken install, not as a
mysterious 5ms-per-call slowdown. Call `client:transport()` any time you
need to know which one you got.

**Capability ceiling of the subprocess transport.** `comb` has no CLI
surface for `hello` or `introspect`, and `watch` is refused: `io.popen` has no
timed read and no cancel, so `next_event(timeout_ms)` and `cancel` could not
be honoured. Those `Client` methods return `nil, err` with `err.kind ==
"unsupported"` over `subprocess`; get/put/refresh/status/eval and a session
all work over either transport. A session over `subprocess` keeps its
directory between spawns and passes it as `--path`, as the Node SDK's
subprocess session does. `refresh` over `subprocess` is one `comb refresh
<key>`, the daemon's op itself, and its refusal (`nothing to refresh: <key>
has no sources`) is the error message. `get` over `subprocess` is one `comb
get -f json <key>`, so its answer is the resolver's: the rendered value, the
directory chain up to `$HOME`, `$HOME` as the global entry, force, the wait, the
daemon's refusals. `eval` is one `comb eval -f json -- <template>`; a
template that does not compile is `err.kind == "server_error"` there, where
the ffi tier reports `parse_error`.

The spawned `comb` renders as the ffi tier does. A call's `opts.env` is its
whole environment (the command runs as `env -i K=V …`, with the absolute path
to `comb`; with no `env` it is this process's), so `env.*` and an `exec()`
child see exactly the map. `--socket` carries `socket_path`, else
`$BEACHCOMBER_SOCKET`, so the map cannot point the tier at another daemon.
`--color never` goes when the render's environment sets a non-empty
`NO_COLOR`, else `--color always`. And `--no-exec` goes unless the client was
connected with `exec = true`.

---

## Requirements

- **LuaJIT** (`ffi` transport) — no extra dependencies, works standalone
  or inside Neovim.
- **PUC Lua 5.1–5.4** (`subprocess` transport) — needs `comb` on `$PATH`.

Either way: no external Lua dependency. The JSON encoder/decoder
(`beachcomber/json.lua`) is hand-rolled stdlib-only, same as before.

---

## Installation

### LuaRocks

```sh
luarocks install --local libbeachcomber
```

### Manual (Neovim)

Add `sdks/lua` to your `package.path`, or copy the `beachcomber/` directory
somewhere on your `runtimepath`.

---

## Quick start

```lua
local comb = require('beachcomber')

local client, err = comb.connect()
if not client then
  error("beachcomber: " .. tostring(err))
end

print(client:transport())  -- "ffi" or "subprocess"

-- get a single field: what `comb get git.branch` prints there, nil on a miss
local branch, err = client:get('git.branch', { cwd = '/my/repo' })
if branch then print(branch) end  -- "main"

-- get a full provider (returns a table)
local git = client:get('git', { cwd = '/my/repo' })
if git then print(git.branch) end  -- "main"

-- the template itself, unrendered: a facet, read like any key
local tpl = client:get('git:template.branch')

-- refresh (force recompute)
client:refresh('git', { path = '/my/repo' })

-- a session keeps a directory of its own for the calls that name none
local s = client:session()
s:set_context('/my/repo')
local branch2 = s:get('git.branch')
s:close()

-- daemon cache status rows
local rows = client:status()
for _, row in ipairs(rows or {}) do
  print(row.provider, row.field, row.age_ms)
end

client:close()
```

### Custom options

```lua
local client = comb.connect({
  socket_path = '/run/user/1000/beachcomber/sock',
  timeout_ms  = 2000,                                -- ffi transport
  connect_timeout_ms = 0,                            -- ffi transport
  autostart   = true,                                -- ffi transport
  exec        = true,                                -- templates may call exec()
  library_path = '/opt/beachcomber/lib/libbeachcomber.dylib', -- ffi: as $BEACHCOMBER_LIB, the only candidate
  comb_bin     = '/opt/beachcomber/bin/comb',                 -- subprocess: override discovery
})
```

The client options are passed to the library's `bc_client_new` as they are
given; the SDK has no defaults of its own, so an option left out is the
library's default.

| Option | Library default | Meaning |
|---|---|---|
| `socket_path` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeout_ms` | 250 | How long one read or write on the socket may take. 0 is refused. |
| `connect_timeout_ms` | 1500 | How long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. |
| `autostart` | on | Start a daemon when none serves the auto-discovered socket. |
| `exec` | off | Whether a template this client renders may call `exec()`; a template that calls it without the option fails with `exec() is disabled in this context`. |

The subprocess transport cannot bound a spawned `comb` (`io.popen` has no
timeout, and `comb` no connect-budget flag), so `comb.connect` refuses
`timeout_ms` and `connect_timeout_ms` there: it returns `nil` and an
`unsupported` Error naming the ffi transport.

---

## API reference

### `comb.connect([opts])` → `Client | nil, Error`

Selects a transport and returns a connected `Client`. See "Custom options"
above for `opts`. `opts.backend` bypasses transport selection entirely
(advanced use / tests) with any table satisfying the backend interface
documented at the top of `beachcomber/client.lua`.

### `Client:transport()` → `"ffi" | "subprocess"`

### `Client:get(key [, opts])` → `value | nil, Error`

The value `comb get` prints for this key, cwd and env (`bc_get`): the
daemon holds the templates (`<provider>:template`, seeded from
configuration or `put`) and the client renders them. A plain field is its
cached value, a templated one is rendered, and a bare provider name is the
whole merged namespace; a miss is `nil` with no `Error`. The unrendered
halves are facets, read like any key: `p:template.x` is the template as
stored, `p:put.x` and `p:source.x` those layers' values. `key` is
`"provider"` or `"provider.field"`.

| `opts` field | Meaning |
|---|---|
| `cwd` | The directory the read addresses (`~` is home, a relative path is against this process's working directory). Default: the global entry. |
| `env` | The caller's environment for `env.*` refs. `nil` (the default) is the calling process's environment, as `comb` renders. |
| `force` | Re-execute every Source the read demands first (`comb get --force`). |
| `wait` | How long to wait for a run the read needs before answering what is stored (`comb get --wait`): a duration (`"0"`, `"50ms"`, `"2s"`) or `"forever"`; anything else is `"parse_error"`, on both transports. `nil` (the default): a stored record waits its Source's budget, a cold read waits for its run. |

### `Client:refresh(key [, opts])` → `true | nil, Error`

Re-execute the Sources `key` demands — every Source of the namespace for a
bare `provider`, the one Source owning the field for `provider.field`. A key
that names no Source (a `put`-created namespace, a facet key, an unknown
name, a field no Source owns) is an error. Over `ffi` this is `bc_refresh`;
over `subprocess` it is one `comb refresh <key>`. `opts.path` is the
directory; `nil` is the global entry.

### `Client:status()` → `table[] | nil, Error`

Cache rows (one per warm cache entry), with the fields
`libbeachcomber::protocol::CacheRow` defines: `provider`, `path`
(`nil` for the global entry), `source`, `field`, `value`, `age_ms`,
plus lifecycle fields. A status that cannot be read is
`nil, Error` on either transport, never an empty list.

### `Client:put(key, data [, opts])` → `true | nil, Error`

Write a static-data entry's value. `data = nil` (or `beachcomber.null`)
removes the node. `opts.ttl` (e.g. `"30s"`), `opts.path` (`nil`: the
global entry), `opts.persist` (keep the put across a daemon restart, best effort).

### `Client:hello()` → `table {protocol_version, daemon_version} | nil, Error`
### `Client:introspect(subject [, opts])` → `table {subject, daemon} or {subject, other} | nil, Error`

`opts.duration_secs` is the `procs` subject's only option. ffi transport
only — `Error{kind="unsupported"}` over subprocess.

### `Client:eval(template_str [, opts])` → `value | nil, Error`

Renders a template that is stored nowhere — the same value `comb eval`
prints (`bc_eval`), every ref resolved as it would be inside a stored
template, so `eval("{{ p.f }}")` and `get("p.f")` agree. `opts.cwd` and
`opts.env` as `Client:get` takes them (no `cwd` is the global entry). ffi
transport only.

`template_str` accepts a single `{{ expr }}` tag, or literal text/several
tags — the former keeps the expression's natural type, the latter is
always a string (a source with no tags at all is its literal text).

### `Client:watch(key [, opts])` → `WatchStream | nil, Error`

ffi transport only. Watches the key's *value* — what `Client:get`
answers for it — so a template edited under the watch fires it just as a
changed cache record does. `opts.cwd` and `opts.env` as `Client:get`
takes them.

`stream:next_event([timeout_ms])` → the event table `{data}` carrying the
value (`nil` data is a miss, not the end); `nil` with no Error once the
stream has ended (the daemon closed it, or it was cancelled); `nil, Error`
of kind `"timeout"` when `timeout_ms` ran out with no event; `nil, Error`
when the stream failed. No `timeout_ms` waits indefinitely, `0` polls.
`stream:cancel()` ends the stream (safe while another thread waits in
`next_event`); `stream:close()` cancels it and releases the handle; both
return `true`. `for event in stream:each() do … end` iterates it, and
raises the Error if the stream fails.

### `Client:session()` → `Session | nil, Error`

A session: one connection (`bc_session_*`) and a directory of its own.
Over `subprocess` there is no connection to keep, and the session remembers
its directory between spawns.

| Method | Answers |
|---|---|
| `s:set_context(path)` | `true`. Later calls that name no directory address `path`. |
| `s:get(key [, opts])` | As `Client:get`; no `opts.cwd` is the session's directory, else the global entry. |
| `s:put(key, data [, opts])` | As `Client:put`; no `opts.path` is the session's directory. |
| `s:refresh(key [, opts])` | As `Client:refresh`; no `opts.path` is the session's directory. |
| `s:close()` | `true`. |

### `Client:close()` → `true`

### `Error`

Every method returns its value or `nil, Error`; a miss is `nil` with no
Error, and a method that answers nothing (`put`, `refresh`,
`set_context`, `close`) returns `true`. An Error carries `kind`, a slug
from `Error.KINDS` (the table's: the ABI's `daemon_not_running`,
`connection_failed`, `io_error`, `parse_error`, `server_error`, `timeout`,
`busy`, `panic`, and this SDK's own
`library_not_found`, `library_symbol` and `unsupported`); `message`, the
ABI's message as it came; and `library_version`, the loaded library's
`bc_version()` (`nil` over `subprocess`, which loads none).
`tostring(err)` gives kind and message. Branch on `kind`.

---

## Discovery

### ffi transport: library discovery

The ffi transport finds `libbeachcomber` by the rule every SDK follows
([`docs/sdk-api.md`](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md#library-discovery),
"Library discovery"). The file is `libbeachcomber.dylib` on macOS and
`libbeachcomber.so` on every other platform, and the candidates are, in
order:

1. `$BEACHCOMBER_LIB`, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library. `opts.library_path` is the same thing
   for a caller that cannot set the environment (standard Lua has no
   `setenv`).
2. Beside `comb`: `<dir>/../lib/<file>`, where `<dir>` is the directory of
   the first executable `comb` on `$PATH`, its symlinks resolved. A Homebrew
   `comb` is a symlink into the Cellar, and the library sits in the Cellar
   beside it; a `.deb` or `.rpm` puts them in `/usr/bin` and `/usr/lib`.
3. The system loader, given the bare file name.

The first candidate that loads is used. When none does, the error is
`library_not_found`, naming every candidate tried and why each failed — a
missing library is a broken install, not a silent downgrade to `subprocess`.
Every symbol the ABI
declares is verified at load time, not on first use; a missing symbol is a
`library_symbol` Error naming the symbol, with the loaded library's
`bc_version()` as its `library_version`, so a version-skew install error is
diagnosable at a glance.

### subprocess transport: `comb` discovery

The `comb` binary is the first executable `comb` on `$PATH` (or
`opts.comb_bin`), the same one library discovery starts from. Standard
Lua has no `setenv`, so `opts.socket_path` (or the daemon's own discovery,
if unset) is threaded into each spawned `comb` invocation's command line
rather than the process environment.

---

## Module layout

```
beachcomber/
  init.lua               -- entry point, transport selection, comb.connect()
  client.lua              -- transport-agnostic Client and Session
  error.lua                -- Error (kind, message, library_version) and Error.KINDS
  watch_stream.lua         -- WatchStream wrapping a backend watch handle
  ffi.lua                  -- LuaJIT ffi: cdef, library discovery/load/symbol check
  ffi_backend.lua           -- backend implementation over ffi.lua
  subprocess_backend.lua    -- backend implementation shelling out to `comb`
  discovery.lua             -- `comb`-on-PATH lookup; ffi library candidate ordering
  json.lua                  -- minimal JSON encoder/decoder (no dependencies)
```

---

## Running tests

```sh
cd sdks/lua
luajit test/test_runner.lua   # exercises the ffi transport's supporting code
lua test/test_runner.lua      # PUC Lua — same suite; the ffi-only tests skip
```

Unit tests run against a mock backend (no live daemon required); the
subprocess backend's `get` tests spawn a `comb daemon` from `$COMB_BIN` or
the workspace's debug build, under both interpreters. For the full
protocol conformance suite against a real daemon:

```sh
TMPDIR=/tmp COMB_BIN=/path/to/comb luajit sdks/lua/conformance_runner.lua   # ffi
TMPDIR=/tmp COMB_BIN=/path/to/comb lua sdks/lua/conformance_runner.lua      # subprocess
```

---

## Neovim example

```lua
-- In your statusline plugin or lualine component:
local ok, comb = pcall(require, 'beachcomber')
if not ok then return '' end

local client = comb.connect()  -- always "ffi" inside Neovim (LuaJIT)
if not client then return '' end

vim.api.nvim_create_autocmd('VimLeavePre', {
  callback = function() client:close() end,
})

local function git_branch()
  local branch = client:get('git.branch', { cwd = vim.fn.getcwd() })
  if not branch then return '' end
  return ' ' .. branch
end
```

All ffi calls are synchronous, so `git_branch()` can be called directly
from a statusline evaluation without scheduling callbacks.

# beachcomber — Lua SDK

Lua client for the [beachcomber](https://github.com/NavistAu/beachcomber)
daemon. A binding over `libbeachcomber`'s C ABI
(`libbeachcomber-ffi/include/beachcomber.h`), not a hand-rolled socket
protocol.

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
surface for `hello` or `introspect`; `eval` is not wired over this tier;
`watch` was tried and rejected (see `beachcomber/subprocess_backend.lua`'s
header comment: piped through `io.popen`, the CLI's stdout buffering means a
`read("*l")` blocks forever even though the daemon already sent the event).
Those `Client` methods return `nil, err` with `err.kind == "unsupported"`
over `subprocess`; get/put/refresh/status/context all work over
either transport. `refresh` over `subprocess` is one `comb refresh <key>`,
the daemon's op itself, and its refusal (`nothing to refresh: <key> has no
sources`) is the error message. `get` over `subprocess` is one `comb get -f
json <key>`, so its answer is the resolver's — the rendered value, the
directory chain up to `$HOME`, `$HOME` as the global entry, force/wait, the
daemon's refusals. A spawned `comb` renders with its own environment, and
this process can only hand it whole, so two things differ from the ffi
tier: a `get` that passes `opts.env` is `err.kind == "unsupported"` (nothing
is spawned), and a `get` without one renders with this process's
environment, where the ffi tier's has no `env.*` at all. The CLI renders
with `exec()` on, so the `exec` option has no effect over `subprocess`; a
caller that must not spawn processes from a template, or that needs its own
`env`, needs the ffi transport.

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
client:refresh('git', '/my/repo')

-- default directory — applies to later calls made without one
client:set_context('/my/repo')
local branch2 = client:get('git.branch')

-- daemon cache status rows
local rows = client:status()
for _, row in ipairs(rows or {}) do
  print(row.provider, row.field, row.age_ms, row.stale)
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
  exec        = true,                                -- ffi transport: templates may call exec()
  library_path = '/opt/beachcomber/lib/libbeachcomber.dylib', -- ffi: override discovery
  comb_bin     = '/opt/beachcomber/bin/comb',                 -- subprocess: override discovery
})
```

The client options are passed to the library's `bc_client_new` as they are
given; the SDK has no defaults of its own, so an option left out is the
library's default.

| Option | Library default | Meaning |
|---|---|---|
| `socket_path` | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The daemon's socket. |
| `timeout_ms` | 100 | How long one read or write on the socket may take. 0 is refused. |
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
| `cwd` | The directory the read addresses (`~` is home, a relative path is against this process's working directory). Default: the `set_context()` path, else the global entry. |
| `env` | The caller's environment for `env.*` refs. `nil` (the default) means none: every `env.*` is absent. ffi transport only. |
| `force` | Re-execute every Source the read demands first (`comb get --force`). |
| `wait` | Re-execute a stale entry inline before answering (`comb get --wait`). |

### `Client:refresh(key [, path])` → `true | nil, Error`

Re-execute the Sources `key` demands — every Source of the namespace for a
bare `provider`, the one Source owning the field for `provider.field`. A key
that names no Source (a `put`-created namespace, a facet key, an unknown
name, a field no Source owns) is an error. Over `ffi` this is `bc_refresh`;
over `subprocess` it is one `comb refresh <key>`.

### `Client:set_context(path)` → `true`

Set the default path used by later calls that don't pass one explicitly.
Client-side (works identically over both transports).

### `Client:status()` → `table[] | nil, Error`

Cache rows (one per warm cache entry): `provider`, `field`, `path`,
`value`, `age_ms`, `stale`, plus lifecycle fields.

### `Client:put(key, data [, ttl [, path]])` → `true | false, Error`

Write a static-data entry's value. `data = nil` (or `beachcomber.null`) removes the node.

### `Client:hello()` → `table {protocol_version, daemon_version} | nil, Error`
### `Client:introspect(subject [, duration_secs])` → `table {subject, daemon, other} | nil, Error`

ffi transport only — `Error{kind="unsupported"}` over subprocess.

### `Client:eval(template_str [, opts])` → `value | nil, Error`

Renders a template that is stored nowhere — the same value `comb eval`
prints (`bc_eval`), every ref resolved as it would be inside a stored
template, so `eval("{{ p.f }}")` and `get("p.f")` agree. `opts.cwd`
(default: the context path, then `"."`), `opts.env` (the caller's
environment for `env.*` refs). ffi transport only.

`template_str` accepts a single `{{ expr }}` tag, or literal text/several
tags — the former keeps the expression's natural type, the latter is
always a string (a source with no tags at all is its literal text).

### `Client:watch(key [, opts])` → `WatchStream | nil, Error`

ffi transport only. Watches the key's *value* — what `Client:get`
answers for it — so a template edited under the watch fires it just as a
changed cache record does. `opts` is `Client:eval`'s: `opts.cwd`
(default: context path, then `"."`), `opts.env`.

`stream:next_event([timeout_ms])` → event table `{data}` carrying the
resolved value (`nil` for a miss), or `nil` on eof/cancelled, or `nil,
Error` on a real error; `-1` (default) blocks indefinitely, `0` polls, `>0`
waits that long. `stream:cancel()`, `stream:close()`, `stream:each()` for a
for-loop.

### `Client:session()` → session object | nil, Error

Advanced: a persistent connection with a directory of its own
(`bc_session_*`). Its `get(key, cwd, env, flags)` is `bc_session_get`, and
returns the backend's response table (`{ok, data, error}`); a `nil` cwd is
the session's directory. Most callers want `set_context()` instead — it
works over either transport by keeping the default path client-side. ffi
transport only.

### `Client:close()`

### `Error`

Every failing call returns `nil, Error` (or `false, Error` for
put). `Error.kind` is a machine-readable slug mirroring the ABI
envelope's `error.kind` (`"server_error"`, `"daemon_not_running"`,
`"unsupported"` for a capability the active transport lacks, ...);
`Error.message` is human-readable; `tostring(err)` gives both.

---

## Discovery

### ffi transport: library discovery

1. `$BEACHCOMBER_LIB`
2. `../lib/<platform name>` relative to the `comb` resolved on `$PATH`
   (the Homebrew-style `bin/` + `lib/` layout — `comb` and the library
   ship together, so the one beside the `comb` you'd actually run is the
   matching one)
3. the platform default dynamic-linker search path

Every candidate tried is remembered: if none load, the error names all of
them, in order — a missing library is a broken install, not a silent
downgrade to `subprocess` (see "Transport" above). Every symbol the ABI
declares is verified at load time, not on first use; a missing symbol's
error names the symbol and the loaded library's `bc_version()`, so a
version-skew install error is diagnosable at a glance.

### subprocess transport: `comb` discovery

The `comb` binary is found on `$PATH` (or via `opts.comb_bin`). Standard
Lua has no `setenv`, so `opts.socket_path` (or the daemon's own discovery,
if unset) is threaded into each spawned `comb` invocation's command line
rather than the process environment.

---

## Module layout

```
beachcomber/
  init.lua               -- entry point, transport selection, comb.connect()
  client.lua              -- transport-agnostic Client
  error.lua                -- idiomatic Error (kind + message)
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

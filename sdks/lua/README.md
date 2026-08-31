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
surface for `hello`, `introspect`, `resolve`, `eval`, or `watch` (the last
was tried and rejected — see `beachcomber/subprocess_backend.lua`'s header
comment: piped through `io.popen`, the CLI's stdout buffering means a
`read("*l")` blocks forever even though the daemon already sent the event).
Those `Client` methods return `nil, err` with `err.kind == "unsupported"`
over `subprocess`; get/put/put_null/refresh/status/context all work fully
over either transport.

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

-- get a single field
local result = client:get('git.branch', '/my/repo')
if result:is_hit() then
  print(result.data)    -- "main"
  print(result.age_ms)  -- 1234
  print(result.stale)   -- false
end

-- get a full provider (returns object)
local r = client:get('git', '/my/repo')
if r:is_hit() then
  print(r:get_str('branch'))  -- "main"
end

-- refresh (force recompute)
client:refresh('git', '/my/repo')

-- default path — applies to subsequent queries made without an explicit path
client:set_context('/my/repo')
local r2 = client:get('git.branch')

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
  socket_path = '/run/user/1000/beachcomber/sock',  -- ffi transport
  timeout_ms  = 2000,                                -- ffi transport
  autostart   = true,                                -- ffi transport
  library_path = '/opt/beachcomber/lib/libbeachcomber.dylib', -- ffi: override discovery
  comb_bin     = '/opt/beachcomber/bin/comb',                 -- subprocess: override discovery
})
```

---

## API reference

### `comb.connect([opts])` → `Client | nil, Error`

Selects a transport and returns a connected `Client`. See "Custom options"
above for `opts`. `opts.backend` bypasses transport selection entirely
(advanced use / tests) with any table satisfying the backend interface
documented at the top of `beachcomber/client.lua`.

### `Client:transport()` → `"ffi" | "subprocess"`

### `Client:get(key [, path])` → `Result | nil, Error`

Read a cached value. `key` is `"provider"` or `"provider.field"`. `path`
overrides `set_context()`'s default, if any.

### `Client:get_with_flags(key, path, force, wait)` → `Result | nil, Error`

Like `get`, with `bc_get`'s `BC_GET_FORCE` / `BC_GET_WAIT` flags.

### `Client:refresh(key [, path])` → `true | nil, Error`

Force the daemon to recompute `key`.

### `Client:set_context(path)` → `true`

Set the default path used by later calls that don't pass one explicitly.
Client-side (works identically over both transports).

### `Client:status()` → `table[] | nil, Error`

Cache rows (one per warm cache entry): `provider`, `field`, `path`,
`value`, `age_ms`, `stale`, plus lifecycle fields.

### `Client:put(key, data [, ttl [, path]])` → `true | false, Error`
### `Client:put_null(key [, path])` → `true | false, Error`

Write to / clear a virtual provider's cached value.

### `Client:hello()` → `table {protocol_version, daemon_version} | nil, Error`
### `Client:introspect(subject [, duration_secs])` → `table {subject, daemon, other} | nil, Error`

ffi transport only — `Error{kind="unsupported"}` over subprocess.

### `Client:resolve(key [, opts])` → `value | nil, Error`
### `Client:eval(template_str [, opts])` → `value | nil, Error`

Client-side field/path-expression resolution (`bc_resolve`/`bc_eval`) —
the same evaluator `comb get`'s resolution layer uses, exposed so a caller
never has to reimplement it. `opts.cwd` (default: the context path, then
`"."`), `opts.env`, `opts.virtual` (expression overrides, keyed
`"provider.field"` or a bare provider name — see
`tests/conformance/README.md`'s `resolve` fixture shape). ffi transport
only.

`template_str` accepts a bare expression, a single `{{ expr }}` tag, or
literal text/several tags — the first two keep the expression's natural
type, the third is always a string.

### `Client:watch(key [, path])` → `WatchStream | nil, Error`

ffi transport only. `stream:next_event([timeout_ms])` → event table
`{data, age_ms, stale}`, or `nil` on eof/cancelled, or `nil, Error` on a
real error; `-1` (default) blocks indefinitely, `0` polls, `>0` waits that
long. `stream:cancel()`, `stream:close()`, `stream:each()` for a for-loop.

### `Client:session()` → session object | nil, Error

Advanced: a persistent connection with true server-side context
(`bc_session_*`). Most callers want `set_context()` instead — it works
over either transport by keeping the default path client-side. ffi
transport only.

### `Client:close()`

### `Result`

| Field | Type | Description |
|---|---|---|
| `data` | any | Cached value; `nil` on a miss |
| `age_ms` | number | Age of the cached value in milliseconds |
| `stale` | boolean | True when past TTL but no fresh value yet |

| Method | Returns | Description |
|---|---|---|
| `result:is_hit()` | boolean | True when `data ~= nil` |
| `result:get_str(field)` | string\|nil, error | Get a string field from object data |

### `Error`

Every failing call returns `nil, Error` (or `false, Error` for
put/put_null). `Error.kind` is a machine-readable slug mirroring the ABI
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
  client.lua              -- transport-agnostic Client and Result
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
lua test/test_runner.lua      # PUC Lua — same suite, mock-backend tests only
```

Unit tests run against a mock backend (no live daemon required). For the
full protocol conformance suite against a real daemon:

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
  local result, err = client:get('git.branch', vim.fn.getcwd())
  if not result or not result:is_hit() then return '' end
  return ' ' .. result.data
end
```

All ffi calls are synchronous, so `git_branch()` can be called directly
from a statusline evaluation without scheduling callbacks.

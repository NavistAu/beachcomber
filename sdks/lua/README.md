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
surface for `hello` or `introspect`; `resolve` and `eval` are not wired over
this tier (`comb get`/`comb eval` resolve through the same library, so a
shell-out would be faithful — nobody has needed it); `watch` was tried and rejected (see `beachcomber/subprocess_backend.lua`'s
header comment: piped through `io.popen`, the CLI's stdout buffering means a
`read("*l")` blocks forever even though the daemon already sent the event).
Those `Client` methods return `nil, err` with `err.kind == "unsupported"`
over `subprocess`; get/put/refresh/status/context all work over
either transport. `refresh` over `subprocess` is one `comb refresh <key>`,
the daemon's op itself, and its refusal (`nothing to refresh: <key> has no
sources`) is the error message. `get` over `subprocess` is one `comb get -f
json <key> <key>:age <key>:stale`, so its answer is the resolver's — the
directory chain up to
`$HOME`, `$HOME` as the global entry, metadata suffixes, force/wait, the
daemon's refusals — with one divergence: `comb get` *resolves*, so a
template-backed key returns its **rendered** value where the ffi tier
returns the raw template half. The template half of a `get`
(`Result.template` and `Result.rest`, see `Result` below) is therefore
ffi-tier only: over `subprocess` both are always `nil`, and a template-only
node is a hit holding the rendered value (age 0, not stale — it has no
cached freshness of its own). The CLI renders with `exec()` on, so the
`exec` option has no effect over `subprocess`; a caller that must not spawn
processes from a template, or that needs the raw template half, needs the
ffi transport.

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
  exec        = true,                                -- ffi transport: templates may call exec()
  library_path = '/opt/beachcomber/lib/libbeachcomber.dylib', -- ffi: override discovery
  comb_bin     = '/opt/beachcomber/bin/comb',                 -- subprocess: override discovery
})
```

`exec` is off by default: a template that calls `exec()` on a client
without it fails with `exec() is disabled in this context`.

---

## API reference

### `comb.connect([opts])` → `Client | nil, Error`

Selects a transport and returns a connected `Client`. See "Custom options"
above for `opts`. `opts.backend` bypasses transport selection entirely
(advanced use / tests) with any table satisfying the backend interface
documented at the top of `beachcomber/client.lua`.

### `Client:transport()` → `"ffi" | "subprocess"`

### `Client:get(key [, path])` → `Result | nil, Error`

Read a node raw — both halves, nothing rendered (see `Result`). `key` is
`"provider"` or `"provider.field"`. `path` overrides `set_context()`'s
default, if any.

### `Client:get_with_flags(key, path, force, wait)` → `Result | nil, Error`

Like `get`, with `bc_get`'s `BC_GET_FORCE` / `BC_GET_WAIT` flags.

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

### `Client:resolve(key [, opts])` → `value | nil, Error`
### `Client:eval(template_str [, opts])` → `value | nil, Error`

`resolve` returns the same value `comb get` returns for this key, cwd and
env (`bc_resolve`): the daemon holds the templates (`<provider>:template`,
seeded from configuration or `put`) and the client renders them. A plain
field is its cached value, a templated one is rendered, and a bare
provider name is the whole merged namespace; a miss is `nil` with no
`Error`. `eval` renders a template that is stored nowhere — the same value
`comb eval` prints (`bc_eval`). `opts.cwd` (default: the context path,
then `"."`), `opts.env` (the caller's environment for `env.*` refs). ffi
transport only.

`template_str` accepts a single `{{ expr }}` tag, or literal text/several
tags — the former keeps the expression's natural type, the latter is
always a string (a source with no tags at all is its literal text).

### `Client:watch(key [, opts])` → `WatchStream | nil, Error`

ffi transport only. Watches the key's *value* — what `Client:resolve`
answers for it — so a template edited under the watch fires it just as a
changed cache record does. `opts` is `Client:resolve`'s: `opts.cwd`
(default: context path, then `"."`), `opts.env`.

`stream:next_event([timeout_ms])` → event table `{data}` carrying the
resolved value (`nil` for a miss), or `nil` on eof/cancelled, or `nil,
Error` on a real error; `-1` (default) blocks indefinitely, `0` polls, `>0`
waits that long. `stream:cancel()`, `stream:close()`, `stream:each()` for a
for-loop.

### `Client:session()` → session object | nil, Error

Advanced: a persistent connection with true server-side context
(`bc_session_*`). Most callers want `set_context()` instead — it works
over either transport by keeping the default path client-side. ffi
transport only.

### `Client:close()`

### `Result`

`get` returns the node as the daemon holds it — both halves, nothing
rendered. A node may hold a cached value (`data`), a template (`template`),
or both. A template-only result is neither a hit nor a miss: it is an
address whose value `resolve` renders — `get` is the raw half,
`resolve(key, {cwd, env})` is how you get the value.

| Field | Type | Description |
|---|---|---|
| `data` | any | Cached value; `nil` when nothing is cached |
| `age_ms` | number | Age of the cached value in milliseconds |
| `stale` | boolean | True when past TTL but no fresh value yet |
| `template` | string\|table\|nil | The template in effect at the address, unrendered: a string for a leaf, a table for a group; `nil` when none is in effect. Always `nil` over `subprocess`. |
| `rest` | string\|nil | The address left over when the template walk stopped at a string — `get("p.t.x")` where `p.t` is a template string gives the template of `p.t` and `rest == "x"`. Always `nil` over `subprocess`. |

| Method | Returns | Description |
|---|---|---|
| `result:is_hit()` | boolean | True when `data ~= nil` |
| `result:is_miss()` | boolean | True when `data == nil` and `template == nil` |
| `result:get_str(field)` | string\|nil, error | Get a string field from object data |

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
  local result, err = client:get('git.branch', vim.fn.getcwd())
  if not result or not result:is_hit() then return '' end
  return ' ' .. result.data
end
```

All ffi calls are synchronous, so `git_branch()` can be called directly
from a statusline evaluation without scheduling callbacks.

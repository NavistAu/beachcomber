---
sidebar_position: 6
---

# Lua SDK

Lua client for the beachcomber daemon. A binding over `libbeachcomber`'s C ABI, not a hand-rolled socket protocol. Published on [LuaRocks](https://luarocks.org/modules/navist/libbeachcomber).

Two transports, selected automatically by `comb.connect()`: under LuaJIT (including Neovim, which always ships LuaJIT), `ffi` calls straight into the shared library (~0.3ms/call); under PUC Lua 5.1–5.4, which has no `ffi`, the SDK falls back to shelling out to the `comb` binary (~5ms/call). Call `client:transport()` to see which one is active.

## Requirements

- Lua 5.1+ or LuaJIT (Neovim-compatible)
- **LuaJIT (incl. Neovim):** no extra dependencies — `ffi` binds directly to `libbeachcomber.{so,dylib}`
- **PUC Lua 5.1–5.4:** no extra dependencies — subprocess fallback needs only a `comb` binary on `$PATH`

## Installation

### LuaRocks

```sh
luarocks install --local libbeachcomber
```

### Manual (Neovim)

Add `sdks/lua` to your `package.path`, or copy the `beachcomber/` directory somewhere on your `runtimepath`.

## Quick start

```lua
local comb = require('beachcomber')

-- Connect (LuaJIT: ffi into libbeachcomber; PUC Lua: comb subprocess)
local client, err = comb.connect()
if not client then
  error("beachcomber: " .. err)
end

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

-- persistent context — path applies to all subsequent queries
client:set_context('/my/repo')
local r2 = client:get('git.branch')  -- uses context path

-- daemon cache status rows
local rows = client:status()
for _, row in ipairs(rows or {}) do
  print(row.provider, row.field, row.age_ms, row.stale)
end

client:close()
```

### Custom socket path

```lua
local client = comb.connect({ socket_path = '/run/user/1000/beachcomber/sock' })
```

## API reference

### `comb.connect([opts])` → `Client | nil, error`

Connect to the daemon and return a `Client`.

| Option | Type | Description |
|---|---|---|
| `socket_path` | string | Override automatic socket discovery |
| `exec` | boolean | `ffi` transport: let templates this client resolves call `exec()`. Off by default — a template that calls it then fails with `exec() is disabled in this context` |
| `backend` | module | Override the socket backend (advanced) |

Returns `nil, error_message` on failure.

### `Client:get(key [, path])` → `Result | nil, error`

Read a node raw — both halves, nothing rendered (see `Result`). `key` is `"provider"` or `"provider.field"`. `path` overrides any connection context.

### `Client:refresh(key [, path])` → `true | nil, error`

Re-execute the Sources `key` demands — every Source of the namespace for a bare `provider`, the one Source owning the field for `provider.field`. A key that names no Source (a `put`-created namespace, a facet key, an unknown name, a field no Source owns) is an error. Over `subprocess` this is one `comb refresh <key>`; the daemon's refusal comes back as `err.kind == "server_error"` in the daemon's words.

### `Client:set_context(path)` → `true | nil, error`

Set the default working-directory path for this connection.

### `Client:status()` → `table[] | nil, error`

Return typed cache rows (one per warm cache entry) as an array of tables with `provider`, `field`, `value`, `age_ms`, and `stale` keys.

### `Client:hello()` → `table | nil, error`

Handshake — returns a table with `daemon_version` and `protocol_version` fields.

### `Client:put(key, data, ttl, path)` → `true | nil, error`

Write a value into the cache as static data. Pass `nil` (or `beachcomber.null`) for `data` to remove the node.

### `Client:introspect(subject, duration_secs)` → `table | nil, error`

Inspect a daemon subsystem. `subject` is a string: `"daemon"`, `"providers"`, `"config"`, `"cache"`, `"lifecycle"`, `"watches"`, `"timers"`, `"demand"`, or `"procs"`.

### `Client:watch(key [, opts])` → `WatchStream | nil, error`

Subscribe to a key's *resolved* value — what `Client:resolve` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `opts.cwd` (default: the context path, then `"."`), `opts.env` — same shape as `resolve`'s opts. Returns a `WatchStream` — call `:next()` to receive `WatchEvent` tables (whose `data` carries no age or staleness) and `:close()` when done. `ffi` transport only.

### `Client:get_with_flags(key, path, force, wait)` → `Result | nil, error`

Read a cached value with optional `force` (recompute) or `wait` (block for fresh value) flags.

### `Client:resolve(key [, opts])` → `value | nil, error`
### `Client:eval(template_str [, opts])` → `value | nil, error`

`resolve` returns the same value `comb get` returns for this key, cwd and env: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace; a miss is `nil` with no error. `eval` renders a template that is stored nowhere — the same value `comb eval` prints. `opts.cwd` (default: the context path, then `"."`), `opts.env` (the caller's environment for `env.*` refs). A template that calls `exec()` needs `exec = true` on the connect. `ffi` transport only.

### `Client:close()`

Close the underlying socket.

### `Result`

`get` returns the node as the daemon holds it — both halves, nothing rendered. A node may hold a cached value (`data`), a template (`template`), or both. A template-only result is neither a hit nor a miss: it is an address whose value `resolve` renders — `get` is the raw half, `resolve(key, {cwd, env})` is how you get the value. The template half is `ffi` transport only: over `subprocess`, `template` and `rest` are always `nil` and a template-only node reads as a miss.

| Field | Type | Description |
|---|---|---|
| `data` | any | Cached value; `nil` when nothing is cached |
| `age_ms` | number | Age of the cached value in milliseconds |
| `stale` | boolean | True when past TTL but no fresh value yet |
| `template` | string\|table\|nil | The template in effect at the address, unrendered: a string for a leaf, a table for a group; `nil` when none is in effect |
| `rest` | string\|nil | The address left over when the template walk stopped at a string — `get("p.t.x")` where `p.t` is a template string gives the template of `p.t` and `rest == "x"` |

| Method | Returns | Description |
|---|---|---|
| `result:is_hit()` | boolean | True when `data ~= nil` |
| `result:is_miss()` | boolean | True when `data == nil` and `template == nil` |
| `result:get_str(field)` | string\|nil, error | Get a string field from object data |

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Socket path discovery

The SDK looks for the daemon socket in this order:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

## Neovim example

```lua
-- In your statusline plugin or lualine component:
local ok, comb = pcall(require, 'beachcomber')
if not ok then return '' end

local client = comb.connect()
if not client then return '' end

-- Keep a persistent client across calls for best performance
vim.api.nvim_create_autocmd('VimLeavePre', {
  callback = function() client:close() end,
})

local function git_branch()
  local result, err = client:get('git.branch', vim.fn.getcwd())
  if not result or not result:is_hit() then return '' end
  return ' ' .. result.data
end
```

Every `ffi` call is synchronous, so `git_branch()` can be called directly from a statusline evaluation without scheduling callbacks.

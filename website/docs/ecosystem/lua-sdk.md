---
sidebar_position: 6
---

# Lua SDK

Lua client for the beachcomber daemon. Communicates over a Unix domain socket using newline-delimited JSON. Published on [LuaRocks](https://luarocks.org/modules/navist/libbeachcomber).

Designed for Neovim plugin authors (via `vim.uv`) but also works in standalone Lua scripts via luasocket.

## Requirements

- Lua 5.1+ or LuaJIT (Neovim-compatible)
- **Inside Neovim:** no extra dependencies (`vim.uv` / `vim.loop` used automatically)
- **Outside Neovim:** [luasocket](https://github.com/lunarmodules/luasocket) 3.0+

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

-- Connect (auto-detects vim.uv or luasocket)
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
| `backend` | module | Override the socket backend (advanced) |

Returns `nil, error_message` on failure.

### `Client:get(key [, path])` → `Result | nil, error`

Read a cached value. `key` is `"provider"` or `"provider.field"`. `path` overrides any connection context.

### `Client:refresh(key [, path])` → `true | nil, error`

Force the daemon to recompute `key`.

### `Client:set_context(path)` → `true | nil, error`

Set the default working-directory path for this connection.

### `Client:status()` → `table[] | nil, error`

Return typed cache rows (one per warm cache entry) as an array of tables with `provider`, `field`, `value`, `age_ms`, and `stale` keys.

### `Client:hello()` → `table | nil, error`

Handshake — returns a table with `daemon_version` and `protocol_version` fields.

### `Client:put(key, data, ttl, path)` → `true | nil, error`

Write a value into the cache as a virtual provider. Pass `nil` for `data` to clear the entry.

### `Client:put_null(key, path)` → `true | nil, error`

Clear a virtual provider entry (shorthand for `put` with `data=nil`).

### `Client:introspect(subject, duration_secs)` → `table | nil, error`

Inspect a daemon subsystem. `subject` is a string: `"daemon"`, `"providers"`, `"config"`, `"cache"`, `"lifecycle"`, or `"watches"`.

### `Client:watch(key, path)` → `WatchStream | nil, error`

Subscribe to live cache updates. Returns a `WatchStream` — call `:next()` to receive `WatchEvent` tables and `:close()` when done.

### `Client:get_with_flags(key, path, force, wait)` → `Result | nil, error`

Read a cached value with optional `force` (recompute) or `wait` (block for fresh value) flags.

### `Client:close()`

Close the underlying socket.

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

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Socket path discovery

The SDK looks for the daemon socket in this order:

1. Config file override (if set in `~/.config/beachcomber/config.toml`)
2. `$BEACHCOMBER_SOCKET` environment variable
3. `$XDG_RUNTIME_DIR/beachcomber/sock` (if `XDG_RUNTIME_DIR` is set and the path exists)
4. `/tmp/beachcomber-<uid>/sock`

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

All socket I/O in the `vim.uv` backend is synchronous so `git_branch()` can be called directly from a statusline evaluation without scheduling callbacks.

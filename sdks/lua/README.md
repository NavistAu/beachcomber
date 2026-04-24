# beachcomber — Lua SDK

Lua client for the [beachcomber](https://github.com/NavistAu/beachcomber)
daemon. Communicates over a Unix domain socket using newline-delimited JSON.

Designed for Neovim plugin authors (via `vim.uv`) but also works in standalone
Lua scripts via luasocket.

---

## Requirements

- Lua 5.1+ or LuaJIT (Neovim-compatible)
- **Inside Neovim:** no extra dependencies (`vim.uv` / `vim.loop` used automatically)
- **Outside Neovim:** [luasocket](https://github.com/lunarmodules/luasocket) 3.0+

---

## Installation

### LuaRocks

```sh
luarocks install --local beachcomber
```

### Manual (Neovim)

Add `sdks/lua` to your `package.path`, or copy the `beachcomber/` directory
somewhere on your `runtimepath`.

---

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

---

## API reference

### `comb.connect([opts])` → `Client | nil, error`

Connect to the daemon and return a `Client`.

| Option | Type | Description |
|---|---|---|
| `socket_path` | string | Override automatic socket discovery |
| `backend` | module | Override the socket backend (advanced) |

Returns `nil, error_message` on failure.

### `Client:get(key [, path])` → `Result | nil, error`

Read a cached value. `key` is `"provider"` or `"provider.field"`.
`path` overrides any connection context.

### `Client:refresh(key [, path])` → `true | nil, error`

Force the daemon to recompute `key`.

### `Client:set_context(path)` → `true | nil, error`

Set the default working-directory path for this connection.

### `Client:status()` → `table[] | nil, error`

Return typed cache rows (one per warm cache entry). Each row has `provider`, `field`, `value`, `age_ms`, and `stale` keys.

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

---

## Socket path discovery

The SDK looks for the daemon socket in this order:

1. `$XDG_RUNTIME_DIR/beachcomber/sock` (if `XDG_RUNTIME_DIR` is set and the path exists)
2. `$TMPDIR/beachcomber-<uid>/sock`
3. `/tmp/beachcomber-<uid>/sock`

---

## Module layout

```
beachcomber/
  init.lua              -- entry point, backend detection, comb.connect()
  client.lua            -- Client and Result types
  discovery.lua         -- socket path discovery
  json.lua              -- minimal JSON encoder/decoder (no dependencies)
  socket_luasocket.lua  -- luasocket backend
  socket_vim.lua        -- vim.uv / vim.loop backend
```

---

## Running tests

```sh
cd sdks/lua
lua test/test_runner.lua
```

The mock Unix socket server tests require luasocket. They are skipped
automatically when luasocket is not available.

---

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

All socket I/O in the `vim.uv` backend is synchronous so `git_branch()` can
be called directly from a statusline evaluation without scheduling callbacks.

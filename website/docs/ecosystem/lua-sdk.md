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

-- get a single field: what `comb get git.branch` prints there, nil on a miss
local branch = client:get('git.branch', { cwd = '/my/repo' })
if branch then print(branch) end  -- "main"

-- get a full provider (returns a table)
local git = client:get('git', { cwd = '/my/repo' })
if git then print(git.branch) end  -- "main"

-- the template itself, unrendered: a facet, read like any key
local tpl = client:get('git:template.branch')

-- refresh (force recompute)
client:refresh('git', '/my/repo')

-- persistent context — the directory of every later call that names none
client:set_context('/my/repo')
local branch2 = client:get('git.branch')  -- reads at the context path

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
| `exec` | boolean | `ffi` transport: let templates this client renders call `exec()`. Off by default — a template that calls it then fails with `exec() is disabled in this context` |
| `backend` | module | Override the socket backend (advanced) |

Returns `nil, error_message` on failure.

### `Client:get(key [, opts])` → `value | nil, error`

The value `comb get` prints for this key, cwd and env: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace; a miss is `nil` with no error. The unrendered halves are facets, read like any key: `p:template.x` is the template as stored, `p:put.x` and `p:source.x` those layers' values. `key` is `"provider"` or `"provider.field"`.

`opts.cwd` is the directory the read addresses (default: the context path, else the global entry); `opts.env` is the caller's environment for `env.*` refs (default: none, so every `env.*` is absent); `opts.force` re-executes every Source the read demands first and `opts.wait` re-executes a stale entry inline, as `comb get --force`/`--wait` do. A template that calls `exec()` needs `exec = true` on the connect. Over `subprocess` a spawned `comb` renders with its own environment: `opts.env` is an `"unsupported"` error there, and a `get` without it sees this process's environment.

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

Subscribe to a key's *resolved* value — what `Client:get` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `opts.cwd` (default: the context path, then `"."`), `opts.env` — same shape as `eval`'s opts. Returns a `WatchStream` — call `:next()` to receive `WatchEvent` tables (whose `data` carries no age or staleness) and `:close()` when done. `ffi` transport only.

### `Client:eval(template_str [, opts])` → `value | nil, error`

Render a template that is stored nowhere — the same value `comb eval` prints, every ref resolved as it would be inside a stored template, so `eval("{{ p.f }}")` and `get("p.f")` agree. `opts.cwd` (default: the context path, then `"."`), `opts.env` (the caller's environment for `env.*` refs). `ffi` transport only.

### `Client:close()`

Close the underlying socket.

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
  local branch = client:get('git.branch', { cwd = vim.fn.getcwd() })
  if not branch then return '' end
  return ' ' .. branch
end
```

Every `ffi` call is synchronous, so `git_branch()` can be called directly from a statusline evaluation without scheduling callbacks.

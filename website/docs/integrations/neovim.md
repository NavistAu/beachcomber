---
sidebar_position: 10
---

# Neovim

## Introduction

Neovim statuslines re-evaluate on every redraw. Traditional approaches — running `git branch`, parsing `kubectl config current-context`, or reading battery files — add latency on each redraw cycle. beachcomber eliminates that cost: cached data is returned in microseconds over a persistent Unix socket connection.

The Lua SDK auto-detects neovim and uses `vim.uv` (neovim's built-in libuv bindings) for non-blocking socket access. No external dependencies are required inside neovim. Outside neovim, the SDK falls back to luasocket if available, or shells out to `comb` as a last resort.

## Prerequisites

- The beachcomber daemon must be running. Verify with `comb s` from a terminal.
- The Lua SDK must be available in neovim's Lua path. Install via LuaRocks:

```sh
luarocks install libbeachcomber
```

Or place the SDK's `beachcomber.lua` (or `beachcomber/` directory) somewhere on `package.path` that neovim can find — for example, inside your Neovim config under `lua/`.

## Without the SDK

If you do not want to install the Lua SDK, you can call the `comb` binary directly using `vim.fn.system()`. This forks a process on each call, but because `comb g` returns in under 1ms it is still fast enough for statusline use:

```lua
-- lua/beachcomber-status.lua (no SDK required)
-- comb g returns plain text by default. g = get, no suffix needed.
local M = {}

local function comb_get(key, path)
    local cmd = path
        and string.format('comb g %s %s', key, vim.fn.shellescape(path))
        or  string.format('comb g %s', key)
    local result = vim.fn.system(cmd)
    if vim.v.shell_error ~= 0 or result == '' then
        return ''
    end
    return vim.trim(result)
end

function M.git_branch() return comb_get('git.branch', vim.fn.getcwd()) end
function M.git_dirty()
    return comb_get('git.dirty', vim.fn.getcwd()) == 'true' and '*' or ''
end
function M.battery()
    local pct = comb_get('battery.percent')
    return pct ~= '' and pct .. '%' or ''
end
function M.kube() return comb_get('kubecontext.context') end

return M
```

This module has the same interface as the SDK-based version below, so lualine and heirline examples work with either.

## With the Lua SDK

The SDK uses `vim.uv` for non-blocking socket access — no process forking. This is the recommended approach if you can install the SDK.

The SDK exposes a single `connect()` call that returns a persistent client. Call `client:get(key, path)` to retrieve a value. The second argument is the working directory path and is required for providers like `git` that are path-scoped.

Create a helper module so the same client is reused across your config:

```lua
-- lua/beachcomber-status.lua
local comb = require('beachcomber')
local client = comb.connect()

local M = {}

function M.git_branch()
    local result = client:get('git.branch', vim.fn.getcwd())
    if result and result:is_hit() then
        return result.data
    end
    return ''
end

function M.git_dirty()
    local result = client:get('git.dirty', vim.fn.getcwd())
    if result and result:is_hit() and result.data == true then
        return '*'
    end
    return ''
end

function M.battery()
    local result = client:get('battery.percent')
    if result and result:is_hit() then
        return result.data .. '%'
    end
    return ''
end

function M.kube()
    local result = client:get('kubecontext.context')
    if result and result:is_hit() then
        return result.data
    end
    return ''
end

return M
```

To use it with the built-in statusline, set `vim.o.statusline` to call your module functions. Because `vim.o.statusline` evaluates `%{}` expressions, use a small wrapper via `statusline` option with `%!` or set it from a function:

```lua
-- In init.lua or a status module
local bc = require('beachcomber-status')

local function build_statusline()
    local branch = bc.git_branch()
    local dirty = bc.git_dirty()
    local kube = bc.kube()
    local bat = bc.battery()

    local left = ''
    if branch ~= '' then
        left = ' ' .. branch .. dirty .. ' '
    end

    local right = ''
    if kube ~= '' then right = right .. ' ' .. kube end
    if bat ~= '' then right = right .. '  ' .. bat end

    return left .. '%=' .. right
end

vim.o.statusline = '%!v:lua.require("beachcomber-status").statusline()'

-- Expose the function at a module level so %! can reach it
local bc = require('beachcomber-status')
bc.statusline = build_statusline
```

## lualine.nvim

lualine accepts plain Lua functions as components. Any function that returns a string can be used directly. Pass the helper module functions as component values:

```lua
local bc = require('beachcomber-status')

require('lualine').setup({
    sections = {
        lualine_a = { 'mode' },
        lualine_b = {
            { bc.git_branch, icon = '' },
            { bc.git_dirty, color = { fg = '#ff6666' } },
        },
        lualine_c = { 'filename' },
        lualine_x = {
            { bc.kube, icon = '☸', cond = function() return bc.kube() ~= '' end },
        },
        lualine_y = { 'filetype' },
        lualine_z = {
            { bc.battery, icon = '', cond = function() return bc.battery() ~= '' end },
        },
    },
})
```

lualine calls each component function on every statusline redraw. Because beachcomber returns cached data over a persistent Unix socket connection, this is effectively free — no process spawning, no filesystem reads on the hot path.

The `cond` field suppresses a component entirely when its value is empty, which avoids rendering a bare icon with no content.

## heirline.nvim

heirline is lower-level than lualine. Components are Lua tables with `provider`, `condition`, and `hl` fields. The `provider` function returns the string to render; `condition` controls whether the component is included at all.

```lua
local bc = require('beachcomber-status')

local GitBranch = {
    condition = function() return bc.git_branch() ~= '' end,
    provider = function() return ' ' .. bc.git_branch() .. bc.git_dirty() .. ' ' end,
    hl = { fg = '#7aa2f7', bold = true },
}

local KubeContext = {
    condition = function() return bc.kube() ~= '' end,
    provider = function() return '☸ ' .. bc.kube() .. ' ' end,
    hl = { fg = '#7dcfff' },
}

local Battery = {
    condition = function() return bc.battery() ~= '' end,
    provider = function() return ' ' .. bc.battery() .. ' ' end,
    hl = { fg = '#9ece6a' },
}

require('heirline').setup({
    statusline = {
        GitBranch,
        { provider = '%=' },  -- center spacer
        KubeContext,
        Battery,
    },
})
```

Note that `condition` and `provider` are both called on each redraw cycle. Calling `bc.git_branch()` twice per component (once in `condition`, once in `provider`) is fine — the SDK returns cached data from memory on the second call within the same redraw.

If you want to avoid the double call, cache the value locally inside a surrounding component table using heirline's `init` hook:

```lua
local GitBranch = {
    init = function(self)
        self.branch = bc.git_branch()
        self.dirty = bc.git_dirty()
    end,
    condition = function(self) return self.branch ~= '' end,
    provider = function(self) return ' ' .. self.branch .. self.dirty .. ' ' end,
    hl = { fg = '#7aa2f7', bold = true },
}
```

## Troubleshooting

- **SDK not found:** if `require('beachcomber')` fails, the SDK is not on neovim's `package.path`. Check `:lua print(package.path)` and ensure the SDK's location is included.
- **Verify from neovim command line:** `:lua print(require('beachcomber').connect():get('git.branch', vim.fn.getcwd()).data)` should print the branch name. If it prints `nil`, the daemon is not running or the socket cannot be found.
- **Without the SDK:** the `vim.fn.system()` approach has no dependency to troubleshoot — if `comb g git.branch .` works in your terminal, it works in neovim.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.

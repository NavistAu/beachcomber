---
sidebar_position: 4
---

# Neovim Integration

The `beachcomber` Lua SDK auto-detects neovim and uses `vim.uv` for zero-dependency socket access:

```lua
-- In your statusline plugin or init.lua
local comb = require('beachcomber')
local client = comb.connect()

local function git_branch()
    local cwd = vim.fn.getcwd()
    local result = client:get('git.branch', cwd)
    if result and result:is_hit() then
        return ' ' .. result.data
    end
    return ''
end
```

Outside neovim, the SDK falls back to luasocket if available, or shells out to `comb` as a last resort.

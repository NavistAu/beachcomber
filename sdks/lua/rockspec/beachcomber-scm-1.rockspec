package = "beachcomber"
version = "scm-1"

source = {
  url = "git+https://github.com/jhogendorn/beachcomber.git",
  dir = "sdks/lua",
}

description = {
  summary  = "Lua client SDK for the beachcomber daemon",
  detailed = [[
    Client library for the beachcomber Unix-socket daemon.
    Provides synchronous get/poke/list/status operations with automatic
    backend detection: vim.uv inside Neovim, luasocket everywhere else.
    Ships a minimal JSON encoder/decoder — no external JSON dependency.
  ]],
  homepage = "https://github.com/jhogendorn/beachcomber",
  license  = "MIT",
}

dependencies = {
  "lua >= 5.1",
  -- luasocket is optional: only required outside Neovim
  -- "luasocket >= 3.0",
}

build = {
  type    = "builtin",
  modules = {
    ["beachcomber"]                  = "beachcomber/init.lua",
    ["beachcomber.client"]           = "beachcomber/client.lua",
    ["beachcomber.discovery"]        = "beachcomber/discovery.lua",
    ["beachcomber.json"]             = "beachcomber/json.lua",
    ["beachcomber.socket_luasocket"] = "beachcomber/socket_luasocket.lua",
    ["beachcomber.socket_vim"]       = "beachcomber/socket_vim.lua",
  },
}

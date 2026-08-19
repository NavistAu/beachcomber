package = "libbeachcomber"
version = "scm-1"

source = {
  url = "git+https://github.com/NavistAu/beachcomber.git",
  dir = "sdks/lua",
}

description = {
  summary  = "Lua client SDK for the beachcomber daemon",
  detailed = [[
    Client library for the beachcomber daemon, binding libbeachcomber's C
    ABI. LuaJIT calls the cdylib directly via `ffi` (~0.3ms/call); PUC Lua
    falls back to shelling out to `comb` (~5ms/call, the sanctioned
    fallback for interpreters with no ffi). Client:transport() reports
    which one is active. Ships a minimal JSON encoder/decoder — no
    external dependency either way.
  ]],
  homepage = "https://github.com/NavistAu/beachcomber",
  license  = "MIT",
}

dependencies = {
  "lua >= 5.1",
}

build = {
  type    = "builtin",
  modules = {
    ["libbeachcomber"]                     = "beachcomber/init.lua",
    ["libbeachcomber.client"]              = "beachcomber/client.lua",
    ["libbeachcomber.discovery"]           = "beachcomber/discovery.lua",
    ["libbeachcomber.error"]               = "beachcomber/error.lua",
    ["libbeachcomber.ffi"]                 = "beachcomber/ffi.lua",
    ["libbeachcomber.ffi_backend"]         = "beachcomber/ffi_backend.lua",
    ["libbeachcomber.subprocess_backend"]  = "beachcomber/subprocess_backend.lua",
    ["libbeachcomber.json"]                = "beachcomber/json.lua",
    ["libbeachcomber.watch_stream"]        = "beachcomber/watch_stream.lua",
  },
}

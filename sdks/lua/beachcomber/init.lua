--- beachcomber — Lua client SDK for the beachcomber daemon.
--
-- A binding over libbeachcomber's C ABI (see libbeachcomber-ffi/include/
-- beachcomber.h). Two transports, selected automatically:
--
--   ffi        LuaJIT's `ffi` module calls straight into the cdylib.
--              ~0.3ms/call. Used whenever `require("ffi")` succeeds —
--              true for LuaJIT and Neovim (which always ships LuaJIT).
--   subprocess Shells out to the `comb` binary. ~5ms/call. The sanctioned
--              fallback for PUC Lua, which has no ffi and so cannot call
--              into a cdylib at all — entered only because ffi is
--              unavailable *by design*, never as a silent recovery from a
--              broken ffi library discovery (a missing/broken library
--              under LuaJIT is a loud error, not a fallback).
--
-- `client:transport()` reports which one is active, so a caller can see a
-- 5ms-per-call transport rather than discover it in a profile.
--
-- Basic usage:
--
--   local comb = require('beachcomber')
--   local client, err = comb.connect()
--   if not client then error(tostring(err)) end
--   local result = client:get('git.branch', '/my/repo')
--   if result:is_hit() then
--       print(result.data)
--   end
--   client:close()
--
-- Requires LuaJIT (ffi transport) or a `comb` binary on $PATH (subprocess
-- transport, any Lua 5.1+).

local client_mod = require("beachcomber.client")

local M = {}

--- Connect to the beachcomber daemon and return a Client.
--
-- @param opts table?  Optional configuration, forwarded to the selected
--   backend:
--   opts.backend       table   Override transport selection entirely (advanced/tests) —
--                               must satisfy the backend interface documented in
--                               beachcomber.client.
--   opts.socket_path   string  ffi transport: override daemon socket discovery.
--   opts.timeout_ms    number  ffi transport: per-call timeout.
--   opts.autostart     boolean ffi transport: auto-spawn the daemon on demand.
--   opts.library_path  string  ffi transport: override library discovery entirely.
--   opts.comb_bin      string  subprocess transport: override `comb` discovery.
--
-- @return Client, or nil, Error
function M.connect(opts)
  opts = opts or {}

  if opts.backend then
    return client_mod.Client.new(opts.backend)
  end

  local has_ffi = pcall(require, "ffi")
  local backend, err
  if has_ffi then
    backend, err = require("beachcomber.ffi_backend").new(opts)
  else
    backend, err = require("beachcomber.subprocess_backend").new(opts)
  end
  if not backend then
    return nil, err
  end

  return client_mod.Client.new(backend)
end

-- Re-export sub-modules for advanced use.
M.discovery   = require("beachcomber.discovery")
M.json        = require("beachcomber.json")
M.Error       = require("beachcomber.error")
M.Client      = client_mod.Client
M.Result      = client_mod.Result
M.WatchStream = require("beachcomber.watch_stream")

--- Sentinel for a JSON null value nested inside put()'d data or a value
-- decoded from the wire (e.g. a fixture/config file parsed via M.json).
-- Lua cannot store a plain nil as a table value (the key would just be
-- absent), so this stands in for it: `data = {v = comb.null}` puts a real
-- null under "v", and a decoded field can be tested with
-- `value == comb.null` / `comb.json.is_null(value)`. A `Result.data` that
-- is Lua `nil` still means "cache miss" as always — the daemon has no way
-- to store an actual null value (see beachcomber.json's M.NULL doc
-- comment), so this sentinel never appears there.
M.null = M.json.NULL

return M

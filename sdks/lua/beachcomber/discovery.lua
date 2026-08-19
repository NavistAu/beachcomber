--- Shared discovery helpers for the beachcomber Lua SDK.
--
-- Two independent things are discovered here:
--
-- 1. The `comb` binary on `$PATH` — used both by the subprocess transport
--    (which shells out to it for every op) and by the ffi transport's
--    library discovery (step 2: `../lib/` relative to the resolved `comb`).
-- 2. The library candidate list for the ffi transport (see
--    `beachcomber.ffi` for the load/verify loop that consumes it) —
--    `library_candidates()` below.
--
-- Library discovery order (the seven-point common contract, point 1):
--   1. `$BEACHCOMBER_LIB`
--   2. `../lib/<platform name>` relative to the resolved `comb` on `$PATH`
--   3. the platform default dynamic-linker search path (bare library name)
--
-- This mirrors the daemon's `../lib/` packaging convention (Homebrew-style
-- `bin/` + `lib/` siblings under one prefix) — see docs/superpowers/plans/
-- 2026-08-15-client-abi-and-sdk-refactor.md, Phase 4 common contract.

local M = {}

--- Find the `comb` binary on `$PATH`.
-- @return string|nil absolute path to the first `comb` found on PATH
function M.find_comb_on_path()
  local path = os.getenv("PATH") or ""
  for dir in path:gmatch("[^:]+") do
    local candidate = dir .. "/comb"
    local f = io.open(candidate, "r")
    if f then
      f:close()
      return candidate
    end
  end
  return nil
end

--- Return the platform-appropriate shared library filename.
-- Requires LuaJIT's `ffi` module (for `ffi.os`) — only meaningful for the
-- ffi transport, which is the only caller.
-- @param ffi_os string  LuaJIT's `ffi.os` value ("OSX", "Linux", "Windows", ...)
-- @return string
function M.platform_lib_filename(ffi_os)
  if ffi_os == "OSX" then
    return "libbeachcomber.dylib"
  elseif ffi_os == "Windows" then
    return "beachcomber.dll"
  end
  return "libbeachcomber.so" -- Linux, BSD, POSIX
end

--- Build the ordered list of library candidates the ffi transport should
-- try, per the discovery contract above. Every candidate is returned
-- (there is no existence probe here — `ffi.load` itself is the probe;
-- see beachcomber.ffi), so the caller can report every path it tried on a
-- loud discovery failure.
-- @param ffi_os string  LuaJIT's `ffi.os` value
-- @return string[] ordered candidate list
function M.library_candidates(ffi_os)
  local libname = M.platform_lib_filename(ffi_os)
  local candidates = {}

  local env_lib = os.getenv("BEACHCOMBER_LIB")
  if env_lib and env_lib ~= "" then
    candidates[#candidates + 1] = env_lib
  end

  local comb_path = M.find_comb_on_path()
  if comb_path then
    local comb_dir = comb_path:match("^(.*)/[^/]+$") or "."
    candidates[#candidates + 1] = comb_dir .. "/../lib/" .. libname
  end

  -- Platform default search path: the bare library name, letting the
  -- dynamic linker (dyld / ld.so) search its own default locations.
  candidates[#candidates + 1] = libname

  return candidates
end

return M

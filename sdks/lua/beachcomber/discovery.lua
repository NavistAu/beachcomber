--- Library discovery for the beachcomber Lua SDK, and the `comb` lookup
--- both tiers share.
--
-- The rule is the one every SDK follows (docs/sdk-api.md, "Library
-- discovery"; sdks/discovery.json is its fixture, which
-- test/test_discovery.lua runs). The file is libbeachcomber.dylib on macOS
-- and libbeachcomber.so on every other platform, and the candidates are,
-- in order:
--
--   1. `$BEACHCOMBER_LIB`, when set and not empty — then the only one: a
--      path set on purpose that does not load is an error, not a reason to
--      load some other library.
--   2. `<dir>/../lib/<file>`, where `<dir>` is the directory of the first
--      executable `comb` on `$PATH`, its symlinks resolved (a Homebrew
--      `comb` is a symlink into the Cellar, with the library beside it).
--   3. The bare file name, for the system loader.
--
-- `beachcomber.ffi` tries them in order and stops at the first that loads.
-- The subprocess tier runs the same first executable `comb` on `$PATH`.

local M = {}

local has_ffi, ffi = pcall(require, "ffi")
if has_ffi then
  pcall(ffi.cdef, [[
    char *realpath(const char *path, char *resolved_path);
    void free(void *ptr);
  ]])
end

--- `path` with every symlink resolved and made absolute (realpath(3)), or
-- nil when it does not resolve. Reached through LuaJIT's ffi: only the ffi
-- tier loads a library, and only library discovery resolves `comb`.
-- @param path string
-- @return string|nil
function M.realpath(path)
  if not has_ffi then
    error("realpath needs LuaJIT's ffi", 2)
  end
  local resolved = ffi.C.realpath(path, nil)
  if resolved == nil then return nil end
  local out = ffi.string(resolved)
  ffi.C.free(resolved)
  return out
end

local function shq(v)
  return "'" .. tostring(v):gsub("'", "'\\''") .. "'"
end

--- The first executable regular file called `name` on `path` (a
-- `$PATH`-style string; default `$PATH`), as found (symlinks unresolved). An
-- empty entry is skipped, and a `name` that is not executable, or is a
-- directory, is passed over.
-- @param name string
-- @param path string?
-- @return string|nil
function M.find_on_path(name, path)
  path = path or os.getenv("PATH") or ""
  local script = "P=" .. shq(path) .. "; N=" .. shq(name) .. "; IFS=:; for d in $P; do "
    .. "[ -n \"$d\" ] && [ -f \"$d/$N\" ] && [ -x \"$d/$N\" ] "
    .. "&& { printf '%s\\n' \"$d/$N\"; break; }; done"
  local pipe = io.popen("/bin/sh -c " .. shq(script) .. " 2>/dev/null", "r")
  if not pipe then return nil end
  local found = pipe:read("*l")
  pipe:close()
  if found == nil or found == "" then return nil end
  return found
end

--- The first `comb` on `path`: [`find_on_path`] of `comb`.
-- @param path string?
-- @return string|nil
function M.find_comb_on_path(path)
  return M.find_on_path("comb", path)
end

--- The library's file name on platform `os_name` (LuaJIT's `ffi.os`):
-- libbeachcomber.dylib on "OSX", libbeachcomber.so on every other.
-- @param os_name string
-- @return string
function M.platform_lib_filename(os_name)
  if os_name == "OSX" then
    return "libbeachcomber.dylib"
  end
  return "libbeachcomber.so"
end

--- `path` less its last component: "/a/b/c" is "/a/b", "/a" is "/".
local function parent(path)
  local up = path:match("^(.*)/[^/]*$")
  if up == nil or up == "" then return "/" end
  return up
end

--- The candidates the ffi tier tries, in order (the rule in the header).
-- Every candidate is returned with no existence probe: loading is the
-- probe, so a failure can name every path tried.
-- @param env table?  The environment discovery reads: `PATH`,
--                     `BEACHCOMBER_LIB` (absent is unset) and `os` (LuaJIT's
--                     `ffi.os`, the default). Nil is the real environment;
--                     standard Lua cannot set one, so tests pass their own.
-- @return string[]
function M.library_candidates(env)
  env = env or { PATH = os.getenv("PATH"), BEACHCOMBER_LIB = os.getenv("BEACHCOMBER_LIB") }
  local file = M.platform_lib_filename(env.os or (has_ffi and ffi.os) or "")

  if env.BEACHCOMBER_LIB and env.BEACHCOMBER_LIB ~= "" then
    return { env.BEACHCOMBER_LIB }
  end

  local candidates = {}
  local comb = M.find_comb_on_path(env.PATH or "")
  local resolved = comb and M.realpath(comb)
  if resolved then
    local prefix = parent(parent(resolved))
    candidates[#candidates + 1] = (prefix == "/" and "" or prefix) .. "/lib/" .. file
  end
  candidates[#candidates + 1] = file
  return candidates
end

return M

--- LuaJIT `ffi` binding over libbeachcomber's C ABI.
--
-- Loads the cdylib, verifies every symbol the ABI declares resolves, and
-- reports the loaded library's version. This module only concerns itself
-- with *loading*; `beachcomber.ffi_backend` wraps the loaded namespace
-- into the backend interface `beachcomber.client` drives.
--
-- Requires LuaJIT (`require("ffi")` must succeed) — callers check that
-- before requiring this module (see beachcomber/init.lua's transport
-- selection).

local ffi = require("ffi")
local discovery = require("beachcomber.discovery")

local M = {}

ffi.cdef[[
typedef struct BcClient BcClient;
typedef struct BcSession BcSession;
typedef struct BcWatch BcWatch;

const char *bc_version(void);

BcClient *bc_client_new(const char *options_json);
void bc_client_free(BcClient *client);

char *bc_get(BcClient *client, const char *key, const char *path, uint32_t flags);
char *bc_put(BcClient *client, const char *key, const char *json_data, const char *ttl, const char *path);
char *bc_put_null(BcClient *client, const char *key, const char *path);
char *bc_refresh(BcClient *client, const char *key, const char *path);
char *bc_status(BcClient *client);
char *bc_introspect(BcClient *client, const char *subject, const char *options_json);
char *bc_hello(BcClient *client);

char *bc_resolve(BcClient *client, const char *key, const char *cwd, const char *env_json, const char *overrides_json);
char *bc_eval(BcClient *client, const char *template_str, const char *cwd, const char *env_json, const char *overrides_json);

BcSession *bc_session_open(BcClient *client);
void bc_session_close(BcSession *session);
char *bc_session_get(BcSession *session, const char *key, const char *path, uint32_t flags);
char *bc_session_put(BcSession *session, const char *key, const char *json_data, const char *ttl, const char *path);
char *bc_session_set_context(BcSession *session, const char *path);

BcWatch *bc_watch_open(BcClient *client, const char *key, const char *path);
char *bc_watch_next(BcWatch *w, int32_t timeout_ms);
void bc_watch_cancel(BcWatch *w);
void bc_watch_free(BcWatch *w);

void bc_string_free(char *ptr);
]]

--- `bc_get` / `bc_session_get` flag bits (mirrors BC_GET_FORCE / BC_GET_WAIT
-- in libbeachcomber-ffi/include/beachcomber.h).
M.BC_GET_FORCE = 1
M.BC_GET_WAIT = 2

-- Every symbol the ABI declares. Verified at load time (common-contract
-- point 3: "required symbols are checked at load, not on first use").
local REQUIRED_SYMBOLS = {
  "bc_version", "bc_client_new", "bc_client_free", "bc_string_free",
  "bc_get", "bc_put", "bc_put_null", "bc_refresh", "bc_status", "bc_introspect", "bc_hello",
  "bc_resolve", "bc_eval",
  "bc_session_open", "bc_session_close", "bc_session_get", "bc_session_put", "bc_session_set_context",
  "bc_watch_open", "bc_watch_next", "bc_watch_cancel", "bc_watch_free",
}

--- Load the cdylib per the discovery contract, verify every required
-- symbol resolves, and return the loaded namespace plus its version.
--
-- @param opts table?  opts.library_path overrides discovery entirely (advanced/tests).
-- @return {lib=cdata, version=string}, or nil, Error
function M.load(opts)
  opts = opts or {}
  local Error = require("beachcomber.error")

  local candidates
  if opts.library_path then
    candidates = { opts.library_path }
  else
    candidates = discovery.library_candidates(ffi.os)
  end

  local lib, tried = nil, {}
  for _, candidate in ipairs(candidates) do
    local ok, loaded_or_err = pcall(ffi.load, candidate)
    if ok then
      lib = loaded_or_err
      break
    end
    tried[#tried + 1] = candidate .. " (" .. tostring(loaded_or_err) .. ")"
  end

  if not lib then
    return nil, Error.new(
      "library_not_found",
      "could not locate libbeachcomber; tried, in order:\n  " .. table.concat(tried, "\n  ")
    )
  end

  -- Symbol check at load, not first use (common contract point 3).
  for _, name in ipairs(REQUIRED_SYMBOLS) do
    local ok = pcall(function() return lib[name] end)
    if not ok then
      local version = "unknown"
      local vok, vptr = pcall(function() return lib.bc_version() end)
      if vok and vptr ~= nil then
        version = ffi.string(vptr)
      end
      return nil, Error.new(
        "missing_symbol",
        string.format("library is missing required symbol %q (loaded library reports bc_version()=%q)", name, version)
      )
    end
  end

  local version = ffi.string(lib.bc_version())

  return { lib = lib, version = version }
end

return M

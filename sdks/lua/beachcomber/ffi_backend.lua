--- Backend implementing the beachcomber.client backend interface over
--- LuaJIT's `ffi`, calling straight into libbeachcomber's C ABI.
--
-- Every op mirrors a `bc_*` C function: encode arguments, call, decode the
-- `char *` JSON envelope, free it via `bc_string_free` (never `bc_version`),
-- and return a canonical response table
-- `{ok, data, age_ms, stale, error={kind,message}}`.
--
-- See beachcomber.ffi for library discovery/loading/symbol verification.

local ffi = require("ffi")
local ffi_loader = require("beachcomber.ffi")
local json = require("beachcomber.json")
local Error = require("beachcomber.error")

local Backend = {}
Backend.__index = Backend

--- Decode a `char *` envelope pointer into a canonical response table,
-- freeing the string (this crate's convention: every returned `char *`
-- must be freed via `bc_string_free`; `bc_version()`'s return is the sole
-- exception and is never routed through here).
local function decode(C, ptr)
  if ptr == nil then
    return { ok = false, error = { kind = "io_error", message = "NULL returned from library call" } }
  end
  local s = ffi.string(ptr)
  C.bc_string_free(ptr)
  local resp = json.decode(s)
  if type(resp) ~= "table" then
    return { ok = false, error = { kind = "parse_error", message = "malformed envelope: " .. tostring(s) } }
  end
  return resp
end

--- `bc_get`/`bc_session_get`-shaped result: `{data:{data,age_ms,stale}}` on
-- ok, `{error:{kind,message}}` otherwise. Flatten to the canonical shape.
local function unwrap_get(env)
  if not env.ok then
    return { ok = false, error = env.error }
  end
  local d = env.data or {}
  return { ok = true, data = d.data, age_ms = d.age_ms, stale = d.stale }
end

local function plain(env)
  if not env.ok then
    return { ok = false, error = env.error }
  end
  return { ok = true, data = env.data }
end

local M = {}

--- @param opts table?  opts.socket_path / opts.timeout_ms / opts.autostart
--                       forwarded into bc_client_new's options_json;
--                       opts.library_path overrides ffi library discovery.
-- @return Backend, or nil, Error
function M.new(opts)
  opts = opts or {}
  local loaded, err = ffi_loader.load({ library_path = opts.library_path })
  if not loaded then
    return nil, err
  end
  local C = loaded.lib

  local options = {}
  if opts.socket_path ~= nil then options.socket_path = opts.socket_path end
  if opts.timeout_ms ~= nil then options.timeout_ms = opts.timeout_ms end
  if opts.autostart ~= nil then options.autostart = opts.autostart end
  local options_json = next(options) and json.encode(options) or nil

  local client_ptr = C.bc_client_new(options_json)
  ffi.gc(client_ptr, C.bc_client_free)

  return setmetatable({
    _C = C,
    _client = client_ptr,
    _version = loaded.version,
    _closed = false,
  }, Backend)
end

function Backend:name()
  return "ffi"
end

function Backend:version()
  return self._version
end

local function flag_bits(flags)
  flags = flags or {}
  local bits = 0
  if flags.force then bits = bits + ffi_loader.BC_GET_FORCE end
  if flags.wait then bits = bits + ffi_loader.BC_GET_WAIT end
  return bits
end

function Backend:get(key, path, flags)
  local ptr = self._C.bc_get(self._client, key, path, flag_bits(flags))
  return unwrap_get(decode(self._C, ptr))
end

function Backend:put(key, data, ttl, path)
  local json_data, enc_err = json.encode(data)
  if not json_data then
    return { ok = false, error = { kind = "parse_error", message = "json encode error: " .. tostring(enc_err) } }
  end
  local ptr = self._C.bc_put(self._client, key, json_data, ttl, path)
  return plain(decode(self._C, ptr))
end

function Backend:put_null(key, path)
  local ptr = self._C.bc_put_null(self._client, key, path)
  return plain(decode(self._C, ptr))
end

function Backend:refresh(key, path)
  local ptr = self._C.bc_refresh(self._client, key, path)
  return plain(decode(self._C, ptr))
end

function Backend:status()
  local ptr = self._C.bc_status(self._client)
  return plain(decode(self._C, ptr))
end

function Backend:introspect(subject, duration_secs)
  local options_json = duration_secs and json.encode({ duration_secs = duration_secs }) or nil
  local ptr = self._C.bc_introspect(self._client, subject, options_json)
  return plain(decode(self._C, ptr))
end

function Backend:hello()
  local ptr = self._C.bc_hello(self._client)
  return plain(decode(self._C, ptr))
end

function Backend:resolve(key, cwd, env_vars, overrides)
  local env_json = json.encode(env_vars or {})
  local overrides_json = json.encode(overrides or {})
  local ptr = self._C.bc_resolve(self._client, key, cwd, env_json, overrides_json)
  return plain(decode(self._C, ptr))
end

function Backend:eval(template_str, cwd, env_vars, overrides)
  local env_json = json.encode(env_vars or {})
  local overrides_json = json.encode(overrides or {})
  local ptr = self._C.bc_eval(self._client, template_str, cwd, env_json, overrides_json)
  return plain(decode(self._C, ptr))
end

-- ── Session (advanced: a persistent connection with server-side context) ──

local Session = {}
Session.__index = Session

function Backend:session_open()
  local sptr = self._C.bc_session_open(self._client)
  ffi.gc(sptr, self._C.bc_session_close)
  return setmetatable({ _C = self._C, _s = sptr }, Session)
end

function Session:get(key, path, flags)
  local ptr = self._C.bc_session_get(self._s, key, path, flag_bits(flags))
  return unwrap_get(decode(self._C, ptr))
end

function Session:put(key, data, ttl, path)
  local json_data = json.encode(data)
  local ptr = self._C.bc_session_put(self._s, key, json_data, ttl, path)
  return plain(decode(self._C, ptr))
end

function Session:set_context(path)
  local ptr = self._C.bc_session_set_context(self._s, path)
  return plain(decode(self._C, ptr))
end

function Session:close()
  if self._s ~= nil then
    ffi.gc(self._s, nil)
    self._C.bc_session_close(self._s)
    self._s = nil
  end
end

-- ── Watch ───────────────────────────────────────────────────────────────

local Watch = {}
Watch.__index = Watch

function Backend:watch_open(key, path)
  local wptr = self._C.bc_watch_open(self._client, key, path)
  if wptr == nil then
    return nil, Error.new("io_error", "bc_watch_open returned NULL (allocation failure)")
  end
  ffi.gc(wptr, self._C.bc_watch_free)
  return setmetatable({ _C = self._C, _w = wptr }, Watch)
end

--- @param timeout_ms number?  -1 (default) blocks indefinitely, 0 polls, >0 waits that long.
--- @return event_table_or_nil, Error_or_nil, outcome_string ("event"|"timeout"|"eof"|"cancelled"|"error")
function Watch:next_event(timeout_ms)
  timeout_ms = timeout_ms or -1
  local ptr = self._C.bc_watch_next(self._w, timeout_ms)
  local env = decode(self._C, ptr)
  if not env.ok then
    return nil, Error.from(env.error), "error"
  end
  if env.outcome == "event" then
    local d = env.data or {}
    return { data = d.data, age_ms = d.age_ms, stale = d.stale }, nil, "event"
  end
  return nil, nil, env.outcome
end

function Watch:cancel()
  self._C.bc_watch_cancel(self._w)
end

function Watch:close()
  if self._w ~= nil then
    ffi.gc(self._w, nil)
    self._C.bc_watch_free(self._w)
    self._w = nil
  end
end

-- ── Lifecycle ───────────────────────────────────────────────────────────

function Backend:close()
  if not self._closed and self._client ~= nil then
    ffi.gc(self._client, nil)
    self._C.bc_client_free(self._client)
    self._closed = true
  end
end

return M

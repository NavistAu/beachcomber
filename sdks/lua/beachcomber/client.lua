--- Transport-agnostic Client and Result for the beachcomber Lua SDK.
--
-- `Client` wraps a "backend" — either `beachcomber.ffi_backend` (LuaJIT
-- `ffi` over the cdylib) or `beachcomber.subprocess_backend` (shells out
-- to `comb`; the sanctioned PUC Lua fallback) — and exposes the public API
-- surface unchanged from the pre-ABI raw-socket implementation:
-- get/get_with_flags/refresh/set_context/status/hello/put/put_null/
-- introspect/watch/close, plus Result:is_hit()/get_str(). `transport()` is
-- new: every method above works identically regardless of which value it
-- returns, but a caller that cares about latency can check it rather than
-- discover a 5ms-per-call transport in a profile.
--
-- A backend implements: name(), get(key,path,flags), put(key,data,ttl,path),
-- put_null(key,path), refresh(key,path), status(), close(), and optionally
-- introspect(subject,duration_secs), hello(), resolve(key,cwd,env,overrides),
-- eval(template,cwd,env,overrides), watch_open(key,path) — each returning a
-- canonical response table `{ok, data, age_ms, stale, error={kind,message}}`.
-- A backend that omits an optional method signals the capability doesn't
-- exist over that transport; Client turns that into Error.unsupported().

local WatchStream = require("beachcomber.watch_stream")
local Error = require("beachcomber.error")

local Client = {}
Client.__index = Client

--- @param backend table  See module doc for the backend interface.
-- @return Client
function Client.new(backend)
  return setmetatable({ _backend = backend, _context_path = nil }, Client)
end

--- Which transport this client is actually using: "ffi" or "subprocess".
-- @return string
function Client:transport()
  return self._backend:name()
end

-- ── Result ────────────────────────────────────────────────────────────────

local Result = {}
Result.__index = Result

--- @param resp table  Canonical response `{data, age_ms, stale}` (ok=true).
-- @return Result
function Result.new(resp)
  return setmetatable({
    data   = resp.data,
    age_ms = resp.age_ms or 0,
    stale  = resp.stale or false,
  }, Result)
end

--- Return true when the cache contains a value (cache hit).
function Result:is_hit()
  return self.data ~= nil
end

--- Get a string field from object data (convenience for whole-provider gets).
-- @param field string
-- @return string|nil value, or nil, error_message
function Result:get_str(field)
  if type(self.data) ~= "table" then
    return nil, "result data is not an object"
  end
  local v = self.data[field]
  if v == nil then
    return nil, "field not found: " .. tostring(field)
  end
  return tostring(v)
end

-- ── Internal ─────────────────────────────────────────────────────────────

--- The effective path for a call: an explicit argument wins; otherwise the
-- context set by set_context(), if any. Client-side (works identically
-- for both transports — no server-side session state required).
function Client:_path(explicit)
  if explicit ~= nil then return explicit end
  return self._context_path
end

-- ── Client methods ───────────────────────────────────────────────────────

--- Query a cached value.
-- @param key string    Provider key, e.g. "git.branch" or "git"
-- @param path string?  Optional working-directory path
-- @return Result, or nil, Error
function Client:get(key, path)
  return self:get_with_flags(key, path, false, false)
end

--- Query a cached value with optional force/wait flags.
-- @param key   string    Provider key
-- @param path  string?   Optional working-directory path
-- @param force boolean?  Force recomputation before returning
-- @param wait  boolean?  Wait for a fresh value if stale
-- @return Result, or nil, Error
function Client:get_with_flags(key, path, force, wait)
  local resp = self._backend:get(key, self:_path(path), { force = force, wait = wait })
  if not resp.ok then return nil, Error.from(resp.error) end
  return Result.new(resp)
end

--- Force recomputation of a provider.
-- @param key string    Provider key to recompute
-- @param path string?  Optional working-directory path
-- @return true, or nil, Error
function Client:refresh(key, path)
  local resp = self._backend:refresh(key, self:_path(path))
  if not resp.ok then return nil, Error.from(resp.error) end
  return true
end

--- Set the default path for subsequent queries made without an explicit
-- path argument. Client-side (works identically over ffi and subprocess).
-- @param path string
-- @return true
function Client:set_context(path)
  self._context_path = path
  return true
end

--- Return the cache row array.
-- @return table[], or nil, Error
function Client:status()
  local resp = self._backend:status()
  if not resp.ok then return nil, Error.from(resp.error) end
  return resp.data or {}
end

--- Query the daemon version and protocol version.
-- @return table {protocol_version, daemon_version}, or nil, Error
function Client:hello()
  if not self._backend.hello then
    return nil, Error.unsupported("hello", self:transport())
  end
  local resp = self._backend:hello()
  if not resp.ok then return nil, Error.from(resp.error) end
  local data = resp.data or {}
  return {
    protocol_version = data.protocol_version or "",
    daemon_version   = data.daemon_version or "",
  }
end

--- Store data into a virtual provider.
-- @param key  string   Provider key
-- @param data any       Value to store
-- @param ttl  string?  Optional TTL (e.g. "30s")
-- @param path string?  Optional working-directory path
-- @return true, or false, Error
function Client:put(key, data, ttl, path)
  local resp = self._backend:put(key, data, ttl, self:_path(path))
  if not resp.ok then return false, Error.from(resp.error) end
  return true
end

--- Clear a virtual provider entry's cached value.
-- @param key  string
-- @param path string?
-- @return true, or false, Error
function Client:put_null(key, path)
  local resp = self._backend:put_null(key, self:_path(path))
  if not resp.ok then return false, Error.from(resp.error) end
  return true
end

--- Introspect daemon internals.
-- @param subject       string
-- @param duration_secs number?
-- @return table {subject, daemon, other}, or nil, Error
function Client:introspect(subject, duration_secs)
  if not self._backend.introspect then
    return nil, Error.unsupported("introspect", self:transport())
  end
  local resp = self._backend:introspect(subject, duration_secs)
  if not resp.ok then return nil, Error.from(resp.error) end
  if subject == "daemon" and type(resp.data) == "table" then
    return { subject = "daemon", daemon = resp.data, other = nil }
  end
  return { subject = subject, daemon = nil, other = resp.data }
end

--- Resolve a virtual field or path expression client-side (via bc_resolve).
-- @param key  string  "provider.field" or a bare provider name
-- @param opts table?  opts.cwd (default: context path, then "."), opts.env,
--                      opts.virtual (field/path expression overrides, keyed
--                      the same way tests/conformance fixtures' `virtual` is)
-- @return value, or nil, Error
function Client:resolve(key, opts)
  if not self._backend.resolve then
    return nil, Error.unsupported("resolve", self:transport())
  end
  opts = opts or {}
  local cwd = opts.cwd or self._context_path or "."
  local resp = self._backend:resolve(key, cwd, opts.env or {}, opts.virtual or {})
  if not resp.ok then return nil, Error.from(resp.error) end
  return resp.data
end

--- Evaluate a value expression client-side (via bc_eval). template_str
-- accepts a bare expression, a single "{{ expr }}" tag, or literal
-- text/several tags — the first two keep the expression's natural type,
-- the third is always a string.
-- @param template_str string
-- @param opts table?  Same shape as Client:resolve's opts.
-- @return value, or nil, Error
function Client:eval(template_str, opts)
  if not self._backend.eval then
    return nil, Error.unsupported("eval", self:transport())
  end
  opts = opts or {}
  local cwd = opts.cwd or self._context_path or "."
  local resp = self._backend:eval(template_str, cwd, opts.env or {}, opts.virtual or {})
  if not resp.ok then return nil, Error.from(resp.error) end
  return resp.data
end

--- Subscribe to changes for a key, returning a WatchStream.
-- @param key  string
-- @param path string?
-- @return WatchStream, or nil, Error
function Client:watch(key, path)
  if not self._backend.watch_open then
    return nil, Error.unsupported("watch", self:transport())
  end
  local handle, err = self._backend:watch_open(key, self:_path(path))
  if not handle then return nil, err end
  return WatchStream.new(handle)
end

--- Open an advanced Session: a persistent connection with true server-side
-- context (bc_session_*). Only meaningful over the ffi transport — most
-- callers want Client:set_context() instead, which works over either
-- transport by keeping the default path client-side.
-- @return session backend object, or nil, Error
function Client:session()
  if not self._backend.session_open then
    return nil, Error.unsupported("session", self:transport())
  end
  return self._backend:session_open()
end

--- Close the underlying connection/library handle.
function Client:close()
  self._backend:close()
end

return {
  Client = Client,
  Result = Result,
}

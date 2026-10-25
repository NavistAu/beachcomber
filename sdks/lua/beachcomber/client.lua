--- Transport-agnostic Client for the beachcomber Lua SDK.
--
-- `Client` wraps a "backend" — either `beachcomber.ffi_backend` (LuaJIT
-- `ffi` over the cdylib) or `beachcomber.subprocess_backend` (shells out
-- to `comb`; the sanctioned PUC Lua fallback) — and exposes one surface
-- over both: get/eval/watch/put/refresh/set_context/status/hello/
-- introspect/session/close. `transport()` reports which backend is active,
-- so a caller that cares about latency can check it rather than discover a
-- 5ms-per-call transport in a profile.
--
-- A backend implements: name(), get(key,cwd,env,flags),
-- put(key,data,ttl,path), status(), close(), and optionally
-- refresh(key,path), introspect(subject,duration_secs), hello(),
-- eval(template,cwd,env), watch_open(key,cwd,env) — each returning a
-- canonical response table `{ok, data, error={kind,message}}`, where `data`
-- of a get or eval is the value itself (nil on a miss). A backend that omits
-- an optional method signals the capability doesn't exist over that
-- transport; Client turns that into Error.unsupported().

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

-- ── Internal ─────────────────────────────────────────────────────────────

--- The effective path for a call: an explicit argument wins; otherwise the
-- context set by set_context(), if any. Client-side (works identically
-- for both transports — no server-side session state required).
function Client:_path(explicit)
  if explicit ~= nil then return explicit end
  return self._context_path
end

-- ── Client methods ───────────────────────────────────────────────────────

--- The value `comb get` prints for this key, cwd and env (via bc_get).
--
-- The daemon holds the templates (`<provider>:template`, seeded from
-- configuration or put); this client renders them. A plain field is its
-- cached value, a templated one is rendered, and a bare provider name is
-- the whole merged namespace. The unrendered halves are facets, read like
-- any key: "p:template.x" is the template as stored, "p:put.x" and
-- "p:source.x" those layers' values.
-- @param key  string  "provider.field" or a bare provider name
-- @param opts table?  opts.cwd: the directory the read addresses (`~` is
--                      home, a relative path is against this process's
--                      working directory); default the set_context() path,
--                      else the global entry.
--                      opts.env: the caller's environment for env.* refs;
--                      nil means none (every env.* is absent).
--                      opts.force: re-execute every Source the read demands first.
--                      opts.wait: re-execute a stale entry inline before answering.
-- @return value, or nil on a miss (no Error), or nil, Error
function Client:get(key, opts)
  opts = opts or {}
  local resp = self._backend:get(key, self:_path(opts.cwd), opts.env, { force = opts.force, wait = opts.wait })
  if not resp.ok then return nil, Error.from(resp.error) end
  return resp.data
end

--- Force recomputation of a provider.
-- @param key string    Provider key to recompute
-- @param path string?  Optional working-directory path
-- @return true, or nil, Error
function Client:refresh(key, path)
  if not self._backend.refresh then
    return nil, Error.unsupported("refresh", self:transport())
  end
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

--- Store data into the cache as static data.
-- @param key  string   Provider key
-- @param data any       Value to store; nil removes the node
-- @param ttl  string?  Optional TTL (e.g. "30s")
-- @param path string?  Optional working-directory path
-- @return true, or false, Error
function Client:put(key, data, ttl, path)
  local resp = self._backend:put(key, data, ttl, self:_path(path))
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

--- Render a template that is stored nowhere — the same value `comb eval`
-- prints (via bc_eval). Every ref resolves exactly as it would inside a
-- stored template, so eval("{{ p.f }}") and get("p.f") agree.
-- template_str accepts a single "{{ expr }}" tag, or literal text/several
-- tags — the former keeps the expression's natural type, the latter is
-- always a string (a source with no tags at all is its literal text).
-- @param template_str string
-- @param opts table?  opts.cwd (default: context path, then "."), opts.env
--                      (the caller's environment for env.* refs)
-- @return value, or nil, Error
function Client:eval(template_str, opts)
  if not self._backend.eval then
    return nil, Error.unsupported("eval", self:transport())
  end
  opts = opts or {}
  local cwd = opts.cwd or self._context_path or "."
  local resp = self._backend:eval(template_str, cwd, opts.env or {})
  if not resp.ok then return nil, Error.from(resp.error) end
  return resp.data
end

--- Subscribe to a key's *value* — what Client:get answers for it — and
-- receive it again whenever it changes, so a template edited under the watch
-- fires it just as a changed cache record does. Returns a WatchStream.
-- @param key  string  "provider.field" or a bare provider name
-- @param opts table?  opts.cwd (default: context path, then "."), opts.env
--                      (the caller's environment for env.* refs)
-- @return WatchStream, or nil, Error
function Client:watch(key, opts)
  if not self._backend.watch_open then
    return nil, Error.unsupported("watch", self:transport())
  end
  opts = opts or {}
  local cwd = opts.cwd or self._context_path or "."
  local handle, err = self._backend:watch_open(key, cwd, opts.env or {})
  if not handle then return nil, err end
  return WatchStream.new(handle)
end

--- Open an advanced Session: a persistent connection that keeps a directory
-- of its own (bc_session_*), sent with each get/put/refresh that names none.
-- Only meaningful over the ffi transport — most callers want
-- Client:set_context() instead, which works over either transport.
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
}

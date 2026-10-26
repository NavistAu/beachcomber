--- Transport-agnostic Client and Session for the beachcomber Lua SDK.
--
-- They follow the SDK API table (docs/sdk-api.md; sdks/api.json, which
-- test/test_api_surface.lua checks this surface against): Client has
-- get/eval/watch/put/refresh/status/hello/introspect/session/close, plus
-- `transport()`; a Session has set_context/get/put/refresh/close. Every
-- method returns its value or `nil, Error`; a miss is `nil` with no Error,
-- and a method that answers nothing returns `true`. Optional arguments are
-- an options table.
--
-- `Client` wraps a "backend" — either `beachcomber.ffi_backend` (LuaJIT
-- `ffi` over the cdylib) or `beachcomber.subprocess_backend` (shells out
-- to `comb`; the sanctioned PUC Lua fallback). `transport()` reports which,
-- so a caller that cares about latency can check it rather than discover a
-- 5ms-per-call transport in a profile.
--
-- A backend implements: name(), get(key,cwd,env,options),
-- put(key,data,ttl,path), status(), close(), and optionally version(),
-- refresh(key,path), introspect(subject,duration_secs), hello(),
-- eval(template,cwd,env), watch_open(key,cwd,env) and session_open() (a
-- handle with get/put/refresh/set_context/close taking the same arguments)
-- — each returning a response table `{ok, data, error={kind,message}}`,
-- where `data` of a get or eval is the value itself (nil on a miss). A
-- backend that omits an optional method signals the capability doesn't
-- exist over that transport; Client turns that into an "unsupported" Error.

local WatchStream = require("beachcomber.watch_stream")
local Error = require("beachcomber.error")
local json = require("beachcomber.json")

--- The value a response table answers, or nil, its Error.
local function answer(resp, library_version)
  if not resp.ok then return nil, Error.from(resp.error, library_version) end
  return resp.data
end

--- true, or nil, the response's Error: what a method that answers nothing
-- returns.
local function done(resp, library_version)
  if not resp.ok then return nil, Error.from(resp.error, library_version) end
  return true
end

--- nil, Error for a call on a closed client or session: its handle is
-- freed, and the library must never see a freed handle.
local function closed(what, library_version)
  return nil, Error.new("io_error", "the " .. what .. " is closed", library_version)
end

--- What a get asks besides its key, cwd and env: `force`, and `wait` as
-- the caller gave it, for the library to read.
local function options_of(opts)
  return { force = opts.force, wait = opts.wait }
end

-- ── Session ──────────────────────────────────────────────────────────────

--- One connection and a directory of its own (`set_context`), which a
-- `get` with no `cwd`, and a `put` or `refresh` with no `path`, address.
local Session = {}
Session.__index = Session

--- @param handle table              A backend session handle.
--- @param library_version string?
function Session.new(handle, library_version)
  return setmetatable({ _handle = handle, _version = library_version }, Session)
end

--- Set this session's directory.
-- @param path string
-- @return true, or nil, Error
function Session:set_context(path)
  if self._closed then return closed("session", self._version) end
  return done(self._handle:set_context(path), self._version)
end

--- Client:get on this session; no `opts.cwd` is the session's directory,
-- else the global entry.
-- @return value, or nil on a miss (no Error), or nil, Error
function Session:get(key, opts)
  if self._closed then return closed("session", self._version) end
  opts = opts or {}
  return answer(self._handle:get(key, opts.cwd, opts.env, options_of(opts)), self._version)
end

--- Client:put on this session; no `opts.path` is the session's directory.
-- @return true, or nil, Error
function Session:put(key, data, opts)
  if self._closed then return closed("session", self._version) end
  opts = opts or {}
  return done(self._handle:put(key, data, opts.ttl, opts.path), self._version)
end

--- Client:refresh on this session; no `opts.path` is the session's directory.
-- @return true, or nil, Error
function Session:refresh(key, opts)
  if self._closed then return closed("session", self._version) end
  opts = opts or {}
  return done(self._handle:refresh(key, opts.path), self._version)
end

--- Close the session's connection.
-- @return true
function Session:close()
  if not self._closed then
    self._closed = true
    self._handle:close()
  end
  return true
end

-- ── Client ───────────────────────────────────────────────────────────────

local Client = {}
Client.__index = Client

--- @param backend table  See the module doc for the backend interface.
-- @return Client
function Client.new(backend)
  local version = backend.version and backend:version() or nil
  return setmetatable({ _backend = backend, _version = version }, Client)
end

--- Which transport this client is actually using: "ffi" or "subprocess".
-- @return string
function Client:transport()
  return self._backend:name()
end

--- An "unsupported" Error for an op this client's backend lacks.
function Client:_unsupported(capability)
  return nil, Error.unsupported(capability, self:transport(), self._version)
end

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
--                      working directory); nil is the global entry.
--                      opts.env: the caller's environment for env.* refs;
--                      nil is the calling process's environment, as comb renders.
--                      opts.force: re-execute every Source the read demands
--                      first, as comb get --force.
--                      opts.wait: how long to wait for a run the read needs
--                      before answering what is stored, as comb get --wait:
--                      a duration ("0", "50ms", "2s") or "forever"; anything
--                      else is a "parse_error". nil is the read's budget: a
--                      stored record waits its Source's budget, a cold read
--                      waits for its run.
-- @return value, or nil on a miss (no Error), or nil, Error
function Client:get(key, opts)
  if self._closed then return closed("client", self._version) end
  opts = opts or {}
  return answer(self._backend:get(key, opts.cwd, opts.env, options_of(opts)), self._version)
end

--- Render a template that is stored nowhere — the same value `comb eval`
-- prints (via bc_eval). Every ref resolves exactly as it would inside a
-- stored template, so eval("{{ p.f }}") and get("p.f") agree.
-- template_str accepts a single "{{ expr }}" tag, or literal text/several
-- tags — the former keeps the expression's natural type, the latter is
-- always a string (a source with no tags at all is its literal text).
-- @param template_str string
-- @param opts table?  opts.cwd and opts.env, as Client:get takes them.
-- @return value, or nil when it renders nothing (no Error), or nil, Error
function Client:eval(template_str, opts)
  if self._closed then return closed("client", self._version) end
  if not self._backend.eval then return self:_unsupported("eval") end
  opts = opts or {}
  return answer(self._backend:eval(template_str, opts.cwd, opts.env), self._version)
end

--- Subscribe to a key's *value* — what Client:get answers for it — and
-- receive it again whenever it changes, so a template edited under the watch
-- fires it just as a changed cache record does.
-- @param key  string  "provider.field" or a bare provider name
-- @param opts table?  opts.cwd and opts.env, as Client:get takes them.
-- @return WatchStream, or nil, Error
function Client:watch(key, opts)
  if self._closed then return closed("client", self._version) end
  if not self._backend.watch_open then return self:_unsupported("watch") end
  opts = opts or {}
  local handle, err = self._backend:watch_open(key, opts.cwd, opts.env)
  if not handle then return nil, err end
  return WatchStream.new(handle, self._version)
end

--- Store data into the cache as static data.
-- @param key  string   Provider key
-- @param data any      Value to store; nil removes the node
-- @param opts table?   opts.ttl (e.g. "30s"), opts.path (nil: the global entry)
-- @return true, or nil, Error
function Client:put(key, data, opts)
  if self._closed then return closed("client", self._version) end
  opts = opts or {}
  return done(self._backend:put(key, data, opts.ttl, opts.path), self._version)
end

--- Re-execute the Sources a key demands.
-- @param key  string
-- @param opts table?  opts.path (nil: the global entry)
-- @return true, or nil, Error
function Client:refresh(key, opts)
  if self._closed then return closed("client", self._version) end
  if not self._backend.refresh then return self:_unsupported("refresh") end
  opts = opts or {}
  return done(self._backend:refresh(key, opts.path), self._version)
end

--- The cache rows. A row stored at the global entry has a nil `path`,
-- whichever transport read it.
-- @return table[], or nil, Error
function Client:status()
  if self._closed then return closed("client", self._version) end
  local rows, err = answer(self._backend:status(), self._version)
  if err then return nil, err end
  if type(rows) ~= "table" then
    return nil, Error.new("parse_error", "status data is not a list of rows", self._version)
  end
  for _, row in ipairs(rows) do
    if row.path == json.NULL then row.path = nil end
  end
  return rows
end

--- The daemon's protocol and build versions.
-- @return table {protocol_version, daemon_version}, or nil, Error
function Client:hello()
  if self._closed then return closed("client", self._version) end
  if not self._backend.hello then return self:_unsupported("hello") end
  local data, err = answer(self._backend:hello(), self._version)
  if err then return nil, err end
  data = data or {}
  return {
    protocol_version = data.protocol_version or "",
    daemon_version   = data.daemon_version or "",
  }
end

--- A report on one of the daemon's subsystems.
-- @param subject string
-- @param opts    table?  opts.duration_secs (the `procs` subject's only)
-- @return table {subject, daemon} for "daemon", {subject, other} for the
--         rest, or nil, Error
function Client:introspect(subject, opts)
  if self._closed then return closed("client", self._version) end
  if not self._backend.introspect then return self:_unsupported("introspect") end
  opts = opts or {}
  local data, err = answer(self._backend:introspect(subject, opts.duration_secs), self._version)
  if err then return nil, err end
  if subject == "daemon" and type(data) == "table" then
    return { subject = "daemon", daemon = data }
  end
  return { subject = subject, other = data }
end

--- A session: one connection and a directory of its own.
-- @return Session, or nil, Error
function Client:session()
  if self._closed then return closed("client", self._version) end
  if not self._backend.session_open then return self:_unsupported("session") end
  return Session.new(self._backend:session_open(), self._version)
end

--- Release the client.
-- @return true
function Client:close()
  if not self._closed then
    self._closed = true
    self._backend:close()
  end
  return true
end

return {
  Client = Client,
  Session = Session,
}

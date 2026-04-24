--- Client implementation for the beachcomber daemon.
--
-- Wraps a socket handle and exposes the protocol operations:
-- get, get_with_flags, refresh, context, status,
-- hello, put, put_null, introspect, watch.

local json = require("beachcomber.json")
local WatchStream = require("beachcomber.watch_stream")

local Client = {}
Client.__index = Client

--- Construct a new Client.
-- @param socket_handle      A connected handle from socket_luasocket or socket_vim.
-- @param backend_factory    Optional function() -> handle, for watch (opens a new conn).
-- @return Client
function Client.new(socket_handle, backend_factory)
  return setmetatable({
    _sock = socket_handle,
    _backend_factory = backend_factory,
  }, Client)
end

-- ── Internal helpers ─────────────────────────────────────────────────────────

--- Send a request and receive a raw decoded response table.
-- Returns the decoded table regardless of resp.ok.
-- On transport/decode errors returns nil, err_string.
local function raw_request(sock, req_table)
  local encoded, enc_err = json.encode(req_table)
  if not encoded then
    return nil, "json encode error: " .. tostring(enc_err)
  end
  local ok, send_err = sock:send_line(encoded .. "\n")
  if not ok then
    return nil, send_err
  end
  local line, recv_err = sock:recv_line()
  if not line then
    return nil, recv_err
  end
  local resp, dec_err = json.decode(line)
  if resp == nil and dec_err then
    return nil, "json decode error: " .. dec_err
  end
  if type(resp) ~= "table" then
    return nil, "unexpected response type"
  end
  return resp
end

--- Send a request and return the response, translating ok=false to nil+error.
local function send_request(sock, req_table)
  local resp, err = raw_request(sock, req_table)
  if not resp then return nil, err end
  if resp.ok == false then
    return nil, resp.error or "server returned ok=false"
  end
  return resp
end

--- Instance-level send helper; returns raw decoded response (not nil-on-error).
function Client:_send(req_table)
  local resp, err = raw_request(self._sock, req_table)
  if not resp then
    return { ok = false, error = err }
  end
  return resp
end

-- ── Result type ──────────────────────────────────────────────────────────────

local Result = {}
Result.__index = Result

--- Create a Result from a raw response table.
-- @param resp table  Decoded response from the daemon
-- @return Result
function Result.new(resp)
  return setmetatable({
    data    = resp.data,
    age_ms  = resp.age_ms or 0,
    stale   = resp.stale or false,
  }, Result)
end

--- Return true when the cache contains a value (cache hit).
-- @return boolean
function Result:is_hit()
  return self.data ~= nil
end

--- Get a string field from object data.
-- Convenience for full-provider responses (e.g. get("git")).
-- @param field string  Field name
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

-- ── Client methods ───────────────────────────────────────────────────────────

--- Query a cached value.
--
-- @param key string    Provider key, e.g. "git.branch" or "git"
-- @param path string?  Optional working-directory path
-- @return Result, or nil, error_message
function Client:get(key, path)
  local req = { op = "get", key = key }
  if path ~= nil then req.path = path end
  local resp, err = send_request(self._sock, req)
  if not resp then
    return nil, err
  end
  return Result.new(resp)
end

--- Force recomputation of a provider.
--
-- @param key string    Provider key to recompute
-- @param path string?  Optional working-directory path
-- @return true, or nil, error_message
function Client:refresh(key, path)
  local req = { op = "refresh", key = key }
  if path ~= nil then req.path = path end
  local resp, err = send_request(self._sock, req)
  if not resp then
    return nil, err
  end
  return true
end

--- Set the default path for subsequent queries on this connection.
--
-- @param path string  Directory path to use as the connection context
-- @return true, or nil, error_message
function Client:set_context(path)
  local resp, err = send_request(self._sock, { op = "context", path = path })
  if not resp then
    return nil, err
  end
  return true
end

--- Return the cache row array.
--
-- @return table[] array of cache rows, or nil, error_message
function Client:status()
  local resp = self:_send({ op = "status" })
  if not resp.ok then return nil, resp.error end
  return resp.data or {}
end

--- Query the daemon version and protocol version.
--
-- @return table {protocol_version, daemon_version}, or nil, error_message
function Client:hello()
  local resp = self:_send({ op = "hello" })
  if not resp.ok then return nil, resp.error end
  return {
    protocol_version = resp.data and resp.data.protocol_version or "",
    daemon_version   = resp.data and resp.data.daemon_version   or "",
  }
end

--- Query a cached value with optional force/wait flags.
--
-- @param key   string    Provider key
-- @param path  string?   Optional working-directory path
-- @param force boolean?  Force recomputation before returning
-- @param wait  boolean?  Wait for a fresh value if stale
-- @return Result, or nil, error_message
function Client:get_with_flags(key, path, force, wait)
  local req = { op = "get", key = key }
  if path  then req.path  = path  end
  if force then req.force = true  end
  if wait  then req.wait  = true  end
  local resp = self:_send(req)
  if not resp.ok then return nil, resp.error end
  return Result.new(resp)
end

--- Store a virtual provider entry in the cache.
--
-- @param key  string   Provider key (e.g. "myprovider")
-- @param data any      Value to store (nil clears the entry)
-- @param ttl  number?  Optional TTL in seconds
-- @param path string?  Optional working-directory path
-- @return true, or false, error_message
function Client:put(key, data, ttl, path)
  local req = { op = "put", key = key }
  if data ~= nil then req.data = data end
  if ttl        then req.ttl  = ttl  end
  if path       then req.path = path end
  local resp = self:_send(req)
  if not resp.ok then return false, resp.error end
  return true
end

--- Clear a virtual provider entry (put with null data).
--
-- @param key  string   Provider key
-- @param path string?  Optional working-directory path
-- @return true, or false, error_message
function Client:put_null(key, path)
  return self:put(key, nil, nil, path)
end

--- Introspect daemon internals.
--
-- @param subject       string   Subject to inspect (e.g. "daemon", "cache", "providers")
-- @param duration_secs number?  Optional observation window in seconds
-- @return table {subject, daemon, other}, or nil, error_message
function Client:introspect(subject, duration_secs)
  local req = { op = "introspect", subject = subject }
  if duration_secs then req.duration_secs = duration_secs end
  local resp = self:_send(req)
  if not resp.ok then return nil, resp.error end
  if subject == "daemon" and type(resp.data) == "table" then
    return { subject = "daemon", daemon = resp.data, other = nil }
  end
  return { subject = subject, daemon = nil, other = resp.data }
end

--- Subscribe to changes for a key, returning a WatchStream.
--
-- Opens a dedicated backend connection (via the backend_factory provided at
-- construction time) so the main connection remains available. Falls back to
-- re-using self._sock when no factory is set (useful in tests).
--
-- @param key  string   Provider key
-- @param path string?  Optional working-directory path
-- @return WatchStream, or nil, error_message
function Client:watch(key, path)
  local backend
  if self._backend_factory then
    local b, err = self._backend_factory()
    if not b then return nil, err end
    backend = b
  else
    -- Fallback for tests: reuse the existing socket.
    backend = self._sock
  end
  local req = { op = "watch", key = key }
  if path then req.path = path end
  local encoded = json.encode(req)
  backend:send_line(encoded .. "\n")
  return WatchStream.new(backend)
end

--- Close the underlying socket connection.
function Client:close()
  self._sock:close()
end

-- Export both Client and Result so tests can reference Result directly.
return {
  Client = Client,
  Result = Result,
}

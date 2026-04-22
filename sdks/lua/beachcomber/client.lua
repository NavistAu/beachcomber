--- Client implementation for the beachcomber daemon.
--
-- Wraps a socket handle and exposes the five protocol operations:
-- get, refresh, context, list, status.

local json = require("beachcomber.json")

local Client = {}
Client.__index = Client

--- Construct a new Client.
-- @param socket_handle  A connected handle from socket_luasocket or socket_vim.
-- @return Client
function Client.new(socket_handle)
  return setmetatable({ _sock = socket_handle }, Client)
end

-- ── Internal helpers ─────────────────────────────────────────────────────────

local function send_request(sock, req_table)
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
  if resp.ok == false then
    return nil, resp.error or "server returned ok=false"
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

--- List available providers.
--
-- @return table[] array of provider descriptors, or nil, error_message
--   Each descriptor has: name (string), global (boolean), fields (string[])
function Client:list()
  local resp, err = send_request(self._sock, { op = "list" })
  if not resp then
    return nil, err
  end
  return resp.data
end

--- Return daemon status information.
--
-- @return table status data, or nil, error_message
function Client:status()
  local resp, err = send_request(self._sock, { op = "status" })
  if not resp then
    return nil, err
  end
  return resp.data
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

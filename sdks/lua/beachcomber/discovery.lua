--- Socket path discovery for the beachcomber daemon.
--
-- Discovery order:
-- 1. $XDG_RUNTIME_DIR/beachcomber/sock
-- 2. $TMPDIR/beachcomber-<uid>/sock
-- 3. /tmp/beachcomber-<uid>/sock

local M = {}

--- Return the effective user ID.
-- Uses io.popen("id -u") which works on macOS and Linux.
-- @return number uid, or nil, error_message
function M.get_uid()
  local f, err = io.popen("id -u 2>/dev/null")
  if not f then
    return nil, "id -u failed: " .. (err or "unknown")
  end
  local out = f:read("*l")
  f:close()
  if not out then
    return nil, "id -u produced no output"
  end
  local uid = tonumber(out)
  if not uid then
    return nil, "id -u returned non-numeric: " .. tostring(out)
  end
  return uid
end

--- Check whether a file/socket exists.
-- Uses a neutral approach compatible with Lua 5.1+.
-- @param path string
-- @return boolean
local function path_exists(path)
  local f = io.open(path, "r")
  if f then
    f:close()
    return true
  end
  return false
end

--- Return the expected socket path for the running daemon.
--
-- Checks standard locations in order. Returns the first candidate path
-- according to the discovery rules. Callers are responsible for verifying
-- the socket is actually reachable.
--
-- @return string path, or nil, error_message
function M.discover_socket_path()
  local xdg = os.getenv("XDG_RUNTIME_DIR")
  if xdg and xdg ~= "" then
    local candidate = xdg .. "/beachcomber/sock"
    if path_exists(candidate) then
      return candidate
    end
  end

  local uid, err = M.get_uid()
  if not uid then
    return nil, "socket path discovery: could not determine uid: " .. err
  end

  local tmpdir = os.getenv("TMPDIR") or "/tmp"
  -- Strip trailing slashes
  tmpdir = tmpdir:gsub("/+$", "")
  return tmpdir .. "/beachcomber-" .. uid .. "/sock"
end

return M

--- Socket path discovery for the beachcomber daemon.
--
-- Mirrors the daemon's bind-path resolution (Config::resolve_socket_path),
-- minus the config-file step which is daemon-only. Discovery order:
-- 1. $BEACHCOMBER_SOCKET  (if set and non-empty)
-- 2. /tmp/beachcomber-<uid>/sock
--
-- There is no existence probe and no session-scoped environment is
-- consulted ($TMPDIR, $XDG_RUNTIME_DIR): the result is the single stable
-- per-user path the daemon binds for the same environment. Non-standard
-- setups point clients at the daemon via BEACHCOMBER_SOCKET.

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

--- Return the expected socket path for the running daemon.
--
-- Resolves to the single path the daemon binds for the current environment.
-- Callers are responsible for verifying the socket is actually reachable.
--
-- @return string path, or nil, error_message
function M.discover_socket_path()
  local sock = os.getenv("BEACHCOMBER_SOCKET")
  if sock and sock ~= "" then
    return sock
  end

  local uid, err = M.get_uid()
  if not uid then
    return nil, "socket path discovery: could not determine uid: " .. err
  end

  return "/tmp/beachcomber-" .. uid .. "/sock"
end

return M

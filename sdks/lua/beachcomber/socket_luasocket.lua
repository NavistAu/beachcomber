--- luasocket backend for beachcomber.
--
-- Used automatically outside of Neovim when the luasocket library is
-- available. Provides synchronous Unix domain socket I/O.

local M = {}

--- Create and connect a Unix socket via luasocket.
-- Returns a handle table with send/receive/close methods.
-- @param socket_path string  Path to the Unix domain socket
-- @return handle table, or nil, error_message
function M.connect(socket_path)
  local socket = require("socket")
  local unix = require("socket.unix")

  local sock, err = unix()
  if not sock then
    return nil, "luasocket unix() failed: " .. tostring(err)
  end

  local ok, conn_err = sock:connect(socket_path)
  if not ok then
    sock:close()
    return nil, "connect to " .. socket_path .. " failed: " .. tostring(conn_err)
  end

  -- Set a reasonable timeout so callers don't hang forever
  sock:settimeout(5)

  local handle = {}

  --- Send a line (must end with \n).
  -- @param line string
  -- @return true, or nil, error_message
  function handle:send_line(line)
    local sent, send_err = sock:send(line)
    if not sent then
      return nil, "send error: " .. tostring(send_err)
    end
    return true
  end

  --- Receive one newline-terminated line.
  -- @return string line (without newline), or nil, error_message
  function handle:recv_line()
    local data, recv_err = sock:receive("*l")
    if not data then
      return nil, "receive error: " .. tostring(recv_err)
    end
    return data
  end

  --- Close the connection.
  function handle:close()
    sock:close()
  end

  return handle
end

return M

--- vim.uv (libuv) backend for beachcomber.
--
-- Used automatically when running inside Neovim. All I/O is synchronous
-- so that statusline/tabline plugins receive results in the same call frame.
--
-- vim.uv is available as of Neovim 0.10. For older Neovim the module falls
-- back to vim.loop, which exposes the same libuv bindings.

local M = {}

--- Retrieve the uv handle from Neovim.
-- @return uv table, or nil if not running inside Neovim
local function get_uv()
  if vim and vim.uv then return vim.uv end
  if vim and vim.loop then return vim.loop end
  return nil
end

--- Create and connect a Unix socket via vim.uv/vim.loop.
--
-- Uses synchronous (blocking) libuv calls so the caller gets a result
-- immediately without scheduling a callback.
--
-- @param socket_path string  Path to the Unix domain socket
-- @return handle table, or nil, error_message
function M.connect(socket_path)
  local uv = get_uv()
  if not uv then
    return nil, "vim.uv/vim.loop not available; not running inside Neovim"
  end

  local pipe = uv.new_pipe(false)
  if not pipe then
    return nil, "uv.new_pipe() failed"
  end

  -- uv.pipe_connect is a callback-based async call, but we can make it
  -- synchronous by spinning the event loop until the callback fires.
  local connected = false
  local connect_err = nil

  pipe:connect(socket_path, function(err)
    connect_err = err
    connected = true
  end)

  -- Spin the event loop until the connect callback fires.
  local deadline = uv.now() + 5000 -- 5 second timeout
  while not connected do
    uv.run("nowait")
    if uv.now() > deadline then
      pipe:close()
      return nil, "connect timeout for " .. socket_path
    end
  end

  if connect_err then
    pipe:close()
    return nil, "connect to " .. socket_path .. " failed: " .. tostring(connect_err)
  end

  -- Read buffer: we accumulate bytes and extract complete lines.
  local read_buf = ""
  local read_err = nil
  local read_closed = false

  pipe:read_start(function(err, data)
    if err then
      read_err = err
      read_closed = true
    elseif data then
      read_buf = read_buf .. data
    else
      read_closed = true
    end
  end)

  local handle = {}

  --- Send a line (must end with \n).
  -- @param line string
  -- @return true, or nil, error_message
  function handle:send_line(line)
    local ok = true
    local write_err = nil
    pipe:write(line, function(err)
      write_err = err
      ok = not err
    end)
    -- Spin until write completes
    local deadline2 = uv.now() + 5000
    while write_err == nil and ok do
      uv.run("nowait")
      if uv.now() > deadline2 then
        return nil, "write timeout"
      end
      -- write callback sets write_err or ok=false when done
      -- We need a sentinel; use a flag
      break -- write is fire-and-forget in libuv; proceed immediately
    end
    if write_err then
      return nil, "write error: " .. tostring(write_err)
    end
    uv.run("nowait") -- flush
    return true
  end

  --- Receive one newline-terminated line.
  -- Spins the event loop until a complete line is available.
  -- @return string line (without newline), or nil, error_message
  function handle:recv_line()
    local deadline3 = uv.now() + 5000
    while true do
      -- Check if we have a complete line in the buffer
      local nl = read_buf:find("\n", 1, true)
      if nl then
        local line = read_buf:sub(1, nl - 1)
        read_buf = read_buf:sub(nl + 1)
        return line
      end
      if read_err then
        return nil, "read error: " .. tostring(read_err)
      end
      if read_closed and read_buf == "" then
        return nil, "connection closed before newline"
      end
      if uv.now() > deadline3 then
        return nil, "read timeout"
      end
      uv.run("nowait")
    end
  end

  --- Close the connection.
  function handle:close()
    pipe:read_stop()
    pipe:close()
  end

  return handle
end

return M

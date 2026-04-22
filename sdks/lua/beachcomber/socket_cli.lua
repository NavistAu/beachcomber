--- CLI fallback backend for beachcomber.
--
-- Used when neither vim.uv nor luasocket is available. Shells out to the
-- `comb` binary for each operation. Unlike the socket backends, this does
-- NOT hold a persistent connection — each send_line/recv_line pair spawns
-- a new `comb` process. The "connection" is stateless.
--
-- Limitations:
--   - set_context is a no-op (each call is a separate process)
--   - Slightly higher latency per call (process spawn overhead)
--   - Requires `comb` to be on PATH

local json = require("beachcomber.json")

local M = {}

--- Find the comb binary on PATH.
-- @return string|nil path to comb binary
local function find_comb()
  local path = os.getenv("PATH") or ""
  for dir in path:gmatch("[^:]+") do
    local candidate = dir .. "/comb"
    local f = io.open(candidate, "r")
    if f then
      f:close()
      return candidate
    end
  end
  return nil
end

--- Shell-escape a string for safe use in a command.
-- @param s string
-- @return string
local function shell_escape(s)
  if not s then return "''" end
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

--- Create a CLI-based handle.
-- The socket_path argument is accepted for interface compatibility but
-- is not used — the comb binary handles socket discovery itself.
-- @param socket_path string  (ignored)
-- @return handle table, or nil, error_message
function M.connect(socket_path)
  local comb_bin = find_comb()
  if not comb_bin then
    return nil, "comb binary not found on PATH"
  end

  -- Track pending request so recv_line knows what command to run
  local pending_request = nil

  local handle = {}

  --- Buffer a request. The actual execution happens in recv_line.
  -- @param line string  JSON request (with trailing newline)
  -- @return true
  function handle:send_line(line)
    -- Strip trailing newline and parse
    local trimmed = line:gsub("%s+$", "")
    local req, err = json.decode(trimmed)
    if not req then
      pending_request = { error = "failed to parse request: " .. tostring(err) }
      return true
    end
    pending_request = req
    return true
  end

  --- Execute the buffered request via `comb` and return the response.
  -- @return string JSON response line, or nil, error_message
  function handle:recv_line()
    if not pending_request then
      return nil, "no pending request"
    end

    local req = pending_request
    pending_request = nil

    if req.error then
      return json.encode({ ok = false, error = req.error })
    end

    local op = req.op

    if op == "get" then
      -- Use JSON format so we get the full structured response
      local cmd = comb_bin .. " get " .. shell_escape(req.key)
      if req.path then
        cmd = cmd .. " " .. shell_escape(req.path)
      end
      cmd = cmd .. " -f json 2>/dev/null"
      local pipe = io.popen(cmd, "r")
      if not pipe then
        return json.encode({ ok = false, error = "failed to run comb" })
      end
      local output = pipe:read("*a")
      local success = pipe:close()
      if not output or output == "" then
        -- Empty output with a successful exit = cache miss.
        -- Empty output with a failing exit = command-level failure; propagate.
        if success then
          return json.encode({ ok = true })
        end
        return json.encode({ ok = false, error = "comb get failed" })
      end
      -- The CLI outputs pretty-printed JSON; return it as-is (it's valid JSON)
      -- But we need a single line, so strip newlines.
      -- Non-empty output is returned as-is regardless of exit status; `comb get -f json`
      -- is expected to exit 0 whenever it writes JSON (hits), and to produce empty
      -- output on misses and on I/O failures (handled in the empty-output branch above).
      return output:gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")

    elseif op == "refresh" then
      -- The `comb refresh` subcommand was removed; use `comb get --force` to
      -- trigger a fresh provider execution with equivalent semantics.
      local cmd = comb_bin .. " get --force " .. shell_escape(req.key)
      if req.path then
        cmd = cmd .. " " .. shell_escape(req.path)
      end
      cmd = cmd .. " >/dev/null 2>&1"
      local pipe = io.popen(cmd, "r")
      if not pipe then
        return json.encode({ ok = false, error = "failed to run comb" })
      end
      pipe:read("*a")
      local success = pipe:close()
      if not success then
        return json.encode({ ok = false, error = "comb get --force failed" })
      end
      return json.encode({ ok = true })

    elseif op == "status" then
      local cmd = comb_bin .. " status 2>/dev/null"
      local pipe = io.popen(cmd, "r")
      if not pipe then
        return json.encode({ ok = false, error = "failed to run comb" })
      end
      local output = pipe:read("*a")
      pipe:close()
      if not output or output == "" then
        return json.encode({ ok = true, data = {} })
      end
      return output:gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")

    elseif op == "context" then
      -- Context is a no-op for CLI backend — each call is a separate process
      return json.encode({ ok = true })

    elseif op == "hello" then
      -- CLI fallback cannot interrogate the daemon for version information.
      return json.encode({ ok = false, error = "hello not supported in CLI fallback" })

    elseif op == "put" then
      -- CLI fallback does not support writing virtual cache entries.
      return json.encode({ ok = false, error = "put not supported in CLI fallback" })

    elseif op == "introspect" then
      -- CLI fallback cannot perform structured daemon introspection.
      return json.encode({ ok = false, error = "introspect not supported in CLI fallback" })

    elseif op == "watch" then
      -- CLI fallback has no persistent connection; watch is not supported.
      return json.encode({ ok = false, error = "watch not supported in CLI fallback" })

    else
      return json.encode({ ok = false, error = "unsupported op: " .. tostring(op) })
    end
  end

  --- No-op for CLI backend (no persistent connection).
  function handle:close()
  end

  return handle
end

return M

--- beachcomber — Lua client SDK for the beachcomber daemon.
--
-- Provides a single entry point that auto-detects the best socket backend
-- (vim.uv when inside Neovim, luasocket otherwise) and returns a connected
-- Client ready for use.
--
-- Basic usage:
--
--   local comb = require('beachcomber')
--   local client = comb.connect()
--   local result = client:get('git.branch', '/my/repo')
--   if result:is_hit() then
--       print(result.data)
--   end
--   client:close()
--
-- Supports Lua 5.1+ (including LuaJIT / Neovim).

local discovery = require("beachcomber.discovery")
local client_mod = require("beachcomber.client")

local M = {}

local RETRY_BACKOFFS = { 0.250, 0.500, 1.000 }

--- Connect to a Unix socket backend with 3 retries (250ms/500ms/1s exponential).
--
-- Retries when the backend returns nil (connection refused / socket absent).
-- Intended to cover the brief restart window when the daemon is restarting.
--
-- @param backend table  Backend module with a connect(socket_path) function
-- @param socket_path string  Path to the Unix domain socket
-- @return handle, or nil, error_message
function M._connect_with_retry(backend, socket_path)
  local last_err = nil
  -- Try luasocket sleep if available; fall back to os.execute for portability.
  local function sleep_secs(s)
    local ok, socket_lib = pcall(require, "socket")
    if ok and socket_lib.sleep then
      socket_lib.sleep(s)
    else
      -- Portable busy-wait fallback (coarse).
      local t = os.time()
      while os.time() - t < math.ceil(s) do end
    end
  end

  for _, backoff in ipairs(RETRY_BACKOFFS) do
    local handle, err = backend.connect(socket_path)
    if handle then return handle end
    last_err = err
    sleep_secs(backoff)
  end
  -- Final attempt.
  return backend.connect(socket_path)
end

--- Detect whether we are running inside Neovim.
-- @return boolean
local function in_neovim()
  return vim ~= nil and (vim.uv ~= nil or vim.loop ~= nil)
end

--- Load the appropriate socket backend.
-- @return backend module, or nil, error_message
local function load_backend()
  if in_neovim() then
    local ok, mod = pcall(require, "beachcomber.socket_vim")
    if ok then return mod end
    return nil, "failed to load vim.uv backend: " .. tostring(mod)
  end

  -- Try luasocket
  local ls_ok = pcall(require, "socket")
  if ls_ok then
    local ok, mod = pcall(require, "beachcomber.socket_luasocket")
    if ok then return mod end
  end

  -- Fall back to CLI backend (shells out to `comb` binary)
  local cli_ok, cli_mod = pcall(require, "beachcomber.socket_cli")
  if cli_ok then return cli_mod end

  return nil, "no socket backend available (install luasocket, add comb to PATH, or run inside Neovim)"
end

--- Connect to the beachcomber daemon and return a Client.
--
-- @param opts table?  Optional configuration:
--   opts.socket_path  string  Override the socket path (skips discovery)
--   opts.backend      module  Override the backend module entirely
--
-- @return Client, or nil, error_message
function M.connect(opts)
  opts = opts or {}

  local socket_path
  if opts.socket_path then
    socket_path = opts.socket_path
  else
    local disc_path, disc_err = discovery.discover_socket_path()
    if not disc_path then
      return nil, "socket discovery failed: " .. tostring(disc_err)
    end
    socket_path = disc_path
  end

  local backend
  if opts.backend then
    backend = opts.backend
  else
    local b, b_err = load_backend()
    if not b then
      return nil, b_err
    end
    backend = b
  end

  local sock, conn_err = M._connect_with_retry(backend, socket_path)
  if not sock then
    return nil, "could not connect to beachcomber daemon at " .. socket_path .. ": " .. tostring(conn_err)
  end

  return client_mod.Client.new(sock)
end

-- Re-export sub-modules for advanced use
M.discovery   = discovery
M.json        = require("beachcomber.json")
M.Client      = client_mod.Client
M.Result      = client_mod.Result
M.WatchStream = require("beachcomber.watch_stream")

return M

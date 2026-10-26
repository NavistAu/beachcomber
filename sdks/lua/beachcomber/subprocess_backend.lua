--- Backend implementing the beachcomber.client backend interface by
--- shelling out to the `comb` binary.
--
-- This is the sanctioned fallback for PUC Lua, which has no `ffi` and so
-- cannot call into the cdylib directly (see docs/superpowers/plans/
-- 2026-08-15-client-abi-and-sdk-refactor.md, Task 4.4: "declare PUC Lua
-- supported only through the subprocess fallback"). It is entered only
-- when `ffi` is unavailable *by design* (beachcomber/init.lua's transport
-- selection) — never as a silent fallback from a broken ffi discovery.
--
-- Every op here is roughly 5ms (process spawn) instead of ~0.3ms (a direct
-- library call); `Client:transport()` reports "subprocess" so a caller can
-- see this in a profile rather than discover it there.
--
-- Capability ceiling: `comb` has no CLI surface for `hello` or `introspect`
-- (no subcommand exposes protocol/daemon version or a structured
-- introspection query). Those methods are absent from this backend table;
-- beachcomber.client turns a missing method into an Error.unsupported()
-- naming this transport, and a conformance runner skips (never silently
-- passes) a fixture that needs one.
--
-- A session is this backend's own: there is no connection to keep between
-- spawns, so `session_open` hands back a handle that remembers its
-- directory (`set_context`) and passes it as `--path` to each `get`, `put`
-- and `refresh` that names none — what the ffi tier's `bc_session_*` do,
-- and what the Node SDK's subprocess session does.
--
-- `status` is one `comb status -f json`; a failure to run it is a
-- `server_error` carrying the CLI's stderr, never an empty list.
--
-- Timeouts: `io.popen` cannot bound or kill a spawned `comb`, and `comb`
-- has no flag for a connect budget, so `timeout_ms` and `connect_timeout_ms`
-- cannot be honoured here. `M.new` refuses either with an "unsupported"
-- Error naming the ffi transport, rather than ignoring it.
--
-- `refresh` is one spawn of `comb refresh <key> [--path P | --global]`: the
-- daemon's `refresh` op itself, which re-executes only the Sources the key
-- demands and refuses a key with none behind it. `comb refresh` prints the
-- daemon's refusal alone on stderr, so that line is the error message here
-- — the same text a `server_error` envelope carries over the ffi tier.
--
-- `get` is one spawn: `comb get -f json <key> [--path P | --global]
-- [--force] [--wait=<wait>]`, the value `comb get` prints. It renders
-- through the same libbeachcomber::resolve the ffi tier's bc_get does, with
-- everything the daemon does for a `get`: the directory chain up to `$HOME`,
-- `$HOME` itself as the global entry, `--force`, the wait as given, and the
-- daemon's refusals. A wait comb cannot read is its usage error, which is the
-- library's "parse_error" in the library's words, as over ffi; a wait that
-- is not a string is the "parse_error" the ABI gives one.
-- A facet key reads like any other (`p:template.x` is the template as
-- stored).
--
-- The spawned `comb` is made to render as the ffi tier does:
--
--   * Environment. A call's `env` map is the spawned comb's whole
--     environment: the command runs as `env -i K=V …`, every pair
--     shell-escaped, so `env.*` and an `exec()` child see exactly the map.
--     With no map the spawn inherits this process's environment, which is
--     what the ffi tier renders with too. The command is the absolute path
--     to comb, since `env -i` leaves no `$PATH` to find it by.
--   * Socket. `--socket <p>` whenever the client has a `socket_path` or this
--     process has `$BEACHCOMBER_SOCKET`: the order `bc_socket_path` picks in.
--     The map can therefore never point the tier at another daemon.
--   * Colour. `--color never` when the render's environment sets a non-empty
--     `NO_COLOR`, else `--color always` (the template filters would otherwise
--     draw nothing into a pipe): the rule `EngineCtx::colour` has.
--   * `exec()`. `--no-exec` unless the client was made with `exec = true`.
--
-- `eval` is one spawn of `comb eval -f json [--path P | --global] --
-- <template>`, spawned as `get` is. Exit 0 is an answer: the value, or
-- `null` when the template renders nothing. Exit 2 is an error, in the CLI's
-- words; unlike the ffi tier this one cannot tell a template that does not
-- compile (`parse_error`) from one that fails to render, so both are
-- `server_error`.
--
-- `watch` stays refused: `io.popen` has no timed read and no cancel, so
-- `next_event(timeout_ms)` and `cancel` could not be honoured.

local json = require("beachcomber.json")
local discovery = require("beachcomber.discovery")
local Error = require("beachcomber.error")

local Backend = {}
Backend.__index = Backend

local function shell_escape(s)
  if s == nil then return "''" end
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

--- The command line for one spawn: `args` (already escaped, may be empty)
-- after comb's global flags, run under `env` alone when a map is given (the
-- header comment). `rendering` adds `--color` and `--no-exec`.
local function command(self, args, env, rendering)
  local parts = {}
  if env ~= nil then
    local names = {}
    for name in pairs(env) do names[#names + 1] = name end
    table.sort(names)
    parts[#parts + 1] = "env -i"
    for _, name in ipairs(names) do
      parts[#parts + 1] = shell_escape(name .. "=" .. tostring(env[name]))
    end
  end
  parts[#parts + 1] = shell_escape(self._bin)
  if self._socket then
    parts[#parts + 1] = "--socket " .. shell_escape(self._socket)
  end
  if rendering then
    local no_colour
    if env ~= nil then no_colour = env.NO_COLOR else no_colour = os.getenv("NO_COLOR") end
    parts[#parts + 1] = "--color " .. ((no_colour ~= nil and no_colour ~= "") and "never" or "always")
    if not self._exec then parts[#parts + 1] = "--no-exec" end
  end
  return table.concat(parts, " ") .. args
end

--- The `--path`/`--global` argument for a call scoped to `path`.
--
-- An absent path means the global entry, exactly as it does on the wire
-- (where the request simply carries no `path` field) — but `comb get` and
-- `comb put` default to the calling process's cwd, so the global entry has
-- to be asked for by name or this transport would silently scope every
-- pathless call to wherever the SDK's host happens to be running.
local function path_arg(path)
  if path == nil then return " --global" end
  return " --path " .. shell_escape(path)
end

--- Run a command, returning (stdout, exit_ok, stderr).
--
-- Exit status is read back via an appended `echo` marker rather than
-- `pipe:close()`'s return value: LuaJIT's `io.popen():close()` has been
-- observed to return `true` regardless of the child's actual exit code on
-- this platform, so it cannot be trusted to detect failure. stderr goes to
-- a temporary file so `get` can read the CLI's per-key `error: <key>: `
-- lines apart from the JSON on stdout.
local function run(cmd)
  local errfile = os.tmpname()
  local pipe = io.popen(cmd .. " 2>" .. shell_escape(errfile) .. "; echo BC_EXIT:$?", "r")
  if not pipe then
    os.remove(errfile)
    return nil, false, "failed to spawn: " .. cmd
  end
  local out = pipe:read("*a") or ""
  pipe:close()
  local err = ""
  local ef = io.open(errfile, "r")
  if ef then
    err = ef:read("*a") or ""
    ef:close()
  end
  os.remove(errfile)
  local body, code = out:match("^(.-)\n?BC_EXIT:(%d+)%s*$")
  if not body then
    return out, false, err
  end
  return body, code == "0", err
end

--- The rest of the first line of `stderr` that starts with `prefix`, or nil.
--
-- `comb get` prints one `error: <k>: <msg>` line per key that failed: the
-- daemon's refusal, or the resolver's own error.
local function line_after(stderr, prefix)
  for line in stderr:gmatch("[^\n]+") do
    if line:sub(1, #prefix) == prefix then
      return line:sub(#prefix + 1)
    end
  end
  return nil
end

--- Parse `comb status -f json` output (one JSON object per line) into an
-- array of row tables.
--
-- Each row's `path` field is JSON null (not omitted) for a globally-scoped
-- entry — confirmed directly against a live daemon (`comb status -f json`
-- emits `"path":null,...`). json.decode turns that into the json.NULL
-- sentinel rather than Lua nil (see beachcomber.json's M.NULL doc comment),
-- so it is collapsed back to nil right here: plain nil means "no path
-- scope", the same way it always has.
local function status_rows(backend)
  local out, ok, err = run(command(backend, " status -f json", nil, true))
  if not ok then
    return nil, ((err or ""):gsub("%s+$", ""))
  end
  local rows = {}
  for line in (out or ""):gmatch("[^\n]+") do
    local row = json.decode(line)
    if type(row) == "table" then
      if row.path == json.NULL then row.path = nil end
      rows[#rows + 1] = row
    end
  end
  return rows
end

local M = {}

--- @param opts table?  opts.comb_bin overrides `comb` discovery (advanced/tests);
--                       opts.socket_path is forwarded to every spawned `comb`
--                       invocation as `$BEACHCOMBER_SOCKET` — standard Lua has
--                       no `setenv`, so this backend prefixes each command
--                       line rather than mutating the process environment.
-- @return Backend, or nil, Error
function M.new(opts)
  opts = opts or {}
  for _, option in ipairs({ "timeout_ms", "connect_timeout_ms" }) do
    if opts[option] ~= nil then
      return nil, Error.new("unsupported", option .. " is not supported over the subprocess transport: "
        .. "io.popen cannot bound a spawned comb; the ffi transport (LuaJIT) honours it")
    end
  end
  local comb_bin = opts.comb_bin or discovery.find_comb_on_path()
  if not comb_bin then
    return nil, Error.new(
      "library_not_found",
      "comb binary not found on $PATH (required for the subprocess transport)"
    )
  end
  -- Absolute, where this process's $PATH finds it: a spawn under `env -i`
  -- has no $PATH of its own.
  if not comb_bin:find("/", 1, true) then
    comb_bin = discovery.find_on_path(comb_bin) or comb_bin
  end
  local socket = opts.socket_path or os.getenv("BEACHCOMBER_SOCKET")
  if socket == "" then socket = nil end
  return setmetatable({ _bin = comb_bin, _socket = socket, _exec = opts.exec == true }, Backend)
end

function Backend:name()
  return "subprocess"
end

--- One `comb get -f json <key>` (the header comment). Exit 0 is an answer:
-- the value on stdout, or `null` for a miss. Exit 2 is an error, reported as
-- `error: <who>: <why>`: an `error: <key>: ` line is the daemon's refusal of
-- `key` or the resolver's own error (a template that does not compile, a
-- cycle, a failed `exec()`), an `error: <key>.<leaf>: ` line a leaf of a
-- group failing, and anything else the CLI failing before it asked at all.
function Backend:get(key, cwd, env, options)
  options = options or {}
  local wait = options.wait
  if wait ~= nil and type(wait) ~= "string" then
    -- What bc_get's options_json says of a wait that is not a string.
    return { ok = false, error = { kind = "parse_error",
      message = "malformed options_json: invalid type: " .. type(wait) .. ", expected a string" } }
  end
  local cmd = command(self, " get -f json " .. shell_escape(key) .. path_arg(cwd), env, true)
  if options.force then cmd = cmd .. " --force" end
  -- `=` keeps a wait that starts with `-` the flag's value.
  if wait ~= nil then cmd = cmd .. " " .. shell_escape("--wait=" .. wait) end
  local out, ok, err = run(cmd)

  if not ok then
    err = err or ""
    -- comb's usage error for `--wait`: the library's words after the flag.
    local refused = err:match("error: invalid value '[^\n]*' for '%-%-wait <[^>]*>': ([^\n]*)")
    if refused then
      return { ok = false, error = { kind = "parse_error", message = refused } }
    end
    local own = line_after(err, "error: " .. key .. ": ")
    if own then
      return { ok = false, error = { kind = "server_error", message = own } }
    end
    local message
    local leaf_prefix = "error: " .. key
    for line in err:gmatch("[^\n]+") do
      local rest = line:sub(#leaf_prefix + 1, #leaf_prefix + 1)
      if line:sub(1, #leaf_prefix) == leaf_prefix and (rest == "." or rest == "[") then
        message = line:sub(#"error: " + 1)
        break
      end
    end
    if not message then message = (err:gsub("%s+$", "")) end
    return { ok = false, error = { kind = "server_error", message = message } }
  end

  local value = json.decode(out or "")
  if value == nil then
    return { ok = false, error = { kind = "parse_error", message = "malformed comb get output: " .. tostring(out) } }
  end
  if value == json.NULL then value = nil end -- a top-level null is absent, as the ffi backend collapses it
  return { ok = true, data = value }
end

--- One `comb eval -f json` (the header comment).
function Backend:eval(template, cwd, env)
  local cmd = command(self, " eval -f json" .. path_arg(cwd) .. " -- " .. shell_escape(template), env, true)
  local out, ok, err = run(cmd)
  if not ok then
    err = (err or ""):gsub("%s+$", "")
    -- The CLI failed before answering at all: no daemon, a bad argument.
    if err == "" or err:sub(1, #"error: daemon:") == "error: daemon:" then
      return { ok = false, error = { kind = "server_error", message = err ~= "" and err or "comb eval failed" } }
    end
    local first = err:match("^[^\n]*")
    first = first:gsub("^error: <eval>: ", ""):gsub("^error: ", "")
    return { ok = false, error = { kind = "server_error", message = first } }
  end
  local value = json.decode(out or "")
  if value == nil then
    return { ok = false, error = { kind = "parse_error", message = "malformed comb eval output: " .. tostring(out) } }
  end
  if value == json.NULL then value = nil end
  return { ok = true, data = value }
end

function Backend:put(key, data, ttl, path)
  local encoded, enc_err = json.encode(data)
  if not encoded then
    return { ok = false, error = { kind = "parse_error", message = "json encode error: " .. tostring(enc_err) } }
  end
  local cmd = command(self, " put " .. shell_escape(key) .. " " .. shell_escape(encoded) .. path_arg(path))
  if ttl then cmd = cmd .. " --ttl " .. shell_escape(ttl) end
  local _, ok, err = run(cmd)
  if not ok then
    return { ok = false, error = { kind = "server_error", message = (err or ""):gsub("%s+$", "") } }
  end
  return { ok = true }
end

--- One `comb refresh`: exit 0 is the daemon's ok; exit 2 is its refusal,
-- printed alone on stderr as `error: <key>: <words>` — `nothing to refresh:
-- <key> has no sources` for a key with no Source behind it — or the CLI's
-- own `error: daemon: …` when it could not reach the daemon. `comb get --force` is not a stand-in: it selects the
-- same Sources but then *reads*, so it answers a sourceless key with a
-- different refusal and an unowned field with a plain miss.
function Backend:refresh(key, path)
  local cmd = command(self, " refresh " .. shell_escape(key) .. path_arg(path))
  local _, ok, err = run(cmd)
  if not ok then
    err = err or ""
    local message = line_after(err, "error: " .. key .. ": ") or err:gsub("%s+$", "")
    return { ok = false, error = { kind = "server_error", message = message } }
  end
  return { ok = true }
end

function Backend:status()
  local rows, err = status_rows(self)
  if not rows then
    return { ok = false, error = { kind = "server_error", message = err ~= "" and err or "comb status failed" } }
  end
  return { ok = true, data = rows }
end

-- ── Session: a directory remembered between spawns (the header comment) ──

local Session = {}
Session.__index = Session

function Backend:session_open()
  return setmetatable({ _backend = self, _path = nil }, Session)
end

function Session:set_context(path)
  self._path = path
  return { ok = true }
end

function Session:get(key, cwd, env, options)
  if cwd == nil then cwd = self._path end
  return self._backend:get(key, cwd, env, options)
end

function Session:put(key, data, ttl, path)
  if path == nil then path = self._path end
  return self._backend:put(key, data, ttl, path)
end

function Session:refresh(key, path)
  if path == nil then path = self._path end
  return self._backend:refresh(key, path)
end

function Session:close()
  -- Nothing held between spawns.
end

-- hello / introspect / watch_open intentionally absent.
--
-- hello / introspect: no CLI surface exposes them.
--
-- watch: `io.popen` has no timed read and no cancel, so `next_event(timeout_ms)`
-- and `cancel` could not be honoured. (The old reason given here, block
-- buffering of a piped `comb watch`, was wrong: its stdout is line-buffered,
-- and the hang seen was a key that missed, which printed nothing.)
--
-- beachcomber.client turns each missing method into
-- Error.unsupported("<name>", "subprocess").

function Backend:close()
  -- Stateless: nothing to release between calls.
end

return M

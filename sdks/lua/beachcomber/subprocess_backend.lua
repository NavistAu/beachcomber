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
-- introspection query). `eval` is not wired over this tier: a spawned
-- `comb eval <template> <cwd>` would render through the same
-- libbeachcomber::resolve the ffi tier's bc_eval does, but with its own
-- environment, the same limit `get` documents below. These methods are
-- absent from this backend table; beachcomber.client turns a missing method
-- into an Error.unsupported() naming this transport, and a conformance
-- runner skips (never silently passes) a fixture that needs one.
--
-- `refresh` is one spawn of `comb refresh <key> [--path P | --global]`: the
-- daemon's `refresh` op itself, which re-executes only the Sources the key
-- demands and refuses a key with none behind it. `comb refresh` prints the
-- daemon's refusal alone on stderr, so that line is the error message here
-- — the same text a `server_error` envelope carries over the ffi tier.
--
-- `get` is one spawn: `comb get -f json <key> [--path P | --global]
-- [--force] [--wait]`, the value `comb get` prints. It renders through the
-- same libbeachcomber::resolve the ffi tier's bc_get does, with everything
-- the daemon does for a `get`: the directory chain up to `$HOME`, `$HOME`
-- itself as the global entry, `--force`/`--wait` and the daemon's refusals.
-- A facet key reads like any other (`p:template.x` is the template as
-- stored).
--
-- Where it parts from the ffi tier: a spawned `comb` renders with its own
-- environment, and this process can only hand it whole. So a caller's
-- `env` cannot be honoured — `get` with one is an "unsupported" error
-- naming the ffi transport, and nothing is spawned — and a `get` without
-- one renders with this process's environment, where the ffi tier's has no
-- `env.*` at all. The CLI renders with `exec()` on, so the client's `exec`
-- option has no effect on this tier; a caller that must not spawn
-- processes from a template needs the ffi transport.

local json = require("beachcomber.json")
local discovery = require("beachcomber.discovery")
local Error = require("beachcomber.error")

local Backend = {}
Backend.__index = Backend

local function shell_escape(s)
  if s == nil then return "''" end
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
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
local function status_rows(comb_bin)
  local out, ok = run(comb_bin .. " status -f json")
  if not ok then
    return {}
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
  local comb_bin = opts.comb_bin or discovery.find_comb_on_path()
  if not comb_bin then
    return nil, Error.new(
      "library_not_found",
      "comb binary not found on $PATH (required for the subprocess transport)"
    )
  end
  local prefix = ""
  if opts.socket_path then
    prefix = "BEACHCOMBER_SOCKET=" .. shell_escape(opts.socket_path) .. " "
  end
  return setmetatable({ _comb = prefix .. comb_bin }, Backend)
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
function Backend:get(key, cwd, env, flags)
  if env ~= nil then
    return { ok = false, error = Error.new("unsupported",
      "get with an env is not supported over the subprocess transport: a spawned comb "
        .. "renders with its own environment; the ffi transport (LuaJIT) honours it") }
  end
  flags = flags or {}
  local cmd = self._comb .. " get -f json " .. shell_escape(key) .. path_arg(cwd)
  if flags.force then cmd = cmd .. " --force" end
  if flags.wait then cmd = cmd .. " --wait" end
  local out, ok, err = run(cmd)

  if not ok then
    err = err or ""
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

function Backend:put(key, data, ttl, path)
  local encoded, enc_err = json.encode(data)
  if not encoded then
    return { ok = false, error = { kind = "parse_error", message = "json encode error: " .. tostring(enc_err) } }
  end
  local cmd = self._comb .. " put " .. shell_escape(key) .. " " .. shell_escape(encoded) .. path_arg(path)
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
  local cmd = self._comb .. " refresh " .. shell_escape(key) .. path_arg(path)
  local _, ok, err = run(cmd)
  if not ok then
    err = err or ""
    local message = line_after(err, "error: " .. key .. ": ") or err:gsub("%s+$", "")
    return { ok = false, error = { kind = "server_error", message = message } }
  end
  return { ok = true }
end

function Backend:status()
  local rows = status_rows(self._comb)
  return { ok = true, data = rows }
end

-- hello / introspect / eval / watch_open intentionally absent.
--
-- hello / introspect: no CLI surface exposes them.
--
-- eval: not wired over this tier (see the header comment).
--
-- watch: `comb watch <key> -f json` was tried and rejected. Piped through
-- `io.popen` (not a tty), the daemon-CLI process's stdout is block- rather
-- than line-buffered, so `pipe:read("*l")` blocks indefinitely even though
-- the process has already written (and would eventually flush) the first
-- event — confirmed by hand against a live daemon: the same command run
-- attached to a real terminal prints its first line immediately. Shipping
-- that as "watch" would be a binding that silently hangs forever, which is
-- worse than the honest Error.unsupported() every other missing op here
-- gets. Buffering behaviour is CLI-side (out of scope: sdks/lua/** only).
--
-- beachcomber.client turns each missing method into
-- Error.unsupported("<name>", "subprocess").

function Backend:close()
  -- Stateless: nothing to release between calls.
end

return M

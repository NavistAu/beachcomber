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
-- Capability ceiling: `comb` has no CLI surface for `hello`, `refresh` or
-- `introspect` (no subcommand exposes protocol/daemon version, a bare
-- re-execution, or a structured introspection query). `resolve`/`eval` are
-- simply not implemented over this tier — `comb get --path <cwd> -f json`
-- and `comb eval <template> <cwd>` resolve through the same
-- libbeachcomber::resolve the ffi tier's bc_resolve/bc_eval do, so a
-- faithful shell-out exists; nobody has wired it yet. All five methods are
-- absent from this backend table; beachcomber.client turns a missing
-- method into an Error.unsupported() naming this transport, and a
-- conformance runner skips (never silently passes) a fixture that needs
-- one.
--
-- `get` is one spawn: `comb get -f json <key> <key>:age <key>:stale
-- [--path P | --global] [--force] [--wait]`. Multi-key `-f json` prints a
-- JSON array in key order, `null` in a missed key's slot, so the three
-- slots are the value, its age and its staleness — the resolver's own
-- answer, with everything the daemon does for a `get`: the directory chain
-- up to `$HOME`, `$HOME` itself as the global entry, metadata suffixes,
-- `--force`/`--wait` and the daemon's refusals. A key that already carries
-- a metadata suffix (`:age`, `:stale`, `:fresh`, `:cache`, `:source`) is
-- asked for alone and answers with age 0 and not stale, as the daemon
-- answers one over the wire.
--
-- The one divergence from the ffi tier: `comb get` *resolves*, so a
-- template-backed key returns its **rendered** value here where `bc_get`
-- returns the raw template half. A Result from this backend never carries
-- `template` or `rest`, and a template-only node is a hit holding the
-- rendered value rather than the "neither hit nor miss" the ffi tier
-- reports. The CLI renders with `exec()` on, so the client's `exec` option
-- has no effect on this tier; a caller that must not spawn processes from
-- a template, or that needs the raw template half, needs the ffi
-- transport. A rendered value has no cached freshness of its own (its
-- `:age`/`:stale` are misses), so it reports age 0 and not stale, as
-- libbeachcomber does for a hit whose freshness is absent.

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
-- a temporary file so `get` can read the CLI's per-key `Error querying`
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
-- `comb get` prints one `Error querying <k>: <msg>` line per key the daemon
-- answered with an error, and `<k>: <msg>` for a key the resolver failed
-- on. The `<key>:age` line starts `Error querying <key>:age: `, so a
-- prefix ending in `: ` never matches a companion's line.
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

local META_SUFFIXES = { age = true, stale = true, fresh = true, cache = true, source = true }

--- True when `key` ends in one of the daemon's metadata suffixes.
local function has_meta_suffix(key)
  local suffix = key:match(":([%w_-]+)$")
  return suffix ~= nil and META_SUFFIXES[suffix] == true
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

--- One `comb get -f json` for the value, its age and its staleness (the
-- header comment): exit 0 is a hit; exit 2 with the CLI's miss line for
-- `key` (`Error querying <key>: no data (cache miss)`) is a miss; exit 2
-- with another `Error querying <key>: ` line is that refusal, in the
-- daemon's words. Exit 2 with a null value slot and no such line is the
-- resolver's own error — a template that does not compile, a cycle, a
-- failed `exec()` — which the CLI prints as `<key>: <msg>`; a rendered
-- value is never null (a missing ref renders `""`), so that slot is never
-- a hit. Exit 2 with the value slot filled is a rendered value whose
-- `:age`/`:stale` companions missed on their own — a hit with age 0.
function Backend:get(key, path, flags)
  flags = flags or {}
  local meta = has_meta_suffix(key)
  local cmd = self._comb .. " get -f json " .. shell_escape(key)
  if not meta then
    cmd = cmd .. " " .. shell_escape(key .. ":age") .. " " .. shell_escape(key .. ":stale")
  end
  cmd = cmd .. path_arg(path)
  if flags.force then cmd = cmd .. " --force" end
  if flags.wait then cmd = cmd .. " --wait" end
  local out, ok, err = run(cmd)
  local decoded = json.decode(out or "")
  local value = decoded
  if not meta and type(decoded) == "table" then value = decoded[1] end

  if not ok then
    err = err or ""
    local own = line_after(err, "Error querying " .. key .. ": ")
    if own == "no data (cache miss)" then
      return { ok = true, data = nil } -- miss
    end
    if own then
      return { ok = false, error = { kind = "server_error", message = own } }
    end
    if decoded == nil or value == json.NULL then
      -- The resolver failed on `key` (its `<key>: <msg>` line), or the CLI
      -- did before answering at all (no daemon, a bad argument).
      local message = line_after(err, key .. ": ")
      if message then message = key .. ": " .. message else message = err:gsub("%s+$", "") end
      return { ok = false, error = { kind = "server_error", message = message } }
    end
    -- Only the `:age`/`:stale` companions missed: `key` itself rendered.
  end

  if value == json.NULL then value = nil end -- a null the daemon answers a suffix with, as the ffi backend collapses it
  if meta then
    return { ok = true, data = value, age_ms = 0, stale = false }
  end
  if type(decoded) ~= "table" then
    return { ok = false, error = { kind = "parse_error", message = "malformed comb get output: " .. tostring(out) } }
  end
  local age_ms, stale = decoded[2], decoded[3]
  if age_ms == json.NULL or age_ms == nil then age_ms = 0 end
  if stale == json.NULL or stale == nil then stale = false end
  return { ok = true, data = value, age_ms = age_ms, stale = stale }
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

function Backend:put_null(key, path)
  local cmd = self._comb .. " put --null " .. shell_escape(key) .. path_arg(path)
  local _, ok, err = run(cmd)
  if not ok then
    return { ok = false, error = { kind = "server_error", message = (err or ""):gsub("%s+$", "") } }
  end
  return { ok = true }
end

function Backend:status()
  local rows = status_rows(self._comb)
  return { ok = true, data = rows }
end

-- hello / introspect / resolve / eval / watch_open intentionally absent.
--
-- hello / introspect: no CLI surface exposes them.
--
-- resolve / eval: not implemented over this tier (see the header comment —
-- `comb get`/`comb eval` would be a faithful shell-out).
--
-- refresh: `comb get --force` is not a stand-in. `refresh` selects the
-- Sources its key demands and refuses a key that names none; `get --force`
-- selects the same Sources but then *reads*, so it answers a sourceless key
-- with its own, different refusal and answers an unowned field with a plain
-- miss. There is no `comb refresh` to shell out to, so this backend does not
-- pretend to have one. (The ffi tier calls `bc_refresh` and does.)
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

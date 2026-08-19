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
-- Capability ceiling: `comb` has no CLI surface for `hello`, `introspect`,
-- `resolve`, or `eval` (no subcommand exposes protocol/daemon version, a
-- structured introspection query, or the client-side expression evaluator
-- bc_resolve/bc_eval wrap). Those methods are simply absent from this
-- backend table; beachcomber.client turns a missing method into an
-- Error.unsupported() naming this transport, and a conformance runner
-- skips (never silently passes) a fixture that needs one.
--
-- `get` on a virtual (put-created) provider does not go through `comb get`:
-- as of this writing that CLI path silently drops the value for entries
-- created via `comb put` (confirmed against a fresh daemon: the wire
-- protocol itself returns the value correctly — `{"op":"get",...}` sent
-- directly over the socket works — but `comb get <virtual>.<field>`
-- returns exit 2 with empty stdout *and* empty stderr). That is a
-- pre-existing bug in src/cli/commands/get.rs, out of scope for this SDK
-- (sdks/lua/** only) and untouched here. This backend works around it
-- entirely client-side by reading `comb status -f json` instead, which
-- reports the same cache rows correctly for both virtual and built-in
-- providers; it walks the requested key's remaining dotted path into the
-- row's `value` itself (matching the daemon's own path-drilling) rather
-- than depending on the buggy code path at all.

local json = require("beachcomber.json")
local discovery = require("beachcomber.discovery")
local Error = require("beachcomber.error")

local Backend = {}
Backend.__index = Backend

local function shell_escape(s)
  if s == nil then return "''" end
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

--- Run a command, returning (stdout+stderr merged, exit_ok).
--
-- Exit status is read back via an appended `echo` marker rather than
-- `pipe:close()`'s return value: LuaJIT's `io.popen():close()` has been
-- observed to return `true` regardless of the child's actual exit code on
-- this platform, so it cannot be trusted to detect failure.
local function run(cmd)
  local pipe = io.popen(cmd .. " 2>&1; echo BC_EXIT:$?", "r")
  if not pipe then
    return nil, false, "failed to spawn: " .. cmd
  end
  local out = pipe:read("*a") or ""
  pipe:close()
  local body, code = out:match("^(.-)\n?BC_EXIT:(%d+)%s*$")
  if not body then
    return out, false
  end
  return body, code == "0"
end

--- Parse `comb status -f json` output (one JSON object per line) into an
-- array of row tables, optionally filtered to a single provider.
local function status_rows(comb_bin, provider)
  local cmd = comb_bin .. " status -f json"
  if provider then
    cmd = cmd .. " --filter " .. shell_escape("provider=" .. provider)
  end
  local out, ok = run(cmd)
  if not ok then
    return {}
  end
  local rows = {}
  for line in (out or ""):gmatch("[^\n]+") do
    local row = json.decode(line)
    if type(row) == "table" then
      rows[#rows + 1] = row
    end
  end
  return rows
end

--- Walk `segments` (dotted path components below the top-level field) into
-- `value`, matching the daemon's own field-drilling (object keys, and
-- array elements addressed by decimal-string index).
local function drill(value, segments, i)
  i = i or 1
  if i > #segments then
    return value
  end
  if type(value) ~= "table" then
    return nil
  end
  return drill(value[segments[i]], segments, i + 1)
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

--- get() deliberately never calls `comb get -f json` for the reasons in
-- this file's header comment: it primes the key (best-effort; ignored on
-- failure — an already-warm virtual provider or a genuinely-unknown one
-- both fall through cleanly) then reads the true value back from
-- `comb status -f json`.
function Backend:get(key, path, flags)
  flags = flags or {}
  local provider, rest = key:match("^([^.]+)%.?(.*)$")
  if not provider then
    return { ok = false, error = { kind = "parse_error", message = "empty key" } }
  end

  local prime_cmd = self._comb .. " get " .. shell_escape(key)
  if path then prime_cmd = prime_cmd .. " --path " .. shell_escape(path) end
  if flags.force then prime_cmd = prime_cmd .. " --force" end
  if flags.wait then prime_cmd = prime_cmd .. " --wait" end
  local prime_out, prime_ok = run(prime_cmd)

  local rows = status_rows(self._comb, provider)
  -- Match this key's path scope: nil/absent path on both sides, or equal.
  local function path_matches(row_path)
    if path == nil then return row_path == nil end
    return row_path == path
  end

  if rest == "" then
    -- Bare provider: merge every matching field into one object.
    local merged, found = {}, false
    for _, row in ipairs(rows) do
      if path_matches(row.path) then
        merged[row.field] = row.value
        found = true
      end
    end
    if found then
      return { ok = true, data = merged }
    end
  else
    local segments = {}
    for seg in rest:gmatch("[^.]+") do segments[#segments + 1] = seg end
    local field = segments[1]
    for _, row in ipairs(rows) do
      if row.field == field and path_matches(row.path) then
        local sub = {}
        for j = 2, #segments do sub[#sub + 1] = segments[j] end
        return { ok = true, data = drill(row.value, sub), age_ms = row.age_ms, stale = row.stale }
      end
    end
  end

  if not prime_ok and prime_out and prime_out ~= "" then
    -- The priming call itself produced a real error message (e.g. "unknown
    -- provider: X") and status has nothing for this provider either.
    return { ok = false, error = { kind = "server_error", message = prime_out:gsub("%s+$", "") } }
  end
  return { ok = true, data = nil } -- miss
end

function Backend:put(key, data, ttl, path)
  local encoded, enc_err = json.encode(data)
  if not encoded then
    return { ok = false, error = { kind = "parse_error", message = "json encode error: " .. tostring(enc_err) } }
  end
  local cmd = self._comb .. " put " .. shell_escape(key) .. " " .. shell_escape(encoded)
  if ttl then cmd = cmd .. " --ttl " .. shell_escape(ttl) end
  if path then cmd = cmd .. " --path " .. shell_escape(path) end
  local out, ok = run(cmd)
  if not ok then
    return { ok = false, error = { kind = "server_error", message = (out or ""):gsub("%s+$", "") } }
  end
  return { ok = true }
end

function Backend:put_null(key, path)
  local cmd = self._comb .. " put --null " .. shell_escape(key)
  if path then cmd = cmd .. " --path " .. shell_escape(path) end
  local out, ok = run(cmd)
  if not ok then
    return { ok = false, error = { kind = "server_error", message = (out or ""):gsub("%s+$", "") } }
  end
  return { ok = true }
end

--- The wire `refresh` op is a lenient no-op on a virtual (put-created)
-- provider (`{"ok":true}` observed directly over the socket). `comb get
-- --force` — the only CLI path to a re-execution — is stricter and
-- errors ("cannot --force virtual provider ... no source to re-execute
-- from"). Since a provider's row `source` (from `comb status`) already
-- says which case we're in, check that client-side first rather than
-- surface the CLI's stricter behaviour as a spurious failure.
function Backend:refresh(key, path)
  local provider = key:match("^([^.]+)")
  local rows = status_rows(self._comb, provider)
  for _, row in ipairs(rows) do
    if row.source == "virtual" then
      return { ok = true }
    end
  end
  local cmd = self._comb .. " get --force " .. shell_escape(key)
  if path then cmd = cmd .. " --path " .. shell_escape(path) end
  local out, ok = run(cmd)
  if not ok then
    return { ok = false, error = { kind = "server_error", message = (out or ""):gsub("%s+$", "") } }
  end
  return { ok = true }
end

function Backend:status()
  local rows = status_rows(self._comb, nil)
  return { ok = true, data = rows }
end

-- hello / introspect / resolve / eval / watch_open intentionally absent.
--
-- hello / introspect / resolve / eval: no CLI surface exposes them.
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

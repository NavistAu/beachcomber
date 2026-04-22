--- Protocol conformance runner for the beachcomber Lua SDK.
--
-- Loads fixture files from tests/conformance/**/*.json relative to the
-- repository root, starts a real daemon, drives the Lua SDK against it,
-- and reports pass/fail per fixture.
--
-- Invoke:
--   COMB_BIN=/path/to/comb lua sdks/lua/conformance_runner.lua
--
-- The COMB_BIN environment variable must point to a built comb binary.
-- The runner auto-discovers the repo root as two directories above this
-- script (sdks/lua/ -> sdks/ -> repo root).

-- ── Path setup ────────────────────────────────────────────────────────────────

local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
-- Resolve: sdks/lua/ -> sdks/ -> repo root
local sdk_root  = script_dir
local repo_root = sdk_root .. "../../"

package.path = sdk_root .. "?.lua;"
            .. sdk_root .. "?/init.lua;"
            .. package.path

local json        = require("beachcomber.json")
local client_mod  = require("beachcomber.client")
local WatchStream = require("beachcomber.watch_stream")

-- ── Helpers ───────────────────────────────────────────────────────────────────

local function die(msg)
  io.stderr:write("[conformance] FATAL: " .. msg .. "\n")
  os.exit(1)
end

local function log(msg)
  print("[conformance] " .. msg)
end

--- Read a file to a string.
local function read_file(path)
  local f, err = io.open(path, "r")
  if not f then return nil, err end
  local content = f:read("*a")
  f:close()
  return content
end

--- Recursively collect *.json paths under a directory using find.
-- Returns a list of absolute paths.
local function collect_fixtures(dir)
  local paths = {}
  local pipe = io.popen('find ' .. dir .. ' -name "*.json" 2>/dev/null', "r")
  if not pipe then return paths end
  for line in pipe:lines() do
    line = line:gsub("%s+$", "")
    if line ~= "" then
      paths[#paths + 1] = line
    end
  end
  pipe:close()
  -- Deterministic order
  table.sort(paths)
  return paths
end

--- Simple Lua-socket-based backend connection for conformance tests.
-- Returns a handle with send_line / recv_line / close.
local function connect_luasocket(socket_path)
  local unix_ok, unix = pcall(require, "socket.unix")
  if not unix_ok then return nil, "luasocket unix not available" end
  local s = unix()
  s:settimeout(5)
  local ok, err = s:connect(socket_path)
  if not ok then return nil, "connect failed: " .. tostring(err) end
  local handle = {}
  function handle:send_line(line)
    local _, werr = s:send(line)
    return not werr, werr
  end
  function handle:recv_line()
    local line, rerr = s:receive("*l")
    return line, rerr
  end
  function handle:close()
    s:close()
  end
  return handle
end

-- ── Daemon lifecycle ──────────────────────────────────────────────────────────

local COMB_BIN = os.getenv("COMB_BIN")
if not COMB_BIN or COMB_BIN == "" then
  die("COMB_BIN environment variable is not set.\nExample: COMB_BIN=/path/to/comb lua sdks/lua/conformance_runner.lua")
end

-- Use a temp socket path to avoid conflicting with a running daemon.
local tmpdir      = (os.getenv("TMPDIR") or "/tmp"):gsub("/+$", "")
local socket_path = tmpdir .. "/beachcomber_conformance_" .. tostring(os.time()) .. ".sock"

log("Starting daemon: " .. COMB_BIN)
log("Socket: " .. socket_path)

local daemon_pid_file = tmpdir .. "/beachcomber_conformance_pid.txt"

-- Launch daemon in background; redirect stdout/stderr to /dev/null.
-- The --socket flag tells comb which socket to bind.
local start_cmd = COMB_BIN
    .. " daemon --socket " .. socket_path
    .. " > /dev/null 2>&1 & echo $!"
local pid_pipe = io.popen(start_cmd, "r")
if not pid_pipe then die("Failed to start daemon") end
local daemon_pid = pid_pipe:read("*l")
pid_pipe:close()

log("Daemon PID: " .. tostring(daemon_pid))

-- Poll until the socket file appears (max 5s).
local deadline = os.time() + 5
local daemon_ready = false
while os.time() < deadline do
  local f = io.open(socket_path, "r")
  if f then
    f:close()
    daemon_ready = true
    break
  end
  -- Busy-poll is acceptable here; the daemon starts in < 100ms normally.
  os.execute("sleep 0.1 2>/dev/null || true")
end

if not daemon_ready then
  os.execute("kill " .. tostring(daemon_pid) .. " 2>/dev/null")
  die("Daemon did not create socket within 5 seconds")
end

log("Daemon ready.")

-- ── Client factory ────────────────────────────────────────────────────────────

local function make_client()
  local handle, err = connect_luasocket(socket_path)
  if not handle then die("Cannot connect to daemon: " .. tostring(err)) end
  local factory = function()
    local h, e = connect_luasocket(socket_path)
    if not h then return nil, e end
    return h
  end
  return client_mod.Client.new(handle, factory)
end

-- ── Fixture execution ─────────────────────────────────────────────────────────

--- Run a single protocol operation via the client and return a raw response table.
-- Returns: resp table with keys ok, data, error, age_ms, stale.
local function run_op(client, op, args)
  args = args or {}

  if op == "get" then
    local req = { op = "get", key = args.key }
    if args.path  then req.path  = args.path  end
    if args.force then req.force = args.force  end
    if args.wait  then req.wait  = args.wait   end
    return client:_send(req)

  elseif op == "put" then
    local req = { op = "put", key = args.key }
    if args.data ~= nil then req.data = args.data end
    if args.ttl         then req.ttl  = args.ttl  end
    if args.path        then req.path = args.path  end
    return client:_send(req)

  elseif op == "refresh" then
    local req = { op = "refresh", key = args.key }
    if args.path then req.path = args.path end
    return client:_send(req)

  elseif op == "context" then
    local req = { op = "context", path = args.path }
    return client:_send(req)

  elseif op == "hello" then
    return client:_send({ op = "hello" })

  elseif op == "status" then
    return client:_send({ op = "status" })

  elseif op == "introspect" then
    local req = { op = "introspect", subject = args.subject }
    if args.duration_secs then req.duration_secs = args.duration_secs end
    return client:_send(req)

  elseif op == "watch" then
    -- For conformance: open a dedicated connection, send watch, read one event.
    local whandle, werr = connect_luasocket(socket_path)
    if not whandle then return { ok = false, error = "watch connect: " .. tostring(werr) } end
    local req = { op = "watch", key = args.key }
    if args.path then req.path = args.path end
    whandle:send_line(json.encode(req) .. "\n")
    local stream = WatchStream.new(whandle)
    local ev, ev_err = stream:next_event()
    stream:close()
    if not ev then
      return { ok = false, error = ev_err or "watch: no event" }
    end
    return { ok = true, data = ev.data, age_ms = ev.age_ms, stale = ev.stale }

  else
    return { ok = false, error = "unknown op: " .. tostring(op) }
  end
end

--- Check whether resp matches the expect block.
-- Returns true, or false, failure_reason_string.
local function check_expect(resp, expect, fixture_name)
  local status = expect.status

  if status == "ok" then
    if not resp.ok then
      return false, "expected ok=true but got ok=false (error: " .. tostring(resp.error) .. ")"
    end

  elseif status == "error" then
    if resp.ok then
      return false, "expected ok=false (error) but got ok=true"
    end
    if expect.error_contains then
      local err_str = tostring(resp.error or "")
      if not err_str:find(expect.error_contains, 1, true) then
        return false, "error message '" .. err_str .. "' does not contain '" .. expect.error_contains .. "'"
      end
    end
    return true  -- error case validated

  elseif status == "hit" then
    if not resp.ok then
      return false, "expected hit but got ok=false (error: " .. tostring(resp.error) .. ")"
    end
    if resp.data == nil then
      return false, "expected a hit (data present) but data is nil"
    end

  elseif status == "miss" then
    if not resp.ok then
      return false, "expected miss but got ok=false (error: " .. tostring(resp.error) .. ")"
    end
    if resp.data ~= nil then
      return false, "expected a miss (no data) but data is present: " .. tostring(resp.data)
    end

  else
    return false, "unknown expect.status: " .. tostring(status)
  end

  -- data_type check
  if expect.data_type then
    local dt = expect.data_type
    local actual_type
    if type(resp.data) == "table" then
      -- Distinguish array vs object by checking for integer key 1.
      if resp.data[1] ~= nil or next(resp.data) == nil then
        actual_type = "array"
      else
        actual_type = "object"
      end
    else
      actual_type = type(resp.data)
    end
    if actual_type ~= dt then
      return false, "expected data_type=" .. dt .. " but got " .. actual_type
    end
  end

  -- data_contains_field
  if expect.data_contains_field then
    if type(resp.data) ~= "table" then
      return false, "data_contains_field check: data is not a table"
    end
    if resp.data[expect.data_contains_field] == nil then
      return false, "data does not contain field: " .. expect.data_contains_field
    end
  end

  -- data_equals
  if expect.data_equals ~= nil then
    if resp.data ~= expect.data_equals then
      return false, "expected data=" .. tostring(expect.data_equals) .. " but got " .. tostring(resp.data)
    end
  end

  -- data_as_text
  if expect.data_as_text ~= nil then
    if tostring(resp.data) ~= tostring(expect.data_as_text) then
      return false, "expected data_as_text='" .. tostring(expect.data_as_text) .. "' but got '" .. tostring(resp.data) .. "'"
    end
  end

  -- age_ms_present
  if expect.age_ms_present then
    if not resp.age_ms or resp.age_ms == 0 and resp.data == nil then
      -- age_ms may be 0 for brand-new entries; presence means the field exists.
      -- We check that age_ms is a number (could be 0).
      if type(resp.age_ms) ~= "number" then
        return false, "expected age_ms to be present but it is absent"
      end
    end
  end

  return true
end

--- Execute one fixture file.
-- Returns: passed (bool), result_message (string)
local function run_fixture(fixture_path, fixture_json)
  local ok_parse, fixture = pcall(json.decode, fixture_json)
  if not ok_parse or type(fixture) ~= "table" then
    return false, "JSON parse error: " .. tostring(fixture)
  end

  local name   = fixture.name or fixture_path
  local setup  = fixture.setup or {}
  local test   = fixture.test
  local expect = fixture.expect

  if not test or not expect then
    return false, "fixture missing 'test' or 'expect' fields"
  end

  -- Each fixture gets its own client connection.
  local client = make_client()

  -- Run setup steps (errors are fatal for the fixture).
  for i, step in ipairs(setup) do
    local sresp = run_op(client, step.op, step.args or {})
    if not sresp.ok then
      client:close()
      return false, string.format("setup step %d (%s) failed: %s", i, step.op, tostring(sresp.error))
    end
  end

  -- Run the test operation.
  local resp = run_op(client, test.op, test.args or {})
  client:close()

  local passed, reason = check_expect(resp, expect, name)
  if not passed then
    return false, reason
  end
  return true
end

-- ── Main loop ─────────────────────────────────────────────────────────────────

local conformance_dir = repo_root .. "tests/conformance"
local fixtures = collect_fixtures(conformance_dir)

if #fixtures == 0 then
  die("No fixture files found under: " .. conformance_dir)
end

log(string.format("Found %d fixture(s) in %s", #fixtures, conformance_dir))
print("")

local passed  = 0
local failed  = 0
local errors  = {}

for _, fpath in ipairs(fixtures) do
  local content, read_err = read_file(fpath)
  if not content then
    failed = failed + 1
    local msg = "could not read file: " .. tostring(read_err)
    errors[#errors + 1] = { path = fpath, reason = msg }
    print("  [FAIL] " .. fpath)
    print("         " .. msg)
  else
    local ok, reason = run_fixture(fpath, content)
    -- Extract a short label from path.
    local label = fpath:match(".*/tests/conformance/(.+)$") or fpath
    if ok then
      passed = passed + 1
      print("  [PASS] " .. label)
    else
      failed = failed + 1
      errors[#errors + 1] = { path = label, reason = reason }
      print("  [FAIL] " .. label)
      print("         " .. reason)
    end
  end
end

-- ── Teardown ──────────────────────────────────────────────────────────────────

log("Stopping daemon (PID " .. tostring(daemon_pid) .. ")")
os.execute("kill " .. tostring(daemon_pid) .. " 2>/dev/null")
os.remove(socket_path)

-- ── Summary ───────────────────────────────────────────────────────────────────

print(string.format("\n  Results: %d passed, %d failed\n", passed, failed))

if failed > 0 then
  os.exit(1)
end

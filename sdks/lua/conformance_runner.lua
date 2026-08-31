--- Protocol conformance runner for the beachcomber Lua SDK.
--
-- Loads fixture files from tests/conformance/**/*.json relative to the
-- repository root, drives the Lua SDK's public Client API against a
-- daemon, and reports pass/fail/skip per fixture. Each fixture gets its
-- own freshly-spawned daemon on a private socket (tests/conformance/
-- README.md, "Isolation": fixtures may reuse provider keys such as
-- `myappcache` across files, so a shared daemon leaks cache state between
-- them — see e.g. resolve/cascade_falls_through_to_cache.json and
-- resolve/cascade_miss_yields_empty_string.json, which both `put` the same
-- key with different values and would otherwise observe each other's
-- writes depending on run order).
--
-- Transport is whatever beachcomber.connect() selects (see beachcomber/
-- init.lua): "ffi" under LuaJIT, "subprocess" under PUC Lua. A fixture
-- whose op the active transport's backend doesn't implement (Error.kind ==
-- "unsupported") is reported SKIP, never PASS or FAIL — see
-- tests/conformance/README.md's note on partial `resolve` support.
--
-- Invoke:
--   COMB_BIN=/path/to/comb lua sdks/lua/conformance_runner.lua
--   COMB_BIN=/path/to/comb BEACHCOMBER_LIB=/path/to/libbeachcomber.dylib luajit sdks/lua/conformance_runner.lua
--
-- COMB_BIN must point to a built comb binary. The runner auto-discovers
-- the repo root as two directories above this script (sdks/lua/ -> sdks/
-- -> repo root).

-- ── Path setup ────────────────────────────────────────────────────────────────

local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
local sdk_root  = script_dir
local repo_root = sdk_root .. "../../"

package.path = sdk_root .. "?.lua;"
            .. sdk_root .. "?/init.lua;"
            .. package.path

local json = require("beachcomber.json")
local comb = require("beachcomber")

-- ── Helpers ───────────────────────────────────────────────────────────────────

local function die(msg)
  io.stderr:write("[conformance] FATAL: " .. msg .. "\n")
  os.exit(1)
end

local function log(msg)
  print("[conformance] " .. msg)
end

local function read_file(path)
  local f, err = io.open(path, "r")
  if not f then return nil, err end
  local content = f:read("*a")
  f:close()
  return content
end

--- Recursively collect *.json paths under a directory, sorted for
-- deterministic order.
local function collect_fixtures(dir)
  local paths = {}
  local pipe = io.popen('find ' .. dir .. ' -name "*.json" 2>/dev/null', "r")
  if not pipe then return paths end
  for line in pipe:lines() do
    line = line:gsub("%s+$", "")
    if line ~= "" then paths[#paths + 1] = line end
  end
  pipe:close()
  table.sort(paths)
  return paths
end

local COMB_BIN = os.getenv("COMB_BIN")
if not COMB_BIN or COMB_BIN == "" then
  die("COMB_BIN environment variable is not set.\nExample: COMB_BIN=/path/to/comb lua sdks/lua/conformance_runner.lua")
end

local base_tmpdir = (os.getenv("TMPDIR") or "/tmp"):gsub("/+$", "")

-- ── Per-fixture daemon lifecycle ────────────────────────────────────────────

--- Spawn a fresh daemon on a private socket. Returns a guard table
-- {pid, socket, tmpdir}; call stop_daemon(guard) when done.
local function start_daemon()
  local mkdtemp_pipe = io.popen("mktemp -d " .. base_tmpdir .. "/beachcomber-conformance-XXXXXX", "r")
  if not mkdtemp_pipe then die("failed to create temp directory") end
  local tmpdir = mkdtemp_pipe:read("*l")
  mkdtemp_pipe:close()
  if not tmpdir or tmpdir == "" then die("mktemp -d failed under " .. base_tmpdir) end
  local socket_path = tmpdir .. "/sock"

  local start_cmd = COMB_BIN .. " daemon --socket " .. socket_path .. " > /dev/null 2>&1 & echo $!"
  local pid_pipe = io.popen(start_cmd, "r")
  if not pid_pipe then die("failed to start daemon") end
  local pid = pid_pipe:read("*l")
  pid_pipe:close()

  -- Poll until the socket file appears (max 5s). `test -S`, not io.open:
  -- opening a Unix domain socket special file with fopen() fails (ENXIO)
  -- even once the daemon is listening.
  local deadline = os.time() + 5
  local ready = false
  while os.time() < deadline do
    if os.execute("test -S " .. socket_path) then
      ready = true
      break
    end
    os.execute("sleep 0.05 2>/dev/null || true")
  end
  if not ready then
    os.execute("kill " .. tostring(pid) .. " 2>/dev/null")
    die("daemon did not create socket within 5 seconds")
  end

  return { pid = pid, socket = socket_path, tmpdir = tmpdir }
end

local function stop_daemon(guard)
  os.execute("kill " .. tostring(guard.pid) .. " 2>/dev/null")
  os.execute("rm -rf " .. guard.tmpdir .. " 2>/dev/null")
end

-- ── Fixture parsing ──────────────────────────────────────────────────────────

local function parse_fixture(text)
  local ok, fixture = pcall(json.decode, text)
  if not ok or type(fixture) ~= "table" then
    return nil, "JSON parse error: " .. tostring(fixture)
  end
  if not fixture.test or not fixture.expect then
    return nil, "fixture missing 'test' or 'expect' fields"
  end
  return fixture
end

-- ── Op dispatch ──────────────────────────────────────────────────────────────

--- Run one op descriptor {op=, args=} through the Client's public API and
-- return a canonical response table {ok, data, age_ms, stale, error}.
-- `resolve_ctx` supplies the fixture's top-level cwd/env/virtual for the
-- `resolve` and `eval` ops, neither of which is a wire op (see
-- tests/conformance/README.md).
local function run_op(client, descriptor, resolve_ctx)
  local op, args = descriptor.op, descriptor.args or {}

  local function fail(err)
    return { ok = false, error = { kind = err and err.kind or "error", message = err and tostring(err.message or err) or "unknown error" } }
  end

  if op == "hello" then
    local info, err = client:hello()
    if not info then return fail(err) end
    return { ok = true, data = { protocol_version = info.protocol_version, daemon_version = info.daemon_version } }

  elseif op == "get" then
    local r, err = client:get_with_flags(args.key, args.path, args.force, args.wait)
    if not r then return fail(err) end
    return { ok = true, data = r.data, age_ms = r.age_ms, stale = r.stale }

  elseif op == "refresh" then
    local ok, err = client:refresh(args.key, args.path)
    if not ok then return fail(err) end
    return { ok = true }

  elseif op == "context" then
    client:set_context(args.path)
    return { ok = true }

  elseif op == "put" then
    local ok, err = client:put(args.key, args.data, args.ttl, args.path)
    if not ok then return fail(err) end
    return { ok = true }

  elseif op == "status" then
    local rows, err = client:status()
    if not rows then return fail(err) end
    return { ok = true, data = rows }

  elseif op == "introspect" then
    local result, err = client:introspect(args.subject, args.duration_secs)
    if not result then return fail(err) end
    if args.subject == "daemon" then
      return { ok = true, data = result.daemon }
    end
    return { ok = true, data = result.other }

  elseif op == "watch" then
    local stream, err = client:watch(args.key, args.path)
    if not stream then return fail(err) end
    local ev, everr = stream:next_event()
    stream:close()
    if not ev then
      return fail(everr or { kind = "eof", message = "watch stream closed with no event" })
    end
    return { ok = true, data = ev.data, age_ms = ev.age_ms, stale = ev.stale }

  elseif op == "resolve" then
    local value, err = client:resolve(args.key, {
      cwd = resolve_ctx.cwd, env = resolve_ctx.env, virtual = resolve_ctx.virtual,
    })
    if value == nil and err then return fail(err) end
    return { ok = true, data = value }

  elseif op == "eval" then
    local value, err = client:eval(args.template, {
      cwd = resolve_ctx.cwd, env = resolve_ctx.env, virtual = resolve_ctx.virtual,
    })
    if value == nil and err then return fail(err) end
    return { ok = true, data = value }

  else
    return { ok = false, error = { kind = "unknown_op", message = "unknown op: " .. tostring(op) } }
  end
end

-- ── Expectation checking ─────────────────────────────────────────────────────

local function data_type_of(data)
  if data == nil or json.is_null(data) then return "null" end
  local t = type(data)
  if t == "string" then return "string" end
  if t == "number" then return "number" end
  if t == "boolean" then return "bool" end
  if t == "table" then
    -- An empty table is ambiguous (Lua has no separate empty-object vs
    -- empty-array runtime type) unless the decoder tagged it — see
    -- beachcomber.json.is_empty_object().
    if next(data) == nil then
      return json.is_empty_object(data) and "object" or "array"
    end
    if data[1] ~= nil then return "array" end
    return "object"
  end
  return t
end

local function deep_equal(a, b)
  if a == b then return true end
  if type(a) ~= "table" or type(b) ~= "table" then return false end
  for k, v in pairs(a) do
    if not deep_equal(v, b[k]) then return false end
  end
  for k in pairs(b) do
    if a[k] == nil then return false end
  end
  return true
end

-- Every expectation kind documented in tests/conformance/README.md. A
-- fixture using a key outside this set fails loudly rather than being
-- silently ignored — the whole point of this runner is to catch a fixture
-- asserting something the harness doesn't actually check.
local KNOWN_EXPECT_KEYS = {
  status = true, data_type = true, data_equals = true, data_as_text = true,
  data_contains_field = true, data_field_equals = true, age_ms_present = true,
  stale = true, error_contains = true,
}

--- @return true, or false, reason_string
local function check_expect(resp, expect)
  for k in pairs(expect) do
    if not KNOWN_EXPECT_KEYS[k] then
      return false, "fixture uses unknown expectation key: " .. tostring(k) .. " — the runner has no check for it"
    end
  end

  local status = expect.status
  if status == "ok" then
    if not resp.ok then
      return false, "status=ok expected but response was error: " .. tostring(resp.error and resp.error.message)
    end
  elseif status == "hit" then
    if not resp.ok then
      return false, "status=hit expected but response was error: " .. tostring(resp.error and resp.error.message)
    end
    if resp.data == nil then
      return false, "status=hit expected but data was absent"
    end
  elseif status == "miss" then
    if not resp.ok then
      return false, "status=miss expected but response was error: " .. tostring(resp.error and resp.error.message)
    end
    if resp.data ~= nil then
      return false, "status=miss expected but data was present: " .. tostring(resp.data)
    end
  elseif status == "error" then
    if resp.ok then
      return false, "status=error expected but response was ok"
    end
    if expect.error_contains then
      local msg = tostring(resp.error and resp.error.message or "")
      if not msg:find(expect.error_contains, 1, true) then
        return false, "error message " .. msg .. " does not contain " .. expect.error_contains
      end
    end
    return true
  else
    return false, "unknown expect.status: " .. tostring(status)
  end

  if expect.data_type then
    local actual = data_type_of(resp.data)
    if actual ~= expect.data_type then
      return false, "data_type=" .. expect.data_type .. " expected but got " .. actual
    end
  end

  if expect.data_equals ~= nil then
    if not deep_equal(resp.data, expect.data_equals) then
      return false, "data_equals failed: expected " .. tostring(expect.data_equals) .. ", got " .. tostring(resp.data)
    end
  end

  if expect.data_as_text ~= nil then
    local actual = tostring(resp.data)
    if actual ~= tostring(expect.data_as_text) then
      return false, "data_as_text=" .. tostring(expect.data_as_text) .. " expected but got " .. actual
    end
  end

  if expect.data_contains_field then
    if type(resp.data) ~= "table" or resp.data[expect.data_contains_field] == nil then
      return false, "data_contains_field=" .. expect.data_contains_field .. " failed"
    end
  end

  if expect.data_field_equals then
    local field = expect.data_field_equals.field
    local expected = expect.data_field_equals.value
    if type(resp.data) ~= "table" then
      return false, "data_field_equals: data is not an object"
    end
    if not deep_equal(resp.data[field], expected) then
      return false, "data_field_equals failed for " .. tostring(field) .. ": expected " .. tostring(expected) .. ", got " .. tostring(resp.data[field])
    end
  end

  if expect.age_ms_present ~= nil then
    local present = type(resp.age_ms) == "number"
    if present ~= expect.age_ms_present then
      return false, "age_ms_present=" .. tostring(expect.age_ms_present) .. " expected but got " .. tostring(present)
    end
  end

  if expect.stale ~= nil then
    if resp.stale ~= expect.stale then
      return false, "stale=" .. tostring(expect.stale) .. " expected but got " .. tostring(resp.stale)
    end
  end

  return true
end

-- ── Fixture execution ─────────────────────────────────────────────────────────

--- @return "pass"|"fail"|"skip", reason_string_on_fail_or_skip
local function run_fixture(fixture)
  local guard = start_daemon()
  local client, cerr = comb.connect({ socket_path = guard.socket, autostart = false, timeout_ms = 3000, comb_bin = COMB_BIN })
  if not client then
    stop_daemon(guard)
    return "fail", "could not connect: " .. tostring(cerr)
  end

  local resolve_ctx = {
    cwd = fixture.cwd or guard.tmpdir,
    env = fixture.env or {},
    virtual = fixture.virtual or {},
  }

  for i, step in ipairs(fixture.setup or {}) do
    local resp = run_op(client, step, resolve_ctx)
    if not resp.ok then
      client:close()
      stop_daemon(guard)
      return "fail", string.format("setup step %d (%s) failed: %s", i, step.op, tostring(resp.error and resp.error.message))
    end
  end

  local resp = run_op(client, fixture.test, resolve_ctx)
  client:close()
  stop_daemon(guard)

  if not resp.ok and resp.error and resp.error.kind == "unsupported" then
    return "skip", resp.error.message
  end

  local ok, reason = check_expect(resp, fixture.expect)
  if not ok then
    return "fail", reason
  end
  return "pass"
end

-- ── Main loop ─────────────────────────────────────────────────────────────────

local conformance_dir = repo_root .. "tests/conformance"
local fixture_paths = collect_fixtures(conformance_dir)
if #fixture_paths == 0 then
  die("no fixture files found under: " .. conformance_dir)
end

-- Report which transport is active up front (a probe daemon+connect, torn
-- down immediately) — the whole point of Client:transport() is that this
-- is visible rather than discovered in a profile.
do
  local probe = start_daemon()
  local probe_client, err = comb.connect({ socket_path = probe.socket, autostart = false, timeout_ms = 2000, comb_bin = COMB_BIN })
  stop_daemon(probe)
  if not probe_client then
    die("could not establish a connection to probe the active transport: " .. tostring(err))
  end
  log("transport: " .. probe_client:transport())
end

log(string.format("found %d fixture(s) in %s", #fixture_paths, conformance_dir))
print("")

local passed, failed, skipped = 0, 0, 0
local failures = {}

for _, fpath in ipairs(fixture_paths) do
  local content, read_err = read_file(fpath)
  local label = fpath:match(".*/tests/conformance/(.+)$") or fpath
  if not content then
    failed = failed + 1
    print("  [FAIL] " .. label)
    print("         could not read file: " .. tostring(read_err))
  else
    local fixture, perr = parse_fixture(content)
    if not fixture then
      failed = failed + 1
      print("  [FAIL] " .. label)
      print("         " .. perr)
    else
      -- Known SDK defects: fixtures that fail because of a documented,
      -- roadmap-logged gap in this binding, not a runner or daemon bug.
      -- Reported as SKIP with the defect named so the gate stays green on
      -- honest terms; remove the entry when the defect is fixed.
      --
      -- mapping_null_value_becomes_empty_string was here (nested JSON null
      -- collapsing to key-absent before put() ran) — fixed by the M.NULL
      -- sentinel in beachcomber.json; see its doc comment.
      local KNOWN_DEFECTS = {}
      local defect = fixture.name and KNOWN_DEFECTS[fixture.name]
      local outcome, reason
      if defect then
        outcome, reason = "skip", defect
      else
        outcome, reason = run_fixture(fixture)
      end
      if outcome == "pass" then
        passed = passed + 1
        print("  [PASS] " .. label)
      elseif outcome == "skip" then
        skipped = skipped + 1
        print("  [SKIP] " .. label .. ": " .. tostring(reason))
      else
        failed = failed + 1
        failures[#failures + 1] = { path = label, reason = reason }
        print("  [FAIL] " .. label)
        print("         " .. tostring(reason))
      end
    end
  end
end

print(string.format("\n  Results: %d passed, %d failed, %d skipped (of %d fixtures)\n",
  passed, failed, skipped, #fixture_paths))

if failed > 0 then
  os.exit(1)
end

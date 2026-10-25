--- Tests for beachcomber.client: Client against a mock backend (the
--- interface documented at the top of beachcomber/client.lua — canonical
--- response tables, not raw sockets), and the ffi backend against a live
--- daemon.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local client_mod = require("beachcomber.client")
  local Client = client_mod.Client

  -- ── Mock backend helper ────────────────────────────────────────────────────

  local function make_backend(overrides)
    local backend = { _calls = {} }
    backend.name = function(_) return "mock" end
    for k, v in pairs(overrides or {}) do backend[k] = v end
    return backend
  end

  -- ── Client:get ────────────────────────────────────────────────────────────

  suite("Client:get")

  test("get returns the value", function()
    local backend = make_backend({
      get = function() return { ok = true, data = "main" } end,
    })
    local value, err = Client.new(backend):get("git.branch", { cwd = "/repo" })
    assert_nil(err)
    assert_eq(value, "main")
  end)

  test("get forwards exactly key, cwd, env and the flags", function()
    local seen, nargs
    local backend = make_backend({
      get = function(_, ...)
        nargs = select("#", ...)
        local key, cwd, env, flags = ...
        seen = { key = key, cwd = cwd, env = env, flags = flags }
        return { ok = true }
      end,
    })
    Client.new(backend):get("git.branch", { cwd = "/myrepo", env = { A = "x" } })
    assert_eq(nargs, 4)
    assert_eq(seen.key, "git.branch")
    assert_eq(seen.cwd, "/myrepo")
    assert_eq(seen.env.A, "x")
    assert_true(not seen.flags.force)
    assert_true(not seen.flags.wait)
  end)

  test("get with no opts is the global entry and no environment", function()
    local seen
    local backend = make_backend({
      get = function(_, key, cwd, env, flags) seen = { cwd = cwd, env = env }; return { ok = true } end,
    })
    Client.new(backend):get("git.branch")
    assert_nil(seen.cwd, "no cwd and no context is the global entry")
    assert_nil(seen.env, "no env is passed as none, not as an empty table")
  end)

  test("get passes force and wait through as the flags table", function()
    local seen_flags
    local backend = make_backend({
      get = function(_, key, cwd, env, flags) seen_flags = flags; return { ok = true } end,
    })
    Client.new(backend):get("git.branch", { force = true, wait = true })
    assert_eq(seen_flags.force, true)
    assert_eq(seen_flags.wait, true)
  end)

  test("a miss is nil with no Error", function()
    local backend = make_backend({ get = function() return { ok = true } end })
    local value, err = Client.new(backend):get("git.branch")
    assert_nil(value)
    assert_nil(err)
  end)

  test("false and 0 are values, not misses", function()
    local backend = make_backend({ get = function() return { ok = true, data = false } end })
    local value, err = Client.new(backend):get("git.dirty")
    assert_eq(value, false)
    assert_nil(err)
  end)

  test("get returns nil+Error when backend returns ok=false", function()
    local backend = make_backend({
      get = function() return { ok = false, error = { kind = "server_error", message = "unknown provider: bad" } } end,
    })
    local value, err = Client.new(backend):get("bad.field")
    assert_nil(value)
    assert_not_nil(err)
    assert_eq(err.kind, "server_error")
    assert_true(tostring(err):find("unknown provider", 1, true) ~= nil)
  end)

  test("the raw read and the old names are gone", function()
    assert_nil(Client.get_with_flags, "get_with_flags is gone: get takes the flags")
    assert_nil(Client.resolve, "resolve is gone: get renders")
    assert_nil(client_mod.Result, "Result is gone: get returns the value")
  end)

  -- ── Client:refresh / set_context ──────────────────────────────────────────

  suite("Client:refresh")

  test("refresh returns true on success and forwards key/path", function()
    local seen
    local backend = make_backend({
      refresh = function(_, key, path) seen = { key = key, path = path }; return { ok = true } end,
    })
    local ok, err = Client.new(backend):refresh("git", "/repo")
    assert_nil(err)
    assert_true(ok)
    assert_eq(seen.key, "git")
    assert_eq(seen.path, "/repo")
  end)

  suite("Client:set_context")

  test("set_context supplies the cwd of a later get that names none", function()
    local seen_cwd
    local backend = make_backend({
      get = function(_, key, cwd) seen_cwd = cwd; return { ok = true } end,
    })
    local c = Client.new(backend)
    c:set_context("/some/dir")
    c:get("git.branch")
    assert_eq(seen_cwd, "/some/dir")
  end)

  test("an explicit cwd overrides the context default", function()
    local seen_cwd
    local backend = make_backend({
      get = function(_, key, cwd) seen_cwd = cwd; return { ok = true } end,
    })
    local c = Client.new(backend)
    c:set_context("/context/dir")
    c:get("git.branch", { cwd = "/explicit/dir" })
    assert_eq(seen_cwd, "/explicit/dir")
  end)

  -- ── Client:status ─────────────────────────────────────────────────────────

  suite("Client:status")

  test("status returns data array", function()
    local backend = make_backend({
      status = function() return { ok = true, data = { { key = "git.branch" }, { key = "git.dirty" } } } end,
    })
    local result, err = Client.new(backend):status()
    assert_nil(err)
    assert_eq(#result, 2)
  end)

  test("status returns empty array when data is nil", function()
    local backend = make_backend({ status = function() return { ok = true } end })
    local result = Client.new(backend):status()
    assert_eq(#result, 0)
  end)

  test("status returns nil+Error on backend error", function()
    local backend = make_backend({
      status = function() return { ok = false, error = { kind = "io_error", message = "internal error" } } end,
    })
    local result, err = Client.new(backend):status()
    assert_nil(result)
    assert_not_nil(err)
  end)

  -- ── Client:transport ─────────────────────────────────────────────────────

  suite("Client:transport")

  test("transport reports the backend's name", function()
    local backend = make_backend({ name = function() return "ffi" end })
    assert_eq(Client.new(backend):transport(), "ffi")
  end)

  -- ── exec option (ffi transport) ───────────────────────────────────────────
  -- An exec() call with no daemon-backed ref never touches the socket, so
  -- whether `opts.exec` reached bc_client_new is observable against a dead
  -- socket with autostart off. Needs LuaJIT and a built libbeachcomber
  -- ($BEACHCOMBER_LIB, or the workspace's own debug build).

  suite("ffi_backend.new{ exec = ... }")

  local function built_lib()
    local env_lib = os.getenv("BEACHCOMBER_LIB")
    if env_lib and env_lib ~= "" then return env_lib end
    local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
    for _, name in ipairs({ "libbeachcomber.dylib", "libbeachcomber.so" }) do
      local path = script_dir .. "../../../target/debug/" .. name
      local f = io.open(path, "rb")
      if f then f:close(); return path end
    end
    return nil
  end

  local has_ffi = pcall(require, "ffi")
  local lib = built_lib()
  local function exec_client(exec)
    local backend, err = require("beachcomber.ffi_backend").new({
      library_path = lib, socket_path = "/nonexistent/beachcomber-test.sock",
      autostart = false, timeout_ms = 500, exec = exec,
    })
    if not backend then return nil, err end
    return Client.new(backend)
  end

  if not has_ffi then
    skip("exec is disabled by default", "no ffi (PUC Lua)")
    skip("exec = true reaches bc_client_new", "no ffi (PUC Lua)")
  elseif not lib then
    skip("exec is disabled by default", "no libbeachcomber built")
    skip("exec = true reaches bc_client_new", "no libbeachcomber built")
  else
    test("exec is disabled by default", function()
      local c, cerr = exec_client(nil)
      assert_nil(cerr)
      local value, err = c:eval("{{ exec(['printf', 'x']) }}", { cwd = "/tmp" })
      assert_nil(value)
      assert_eq(err.kind, "server_error")
      assert_true(tostring(err.message):find("exec() is disabled in this context", 1, true) ~= nil, tostring(err.message))
    end)

    test("exec = true reaches bc_client_new", function()
      local c, cerr = exec_client(true)
      assert_nil(cerr)
      local value, err = c:eval("{{ exec(['printf', 'x']) }}", { cwd = "/tmp" })
      assert_nil(err)
      assert_eq(value, "x")
    end)
  end

  -- ── get / eval against a live daemon (ffi transport) ──────────────────────
  -- bc_get answers a miss as `"data":null`; the binding must hand that back
  -- as Lua nil (the miss Client:get documents), never as the json.NULL
  -- sentinel. Needs LuaJIT, the built library, and a comb binary
  -- ($COMB_BIN, or the workspace's own debug build).

  suite("ffi_backend get/eval against a live daemon")

  local function built_comb()
    local env_bin = os.getenv("COMB_BIN")
    if env_bin and env_bin ~= "" then return env_bin end
    local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
    local path = script_dir .. "../../../target/debug/comb"
    local f = io.open(path, "rb")
    if f then f:close(); return path end
    return nil
  end

  -- Spawn a daemon on a private socket, with HOME/XDG_CONFIG_HOME/XDG_STATE_HOME
  -- pointed into its tempdir so its managed defaults and its log never touch
  -- the real config or state.
  local function start_daemon(comb_bin)
    local p = io.popen("mktemp -d " .. (os.getenv("TMPDIR") or "/tmp"):gsub("/+$", "") .. "/beachcomber-luatest-XXXXXX", "r")
    local tmpdir = p and p:read("*l")
    if p then p:close() end
    if not tmpdir or tmpdir == "" then return nil end
    local socket_path = tmpdir .. "/sock"
    local pid_pipe = io.popen("XDG_CONFIG_HOME=" .. tmpdir .. " HOME=" .. tmpdir
      .. " XDG_STATE_HOME=" .. tmpdir .. "/.local/state " .. comb_bin
      .. " daemon --socket " .. socket_path .. " > /dev/null 2>&1 & echo $!", "r")
    local pid = pid_pipe and pid_pipe:read("*l")
    if pid_pipe then pid_pipe:close() end
    local deadline = os.time() + 5
    while os.time() < deadline do
      -- Lua 5.1/LuaJIT return the raw status (256 on failure, truthy); 5.2+
      -- return true/nil.
      local rc = os.execute("test -S " .. socket_path)
      if rc == true or rc == 0 then
        return { pid = pid, socket = socket_path, tmpdir = tmpdir }
      end
      os.execute("sleep 0.05 2>/dev/null || true")
    end
    os.execute("kill " .. tostring(pid) .. " 2>/dev/null; rm -rf " .. tmpdir)
    return nil
  end

  local comb_bin = built_comb()
  local live_tests = {
    "get of a missing key is nil with no Error",
    "eval rendering none is nil, not the null sentinel",
    "get renders a put template with the caller's environment",
    "refresh of a static namespace is the daemon's words alone",
    "the facets read the unrendered halves",
    "get takes force and wait",
    "get with no cwd reads at the set_context directory",
    "get with no cwd and no context reads the global entry",
  }
  if not has_ffi then
    for _, name in ipairs(live_tests) do skip(name, "no ffi (PUC Lua)") end
  elseif not lib or not comb_bin then
    for _, name in ipairs(live_tests) do skip(name, "no libbeachcomber/comb built") end
  else
    local guard = start_daemon(comb_bin)
    if not guard then
      for _, name in ipairs(live_tests) do skip(name, "daemon did not start") end
    else
      local backend = require("beachcomber.ffi_backend").new({
        library_path = lib, socket_path = guard.socket, autostart = false, timeout_ms = 3000,
      })
      local c = Client.new(backend)

      test(live_tests[1], function()
        assert_true(c:put("revns", { v = 1 }))
        local value, err = c:get("revns.nope", { cwd = "/tmp" })
        assert_nil(err)
        assert_nil(value)
      end)

      test(live_tests[2], function()
        -- A template rendering none is absent: nil, as a miss is, and
        -- never json.NULL through the binding.
        local value, err = c:eval("{{ none }}", { cwd = "/tmp" })
        assert_nil(err)
        assert_nil(value)
      end)

      test(live_tests[3], function()
        assert_true(c:put("revtpl:template", { label = "{{ 'resolved-' ~ env.MYVAR }}" }))
        local value, err = c:get("revtpl.label", { cwd = "/tmp", env = { MYVAR = "x" } })
        assert_nil(err)
        assert_eq(value, "resolved-x")
      end)

      test(live_tests[4], function()
        -- The same text `comb refresh` prints and the subprocess tier
        -- reports (test_subprocess.lua) — no `server error: ` prefix.
        assert_true(c:put("revstatic", { v = 1 }))
        local ok, err = c:refresh("revstatic")
        assert_nil(ok)
        assert_eq(err.kind, "server_error")
        -- A prefix: on a connection with detected version skew the library
        -- appends ` (client is …, daemon is …)`.
        local refusal = "nothing to refresh: revstatic has no sources"
        assert_true(err.message:sub(1, #refusal) == refusal, err.message)
      end)

      test(live_tests[5], function()
        assert_true(c:put("revhalf", { v = 1 }))
        assert_true(c:put("revhalf:template", { v = "{{ 2 }}" }))
        local value, err = c:get("revhalf.v")
        assert_nil(err)
        assert_eq(value, 2, "the template wins over the value beside it")
        value, err = c:get("revhalf:template.v")
        assert_nil(err)
        assert_eq(value, "{{ 2 }}", "the template facet is the template as stored")
        value, err = c:get("revhalf:put.v")
        assert_nil(err)
        assert_eq(value, 1, "the put facet is the stored value")
      end)

      test(live_tests[6], function()
        local value, err = c:get("hostname.short", { force = true, wait = true })
        assert_nil(err)
        assert_eq(type(value), "string")
      end)

      test(live_tests[7], function()
        assert_true(c:put("revctx", { v = "global" }))
        assert_true(c:put("revctx", { v = "tmp" }, nil, "/tmp"))
        local in_tmp = Client.new(backend)
        in_tmp:set_context("/tmp")
        local value, err = in_tmp:get("revctx.v")
        assert_nil(err)
        assert_eq(value, "tmp")
      end)

      test(live_tests[8], function()
        local value, err = c:get("revctx.v")
        assert_nil(err)
        assert_eq(value, "global")
      end)

      c:close()
      os.execute("kill " .. tostring(guard.pid) .. " 2>/dev/null; rm -rf " .. guard.tmpdir)
    end
  end
end

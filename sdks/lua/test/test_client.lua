--- Tests for beachcomber.client: Client against a mock backend (the
--- interface documented at the top of beachcomber/client.lua — canonical
--- response tables, not raw sockets), and the ffi backend against a live
--- daemon.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local testdaemon = require("test.testdaemon")
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

  test("get forwards exactly key, cwd, env and the options", function()
    local seen, nargs
    local backend = make_backend({
      get = function(_, ...)
        nargs = select("#", ...)
        local key, cwd, env, options = ...
        seen = { key = key, cwd = cwd, env = env, options = options }
        return { ok = true }
      end,
    })
    Client.new(backend):get("git.branch", { cwd = "/myrepo", env = { A = "x" } })
    assert_eq(nargs, 4)
    assert_eq(seen.key, "git.branch")
    assert_eq(seen.cwd, "/myrepo")
    assert_eq(seen.env.A, "x")
    assert_true(not seen.options.force)
    assert_nil(seen.options.wait)
  end)

  test("get with no opts is the global entry and no environment", function()
    local seen
    local backend = make_backend({
      get = function(_, key, cwd, env, options) seen = { cwd = cwd, env = env }; return { ok = true } end,
    })
    Client.new(backend):get("git.branch")
    assert_nil(seen.cwd, "no cwd and no context is the global entry")
    assert_nil(seen.env, "no env is passed as none, not as an empty table")
  end)

  test("get passes force and the wait through as the options table", function()
    local seen_options
    local backend = make_backend({
      get = function(_, key, cwd, env, options) seen_options = options; return { ok = true } end,
    })
    Client.new(backend):get("git.branch", { force = true, wait = "50ms" })
    assert_eq(seen_options.force, true)
    assert_eq(seen_options.wait, "50ms")
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
    assert_nil(Client.get_with_flags, "get_with_flags is gone: get takes the options")
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
    local ok, err = Client.new(backend):refresh("git", { path = "/repo" })
    assert_nil(err)
    assert_true(ok)
    assert_eq(seen.key, "git")
    assert_eq(seen.path, "/repo")
  end)

  suite("Client:session")

  --- A backend whose session handle records what it was asked, answering
  -- each op with `answers[op]` (a response table) or ok.
  local function session_backend(answers)
    local calls = {}
    local handle = {}
    for _, op in ipairs({ "get", "put", "refresh", "set_context" }) do
      handle[op] = function(_, ...)
        calls[#calls + 1] = { op = op, args = { ... }, n = select("#", ...) }
        return (answers or {})[op] or { ok = true }
      end
    end
    handle.close = function() calls[#calls + 1] = { op = "close" } end
    return make_backend({ session_open = function() return handle end }), calls
  end

  test("a client keeps no directory of its own: set_context is a session's", function()
    assert_nil(Client.set_context, "set_context is a session's, not the client's")
  end)

  test("a session's get answers the value, like the client's", function()
    local backend, calls = session_backend({ get = { ok = true, data = "main" } })
    local s = Client.new(backend):session()
    local value, err = s:get("git.branch", { env = { A = "x" }, force = true })
    assert_nil(err)
    assert_eq(value, "main")
    assert_eq(calls[1].op, "get")
    assert_eq(calls[1].args[1], "git.branch")
    assert_nil(calls[1].args[2], "no cwd: the session's directory, which the handle keeps")
    assert_eq(calls[1].args[3].A, "x")
    assert_eq(calls[1].args[4].force, true)
  end)

  test("a session's miss is nil with no Error, and its failure nil, Error", function()
    local backend = session_backend({
      get = { ok = false, error = { kind = "busy", message = "in use" } },
    })
    local s = Client.new(backend):session()
    local value, err = s:get("k")
    assert_nil(value)
    assert_eq(err.kind, "busy")
    assert_eq(err.message, "in use")
    backend = session_backend({ get = { ok = true } })
    value, err = Client.new(backend):session():get("k")
    assert_nil(value)
    assert_nil(err)
  end)

  test("a session's set_context, put, refresh and close answer true", function()
    local backend, calls = session_backend()
    local s = Client.new(backend):session()
    assert_eq(s:set_context("/repo"), true)
    assert_eq(s:put("k", { v = 1 }, { ttl = "5s", path = "/p" }), true)
    assert_eq(s:refresh("k", { path = "/p" }), true)
    assert_eq(s:close(), true)
    assert_eq(calls[1].args[1], "/repo")
    assert_eq(calls[2].args[3], "5s")
    assert_eq(calls[2].args[4], "/p")
    assert_eq(calls[3].args[2], "/p")
    assert_eq(calls[4].op, "close")
  end)

  test("a session's put failure is nil, Error", function()
    local backend = session_backend({ put = { ok = false, error = { kind = "server_error", message = "no" } } })
    local ok, err = Client.new(backend):session():put("k", 1)
    assert_nil(ok)
    assert_eq(err.kind, "server_error")
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

  test("status data that is not a list is a parse_error, not an empty list", function()
    local backend = make_backend({ status = function() return { ok = true } end })
    local result, err = Client.new(backend):status()
    assert_nil(result)
    assert_eq(err.kind, "parse_error")
  end)

  test("a row stored at the global entry has a nil path, whichever tier read it", function()
    local null = require("beachcomber.json").NULL
    local backend = make_backend({
      status = function() return { ok = true, data = { { provider = "p", path = null } } } end,
    })
    local rows = Client.new(backend):status()
    assert_nil(rows[1].path)
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
    local pid = testdaemon.start(comb_bin, tmpdir, socket_path)
    if pid then return { pid = pid, socket = socket_path, tmpdir = tmpdir } end
    os.execute("rm -rf " .. tmpdir)
    return nil
  end

  local comb_bin = built_comb()
  local live_tests = {
    "get of a missing key is nil with no Error",
    "eval rendering none is nil, not the null sentinel",
    "get renders a put template with the caller's environment",
    "refresh of a static namespace is the daemon's words alone",
    "the facets read the unrendered halves",
    "get takes force and a wait duration, and refuses one that is not",
    "a session's get with no cwd reads at its directory",
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
      -- The daemon is stopped however the body ends: a backend that fails
      -- to load must not leave it running.
      local ran, failure = pcall(function()
      local backend, berr = require("beachcomber.ffi_backend").new({
        library_path = lib, socket_path = guard.socket, autostart = false, timeout_ms = 3000,
      })
      if not backend then error("ffi backend: " .. tostring(berr), 0) end
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
        assert_eq(err.library_version, backend:version(), "the library's version is its own field")
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
        for _, wait in ipairs({ "forever", "0", "50ms" }) do
          local value, err = c:get("hostname.short", { force = true, wait = wait })
          assert_nil(err)
          assert_eq(type(value), "string")
        end
        local value, err = c:get("hostname.short", { wait = "soon" })
        assert_nil(value)
        assert_eq(err.kind, "parse_error")
        assert_true(err.message:find("invalid wait `soon`", 1, true) ~= nil, err.message)
        value, err = c:get("hostname.short", { wait = true })
        assert_nil(value)
        assert_eq(err.kind, "parse_error")
      end)

      test(live_tests[7], function()
        assert_true(c:put("revctx", { v = "global" }))
        assert_true(c:put("revctx", { v = "tmp" }, { path = "/tmp" }))
        local s = c:session()
        assert_eq(s:set_context("/tmp"), true)
        local value, err = s:get("revctx.v")
        assert_nil(err)
        assert_eq(value, "tmp")
        assert_eq(s:close(), true)
      end)

      test(live_tests[8], function()
        local value, err = c:get("revctx.v")
        assert_nil(err)
        assert_eq(value, "global")
      end)

      c:close()
      end)
      os.execute("kill " .. tostring(guard.pid) .. " 2>/dev/null; rm -rf " .. guard.tmpdir)
      if not ran then error(failure, 0) end
    end
  end
end

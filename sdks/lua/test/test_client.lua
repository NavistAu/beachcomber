--- Tests for beachcomber.client: Result accessors and Client against a
--- mock backend (the interface documented at the top of
--- beachcomber/client.lua — canonical response tables, not raw sockets).

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local client_mod = require("beachcomber.client")
  local Client = client_mod.Client
  local Result = client_mod.Result

  -- ── Result accessors ──────────────────────────────────────────────────────

  suite("Result accessors")

  test("is_hit returns false on miss", function()
    local r = Result.new({ data = nil })
    assert_true(not r:is_hit(), "nil data should be a miss")
    assert_true(r:is_miss(), "no data and no template is a miss")
    assert_nil(r.data)
    assert_eq(r.age_ms, 0)
    assert_eq(r.stale, false)
    assert_nil(r.template)
    assert_nil(r.rest)
  end)

  test("template-only result is neither a hit nor a miss", function()
    local r = Result.new({ data = nil, template = "{{ 1 }}" })
    assert_eq(r.template, "{{ 1 }}")
    assert_nil(r.data)
    assert_true(not r:is_hit(), "no data is not a hit")
    assert_true(not r:is_miss(), "a template-only node is not a miss")
    assert_nil(r.rest)
  end)

  test("a hit carries the template half when the node has both", function()
    local r = Result.new({ data = 1, age_ms = 12, stale = false, template = "{{ 2 }}", rest = "x" })
    assert_true(r:is_hit())
    assert_true(not r:is_miss())
    assert_eq(r.data, 1)
    assert_eq(r.template, "{{ 2 }}")
    assert_eq(r.rest, "x")
  end)

  test("a plain hit has no template half", function()
    local r = Result.new({ data = "main", age_ms = 500, stale = false })
    assert_true(r:is_hit())
    assert_nil(r.template)
    assert_nil(r.rest)
  end)

  test("is_hit returns true on hit", function()
    local r = Result.new({ data = "main", age_ms = 500, stale = false })
    assert_true(r:is_hit())
    assert_eq(r.data, "main")
    assert_eq(r.age_ms, 500)
  end)

  test("is_hit true for numeric 0 data", function()
    local r = Result.new({ data = 0, age_ms = 10, stale = false })
    assert_true(r:is_hit())
  end)

  test("is_hit true for boolean false data", function()
    local r = Result.new({ data = false, age_ms = 10, stale = false })
    assert_true(r:is_hit())
  end)

  test("get_str on object data", function()
    local r = Result.new({ data = { branch = "main", dirty = false }, age_ms = 100 })
    local v, err = r:get_str("branch")
    assert_nil(err)
    assert_eq(v, "main")
  end)

  test("get_str missing field", function()
    local r = Result.new({ data = { branch = "main" } })
    local v, err = r:get_str("dirty")
    assert_nil(v)
    assert_not_nil(err)
  end)

  test("get_str on non-object data errors", function()
    local r = Result.new({ data = "main" })
    local v, err = r:get_str("anything")
    assert_nil(v)
    assert_not_nil(err)
  end)

  test("stale defaults to false", function()
    local r = Result.new({ data = "main", age_ms = 1000 })
    assert_eq(r.stale, false)
  end)

  -- ── Mock backend helper ────────────────────────────────────────────────────

  local function make_backend(overrides)
    local backend = { _calls = {} }
    backend.name = function(_) return "mock" end
    for k, v in pairs(overrides or {}) do backend[k] = v end
    return backend
  end

  -- ── Client:get / get_with_flags ───────────────────────────────────────────

  suite("Client:get")

  test("get returns Result on hit", function()
    local backend = make_backend({
      get = function(_, key, path, flags)
        return { ok = true, data = "main", age_ms = 500, stale = false }
      end,
    })
    local c = Client.new(backend)
    local r, err = c:get("git.branch", "/repo")
    assert_nil(err)
    assert_true(r:is_hit())
    assert_eq(r.data, "main")
  end)

  test("get forwards key and path to the backend", function()
    local seen
    local backend = make_backend({
      get = function(_, key, path, flags)
        seen = { key = key, path = path }
        return { ok = true }
      end,
    })
    Client.new(backend):get("git.branch", "/myrepo")
    assert_eq(seen.key, "git.branch")
    assert_eq(seen.path, "/myrepo")
  end)

  test("get returns miss Result when data is nil", function()
    local backend = make_backend({ get = function() return { ok = true } end })
    local r = Client.new(backend):get("git.branch")
    assert_true(not r:is_hit())
    assert_true(r:is_miss())
  end)

  test("get passes the backend's template half through", function()
    local backend = make_backend({
      get = function() return { ok = true, template = "{{ env.A }}", rest = "x" } end,
    })
    local r, err = Client.new(backend):get("fx.t.x")
    assert_nil(err)
    assert_eq(r.template, "{{ env.A }}")
    assert_eq(r.rest, "x")
    assert_true(not r:is_hit())
    assert_true(not r:is_miss())
  end)

  test("get passes a template group through as a table", function()
    local backend = make_backend({
      get = function() return { ok = true, template = { foo = "{{ 1 }}" } } end,
    })
    local r = Client.new(backend):get("fx.widgets")
    assert_eq(r.template.foo, "{{ 1 }}")
    assert_true(not r:is_miss())
  end)

  test("get returns nil+Error when backend returns ok=false", function()
    local backend = make_backend({
      get = function() return { ok = false, error = { kind = "server_error", message = "unknown provider: bad" } } end,
    })
    local r, err = Client.new(backend):get("bad.field")
    assert_nil(r)
    assert_not_nil(err)
    assert_eq(err.kind, "server_error")
    assert_true(tostring(err):find("unknown provider", 1, true) ~= nil)
  end)

  suite("Client:get_with_flags")

  test("get_with_flags propagates force/wait as a flags table", function()
    local seen_flags
    local backend = make_backend({
      get = function(_, key, path, flags) seen_flags = flags; return { ok = true } end,
    })
    Client.new(backend):get_with_flags("git.branch", nil, true, nil)
    assert_eq(seen_flags.force, true)
    assert_true(not seen_flags.wait)
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

  test("set_context supplies the default path to a later call with no explicit path", function()
    local seen_path
    local backend = make_backend({
      get = function(_, key, path) seen_path = path; return { ok = true } end,
    })
    local c = Client.new(backend)
    c:set_context("/some/dir")
    c:get("git.branch")
    assert_eq(seen_path, "/some/dir")
  end)

  test("an explicit path argument overrides the context default", function()
    local seen_path
    local backend = make_backend({
      get = function(_, key, path) seen_path = path; return { ok = true } end,
    })
    local c = Client.new(backend)
    c:set_context("/context/dir")
    c:get("git.branch", "/explicit/dir")
    assert_eq(seen_path, "/explicit/dir")
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

  -- ── resolve / eval against a live daemon (ffi transport) ──────────────────
  -- bc_resolve answers a miss as `"data":null`; the binding must hand that
  -- back as Lua nil (the miss Client:resolve documents), never as the
  -- json.NULL sentinel. Needs LuaJIT, the built library, and a comb binary
  -- ($COMB_BIN, or the workspace's own debug build).

  suite("ffi_backend resolve/eval against a live daemon")

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
    "resolve of a missing key is nil with no Error",
    "eval rendering none is the empty string, not the null sentinel",
    "resolve renders a put template",
    "refresh of a static namespace is the daemon's words alone",
    "a miss carries age 0 and stale false, not the null sentinel",
    "a template-only node carries age 0 and stale false, not the null sentinel",
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
        local value, err = c:resolve("revns.nope", { cwd = "/tmp" })
        assert_nil(err)
        assert_nil(value)
      end)

      test(live_tests[2], function()
        -- Canon: a missing ref is falsy and renders "" — a string, never
        -- a top-level null (and so never json.NULL through the binding).
        local value, err = c:eval("{{ none }}", { cwd = "/tmp" })
        assert_nil(err)
        assert_eq(value, "")
      end)

      test(live_tests[3], function()
        assert_true(c:put("revtpl:template", { label = "{{ 'resolved-' ~ env.MYVAR }}" }))
        local value, err = c:resolve("revtpl.label", { cwd = "/tmp", env = { MYVAR = "x" } })
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

      -- bc_get answers a miss and a template-only node with `age_ms` and
      -- `stale` null; the binding must collapse those so `if r.stale then`
      -- is false there, as it is in every other SDK.
      test(live_tests[5], function()
        assert_true(c:put("revmiss", { v = 1 }))
        local r, err = c:get("revmiss.nope")
        assert_nil(err)
        assert_true(r:is_miss())
        assert_eq(r.age_ms, 0)
        assert_eq(r.stale, false)
      end)

      test(live_tests[6], function()
        assert_true(c:put("revfresh:template", { t = "{{ 1 }}" }))
        local r, err = c:get("revfresh.t")
        assert_nil(err)
        assert_eq(r.template, "{{ 1 }}")
        assert_true(not r:is_hit() and not r:is_miss())
        assert_eq(r.age_ms, 0)
        assert_eq(r.stale, false)
      end)

      c:close()
      os.execute("kill " .. tostring(guard.pid) .. " 2>/dev/null; rm -rf " .. guard.tmpdir)
    end
  end
end

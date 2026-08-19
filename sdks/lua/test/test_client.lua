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
    assert_nil(r.data)
    assert_eq(r.age_ms, 0)
    assert_eq(r.stale, false)
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
end

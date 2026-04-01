--- Tests for beachcomber.client, including Result accessors and a mock server.
--
-- The mock server test requires luasocket. If luasocket is not installed the
-- mock-server tests are skipped gracefully.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local client_mod = require("beachcomber.client")
  local Client = client_mod.Client
  local Result = client_mod.Result
  local json   = require("beachcomber.json")

  -- ── Result accessors ──────────────────────────────────────────────────────

  suite("Result accessors")

  test("is_hit returns false on miss", function()
    local r = Result.new({ ok = true })
    assert_true(not r:is_hit(), "empty response should be a miss")
    assert_nil(r.data)
    assert_eq(r.age_ms, 0)
    assert_eq(r.stale, false)
  end)

  test("is_hit returns true on hit", function()
    local r = Result.new({ ok = true, data = "main", age_ms = 500, stale = false })
    assert_true(r:is_hit())
    assert_eq(r.data, "main")
    assert_eq(r.age_ms, 500)
    assert_eq(r.stale, false)
  end)

  test("is_hit true for numeric 0 data", function()
    -- data = 0 is a valid hit value
    local r = Result.new({ ok = true, data = 0, age_ms = 10, stale = false })
    assert_true(r:is_hit())
  end)

  test("is_hit true for boolean false data", function()
    local r = Result.new({ ok = true, data = false, age_ms = 10, stale = false })
    -- false is a valid value (e.g. git dirty = false), but Lua nil check:
    -- false ~= nil, so is_hit should be true
    assert_true(r:is_hit())
  end)

  test("get_str on object data", function()
    local r = Result.new({ ok = true, data = { branch = "main", dirty = false }, age_ms = 100 })
    local v, err = r:get_str("branch")
    assert_nil(err)
    assert_eq(v, "main")
  end)

  test("get_str missing field", function()
    local r = Result.new({ ok = true, data = { branch = "main" } })
    local v, err = r:get_str("dirty")
    -- dirty is false which isn't nil; this should work
    -- Actually dirty is not in data at all, so returns nil + error
    assert_nil(v)
    assert_not_nil(err)
  end)

  test("get_str on non-object data errors", function()
    local r = Result.new({ ok = true, data = "main" })
    local v, err = r:get_str("anything")
    assert_nil(v)
    assert_not_nil(err)
  end)

  test("stale defaults to false", function()
    local r = Result.new({ ok = true, data = "main", age_ms = 1000 })
    assert_eq(r.stale, false)
  end)

  test("stale propagates from response", function()
    local r = Result.new({ ok = true, data = "main", age_ms = 99999, stale = true })
    assert_true(r.stale)
  end)

  -- ── Mock handle helper ────────────────────────────────────────────────────
  -- Build a fake socket handle that replays canned responses.

  local function make_mock_handle(responses)
    local idx = 0
    local sent = {}
    return {
      _sent = sent,
      send_line = function(_, line)
        sent[#sent + 1] = line
        return true
      end,
      recv_line = function(_)
        idx = idx + 1
        local resp = responses[idx]
        if not resp then
          return nil, "mock handle: no more responses"
        end
        return resp
      end,
      close = function(_) end,
    }
  end

  -- ── Client with mock handle ───────────────────────────────────────────────

  suite("Client (mock handle)")

  test("get returns Result on hit", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = "main", age_ms = 500, stale = false }),
    })
    local c = Client.new(handle)
    local r, err = c:get("git.branch", "/repo")
    assert_nil(err)
    assert_not_nil(r)
    assert_true(r:is_hit())
    assert_eq(r.data, "main")
    assert_eq(r.age_ms, 500)
  end)

  test("get sends correct JSON", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:get("git.branch", "/myrepo")
    local sent_line = handle._sent[1]
    assert_not_nil(sent_line)
    local req = json.decode(sent_line)
    assert_eq(req.op, "get")
    assert_eq(req.key, "git.branch")
    assert_eq(req.path, "/myrepo")
  end)

  test("get without path omits path field", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:get("git.branch")
    local req = json.decode(handle._sent[1])
    assert_nil(req.path)
  end)

  test("get returns miss Result when no data field", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local r = c:get("git.branch")
    assert_not_nil(r)
    assert_true(not r:is_hit())
  end)

  test("get returns nil+error when server returns ok=false", function()
    local handle = make_mock_handle({
      json.encode({ ok = false, error = "unknown provider: bad" }),
    })
    local c = Client.new(handle)
    local r, err = c:get("bad.field")
    assert_nil(r)
    assert_not_nil(err)
    assert_true(err:find("unknown provider", 1, true) ~= nil, "error should mention provider: " .. err)
  end)

  test("poke sends correct op", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local ok, err = c:poke("git", "/repo")
    assert_nil(err)
    assert_true(ok)
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "poke")
    assert_eq(req.key, "git")
    assert_eq(req.path, "/repo")
  end)

  test("set_context sends correct op", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local ok, err = c:set_context("/some/dir")
    assert_nil(err)
    assert_true(ok)
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "context")
    assert_eq(req.path, "/some/dir")
  end)

  test("list returns provider array", function()
    local handle = make_mock_handle({
      json.encode({
        ok = true,
        data = {
          { name = "git", global = false, fields = {"branch", "dirty"} },
          { name = "hostname", global = true, fields = {"value"} },
        }
      }),
    })
    local c = Client.new(handle)
    local providers, err = c:list()
    assert_nil(err)
    assert_not_nil(providers)
    assert_eq(#providers, 2)
    assert_eq(providers[1].name, "git")
    assert_eq(providers[2].name, "hostname")
  end)

  test("status returns data table", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = { queue_depth = 0, cache_entries = 42 } }),
    })
    local c = Client.new(handle)
    local s, err = c:status()
    assert_nil(err)
    assert_not_nil(s)
    assert_eq(s.cache_entries, 42)
  end)

  -- ── Mock Unix socket server (requires luasocket) ──────────────────────────

  suite("Client (mock Unix socket server)")

  local socket_ok, socket_mod = pcall(require, "socket")
  local unix_ok, unix_mod = pcall(require, "socket.unix")

  if not socket_ok or not unix_ok then
    skip("all mock-server tests", "luasocket not available")
    return
  end

  -- Helper: spin up a trivial Unix socket server, run one exchange, return.
  -- server_fn receives (client_sock) and should read one line and write one.
  local function with_mock_server(tmppath, server_fn, client_fn)
    -- Remove leftover socket file
    os.remove(tmppath)

    local server = unix_mod()
    server:bind(tmppath)
    server:listen(1)
    server:settimeout(2)

    -- Client connects in the background by forking... but Lua has no fork.
    -- Instead we use non-blocking accept + do client work in-process via
    -- a second coroutine-style interleave.
    --
    -- Simpler approach: connect client first (non-blocking), then accept.
    local luasocket_backend = require("beachcomber.socket_luasocket")
    local sock_handle, conn_err = luasocket_backend.connect(tmppath)

    -- If connect failed (server not ready yet), accept first then retry.
    -- In practice luasocket connect to a bound/listening socket succeeds
    -- immediately on the same machine, so we accept after.
    local csock, acc_err = server:accept()
    if not csock then
      server:close()
      os.remove(tmppath)
      error("mock server accept failed: " .. tostring(acc_err))
    end
    csock:settimeout(2)

    -- If connect failed before accept was ready, try again after accept
    if not sock_handle then
      sock_handle, conn_err = luasocket_backend.connect(tmppath)
    end
    if not sock_handle then
      csock:close()
      server:close()
      os.remove(tmppath)
      error("mock client connect failed: " .. tostring(conn_err))
    end

    -- Run server handler (synchronous, one exchange)
    local ok, srv_err = pcall(server_fn, csock)
    csock:close()
    server:close()
    os.remove(tmppath)

    if not ok then
      sock_handle:close()
      error("mock server error: " .. tostring(srv_err))
    end

    -- Run client test
    client_fn(sock_handle)
    sock_handle:close()
  end

  local function tmppath()
    local tmpdir = os.getenv("TMPDIR") or "/tmp"
    tmpdir = tmpdir:gsub("/+$", "")
    return tmpdir .. "/beachcomber_test_" .. tostring(math.random(100000, 999999)) .. ".sock"
  end

  test("get over real Unix socket", function()
    local path = tmppath()
    with_mock_server(
      path,
      function(csock)
        -- Read request line
        local line = csock:receive("*l")
        assert_not_nil(line)
        local req = json.decode(line)
        assert_eq(req.op, "get")
        -- Send response
        csock:send(json.encode({ ok = true, data = "feature-branch", age_ms = 250, stale = false }) .. "\n")
      end,
      function(handle)
        local c = Client.new(handle)
        local r, err = c:get("git.branch", "/testrepo")
        assert_nil(err)
        assert_not_nil(r)
        assert_true(r:is_hit())
        assert_eq(r.data, "feature-branch")
        assert_eq(r.age_ms, 250)
      end
    )
  end)

  test("list over real Unix socket", function()
    local path = tmppath()
    with_mock_server(
      path,
      function(csock)
        csock:receive("*l") -- discard request
        csock:send(json.encode({
          ok = true,
          data = { { name = "git", global = false, fields = {"branch"} } }
        }) .. "\n")
      end,
      function(handle)
        local c = Client.new(handle)
        local providers, err = c:list()
        assert_nil(err)
        assert_eq(#providers, 1)
        assert_eq(providers[1].name, "git")
      end
    )
  end)

  test("server error propagates", function()
    local path = tmppath()
    with_mock_server(
      path,
      function(csock)
        csock:receive("*l")
        csock:send(json.encode({ ok = false, error = "unknown provider: nope" }) .. "\n")
      end,
      function(handle)
        local c = Client.new(handle)
        local r, err = c:get("nope.field")
        assert_nil(r)
        assert_not_nil(err)
        assert_true(err:find("unknown provider", 1, true) ~= nil)
      end
    )
  end)
end

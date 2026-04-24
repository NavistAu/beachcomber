--- Tests for the new Phase 10 Client methods:
-- hello, get_with_flags, put, put_null, introspect, status_rows, watch.
--
-- All tests use a mock handle to avoid requiring a live daemon or luasocket.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local client_mod  = require("beachcomber.client")
  local WatchStream = require("beachcomber.watch_stream")
  local Client      = client_mod.Client
  local Result      = client_mod.Result
  local json        = require("beachcomber.json")

  -- ── Mock handle helper ────────────────────────────────────────────────────

  local function make_mock_handle(responses)
    local idx  = 0
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

  -- ── hello ─────────────────────────────────────────────────────────────────

  suite("Client:hello")

  test("hello returns protocol_version and daemon_version", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = { protocol_version = "1", daemon_version = "0.9.0" } }),
    })
    local c = Client.new(handle)
    local info, err = c:hello()
    assert_nil(err)
    assert_not_nil(info)
    assert_eq(info.protocol_version, "1")
    assert_eq(info.daemon_version, "0.9.0")
  end)

  test("hello sends correct op", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = { protocol_version = "1", daemon_version = "0.9.0" } }),
    })
    local c = Client.new(handle)
    c:hello()
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "hello")
  end)

  test("hello defaults empty strings when data missing", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local info, err = c:hello()
    assert_nil(err)
    assert_not_nil(info)
    assert_eq(info.protocol_version, "")
    assert_eq(info.daemon_version, "")
  end)

  test("hello returns nil+error on server error", function()
    local handle = make_mock_handle({
      json.encode({ ok = false, error = "not implemented" }),
    })
    local c = Client.new(handle)
    local info, err = c:hello()
    assert_nil(info)
    assert_not_nil(err)
  end)

  -- ── get_with_flags ────────────────────────────────────────────────────────

  suite("Client:get_with_flags")

  test("get_with_flags returns Result", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = "main", age_ms = 10, stale = false }),
    })
    local c = Client.new(handle)
    local r, err = c:get_with_flags("git.branch", "/repo", false, false)
    assert_nil(err)
    assert_not_nil(r)
    assert_true(r:is_hit())
    assert_eq(r.data, "main")
  end)

  test("get_with_flags propagates force=true", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:get_with_flags("git.branch", nil, true, nil)
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "get")
    assert_eq(req.force, true)
    assert_nil(req.wait)
  end)

  test("get_with_flags propagates wait=true", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:get_with_flags("git.branch", nil, nil, true)
    local req = json.decode(handle._sent[1])
    assert_eq(req.wait, true)
    assert_nil(req.force)
  end)

  test("get_with_flags omits flags when false/nil", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:get_with_flags("git.branch", nil, false, false)
    local req = json.decode(handle._sent[1])
    assert_nil(req.force)
    assert_nil(req.wait)
  end)

  test("get_with_flags includes path when provided", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:get_with_flags("git.branch", "/mypath", nil, nil)
    local req = json.decode(handle._sent[1])
    assert_eq(req.path, "/mypath")
  end)

  -- ── put ───────────────────────────────────────────────────────────────────

  suite("Client:put")

  test("put sends correct op and key", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local ok, err = c:put("mykey", { x = 1 })
    assert_nil(err)
    assert_true(ok)
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "put")
    assert_eq(req.key, "mykey")
  end)

  test("put sends data payload", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:put("mykey", { color = "blue" })
    local req = json.decode(handle._sent[1])
    assert_not_nil(req.data)
    assert_eq(req.data.color, "blue")
  end)

  test("put with ttl includes ttl field", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:put("mykey", { v = 1 }, 60)
    local req = json.decode(handle._sent[1])
    assert_eq(req.ttl, 60)
  end)

  test("put with path includes path field", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:put("mykey", { v = 1 }, nil, "/mypath")
    local req = json.decode(handle._sent[1])
    assert_eq(req.path, "/mypath")
  end)

  test("put returns false+error on server error", function()
    local handle = make_mock_handle({
      json.encode({ ok = false, error = "must be a JSON object" }),
    })
    local c = Client.new(handle)
    local ok, err = c:put("mykey", "not-an-object")
    assert_eq(ok, false)
    assert_not_nil(err)
    assert_true(err:find("object", 1, true) ~= nil)
  end)

  -- ── put_null ──────────────────────────────────────────────────────────────

  suite("Client:put_null")

  test("put_null sends put op without data field", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local ok, err = c:put_null("mykey")
    assert_nil(err)
    assert_true(ok)
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "put")
    assert_eq(req.key, "mykey")
    assert_nil(req.data)
  end)

  test("put_null passes path when provided", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    c:put_null("mykey", "/somepath")
    local req = json.decode(handle._sent[1])
    assert_eq(req.path, "/somepath")
  end)

  -- ── introspect ────────────────────────────────────────────────────────────

  suite("Client:introspect")

  test("introspect daemon returns typed daemon shape", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = { pid = 1234, version = "0.9.0", uptime_secs = 100 } }),
    })
    local c = Client.new(handle)
    local result, err = c:introspect("daemon")
    assert_nil(err)
    assert_not_nil(result)
    assert_eq(result.subject, "daemon")
    assert_not_nil(result.daemon)
    assert_eq(result.daemon.pid, 1234)
    assert_nil(result.other)
  end)

  test("introspect non-daemon subject returns other", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = { entries = 5 } }),
    })
    local c = Client.new(handle)
    local result, err = c:introspect("cache")
    assert_nil(err)
    assert_not_nil(result)
    assert_eq(result.subject, "cache")
    assert_nil(result.daemon)
    assert_not_nil(result.other)
    assert_eq(result.other.entries, 5)
  end)

  test("introspect sends correct op and subject", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = {} }),
    })
    local c = Client.new(handle)
    c:introspect("providers")
    local req = json.decode(handle._sent[1])
    assert_eq(req.op, "introspect")
    assert_eq(req.subject, "providers")
  end)

  test("introspect with duration_secs sends duration_secs", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = {} }),
    })
    local c = Client.new(handle)
    c:introspect("timers", 30)
    local req = json.decode(handle._sent[1])
    assert_eq(req.duration_secs, 30)
  end)

  test("introspect returns nil+error on server error", function()
    local handle = make_mock_handle({
      json.encode({ ok = false, error = "unknown subject" }),
    })
    local c = Client.new(handle)
    local result, err = c:introspect("bad_subject")
    assert_nil(result)
    assert_not_nil(err)
  end)

  -- ── status_rows ───────────────────────────────────────────────────────────

  suite("Client:status_rows")

  test("status_rows returns array of rows", function()
    local rows = {
      { key = "git.branch", age_ms = 100 },
      { key = "git.dirty",  age_ms = 200 },
    }
    local handle = make_mock_handle({
      json.encode({ ok = true, data = rows }),
    })
    local c = Client.new(handle)
    local result, err = c:status_rows()
    assert_nil(err)
    assert_not_nil(result)
    assert_eq(#result, 2)
    assert_eq(result[1].key, "git.branch")
    assert_eq(result[2].age_ms, 200)
  end)

  test("status_rows returns empty array when no data", function()
    local handle = make_mock_handle({
      json.encode({ ok = true }),
    })
    local c = Client.new(handle)
    local result, err = c:status_rows()
    assert_nil(err)
    assert_not_nil(result)
    assert_eq(#result, 0)
  end)

  test("status_rows returns nil+error on server error", function()
    local handle = make_mock_handle({
      json.encode({ ok = false, error = "internal error" }),
    })
    local c = Client.new(handle)
    local result, err = c:status_rows()
    assert_nil(result)
    assert_not_nil(err)
  end)

  test("status_row exposes lifecycle fields as raw table fields", function()
    local rows = {
      {
        provider          = "git",
        kind              = { kind = "lifecycle", decay = 0, watches_files = true },
        poll_interval_secs = 5,
        keep_alive_polls  = 3,
        fsevents_reinstate = false,
      },
    }
    local handle = make_mock_handle({
      json.encode({ ok = true, data = rows }),
    })
    local c = Client.new(handle)
    local result, err = c:status_rows()
    assert_nil(err)
    assert_not_nil(result)
    local git
    for _, r in ipairs(result) do
      if r.provider == "git" then git = r; break end
    end
    assert_not_nil(git)
    assert_not_nil(git.kind)
    assert_eq(git.kind.kind, "lifecycle")
    assert_true(git.poll_interval_secs > 0)
    assert_true(git.keep_alive_polls > 0)
  end)

  -- ── watch / WatchStream ───────────────────────────────────────────────────

  suite("Client:watch and WatchStream")

  test("watch sends correct op and returns WatchStream", function()
    local ws_handle = make_mock_handle({
      json.encode({ ok = true, data = 42, age_ms = 5 }),
    })
    -- Factory returns a fresh mock handle for the dedicated watch connection.
    local factory_called = false
    local factory = function()
      factory_called = true
      return ws_handle
    end
    local main_handle = make_mock_handle({})
    local c = Client.new(main_handle, factory)
    local stream, err = c:watch("fixture.count", "/repo")
    assert_nil(err)
    assert_not_nil(stream)
    assert_true(factory_called)
    -- The watch request was sent on the dedicated handle.
    local req = json.decode(ws_handle._sent[1])
    assert_eq(req.op, "watch")
    assert_eq(req.key, "fixture.count")
    assert_eq(req.path, "/repo")
  end)

  test("WatchStream:next_event returns event table", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = 7, age_ms = 12, stale = false }),
    })
    local stream = WatchStream.new(handle)
    local ev, err = stream:next_event()
    assert_nil(err)
    assert_not_nil(ev)
    assert_eq(ev.data, 7)
    assert_eq(ev.age_ms, 12)
    assert_eq(ev.stale, false)
  end)

  test("WatchStream:next_event returns nil on server error", function()
    local handle = make_mock_handle({
      json.encode({ ok = false, error = "watch closed" }),
    })
    local stream = WatchStream.new(handle)
    local ev, err = stream:next_event()
    assert_nil(ev)
    assert_not_nil(err)
  end)

  test("WatchStream:next_event returns nil after close", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = 1, age_ms = 1 }),
    })
    local stream = WatchStream.new(handle)
    stream:close()
    local ev, _ = stream:next_event()
    assert_nil(ev)
  end)

  test("WatchStream:each iterates events", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = 1, age_ms = 1 }),
      json.encode({ ok = true, data = 2, age_ms = 2 }),
      -- no more responses -> nil from recv_line -> iteration ends
    })
    local stream = WatchStream.new(handle)
    local collected = {}
    for ev in stream:each() do
      collected[#collected + 1] = ev.data
    end
    assert_eq(#collected, 2)
    assert_eq(collected[1], 1)
    assert_eq(collected[2], 2)
  end)

  test("WatchStream stale defaults to false", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = "x", age_ms = 0 }),
    })
    local stream = WatchStream.new(handle)
    local ev = stream:next_event()
    assert_not_nil(ev)
    assert_eq(ev.stale, false)
  end)

  test("WatchStream stale propagates true", function()
    local handle = make_mock_handle({
      json.encode({ ok = true, data = "x", age_ms = 9999, stale = true }),
    })
    local stream = WatchStream.new(handle)
    local ev = stream:next_event()
    assert_not_nil(ev)
    assert_true(ev.stale)
  end)

  -- ── socket_cli fallback rejects unsupported ops ───────────────────────────

  suite("socket_cli: unsupported ops return ok=false")

  -- Stub io.open so find_comb() succeeds.
  local function with_comb_on_path(fn)
    local saved = io.open
    io.open = function(path, mode)
      if path:match("/comb$") then
        return { close = function() end }
      end
      return saved(path, mode)
    end
    local ok, err = pcall(fn)
    io.open = saved
    if not ok then error(err, 0) end
  end

  local function reload_socket_cli()
    package.loaded["beachcomber.socket_cli"] = nil
    return require("beachcomber.socket_cli")
  end

  local function cli_response_for(op, extra)
    local req = { op = op }
    if extra then
      for k, v in pairs(extra) do req[k] = v end
    end
    local resp_line
    with_comb_on_path(function()
      local socket_cli = reload_socket_cli()
      local handle = socket_cli.connect()
      handle:send_line(json.encode(req) .. "\n")
      resp_line = handle:recv_line()
    end)
    return json.decode(resp_line)
  end

  test("hello returns ok=false with clear error", function()
    local resp = cli_response_for("hello")
    assert_eq(resp.ok, false)
    assert_not_nil(resp.error)
    assert_true(resp.error:find("hello", 1, true) ~= nil or
                resp.error:find("not supported", 1, true) ~= nil)
  end)

  test("put returns ok=false with clear error", function()
    local resp = cli_response_for("put", { key = "x", data = { v = 1 } })
    assert_eq(resp.ok, false)
    assert_not_nil(resp.error)
    assert_true(resp.error:find("not supported", 1, true) ~= nil)
  end)

  test("introspect returns ok=false with clear error", function()
    local resp = cli_response_for("introspect", { subject = "daemon" })
    assert_eq(resp.ok, false)
    assert_not_nil(resp.error)
    assert_true(resp.error:find("not supported", 1, true) ~= nil)
  end)

  test("watch returns ok=false with clear error", function()
    local resp = cli_response_for("watch", { key = "git.branch" })
    assert_eq(resp.ok, false)
    assert_not_nil(resp.error)
    assert_true(resp.error:find("not supported", 1, true) ~= nil)
  end)
end

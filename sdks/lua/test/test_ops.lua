--- Tests for Client:hello/put/put_null/introspect/resolve/eval/watch, and
--- for the unsupported-capability path (a backend that omits an optional
--- method) — against a mock backend, matching test_client.lua's pattern.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local client_mod  = require("beachcomber.client")
  local Client      = client_mod.Client

  local function make_backend(overrides)
    local backend = { name = function(_) return "mock" end }
    for k, v in pairs(overrides or {}) do backend[k] = v end
    return backend
  end

  -- ── hello ─────────────────────────────────────────────────────────────────

  suite("Client:hello")

  test("hello returns protocol_version and daemon_version", function()
    local backend = make_backend({
      hello = function() return { ok = true, data = { protocol_version = "1", daemon_version = "0.9.0" } } end,
    })
    local info, err = Client.new(backend):hello()
    assert_nil(err)
    assert_eq(info.protocol_version, "1")
    assert_eq(info.daemon_version, "0.9.0")
  end)

  test("hello defaults empty strings when data missing", function()
    local backend = make_backend({ hello = function() return { ok = true } end })
    local info, err = Client.new(backend):hello()
    assert_nil(err)
    assert_eq(info.protocol_version, "")
    assert_eq(info.daemon_version, "")
  end)

  test("hello returns nil+Error on backend error", function()
    local backend = make_backend({
      hello = function() return { ok = false, error = { kind = "server_error", message = "not implemented" } } end,
    })
    local info, err = Client.new(backend):hello()
    assert_nil(info)
    assert_not_nil(err)
  end)

  test("hello returns Error.unsupported when the backend omits it", function()
    local backend = make_backend({})
    local info, err = Client.new(backend):hello()
    assert_nil(info)
    assert_eq(err.kind, "unsupported")
    assert_true(tostring(err):find("mock", 1, true) ~= nil, "unsupported error should name the transport")
  end)

  -- ── put / put_null ────────────────────────────────────────────────────────

  suite("Client:put")

  test("put forwards key, data, ttl, path", function()
    local seen
    local backend = make_backend({
      put = function(_, key, data, ttl, path) seen = { key = key, data = data, ttl = ttl, path = path }; return { ok = true } end,
    })
    local ok, err = Client.new(backend):put("mykey", { color = "blue" }, "60s", "/p")
    assert_nil(err)
    assert_true(ok)
    assert_eq(seen.key, "mykey")
    assert_eq(seen.data.color, "blue")
    assert_eq(seen.ttl, "60s")
    assert_eq(seen.path, "/p")
  end)

  test("put returns false+Error on backend error", function()
    local backend = make_backend({
      put = function() return { ok = false, error = { kind = "server_error", message = "must be a JSON object" } } end,
    })
    local ok, err = Client.new(backend):put("mykey", "not-an-object")
    assert_eq(ok, false)
    assert_true(tostring(err):find("object", 1, true) ~= nil)
  end)

  suite("Client:put_null")

  test("put_null forwards key and path", function()
    local seen
    local backend = make_backend({
      put_null = function(_, key, path) seen = { key = key, path = path }; return { ok = true } end,
    })
    local ok, err = Client.new(backend):put_null("mykey", "/somepath")
    assert_nil(err)
    assert_true(ok)
    assert_eq(seen.key, "mykey")
    assert_eq(seen.path, "/somepath")
  end)

  -- ── introspect ────────────────────────────────────────────────────────────

  suite("Client:introspect")

  test("introspect daemon returns typed daemon shape", function()
    local backend = make_backend({
      introspect = function(_, subject) return { ok = true, data = { pid = 1234, version = "0.9.0" } } end,
    })
    local result, err = Client.new(backend):introspect("daemon")
    assert_nil(err)
    assert_eq(result.subject, "daemon")
    assert_eq(result.daemon.pid, 1234)
    assert_nil(result.other)
  end)

  test("introspect non-daemon subject returns other", function()
    local backend = make_backend({
      introspect = function() return { ok = true, data = { entries = 5 } } end,
    })
    local result, err = Client.new(backend):introspect("cache")
    assert_nil(err)
    assert_eq(result.subject, "cache")
    assert_nil(result.daemon)
    assert_eq(result.other.entries, 5)
  end)

  test("introspect forwards subject and duration_secs", function()
    local seen
    local backend = make_backend({
      introspect = function(_, subject, duration_secs) seen = { subject = subject, duration_secs = duration_secs }; return { ok = true, data = {} } end,
    })
    Client.new(backend):introspect("timers", 30)
    assert_eq(seen.subject, "timers")
    assert_eq(seen.duration_secs, 30)
  end)

  test("introspect returns Error.unsupported when the backend omits it", function()
    local result, err = Client.new(make_backend({})):introspect("daemon")
    assert_nil(result)
    assert_eq(err.kind, "unsupported")
  end)

  -- ── resolve / eval ────────────────────────────────────────────────────────

  suite("Client:resolve")

  test("resolve forwards key, cwd, env, virtual overrides", function()
    local seen
    local backend = make_backend({
      resolve = function(_, key, cwd, env, overrides)
        seen = { key = key, cwd = cwd, env = env, overrides = overrides }
        return { ok = true, data = "baz" }
      end,
    })
    local value, err = Client.new(backend):resolve("filters.based", {
      cwd = "/tmp", env = { PYVAR = "/foo/bar/baz" }, virtual = { ["filters.based"] = "env.PYVAR | basename" },
    })
    assert_nil(err)
    assert_eq(value, "baz")
    assert_eq(seen.key, "filters.based")
    assert_eq(seen.cwd, "/tmp")
    assert_eq(seen.env.PYVAR, "/foo/bar/baz")
    assert_eq(seen.overrides["filters.based"], "env.PYVAR | basename")
  end)

  test("resolve defaults cwd to the context path", function()
    local seen_cwd
    local backend = make_backend({
      resolve = function(_, key, cwd) seen_cwd = cwd; return { ok = true, data = nil } end,
    })
    local c = Client.new(backend)
    c:set_context("/ctx")
    c:resolve("myproject")
    assert_eq(seen_cwd, "/ctx")
  end)

  test("resolve returns Error.unsupported when the backend omits it", function()
    local value, err = Client.new(make_backend({})):resolve("x.y")
    assert_nil(value)
    assert_eq(err.kind, "unsupported")
  end)

  suite("Client:eval")

  test("eval forwards template, cwd, env, overrides", function()
    local seen
    local backend = make_backend({
      eval = function(_, template, cwd, env, overrides)
        seen = { template = template, cwd = cwd }
        return { ok = true, data = "rendered" }
      end,
    })
    local value = Client.new(backend):eval("{{ git.branch }}", { cwd = "/repo" })
    assert_eq(value, "rendered")
    assert_eq(seen.template, "{{ git.branch }}")
    assert_eq(seen.cwd, "/repo")
  end)

  -- ── watch / WatchStream ───────────────────────────────────────────────────

  suite("Client:watch and WatchStream")

  test("watch opens via the backend and returns a WatchStream", function()
    local opened_with
    local watch_handle = {
      next_event = function(_, timeout_ms) return { data = 42, age_ms = 5, stale = false }, nil, "event" end,
      cancel = function(_) end,
      close = function(_) end,
    }
    local backend = make_backend({
      watch_open = function(_, key, path) opened_with = { key = key, path = path }; return watch_handle end,
    })
    local stream, err = Client.new(backend):watch("fixture.count", "/repo")
    assert_nil(err)
    assert_not_nil(stream)
    assert_eq(opened_with.key, "fixture.count")
    assert_eq(opened_with.path, "/repo")

    local ev = stream:next_event()
    assert_eq(ev.data, 42)
    assert_eq(ev.age_ms, 5)
  end)

  test("WatchStream:next_event returns nil, Error on outcome=error", function()
    local Error = require("beachcomber.error")
    local watch_handle = {
      next_event = function() return nil, Error.new("server_error", "watch closed"), "error" end,
    }
    local backend = make_backend({ watch_open = function() return watch_handle end })
    local stream = Client.new(backend):watch("k")
    local ev, err = stream:next_event()
    assert_nil(ev)
    assert_not_nil(err)
  end)

  test("WatchStream:next_event returns nil (no error) on outcome=eof", function()
    local watch_handle = { next_event = function() return nil, nil, "eof" end }
    local backend = make_backend({ watch_open = function() return watch_handle end })
    local stream = Client.new(backend):watch("k")
    local ev, err = stream:next_event()
    assert_nil(ev)
    assert_nil(err)
  end)

  test("WatchStream:each iterates events until eof", function()
    local calls = 0
    local watch_handle = {
      next_event = function()
        calls = calls + 1
        if calls <= 2 then return { data = calls }, nil, "event" end
        return nil, nil, "eof"
      end,
    }
    local backend = make_backend({ watch_open = function() return watch_handle end })
    local stream = Client.new(backend):watch("k")
    local collected = {}
    for ev in stream:each() do collected[#collected + 1] = ev.data end
    assert_eq(#collected, 2)
    assert_eq(collected[1], 1)
    assert_eq(collected[2], 2)
  end)

  test("watch returns Error.unsupported when the backend omits watch_open", function()
    local stream, err = Client.new(make_backend({})):watch("k")
    assert_nil(stream)
    assert_eq(err.kind, "unsupported")
  end)
end

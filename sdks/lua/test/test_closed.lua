--- A closed client or session is an io_error on any later call, answered
--- before the backend is reached; a closed watch stream has ended
--- (docs/sdk-api.md: a closed handle). Handing the library a freed handle
--- would crash the process, so the backend must never see one.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local Client = require("beachcomber.client").Client
  local WatchStream = require("beachcomber.watch_stream")

  --- A backend that records every call it receives.
  local function recording_backend()
    local calls = {}
    local backend = {}
    function backend:name() return "ffi" end
    function backend:get() calls[#calls + 1] = "get"; return { ok = true, data = 1 } end
    function backend:hello()
      calls[#calls + 1] = "hello"
      return { ok = true, data = { protocol_version = "1", daemon_version = "x" } }
    end
    function backend:session_open()
      return {
        get = function() calls[#calls + 1] = "session.get"; return { ok = true, data = 1 } end,
        close = function() calls[#calls + 1] = "session.close" end,
      }
    end
    function backend:close() calls[#calls + 1] = "close" end
    return backend, calls
  end

  suite("a closed handle")

  test("a closed client is an io_error, and closing twice closes once", function()
    local backend, calls = recording_backend()
    local client = Client.new(backend)
    assert_true(client:close())
    assert_true(client:close())
    local value, err = client:get("p.f")
    assert_nil(value)
    assert_eq(err.kind, "io_error")
    assert_eq(err.message, "the client is closed")
    local _, hello_err = client:hello()
    assert_eq(hello_err.kind, "io_error")
    assert_eq(table.concat(calls, ","), "close")
  end)

  test("a closed session is an io_error", function()
    local backend, calls = recording_backend()
    local session = Client.new(backend):session()
    assert_true(session:close())
    assert_true(session:close())
    local value, err = session:get("p.f")
    assert_nil(value)
    assert_eq(err.kind, "io_error")
    assert_eq(err.message, "the session is closed")
    assert_eq(table.concat(calls, ","), "session.close")
  end)

  test("a closed watch stream has ended, and cancel is a no-op", function()
    local handle_calls = {}
    local handle = {
      next_event = function() handle_calls[#handle_calls + 1] = "next_event" end,
      cancel = function() handle_calls[#handle_calls + 1] = "cancel" end,
      close = function() handle_calls[#handle_calls + 1] = "close" end,
    }
    local stream = WatchStream.new(handle)
    stream:close()
    local before = #handle_calls
    assert_nil(stream:next_event())
    assert_true(stream:cancel())
    assert_eq(#handle_calls, before)
  end)
end

--- The SDK API table (docs/sdk-api.md), checked: this SDK's public surface
--- against sdks/api.json, the table's machine-readable form. A method, an
--- option, an error kind or a typed field added or dropped here without the
--- table fails this suite. Runs under PUC Lua and LuaJIT; the ffi backend's
--- options check needs LuaJIT.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local json = require("beachcomber.json")
  local Client = require("beachcomber.client").Client
  local WatchStream = require("beachcomber.watch_stream")
  local Error = require("beachcomber.error")

  local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
  local sdk_dir = script_dir .. "../"

  local function read_file(path)
    local f = assert(io.open(path, "r"), "cannot read " .. path)
    local body = f:read("*a")
    f:close()
    return body
  end

  local api = json.decode(read_file(sdk_dir .. "../api.json"))

  --- The sorted names of `t`'s public functions: not `_`-prefixed, not a
  -- constructor, not in `except`.
  local function public_functions(t, except)
    local names = {}
    for name, value in pairs(t) do
      if type(value) == "function" and not name:match("^_") and name ~= "new" and not (except or {})[name] then
        names[#names + 1] = name
      end
    end
    table.sort(names)
    return names
  end

  local function sorted(list, extra)
    local out = {}
    for _, v in ipairs(list) do out[#out + 1] = v end
    for _, v in ipairs(extra or {}) do out[#out + 1] = v end
    table.sort(out)
    return out
  end

  local function keys_of(t)
    local out = {}
    for k in pairs(t) do out[#out + 1] = k end
    table.sort(out)
    return out
  end

  local function assert_same(actual, expected, what)
    assert_eq(table.concat(actual, ","), table.concat(expected, ","), what)
  end

  --- A backend with every op, answering each with `answer` (a response
  -- table) — enough to reach a Session and a WatchStream.
  local function stub_backend()
    local session = {
      get = function() return { ok = true, data = "v" } end,
      put = function() return { ok = true } end,
      refresh = function() return { ok = true } end,
      set_context = function() return { ok = true } end,
      close = function() end,
    }
    local watch = {
      next_event = function() return { data = 1 }, nil, "event" end,
      cancel = function() end,
      close = function() end,
    }
    return {
      name = function() return "stub" end,
      session_open = function() return session end,
      watch_open = function() return watch end,
      hello = function()
        return { ok = true, data = { protocol_version = "1", daemon_version = "2", extra = "dropped" } }
      end,
      introspect = function(_, subject)
        if subject == "daemon" then return { ok = true, data = { pid = 1 } } end
        return { ok = true, data = { anything = true } }
      end,
    }
  end

  suite("the SDK API table (sdks/api.json)")

  test("the client's methods are the table's", function()
    assert_same(public_functions(Client), sorted(api.client.methods, api.tiered_client.methods), "Client")
  end)

  test("a session's methods are the table's", function()
    local session = Client.new(stub_backend()):session()
    assert_not_nil(session, "session() returns a Session")
    assert_same(public_functions(getmetatable(session).__index), sorted(api.session.methods), "Session")
  end)

  test("a watch stream's methods are the table's, besides the language's iterator", function()
    -- `each` is Lua's way to iterate (`for ev in stream:each() do`), as
    -- the table allows each language its own.
    assert_same(public_functions(WatchStream, { each = true }), sorted(api.watch_stream.methods), "WatchStream")
  end)

  if not pcall(require, "ffi") then
    skip("the ffi backend sends every client option the table names", "no ffi (PUC Lua)")
  else
    test("the ffi backend sends every client option the table names", function()
      local given = {}
      for i, name in ipairs(api.client.options) do given[name] = i end
      local sent = json.decode(require("beachcomber.ffi_backend").options_json(given))
      assert_same(keys_of(sent), sorted(api.client.options), "options_json")
    end)
  end

  test("the error kinds are the table's", function()
    assert_same(sorted(Error.KINDS), sorted(api.error_kinds), "Error.KINDS")
  end)

  test("an error carries the table's fields", function()
    local err = Error.new("timeout", "took too long", "9.9.9")
    for _, field in ipairs(api.error_fields) do
      assert_not_nil(err[field], "Error." .. field)
    end
    assert_eq(err.message, "took too long", "the message is the one given, nothing added")
  end)

  test("every kind the SDK raises itself is in the table", function()
    local known = {}
    for _, kind in ipairs(api.error_kinds) do known[kind] = true end
    for _, module in ipairs({ "client", "error", "ffi", "ffi_backend", "subprocess_backend", "watch_stream", "init" }) do
      local source = read_file(sdk_dir .. "beachcomber/" .. module .. ".lua")
      for kind in source:gmatch('Error%.new%(%s*"([%w_]+)"') do
        assert_true(known[kind], module .. ".lua raises unknown kind " .. kind)
      end
      for kind in source:gmatch('kind%s*=%s*"([%w_]+)"') do
        assert_true(known[kind], module .. ".lua builds unknown kind " .. kind)
      end
    end
  end)

  test("hello answers exactly the table's fields", function()
    local info = Client.new(stub_backend()):hello()
    assert_same(keys_of(info), sorted(api.types.hello_info), "hello")
  end)

  test("an introspect response carries only the table's fields", function()
    local allowed = {}
    for _, field in ipairs(api.types.introspect_response) do allowed[field] = true end
    local c = Client.new(stub_backend())
    for _, subject in ipairs({ "daemon", "cache" }) do
      local response = c:introspect(subject)
      assert_eq(response.subject, subject)
      for field in pairs(response) do
        assert_true(allowed[field], "introspect " .. subject .. " carries " .. field)
      end
    end
  end)

  test("a watch event carries exactly the table's fields", function()
    local event = Client.new(stub_backend()):watch("k"):next_event()
    assert_same(keys_of(event), sorted(api.types.watch_event), "watch event")
  end)
end

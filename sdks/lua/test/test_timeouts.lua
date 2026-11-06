--- Timeouts are the ABI's options (`bc_client_new`'s `timeout_ms` and
--- `connect_timeout_ms`), passed through as given with no default of this
--- SDK's own; the subprocess backend, which cannot bound a spawn, refuses
--- them.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local json = require("beachcomber.json")

  suite("timeouts: the ABI's options, passed through as given")

  local has_ffi = pcall(require, "ffi")

  local function built_lib()
    local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
    for _, name in ipairs({ "libbeachcomber.dylib", "libbeachcomber.so" }) do
      local path = script_dir .. "../../../target/debug/" .. name
      local f = io.open(path, "rb")
      if f then f:close(); return path end
    end
    return nil
  end

  local option_tests = {
    "no options is no options_json at all",
    "options_json carries exactly the keys given, under the ABI's spelling",
    "connect_timeout_ms = 0 is one attempt: a socket nobody serves fails at once",
  }

  if not has_ffi then
    for _, name in ipairs(option_tests) do skip(name, "no ffi (PUC Lua)") end
  else
    local ffi = require("ffi")
    local ffi_backend = require("beachcomber.ffi_backend")

    test(option_tests[1], function()
      assert_nil(ffi_backend.options_json({}))
    end)

    test(option_tests[2], function()
      local decoded = json.decode(ffi_backend.options_json({ timeout_ms = 250, connect_timeout_ms = 0 }))
      assert_eq(decoded.timeout_ms, 250)
      assert_eq(decoded.connect_timeout_ms, 0)
      local n = 0
      for _ in pairs(decoded) do n = n + 1 end
      assert_eq(n, 2, "only the keys given")
    end)

    local lib = built_lib()
    if not lib then
      skip(option_tests[3], "no libbeachcomber built")
    else
      pcall(ffi.cdef, [[
        typedef struct { long tv_sec; long tv_nsec; } bc_test_timespec;
        int clock_gettime(int clk_id, bc_test_timespec *tp);
      ]])
      local CLOCK_MONOTONIC = ffi.os == "OSX" and 6 or 1
      local function now_ms()
        local ts = ffi.new("bc_test_timespec")
        ffi.C.clock_gettime(CLOCK_MONOTONIC, ts)
        return tonumber(ts.tv_sec) * 1000 + tonumber(ts.tv_nsec) / 1e6
      end

      test(option_tests[3], function()
        local p = io.popen("mktemp -d /tmp/bclua-XXXXXX", "r")
        local dir = p:read("*l")
        p:close()
        local backend, cerr = ffi_backend.new({
          library_path = lib, socket_path = dir .. "/sock", autostart = false, connect_timeout_ms = 0,
        })
        assert_not_nil(backend, tostring(cerr))
        local client = require("beachcomber.client").Client.new(backend)
        local started = now_ms()
        local value, err = client:hello()
        local took = now_ms() - started
        client:close()
        os.execute("rm -rf " .. dir)
        assert_nil(value)
        assert_eq(err and err.kind, "connection_failed")
        assert_true(took < 250, "one attempt took " .. tostring(took) .. "ms")
      end)
    end
  end

  suite("timeouts: the subprocess backend cannot bound a spawn")

  local subprocess_backend = require("beachcomber.subprocess_backend")

  for _, option in ipairs({ "timeout_ms", "connect_timeout_ms" }) do
    test("the subprocess backend refuses " .. option, function()
      local backend, err = subprocess_backend.new({ comb_bin = "comb", [option] = 0 })
      assert_nil(backend)
      assert_eq(err and err.kind, "unsupported")
      assert_true(err.message:find(option, 1, true) ~= nil, "names the option: " .. err.message)
      assert_true(err.message:find("ffi", 1, true) ~= nil, "names the ffi transport: " .. err.message)
    end)
  end

  test("the subprocess backend with neither is made", function()
    assert_not_nil(subprocess_backend.new({ comb_bin = "comb" }))
  end)
end

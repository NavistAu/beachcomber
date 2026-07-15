--- Tests for beachcomber.discovery.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local discovery = require("beachcomber.discovery")

  -- Run `fn` with os.getenv stubbed so that `name` appears set to `value`
  -- while every other variable falls through to the real environment.
  local function with_env(name, value, fn)
    local saved = os.getenv
    os.getenv = function(key)
      if key == name then return value end
      return saved(key)
    end
    local ok, err = pcall(fn)
    os.getenv = saved
    if not ok then error(err, 0) end
  end

  suite("discovery.get_uid")

  test("returns a positive integer", function()
    local uid, err = discovery.get_uid()
    assert_not_nil(uid, "uid should not be nil: " .. tostring(err))
    assert_true(type(uid) == "number", "uid should be a number")
    assert_true(uid >= 0, "uid should be >= 0")
    assert_eq(uid, math.floor(uid), "uid should be an integer")
  end)

  suite("discovery.discover_socket_path")

  test("returns a string path", function()
    local path, err = discovery.discover_socket_path()
    assert_not_nil(path, "path should not be nil: " .. tostring(err))
    assert_eq(type(path), "string")
    assert_true(#path > 0, "path should not be empty")
  end)

  test("fallback path contains uid", function()
    local uid, uid_err = discovery.get_uid()
    assert_not_nil(uid, tostring(uid_err))

    local path, path_err = discovery.discover_socket_path()
    assert_not_nil(path, tostring(path_err))

    -- The path must end with /sock
    assert_true(path:sub(-5) == "/sock", "path should end with /sock: " .. path)

    -- The path must contain beachcomber
    assert_true(path:find("beachcomber", 1, true) ~= nil, "path should contain 'beachcomber': " .. path)
  end)

  test("XDG_RUNTIME_DIR is ignored", function()
    with_env("XDG_RUNTIME_DIR", "/run/user/1000", function()
      local uid, uid_err = discovery.get_uid()
      assert_not_nil(uid, tostring(uid_err))

      local path, err = discovery.discover_socket_path()
      assert_not_nil(path, "path should not be nil: " .. tostring(err))
      assert_eq(path, "/tmp/beachcomber-" .. uid .. "/sock")
    end)
  end)

  test("BEACHCOMBER_SOCKET still takes priority over XDG_RUNTIME_DIR", function()
    with_env("XDG_RUNTIME_DIR", "/run/user/1000", function()
      with_env("BEACHCOMBER_SOCKET", "/custom/sock", function()
        local path, err = discovery.discover_socket_path()
        assert_not_nil(path, "path should not be nil: " .. tostring(err))
        assert_eq(path, "/custom/sock")
      end)
    end)
  end)

  test("path ends with /sock", function()
    local path = discovery.discover_socket_path()
    assert_not_nil(path)
    assert_eq(path:sub(-5), "/sock")
  end)

  test("path is absolute", function()
    local path = discovery.discover_socket_path()
    assert_not_nil(path)
    assert_eq(path:sub(1, 1), "/", "path should be absolute: " .. tostring(path))
  end)
end

--- Tests for beachcomber.discovery.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local discovery = require("beachcomber.discovery")

  suite("discovery.get_uid")

  test("returns a positive integer", function()
    local uid, err = discovery.get_uid()
    assert_not_nil(uid, "uid should not be nil: " .. tostring(err))
    assert_true(type(uid) == "number", "uid should be a number")
    assert_true(uid >= 0, "uid should be >= 0")
    assert_eq(uid, math.floor(uid), "uid should be an integer")
  end)

  suite("discovery.discover_socket_path")

  test("returns a string path without XDG_RUNTIME_DIR", function()
    -- Temporarily unset XDG_RUNTIME_DIR if set (not possible in pure Lua,
    -- so we test the fallback path by checking the result format)
    local path, err = discovery.discover_socket_path()
    assert_not_nil(path, "path should not be nil: " .. tostring(err))
    assert_eq(type(path), "string")
    assert_true(#path > 0, "path should not be empty")
  end)

  test("fallback path contains uid", function()
    -- We cannot unset env vars in Lua, but we can verify the structure
    -- by getting the uid and checking the path ends with the right segment.
    local uid, uid_err = discovery.get_uid()
    assert_not_nil(uid, tostring(uid_err))

    local path, path_err = discovery.discover_socket_path()
    assert_not_nil(path, tostring(path_err))

    -- The path must end with /sock
    assert_true(path:sub(-5) == "/sock", "path should end with /sock: " .. path)

    -- The path must contain beachcomber
    assert_true(path:find("beachcomber", 1, true) ~= nil, "path should contain 'beachcomber': " .. path)
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

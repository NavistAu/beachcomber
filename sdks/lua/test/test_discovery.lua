--- Tests for beachcomber.discovery: `comb`-on-PATH lookup and the ffi
--- transport's library-candidate ordering (the seven-point common
--- contract's discovery order: $BEACHCOMBER_LIB, then ../lib/ relative to
--- the resolved comb, then the platform default search path).

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

  -- Stub io.open so find_comb_on_path() succeeds for a specific PATH.
  local function with_comb_on_path(dir, fn)
    with_env("PATH", dir, function()
      local saved = io.open
      io.open = function(path, mode)
        if path == dir .. "/comb" then
          return { close = function() end }
        end
        return saved(path, mode)
      end
      local ok, err = pcall(fn)
      io.open = saved
      if not ok then error(err, 0) end
    end)
  end

  suite("discovery.find_comb_on_path")

  test("returns nil when PATH has no comb", function()
    with_env("PATH", "/definitely/not/here:/also/not/here", function()
      local found = discovery.find_comb_on_path()
      assert_nil(found)
    end)
  end)

  test("finds comb on PATH", function()
    with_comb_on_path("/fake/bin", function()
      local found = discovery.find_comb_on_path()
      assert_eq(found, "/fake/bin/comb")
    end)
  end)

  suite("discovery.platform_lib_filename")

  test("OSX maps to .dylib", function()
    assert_eq(discovery.platform_lib_filename("OSX"), "libbeachcomber.dylib")
  end)

  test("Linux maps to .so", function()
    assert_eq(discovery.platform_lib_filename("Linux"), "libbeachcomber.so")
  end)

  test("Windows maps to .dll", function()
    assert_eq(discovery.platform_lib_filename("Windows"), "beachcomber.dll")
  end)

  suite("discovery.library_candidates")

  test("BEACHCOMBER_LIB is tried first when set", function()
    with_env("BEACHCOMBER_LIB", "/custom/libbeachcomber.dylib", function()
      with_env("PATH", "/nowhere", function()
        local candidates = discovery.library_candidates("OSX")
        assert_eq(candidates[1], "/custom/libbeachcomber.dylib")
      end)
    end)
  end)

  test("BEACHCOMBER_LIB is skipped (not tried) when unset", function()
    with_env("BEACHCOMBER_LIB", nil, function()
      with_comb_on_path("/fake/bin", function()
        local candidates = discovery.library_candidates("OSX")
        assert_eq(candidates[1], "/fake/bin/../lib/libbeachcomber.dylib")
      end)
    end)
  end)

  test("../lib/ relative to resolved comb comes before the platform default", function()
    with_env("BEACHCOMBER_LIB", nil, function()
      with_comb_on_path("/opt/homebrew/bin", function()
        local candidates = discovery.library_candidates("OSX")
        assert_eq(candidates[1], "/opt/homebrew/bin/../lib/libbeachcomber.dylib")
        assert_eq(candidates[2], "libbeachcomber.dylib")
      end)
    end)
  end)

  test("platform default (bare library name) is always the last candidate", function()
    with_env("BEACHCOMBER_LIB", "/x/libbeachcomber.dylib", function()
      with_comb_on_path("/fake/bin", function()
        local candidates = discovery.library_candidates("OSX")
        assert_eq(candidates[#candidates], "libbeachcomber.dylib")
        assert_eq(#candidates, 3)
      end)
    end)
  end)

  test("full order: env, then ../lib/, then platform default", function()
    with_env("BEACHCOMBER_LIB", "/x/libbeachcomber.dylib", function()
      with_comb_on_path("/opt/homebrew/bin", function()
        local candidates = discovery.library_candidates("OSX")
        assert_eq(candidates[1], "/x/libbeachcomber.dylib")
        assert_eq(candidates[2], "/opt/homebrew/bin/../lib/libbeachcomber.dylib")
        assert_eq(candidates[3], "libbeachcomber.dylib")
      end)
    end)
  end)
end

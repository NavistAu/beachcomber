-- Tests for the CLI-fallback backend (sdks/lua/beachcomber/socket_cli.lua).
-- We stub io.popen and io.open to simulate the `comb` binary on PATH and
-- observe how the backend handles pipe exit statuses.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local json = require("beachcomber.json")

  -- Force a fresh copy of socket_cli for each test so that module-level
  -- globals (like a leaked shell_escape) are observable after each run.
  local function reload_socket_cli()
    package.loaded["beachcomber.socket_cli"] = nil
    return require("beachcomber.socket_cli")
  end

  -- Stub a pipe object with configurable output and close status.
  local function make_pipe(output, close_result)
    local pipe = {}
    local consumed = false
    function pipe:read(_)
      if consumed then return nil end
      consumed = true
      return output
    end
    function pipe:close()
      return close_result
    end
    return pipe
  end

  -- Run `fn` with io.popen stubbed to return a make_pipe result. Captures
  -- the last command passed to io.popen for later inspection.
  local function with_popen_stub(output, close_result, fn)
    local saved = io.popen
    local captured_cmd
    io.popen = function(cmd, mode)
      captured_cmd = cmd
      return make_pipe(output, close_result)
    end
    local ok, err = pcall(fn, function() return captured_cmd end)
    io.popen = saved
    if not ok then error(err, 0) end
  end

  -- Stub io.open so find_comb() succeeds (socket_cli looks up "<dir>/comb").
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

  suite("socket_cli fallback backend")

  test("get: pipe close failure returns ok=false with an error", function()
    with_comb_on_path(function()
      with_popen_stub("", false, function(_get_cmd)
        local socket_cli = reload_socket_cli()
        local handle = socket_cli.connect()
        handle:send_line(json.encode({ op = "get", key = "git.branch" }) .. "\n")
        local line = handle:recv_line()
        local resp = json.decode(line)
        assert_eq(resp.ok, false, "ok must be false when pipe:close() returns false")
        assert_true(type(resp.error) == "string" and #resp.error > 0,
          "error message must be present")
      end)
    end)
  end)

  test("refresh: shells to `comb get --force`, not removed `comb refresh`", function()
    with_comb_on_path(function()
      with_popen_stub("", true, function(get_cmd)
        local socket_cli = reload_socket_cli()
        local handle = socket_cli.connect()
        handle:send_line(json.encode({ op = "refresh", key = "git" }) .. "\n")
        handle:recv_line()
        local cmd = get_cmd()
        assert_not_nil(cmd, "io.popen must have been called")
        assert_true(cmd:find(" get ", 1, true) ~= nil,
          "refresh command must invoke `comb get` (got: " .. tostring(cmd) .. ")")
        assert_true(cmd:find("--force", 1, true) ~= nil,
          "refresh command must pass --force (got: " .. tostring(cmd) .. ")")
        assert_nil(cmd:find(" refresh ", 1, true),
          "refresh must NOT call removed `comb refresh` subcommand (got: " .. tostring(cmd) .. ")")
      end)
    end)
  end)

  test("refresh: pipe close failure returns ok=false", function()
    with_comb_on_path(function()
      with_popen_stub("", false, function(_get_cmd)
        local socket_cli = reload_socket_cli()
        local handle = socket_cli.connect()
        handle:send_line(json.encode({ op = "refresh", key = "git" }) .. "\n")
        local line = handle:recv_line()
        local resp = json.decode(line)
        assert_eq(resp.ok, false,
          "refresh must return ok=false when the underlying command fails")
      end)
    end)
  end)

  test("refresh: pipe close success returns ok=true", function()
    with_comb_on_path(function()
      with_popen_stub("", true, function(_get_cmd)
        local socket_cli = reload_socket_cli()
        local handle = socket_cli.connect()
        handle:send_line(json.encode({ op = "refresh", key = "git" }) .. "\n")
        local line = handle:recv_line()
        local resp = json.decode(line)
        assert_eq(resp.ok, true,
          "refresh must return ok=true when the underlying command succeeds")
      end)
    end)
  end)

  test("shell_escape is local, not a module-level global", function()
    _G.shell_escape = nil
    reload_socket_cli()
    assert_nil(_G.shell_escape,
      "shell_escape must be declared local, not leaked to _G")
  end)
end

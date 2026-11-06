--- Library discovery (docs/sdk-api.md, "Library discovery"): the shared
--- fixture sdks/discovery.json, run case by case against this SDK's
--- candidate list, plus the `comb`-on-PATH lookup the subprocess tier uses.
---
--- Each fixture case builds a real directory tree under /tmp. Resolving
--- `comb`'s symlinks needs realpath(3), which this SDK reaches through
--- LuaJIT's ffi — only the ffi tier ever loads a library — so the fixture
--- cases skip under PUC Lua.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local discovery = require("beachcomber.discovery")
  local json = require("beachcomber.json")
  local has_ffi, ffi = pcall(require, "ffi")

  local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
  local sdk_dir = script_dir .. "../"

  local function read_file(path)
    local f = assert(io.open(path, "r"), "cannot read " .. path)
    local body = f:read("*a")
    f:close()
    return body
  end

  local function shq(s)
    return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
  end

  local function sh(cmd)
    local ok = os.execute(cmd)
    return ok == true or ok == 0
  end

  --- A fresh directory under /tmp, as mktemp names it (symlinks unresolved).
  local function mktemp()
    local p = assert(io.popen("mktemp -d /tmp/bcdisc.XXXXXX"))
    local dir = p:read("*l")
    p:close()
    return dir
  end

  --- Build `tree` under `root`: "exe" an executable file, "file" one that
  --- is not, "-> target" a symlink to target.
  local function build(root, tree, subst)
    for rel, kind in pairs(tree) do
      local path = root .. "/" .. subst(rel)
      assert(sh("mkdir -p " .. shq(path:match("^(.*)/[^/]+$"))))
      local target = kind:match("^%-> (.+)$")
      if target then
        assert(sh("ln -s " .. shq(subst(target)) .. " " .. shq(path)))
      elseif kind == "exe" then
        assert(sh(": > " .. shq(path) .. " && chmod 755 " .. shq(path)))
      else
        assert(sh(": > " .. shq(path) .. " && chmod 644 " .. shq(path)))
      end
    end
  end

  suite("discovery: sdks/discovery.json")

  local fixture = json.decode(read_file(sdk_dir .. "../discovery.json"))
  for _, case in ipairs(fixture.cases) do
    if not has_ffi then
      skip(case.name, "resolving comb's symlinks needs LuaJIT's ffi realpath(3)")
    else
      test(case.name, function()
        local made = mktemp()
        local lib = ffi.os == "OSX" and "libbeachcomber.dylib" or "libbeachcomber.so"
        local ok, err = pcall(function()
          local root = discovery.realpath(made)
          local function subst(s)
            return (s:gsub("{root}", root):gsub("{lib}", lib))
          end
          build(root, case.tree, subst)
          local path = {}
          for i, dir in ipairs(case.path) do path[i] = subst(dir) end
          local env = {
            PATH = table.concat(path, ":"),
            BEACHCOMBER_LIB = case.lib_env and subst(case.lib_env) or nil,
            os = ffi.os,
          }
          local expect = {}
          for i, p in ipairs(case.expect) do expect[i] = subst(p) end
          assert_eq(table.concat(discovery.library_candidates(env), "\n"), table.concat(expect, "\n"))
        end)
        sh("rm -rf " .. shq(made))
        if not ok then error(err, 0) end
      end)
    end
  end

  suite("discovery: the real environment")

  -- The runner sets $BEACHCOMBER_LIB to the build under test (README,
  -- "Running tests"), so with no environment given it is the one candidate,
  -- and the loader finds the library through it.
  local real_lib = os.getenv("BEACHCOMBER_LIB")
  if not has_ffi then
    skip("the loader reads the real environment", "the loader needs LuaJIT's ffi")
  elseif not real_lib or real_lib == "" then
    skip("the loader reads the real environment", "$BEACHCOMBER_LIB is not set")
  else
    test("the loader reads the real environment", function()
      assert_eq(table.concat(discovery.library_candidates(), "\n"), real_lib)
      local loaded, err = require("beachcomber.ffi").load()
      assert_not_nil(loaded, err and err.message)
    end)
  end

  suite("discovery.find_comb_on_path")

  test("the first executable comb on PATH, passing over one that is not", function()
    local root = mktemp()
    local ok, err = pcall(function()
      build(root, { ["a/comb"] = "file", ["b/comb"] = "exe", ["c/comb"] = "exe" }, function(s) return s end)
      local found = discovery.find_comb_on_path(root .. "/a:" .. root .. "/b:" .. root .. "/c")
      assert_eq(found, root .. "/b/comb")
    end)
    sh("rm -rf " .. shq(root))
    if not ok then error(err, 0) end
  end)

  test("a directory named comb is not a comb", function()
    local root = mktemp()
    local ok, err = pcall(function()
      assert(sh("mkdir -p " .. shq(root .. "/a/comb")))
      assert_nil(discovery.find_comb_on_path(root .. "/a"))
    end)
    sh("rm -rf " .. shq(root))
    if not ok then error(err, 0) end
  end)

  test("no comb on PATH is nil", function()
    assert_nil(discovery.find_comb_on_path("/definitely/not/here:/also/not/here"))
  end)

  suite("discovery.platform_lib_filename")

  test("OSX is .dylib", function()
    assert_eq(discovery.platform_lib_filename("OSX"), "libbeachcomber.dylib")
  end)

  test("every other platform is .so", function()
    assert_eq(discovery.platform_lib_filename("Linux"), "libbeachcomber.so")
    assert_eq(discovery.platform_lib_filename("BSD"), "libbeachcomber.so")
    assert_eq(discovery.platform_lib_filename("Windows"), "libbeachcomber.so")
  end)
end

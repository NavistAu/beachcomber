--- Tests for beachcomber.subprocess_backend's `get` and `refresh` against a
--- live daemon.
---
--- The backend's `get` is one `comb get -f json <key>` (its header comment),
--- so its answer is the resolver's: a template renders, the directory chain,
--- `$HOME` as the global entry, `--force`/`--wait`, the daemon's refusals.
--- A caller's `env` is the spawned comb's whole environment (`env -i`), and
--- `eval` is one `comb eval -f json`. `refresh` is one `comb refresh <key>`, and its refusal
--- is the daemon's line. Runs under PUC Lua and LuaJIT
--- alike: the backend is plain Lua over `io.popen`. Needs a comb binary
--- ($COMB_BIN, or the workspace's own debug build).

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local Client = require("beachcomber.client").Client

  suite("subprocess_backend get against a live daemon")

  local function built_comb()
    local env_bin = os.getenv("COMB_BIN")
    if env_bin and env_bin ~= "" then return env_bin end
    local script_dir = (arg and arg[0]) and arg[0]:match("(.*/)") or "./"
    local path = script_dir .. "../../../target/debug/comb"
    local f = io.open(path, "rb")
    if f then f:close(); return path end
    return nil
  end

  local function read_line(cmd)
    local p = io.popen(cmd, "r")
    local line = p and p:read("*l")
    if p then p:close() end
    return line
  end

  -- Spawn a daemon on a private socket, with HOME/XDG_CONFIG_HOME/XDG_STATE_HOME
  -- pointed into its tempdir so its managed defaults and its log never touch
  -- the real config or state. Alongside it, a `comb` stand-in script that
  -- appends its argv to `argv_log` and runs the real binary under the same
  -- HOME, so the children this backend spawns read the daemon's config (a
  -- config-file `socket_path` beats $BEACHCOMBER_SOCKET) and the test can
  -- see what was asked for.
  local function start_daemon(comb_bin)
    local tmpdir = read_line("mktemp -d " .. (os.getenv("TMPDIR") or "/tmp"):gsub("/+$", "") .. "/beachcomber-luasub-XXXXXX")
    if not tmpdir or tmpdir == "" then return nil end
    local env = "HOME=" .. tmpdir .. " XDG_CONFIG_HOME=" .. tmpdir .. " XDG_STATE_HOME=" .. tmpdir .. "/.local/state"
    local socket_path = tmpdir .. "/sock"
    local pid = read_line(env .. " " .. comb_bin .. " daemon --socket " .. socket_path .. " > /dev/null 2>&1 & echo $!")
    local argv_log = tmpdir .. "/argv.log"
    local stand_in = tmpdir .. "/comb"
    local f = io.open(stand_in, "w")
    f:write("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '" .. argv_log .. "'\n" .. env .. " exec '" .. comb_bin .. "' \"$@\"\n")
    f:close()
    os.execute("chmod +x " .. stand_in)
    local deadline = os.time() + 5
    while os.time() < deadline do
      -- Lua 5.1/LuaJIT return the raw status (256 on failure, truthy); 5.2+
      -- return true/nil.
      local rc = os.execute("test -S " .. socket_path)
      if rc == true or rc == 0 then
        return { pid = pid, socket = socket_path, tmpdir = tmpdir, comb = stand_in, argv_log = argv_log, home = tmpdir }
      end
      os.execute("sleep 0.05 2>/dev/null || true")
    end
    os.execute("kill " .. tostring(pid) .. " 2>/dev/null; rm -rf " .. tmpdir)
    return nil
  end

  local function last_call(guard)
    return read_line("tail -n 1 '" .. guard.argv_log .. "'") or ""
  end

  --- The last call's argv from its subcommand on, without the global flags.
  local function last_argv(guard)
    local line = last_call(guard)
    return (line:gsub("^.- (get )", "%1"):gsub("^.- (put )", "%1"):gsub("^.- (refresh )", "%1"):gsub("^.- (eval )", "%1"))
  end

  local comb_bin = built_comb()
  local names = {
    "a scalar field is its value",
    "a string field is its raw string",
    "a bare provider is every field merged into one object",
    "a dotted address drills into the field value",
    "an absent field is a miss, not an error",
    "a daemon rejection is a server_error in the daemon's words",
    "a template-only node is its rendered value",
    "no cwd reads the global entry",
    "an ancestor's put is read from a child path (the directory chain)",
    "a path equal to the daemon's $HOME is the global entry",
    "a colon below the provider segment is part of the field",
    "wait reaches the wire as --wait, in one comb get",
    "--force on a static namespace is the daemon's refusal",
    "a template that cannot resolve is the resolver's error, not a null hit",
    "a namespace read with one bad leaf is the resolver's error, not a null hit",
    "a template cycle is the resolver's error",
    "a failed exec() is a server_error, never a transport kind",
    "a group whose leaf fails reports the leaf's line",
    "refresh of a provider is ok, through comb refresh",
    "refresh of a static namespace is the daemon's refusal, in its words alone",
    "a key is asked alone, in one spawn",
    "a facet key reads the unrendered half",
    "the global flags a get spawns with are --socket, --color and --no-exec",
    "a session over this tier keeps its directory and sends it as --path",
    "a status that cannot run is nil, Error, not an empty list",
    "the map is the template env: a map-only variable hits, a host-only one misses",
    "an exec child sees the map's variables",
    "with exec off the call fails with exec() is disabled",
    "NO_COLOR in the map stops fg drawing escapes",
    "eval keeps a number a number and renders the map",
    "eval of a template that cannot render is a server_error",
  }
  if not comb_bin then
    for _, name in ipairs(names) do skip(name, "no comb built") end
    return
  end
  local guard = start_daemon(comb_bin)
  if not guard then
    for _, name in ipairs(names) do skip(name, "daemon did not start") end
    return
  end

  local backend = require("beachcomber.subprocess_backend").new({ comb_bin = guard.comb, socket_path = guard.socket })
  local c = Client.new(backend)
  local proj = guard.tmpdir .. "/proj/sub"

  -- The seeds and every case run under one pcall so the daemon and its
  -- tempdir are always cleaned up, whatever fails (the Node suite's `after`).
  local ran, failure = pcall(function()
  os.execute("mkdir -p " .. proj)
  assert_true(c:put("sp", { n = 1, s = "str", o = { k = "v" } }, { path = "/tmp" }))
  assert_true(c:put("sp:template", { t = "{{ 1 + 1 }}" }, { path = "/tmp" }))
  assert_true(c:put("spg", { g = 1 }))
  assert_true(c:put("bad", { n = 1 }, { path = "/tmp" }))
  assert_true(c:put("bad:template", {
    bad = "{{ bad.bad }}",
    cyc = "{{ bad.cyc }}",
    ex = '{{ exec("definitely-missing-cmd-4f2a") }}',
    grp = { leaf = "{{ bad.grp.leaf }}" },
  }, { path = "/tmp" }))

  test(names[1], function()
    local value, err = c:get("sp.n", { cwd = "/tmp" })
    assert_nil(err)
    assert_eq(value, 1)
  end)

  test(names[2], function()
    assert_eq(c:get("sp.s", { cwd = "/tmp" }), "str")
  end)

  test(names[3], function()
    local value = c:get("sp", { cwd = "/tmp" })
    assert_eq(value.n, 1)
    assert_eq(value.s, "str")
    assert_eq(value.o.k, "v")
    assert_eq(value.t, 2, "the namespace's templates render into it")
  end)

  test(names[4], function()
    assert_eq(c:get("sp.o.k", { cwd = "/tmp" }), "v")
  end)

  test(names[5], function()
    local value, err = c:get("sp.nope", { cwd = "/tmp" })
    assert_nil(err)
    assert_nil(value)
  end)

  test(names[6], function()
    local value, err = c:get("a:b:c.x", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message:sub(1, #"invalid facet key: a:b:c"), "invalid facet key: a:b:c", "the daemon's words alone")
  end)

  test(names[7], function()
    local value, err = c:get("sp.t", { cwd = "/tmp" })
    assert_nil(err)
    assert_eq(value, 2)
  end)

  test(names[8], function()
    assert_eq(c:get("spg.g"), 1)
    assert_eq(last_argv(guard), "get -f json spg.g --global")
  end)

  test(names[9], function()
    assert_true(c:put("chain", { a = 1 }))
    assert_eq(c:get("chain.a", { cwd = proj }), 1)
  end)

  test(names[10], function()
    assert_eq(c:get("spg.g", { cwd = guard.home }), 1)
  end)

  test(names[21], function()
    local value, err = c:get("sp.n", { cwd = "/tmp" })
    assert_nil(err)
    assert_eq(value, 1)
    assert_eq(last_argv(guard), "get -f json sp.n --path /tmp")
  end)

  test(names[22], function()
    local value, err = c:get("sp:template.t", { cwd = "/tmp" })
    assert_nil(err)
    assert_eq(value, "{{ 1 + 1 }}", "the template as stored")
    assert_eq(last_argv(guard), "get -f json sp:template.t --path /tmp")
    value, err = c:get("sp:put.n", { cwd = "/tmp" })
    assert_nil(err)
    assert_eq(value, 1, "the stored value")
  end)

  test(names[23], function()
    assert_eq(c:get("sp.n", { cwd = "/tmp" }), 1)
    assert_eq(last_call(guard),
      "--socket " .. guard.socket .. " --color always --no-exec get -f json sp.n --path /tmp")
  end)

  test(names[26], function()
    assert_true(c:put("mapenv:template", { m = "{{ env.ONLYINMAP }}", h = "{{ env.PATH }}" }, { path = "/tmp" }))
    assert_eq(c:get("mapenv.m", { cwd = "/tmp", env = { ONLYINMAP = "map" } }), "map")
    -- PATH is set in this process but absent from the map.
    local value, err = c:get("mapenv.h", { cwd = "/tmp", env = { ONLYINMAP = "map" } })
    assert_nil(err)
    assert_nil(value, "a host-only variable misses")
    assert_eq(c:get("mapenv.h", { cwd = "/tmp" }), os.getenv("PATH"), "no map: this process's environment")
    assert_true(last_call(guard):find("env -i", 1, true) == nil)
  end)

  test(names[27], function()
    assert_true(c:put("mapexec:template", { v = "{{ exec('echo \"$MARK\"') | trim }}" }, { path = "/tmp" }))
    local execc = Client.new(require("beachcomber.subprocess_backend").new({
      comb_bin = guard.comb, socket_path = guard.socket, exec = true }))
    local value, err = execc:get("mapexec.v", { cwd = "/tmp", env = { MARK = "from-map", PATH = "/usr/bin:/bin" } })
    assert_nil(err)
    assert_eq(value, "from-map")
    assert_true(last_call(guard):find("--no-exec", 1, true) == nil, "exec on drops --no-exec")
  end)

  test(names[28], function()
    assert_true(c:put("noexec:template", { v = "{{ exec('echo hi') }}" }, { path = "/tmp" }))
    local value, err = c:get("noexec.v", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("exec() is disabled", 1, true) ~= nil, tostring(err))
  end)

  test(names[29], function()
    assert_true(c:put("hue:template", { v = "{{ 'x' | fg('#ff0000') }}" }, { path = "/tmp" }))
    local drawn = c:get("hue.v", { cwd = "/tmp", env = {} })
    assert_true(tostring(drawn):find("\27[", 1, true) ~= nil, "escapes drawn")
    assert_true(last_call(guard):find("--color always", 1, true) ~= nil)
    assert_eq(c:get("hue.v", { cwd = "/tmp", env = { NO_COLOR = "1" } }), "x")
    assert_true(last_call(guard):find("--color never", 1, true) ~= nil)
  end)

  test(names[30], function()
    assert_eq(c:eval("{{ 1 + 1 }}", { cwd = "/tmp" }), 2)
    assert_eq(c:eval("n={{ sp.n }}", { cwd = "/tmp" }), "n=1")
    assert_eq(c:eval("{{ env.ONLYINMAP }}", { cwd = "/tmp", env = { ONLYINMAP = "m" } }), "m")
    local value, err = c:eval("{{ none }}", { cwd = "/tmp" })
    assert_nil(err)
    assert_nil(value, "a template that renders nothing is absent")
  end)

  test(names[31], function()
    local value, err = c:eval("{{ exec('echo hi') }}", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("exec() is disabled", 1, true) ~= nil, tostring(err))
  end)

  test(names[11], function()
    local value, err = c:get("sp.n:age", { cwd = "/tmp" })
    assert_nil(err)
    assert_nil(value)
    assert_eq(last_argv(guard), "get -f json sp.n:age --path /tmp")
    -- A bracketed key keeps its brackets.
    value, err = c:get("sp['o'].k", { cwd = "/tmp" })
    assert_nil(err)
    assert_eq(value, "v")
    assert_eq(last_argv(guard), "get -f json sp['o'].k --path /tmp")
  end)

  test(names[12], function()
    local value, err = c:get("sp.n", { cwd = "/tmp", wait = true })
    assert_nil(err)
    assert_eq(value, 1)
    assert_eq(last_argv(guard), "get -f json sp.n --path /tmp --wait")
  end)

  test(names[13], function()
    local value, err = c:get("sp.n", { cwd = "/tmp", force = true })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message:sub(1, #"cannot --force static namespace"), "cannot --force static namespace", "the daemon's words alone")
    assert_eq(last_argv(guard), "get -f json sp.n --path /tmp --force")
  end)

  test(names[14], function()
    local value, err = c:get("bad.bad", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("cycle", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[15], function()
    local value, err = c:get("bad", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("cycle", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[16], function()
    local value, err = c:get("bad.cyc", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("cycle", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[17], function()
    local execc = Client.new(require("beachcomber.subprocess_backend").new({
      comb_bin = guard.comb, socket_path = guard.socket, exec = true }))
    local value, err = execc:get("bad.ex", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("definitely-missing-cmd-4f2a", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[18], function()
    local value, err = c:get("bad.grp", { cwd = "/tmp" })
    assert_nil(value)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message:sub(1, 14), "bad.grp.leaf: ")
  end)

  test(names[24], function()
    local s, serr = c:session()
    assert_nil(serr)
    local value, err = s:get("sp.n")
    assert_nil(err)
    assert_nil(value, "no directory yet: the global entry")
    assert_eq(last_argv(guard), "get -f json sp.n --global")
    assert_eq(s:set_context("/tmp"), true)
    value, err = s:get("sp.n")
    assert_nil(err)
    assert_eq(value, 1)
    assert_eq(last_argv(guard), "get -f json sp.n --path /tmp")
    assert_eq(s:put("sess", { v = 1 }), true)
    assert_eq(last_argv(guard), "put sess {\"v\":1} --path /tmp")
    value = s:get("spg.g", { cwd = guard.home })
    assert_eq(value, 1, "an explicit cwd wins over the session's directory")
    assert_eq(s:close(), true)
  end)

  test(names[25], function()
    local failing = require("beachcomber.subprocess_backend").new({ comb_bin = "false" })
    local rows, err = Client.new(failing):status()
    assert_nil(rows)
    assert_not_nil(err)
    assert_eq(err.kind, "server_error")
  end)

  test(names[19], function()
    local ok, err = c:refresh("hostname")
    assert_nil(err)
    assert_true(ok)
    assert_eq(last_argv(guard), "refresh hostname --global")
  end)

  test(names[20], function()
    local ok, err = c:refresh("sp", { path = "/tmp" })
    assert_nil(ok)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message, "nothing to refresh: sp has no sources", "the daemon's words alone")
    assert_eq(last_argv(guard), "refresh sp --path /tmp")
  end)
  end)

  c:close()
  os.execute("kill " .. tostring(guard.pid) .. " 2>/dev/null; rm -rf " .. guard.tmpdir)
  if not ran then error(failure, 0) end
end

--- Tests for beachcomber.subprocess_backend's `get` and `refresh` against a
--- live daemon.
---
--- The backend asks `comb get -f json <key> <p>:cache.<rest>` for the
--- value and its `:cache` answer (age, staleness) in one spawn (its header comment), so
--- its answer is the resolver's: the directory chain, `$HOME` as the global
--- entry, `--force`/`--wait`, the daemon's refusals — and
--- a template-backed key renders. `refresh` is one `comb refresh <key>`,
--- and its refusal is the daemon's line. Runs under PUC Lua and LuaJIT
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

  local function last_argv(guard)
    return read_line("tail -n 1 '" .. guard.argv_log .. "'") or ""
  end

  local comb_bin = built_comb()
  local names = {
    "a scalar field is a hit with the raw value",
    "a string field is a hit with the raw string",
    "a bare provider is every field merged into one object",
    "a dotted address drills into the field value",
    "an absent field is a miss, not an error",
    "a daemon rejection is a server_error in the daemon's words",
    "a template-only node is a hit holding the rendered value, with no template half",
    "no path reads the global entry",
    "an ancestor's put is read from a child path (the directory chain)",
    "a path equal to the daemon's $HOME is the global entry",
    "a colon outside the provider segment is an invalid key, asked with its :cache slot",
    "wait reaches the wire as --wait, in one comb get with the :cache slot",
    "--force on a static namespace is the daemon's refusal",
    "a template that does not compile is the resolver's error, not a null hit",
    "a namespace read with one bad leaf is the resolver's error, not a null hit",
    "a template cycle is the resolver's error",
    "a failed exec() is a server_error, never a transport kind",
    "a group whose leaf fails reports the leaf's line, not the companion misses",
    "refresh of a provider is ok, through comb refresh",
    "refresh of a static namespace is the daemon's refusal, in its words alone",
    "a leaf asks for its :cache leaf in the same spawn and reports its freshness",
    "a bare provider asks for its :cache node",
    "a facet key is asked alone and reports age 0, not stale",
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
  assert_true(c:put("sp", { n = 1, s = "str", o = { k = "v" } }, nil, "/tmp"))
  assert_true(c:put("sp:template", { t = "{{ 1 + 1 }}" }, nil, "/tmp"))
  assert_true(c:put("spg", { g = 1 }))
  assert_true(c:put("bad", { n = 1 }, nil, "/tmp"))
  assert_true(c:put("bad:template", {
    bad = "{{ 1 + }}",
    cyc = "{{ bad.cyc }}",
    ex = '{{ exec("definitely-missing-cmd-4f2a") }}',
    grp = { leaf = "{{ 1 + }}" },
  }, nil, "/tmp"))

  test(names[1], function()
    local r, err = c:get("sp.n", "/tmp")
    assert_nil(err)
    assert_true(r:is_hit())
    assert_eq(r.data, 1)
    assert_eq(type(r.age_ms), "number")
  end)

  test(names[2], function()
    local r = c:get("sp.s", "/tmp")
    assert_true(r:is_hit())
    assert_eq(r.data, "str")
  end)

  test(names[3], function()
    local r = c:get("sp", "/tmp")
    assert_true(r:is_hit())
    assert_eq(r.data.n, 1)
    assert_eq(r.data.s, "str")
    assert_eq(r.data.o.k, "v")
  end)

  test(names[4], function()
    local r = c:get("sp.o.k", "/tmp")
    assert_true(r:is_hit())
    assert_eq(r.data, "v")
  end)

  test(names[5], function()
    local r, err = c:get("sp.nope", "/tmp")
    assert_nil(err)
    assert_true(r:is_miss())
  end)

  test(names[6], function()
    local r, err = c:get("a:b:c.x", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message:sub(1, #"invalid facet key: a:b:c"), "invalid facet key: a:b:c", "the daemon's words alone")
  end)

  test(names[7], function()
    local r, err = c:get("sp.t", "/tmp")
    assert_nil(err)
    assert_true(r:is_hit())
    assert_eq(r.data, 2)
    assert_nil(r.template)
    assert_nil(r.rest)
    assert_eq(r.age_ms, 0)
    assert_eq(r.stale, false)
  end)

  test(names[8], function()
    local r = c:get("spg.g")
    assert_true(r:is_hit())
    assert_eq(r.data, 1)
  end)

  test(names[9], function()
    assert_true(c:put("chain", { a = 1 }))
    local r = c:get("chain.a", proj)
    assert_true(r:is_hit())
    assert_eq(r.data, 1)
  end)

  test(names[10], function()
    local r = c:get("spg.g", guard.home)
    assert_true(r:is_hit())
    assert_eq(r.data, 1)
  end)

  test(names[21], function()
    local r, err = c:get("sp.n", "/tmp")
    assert_nil(err)
    assert_eq(last_argv(guard), "get -f json sp.n sp:cache.n --path /tmp")
    assert_eq(type(r.age_ms), "number")
    assert_eq(r.stale, false)
  end)

  test(names[22], function()
    local r, err = c:get("sp", "/tmp")
    assert_nil(err)
    assert_true(r:is_hit())
    assert_eq(last_argv(guard), "get -f json sp sp:cache --path /tmp")
    assert_eq(type(r.age_ms), "number")
  end)

  test(names[23], function()
    local r, err = c:get("sp:put.n", "/tmp")
    assert_nil(err)
    assert_eq(last_argv(guard), "get -f json sp:put.n --path /tmp")
    assert_eq(r.age_ms, 0)
    assert_eq(r.stale, false)
  end)

  test(names[11], function()
    local r, err = c:get("sp.n:age", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message, "invalid key: sp.n:age")
    assert_eq(last_argv(guard), "get -f json sp.n:age sp:cache.n:age --path /tmp")
  end)

  test(names[12], function()
    local r, err = c:get_with_flags("sp.n", "/tmp", false, true)
    assert_nil(err)
    assert_true(r:is_hit())
    assert_eq(last_argv(guard), "get -f json sp.n sp:cache.n --path /tmp --wait")
  end)

  test(names[13], function()
    local r, err = c:get_with_flags("sp.n", "/tmp", true, false)
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message:sub(1, #"cannot --force static namespace"), "cannot --force static namespace", "the daemon's words alone")
    assert_eq(last_argv(guard), "get -f json sp.n sp:cache.n --path /tmp --force")
  end)

  test(names[14], function()
    local r, err = c:get("bad.bad", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("expression compile error", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[15], function()
    local r, err = c:get("bad", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("expression compile error", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[16], function()
    local r, err = c:get("bad.cyc", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("cycle", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[17], function()
    local r, err = c:get("bad.ex", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_true(err.message:find("definitely-missing-cmd-4f2a", 1, true) ~= nil, "expected the resolver's error, got: " .. tostring(err))
  end)

  test(names[18], function()
    local r, err = c:get("bad.grp", "/tmp")
    assert_nil(r)
    assert_eq(err.kind, "server_error")
    assert_eq(err.message:sub(1, 14), "bad.grp.leaf: ")
    assert_true(err.message:find("Error querying", 1, true) == nil, "companion miss lines must not be the message: " .. tostring(err))
  end)

  test(names[19], function()
    local ok, err = c:refresh("hostname")
    assert_nil(err)
    assert_true(ok)
    assert_eq(last_argv(guard), "refresh hostname --global")
  end)

  test(names[20], function()
    local ok, err = c:refresh("sp", "/tmp")
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

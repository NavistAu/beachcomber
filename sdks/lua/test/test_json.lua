--- Tests for beachcomber.json encode/decode.

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
  local json = require("beachcomber.json")

  suite("json.decode")

  test("decode true/false/null", function()
    assert_eq(json.decode("true"), true)
    assert_eq(json.decode("false"), false)
    -- null decodes to nil; decode returns nil,nil (no error second return)
    local v, err = json.decode("null")
    assert_nil(v)
    assert_nil(err)
  end)

  test("decode integers", function()
    assert_eq(json.decode("0"), 0)
    assert_eq(json.decode("42"), 42)
    assert_eq(json.decode("-7"), -7)
  end)

  test("decode floats", function()
    assert_eq(json.decode("3.14"), 3.14)
    assert_eq(json.decode("-0.5"), -0.5)
    assert_eq(json.decode("1e2"), 100)
  end)

  test("decode strings", function()
    assert_eq(json.decode('"hello"'), "hello")
    assert_eq(json.decode('"with \\"quotes\\""'), 'with "quotes"')
    assert_eq(json.decode('"tab\\there"'), "tab\there")
    assert_eq(json.decode('"new\\nline"'), "new\nline")
    assert_eq(json.decode('""'), "")
  end)

  test("decode \\u unicode escapes", function()
    assert_eq(json.decode('"\\u0041"'), "A")
    assert_eq(json.decode('"\\u00e9"'), "\195\169") -- é in UTF-8
  end)

  test("decode empty object", function()
    local t = json.decode("{}")
    assert_eq(type(t), "table")
    local count = 0
    for _ in pairs(t) do count = count + 1 end
    assert_eq(count, 0)
  end)

  test("decode flat object", function()
    local t = json.decode('{"ok":true,"data":"main","age_ms":1234}')
    assert_eq(t.ok, true)
    assert_eq(t.data, "main")
    assert_eq(t.age_ms, 1234)
  end)

  test("decode nested object", function()
    local t = json.decode('{"a":{"b":{"c":1}}}')
    assert_eq(t.a.b.c, 1)
  end)

  test("decode array", function()
    local a = json.decode('[1,2,3]')
    assert_eq(#a, 3)
    assert_eq(a[1], 1)
    assert_eq(a[3], 3)
  end)

  test("decode empty array", function()
    local a = json.decode('[]')
    assert_eq(type(a), "table")
    assert_eq(#a, 0)
  end)

  test("decode array with mixed types", function()
    local a = json.decode('[true,null,"x",42]')
    assert_eq(a[1], true)
    assert_nil(a[2])
    assert_eq(a[3], "x")
    assert_eq(a[4], 42)
  end)

  test("decode whitespace tolerance", function()
    local t = json.decode(' { "ok" : true , "v" : 1 } ')
    assert_eq(t.ok, true)
    assert_eq(t.v, 1)
  end)

  test("decode error: invalid JSON", function()
    local v, err = json.decode("{invalid}")
    assert_nil(v)
    assert_not_nil(err)
  end)

  test("decode error: trailing garbage", function()
    local v, err = json.decode('{"ok":true} garbage')
    assert_nil(v)
    assert_not_nil(err)
  end)

  test("decode error: non-string input", function()
    local v, err = json.decode(42)
    assert_nil(v)
    assert_not_nil(err)
  end)

  -- ── Encode ──

  suite("json.encode")

  test("encode nil", function()
    assert_eq(json.encode(nil), "null")
  end)

  test("encode booleans", function()
    assert_eq(json.encode(true), "true")
    assert_eq(json.encode(false), "false")
  end)

  test("encode integers", function()
    assert_eq(json.encode(0), "0")
    assert_eq(json.encode(42), "42")
    assert_eq(json.encode(-7), "-7")
  end)

  test("encode float", function()
    local s = json.encode(3.14)
    assert_not_nil(s)
    local n = tonumber(s)
    assert_true(math.abs(n - 3.14) < 1e-10, "float roundtrip")
  end)

  test("encode plain string", function()
    assert_eq(json.encode("hello"), '"hello"')
  end)

  test("encode string with special chars", function()
    assert_eq(json.encode('say "hi"'), '"say \\"hi\\""')
    assert_eq(json.encode("line\nnew"), '"line\\nnew"')
    assert_eq(json.encode("tab\there"), '"tab\\there"')
  end)

  test("encode empty table as object", function()
    assert_eq(json.encode({}), "{}")
  end)

  test("encode array table", function()
    local s = json.encode({10, 20, 30})
    assert_eq(s, "[10,20,30]")
  end)

  test("encode object table", function()
    local s = json.encode({ok = true})
    assert_not_nil(s)
    -- Round-trip to verify
    local t = json.decode(s)
    assert_eq(t.ok, true)
  end)

  test("encode nested structure", function()
    local s = json.encode({op = "get", key = "git.branch", path = "/repo"})
    local t = json.decode(s)
    assert_eq(t.op, "get")
    assert_eq(t.key, "git.branch")
    assert_eq(t.path, "/repo")
  end)

  -- ── Round-trip ──

  suite("json round-trip")

  test("object round-trip", function()
    local orig = {ok = true, data = "main", age_ms = 1234, stale = false}
    local encoded = json.encode(orig)
    local decoded = json.decode(encoded)
    assert_eq(decoded.ok, true)
    assert_eq(decoded.data, "main")
    assert_eq(decoded.age_ms, 1234)
    assert_eq(decoded.stale, false)
  end)

  test("provider list round-trip", function()
    local orig = {
      ok = true,
      data = {
        {name = "git", global = false, fields = {"branch", "dirty"}},
        {name = "hostname", global = true, fields = {"value"}},
      }
    }
    local encoded = json.encode(orig)
    local decoded = json.decode(encoded)
    assert_eq(decoded.ok, true)
    assert_eq(#decoded.data, 2)
    assert_eq(decoded.data[1].name, "git")
    assert_eq(#decoded.data[1].fields, 2)
  end)
end

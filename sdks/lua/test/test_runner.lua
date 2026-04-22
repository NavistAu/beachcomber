--- Simple test runner for the beachcomber Lua SDK.
--
-- Run with:
--   lua test/test_runner.lua
-- or:
--   luajit test/test_runner.lua
--
-- Exit code is 0 on success, 1 if any test fails.

-- Add the SDK root to package.path so modules resolve correctly.
-- This allows running from any directory as long as you invoke via
-- `lua test/test_runner.lua` from the sdks/lua/ directory, or with
-- an explicit path. Adjust if needed.
local script_dir = (arg and arg[0]) and arg[0]:match("(.*/?)") or "./"
-- Resolve to sdks/lua root (parent of test/)
local sdk_root = script_dir .. "../"
package.path = sdk_root .. "?.lua;" .. sdk_root .. "?/init.lua;" .. package.path

-- ── Runner state ─────────────────────────────────────────────────────────────

local passed  = 0
local failed  = 0
local skipped = 0
local current_suite = ""

local function suite(name)
  current_suite = name
  print("\n  " .. name)
end

local function test(name, fn)
  local ok, err = pcall(fn)
  if ok then
    passed = passed + 1
    print("    [PASS] " .. name)
  else
    failed = failed + 1
    print("    [FAIL] " .. name)
    print("           " .. tostring(err))
  end
end

local function skip(name, reason)
  skipped = skipped + 1
  print("    [SKIP] " .. name .. " (" .. (reason or "skipped") .. ")")
end

local function assert_eq(a, b, msg)
  if a ~= b then
    error((msg or "assertion failed") .. ": expected " .. tostring(b) .. ", got " .. tostring(a), 2)
  end
end

local function assert_true(v, msg)
  if not v then
    error((msg or "expected true, got false/nil"), 2)
  end
end

local function assert_nil(v, msg)
  if v ~= nil then
    error((msg or "expected nil, got") .. ": " .. tostring(v), 2)
  end
end

local function assert_not_nil(v, msg)
  if v == nil then
    error((msg or "expected non-nil value"), 2)
  end
end

-- ── Load test modules ────────────────────────────────────────────────────────

require("test.test_json")(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
require("test.test_discovery")(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
require("test.test_client")(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)
require("test.test_socket_cli")(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)

-- ── Summary ──────────────────────────────────────────────────────────────────

print(string.format(
  "\n  Results: %d passed, %d failed, %d skipped\n",
  passed, failed, skipped
))

if failed > 0 then
  os.exit(1)
end

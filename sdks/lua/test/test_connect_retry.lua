--- Tests for connect retry behavior in the beachcomber Lua SDK.
--
-- Invoked by test_runner.lua via require("test.test_connect_retry").

return function(suite, test, skip, assert_eq, assert_true, assert_nil, assert_not_nil)

  suite("Connect retry")

  test("_connect_with_retry is exposed on the module", function()
    local beachcomber = require("beachcomber")
    assert_not_nil(beachcomber._connect_with_retry, "_connect_with_retry should be exposed")
    assert_eq(type(beachcomber._connect_with_retry), "function")
  end)

  test("_connect_with_retry exhaust returns nil and error on nonexistent socket", function()
    local beachcomber = require("beachcomber")

    -- Create a backend that always fails (simulates daemon not running).
    local always_fail = {
      connect = function(_path)
        return nil, "connection refused"
      end
    }

    local start = os.time()
    local handle, err = beachcomber._connect_with_retry(always_fail, "/tmp/nosock_test.sock")
    local elapsed = os.time() - start

    assert_nil(handle, "should return nil on failure")
    assert_not_nil(err, "should return an error message")
    -- After 3 backoffs (0.25+0.5+1.0=1.75s), os.time() resolution is 1s so
    -- elapsed should be >= 1. Skip exact timing check for portability.
    assert_true(elapsed >= 1, "should have waited through backoffs")
  end)

  test("_connect_with_retry succeeds immediately when backend connects on first try", function()
    local beachcomber = require("beachcomber")

    local fake_handle = { close = function() end }
    local call_count = 0
    local instant_backend = {
      connect = function(_path)
        call_count = call_count + 1
        return fake_handle, nil
      end
    }

    local handle, err = beachcomber._connect_with_retry(instant_backend, "/tmp/test.sock")
    assert_not_nil(handle, "should return handle")
    assert_nil(err, "should not return error")
    assert_eq(call_count, 1, "should connect on first try")
  end)

  test("_connect_with_retry succeeds after initial failures", function()
    local beachcomber = require("beachcomber")

    local fake_handle = { close = function() end }
    local call_count = 0
    -- Fail the first 2 attempts, succeed on the 3rd.
    local retry_backend = {
      connect = function(_path)
        call_count = call_count + 1
        if call_count < 3 then
          return nil, "connection refused"
        end
        return fake_handle, nil
      end
    }

    local handle, err = beachcomber._connect_with_retry(retry_backend, "/tmp/test.sock")
    assert_not_nil(handle, "should eventually succeed")
    assert_nil(err, "should not return error on success")
    assert_true(call_count >= 3, "should have retried at least twice")
  end)

end

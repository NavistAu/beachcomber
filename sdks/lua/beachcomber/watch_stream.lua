--- WatchStream — a stream of watch events from the beachcomber daemon.
--
-- Returned by Client:watch(). Thin, transport-agnostic wrapper around a
-- backend watch handle (beachcomber.ffi_backend's Watch or
-- beachcomber.subprocess_backend's Watch), both of which expose
-- next_event(timeout_ms)/cancel()/close() returning
-- (event_or_nil, Error_or_nil, outcome_string).

local WatchStream = {}
WatchStream.__index = WatchStream

--- @param handle table  A backend watch handle.
-- @return WatchStream
function WatchStream.new(handle)
  return setmetatable({ _handle = handle, _closed = false }, WatchStream)
end

--- Block for the next event (or return sooner per `timeout_ms`; see the
--- active backend's Watch:next_event for exact semantics — the subprocess
--- transport does not honour a timeout and always blocks for the next line).
-- @param timeout_ms number?  -1 (default) blocks indefinitely, 0 polls, >0 waits that long.
-- @return event table {data, age_ms, stale}, or nil on eof/cancelled, or nil, Error on error
function WatchStream:next_event(timeout_ms)
  if self._closed then return nil end
  local ev, err, outcome = self._handle:next_event(timeout_ms)
  if outcome == "error" then
    return nil, err
  end
  return ev -- nil on timeout/eof/cancelled; the event table on "event"
end

--- Cancel a pending or future next_event() call without closing the handle.
function WatchStream:cancel()
  if self._handle.cancel then self._handle:cancel() end
end

--- Close the watch stream and release the underlying handle.
function WatchStream:close()
  if not self._closed then
    self._closed = true
    self._handle:close()
  end
end

--- Iterator for use in a for-loop: `for event in stream:each() do ... end`.
-- Ends when next_event() returns nil (eof/cancelled/error).
-- @return function iterator
function WatchStream:each()
  return function()
    return self:next_event()
  end
end

return WatchStream

--- WatchStream — a stream of watch events from the beachcomber daemon.
--
-- Each event is `{data = value}`: the value `Client:get` answers for the
-- watched key at that moment, re-resolved whenever any key the resolution
-- reads changes, including the provider's template tree. Absent `data` is a
-- miss, not the end of the stream.
--
-- Returned by Client:watch(). A thin wrapper around a backend watch handle
-- (beachcomber.ffi_backend's Watch), whose next_event(timeout_ms) answers
-- (event_or_nil, Error_or_nil, outcome) with outcome one of "event",
-- "timeout", "eof", "cancelled" or "error".

local Error = require("beachcomber.error")

local WatchStream = {}
WatchStream.__index = WatchStream

--- @param handle table               A backend watch handle.
--- @param library_version string?    The loaded library's version, for errors.
-- @return WatchStream
function WatchStream.new(handle, library_version)
  return setmetatable({ _handle = handle, _closed = false, _version = library_version }, WatchStream)
end

--- The next event.
-- @param timeout_ms number?  How long to wait, in ms: nil waits indefinitely,
--                            0 polls.
-- @return the event table `{data}`; or nil (no Error) once the stream has
--         ended (the daemon closed it, or it was cancelled); or nil, an Error
--         of kind "timeout" when `timeout_ms` ran out with no event; or nil,
--         Error when the stream failed.
function WatchStream:next_event(timeout_ms)
  if self._closed then return nil end
  timeout_ms = timeout_ms or -1
  local ev, err, outcome = self._handle:next_event(timeout_ms)
  if outcome == "event" then
    return ev
  end
  if outcome == "timeout" then
    return nil, Error.new("timeout", string.format("no watch event within %dms", timeout_ms), self._version)
  end
  if outcome == "error" then
    return nil, err
  end
  return nil -- eof or cancelled: the end of the stream
end

--- End the stream: a next_event() waiting now, or called later, answers its
-- end. Safe to call while another coroutine or thread waits in next_event.
-- @return true
function WatchStream:cancel()
  if not self._closed and self._handle.cancel then self._handle:cancel() end
  return true
end

--- Cancel the stream and release its handle.
-- @return true
function WatchStream:close()
  if not self._closed then
    self._closed = true
    if self._handle.cancel then self._handle:cancel() end
    self._handle:close()
  end
  return true
end

--- Lua's way to iterate a stream: `for event in stream:each() do ... end`.
-- The loop ends with the stream; a stream that fails raises its Error,
-- which an iterator could not otherwise report.
-- @return function iterator
function WatchStream:each()
  return function()
    local ev, err = self:next_event()
    if err then error(err, 0) end
    return ev
  end
end

return WatchStream

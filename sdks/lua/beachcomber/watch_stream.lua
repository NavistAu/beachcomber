--- WatchStream — a stream of watch events from the beachcomber daemon.
--
-- Returned by Client:watch(). Use :next_event() to poll for events or
-- the :each() iterator for a for-loop idiom.

local json = require("beachcomber.json")

local WatchStream = {}
WatchStream.__index = WatchStream

--- Create a WatchStream wrapping a backend handle.
-- The handle must already have had the watch request sent on it.
-- @param backend  Backend handle with :recv_line() and :close()
-- @return WatchStream
function WatchStream.new(backend)
  return setmetatable({ backend = backend, closed = false }, WatchStream)
end

--- Block for the next event. Returns nil when the connection closes.
-- @return table {data, age_ms, stale}, or nil, error_message
function WatchStream:next_event()
  if self.closed then return nil end
  local line, err = self.backend:recv_line()
  if not line then
    return nil, err
  end
  local ok, resp = pcall(json.decode, line)
  if not ok or not resp then
    return nil, "parse error"
  end
  if not resp.ok then
    return nil, resp.error or "watch error"
  end
  return {
    data   = resp.data,
    age_ms = resp.age_ms or 0,
    stale  = resp.stale == true,
  }
end

--- Close the watch stream and the underlying backend connection.
function WatchStream:close()
  self.closed = true
  if self.backend and self.backend.close then
    self.backend:close()
  end
end

--- Iterator for use in a for-loop:
--   for event in stream:each() do ... end
-- The loop ends when next_event returns nil.
-- @return function iterator
function WatchStream:each()
  return function()
    local ev, _ = self:next_event()
    return ev
  end
end

return WatchStream

--- Idiomatic error object for the beachcomber Lua SDK.
--
-- Wraps a machine-readable `kind` (mirroring the ABI envelope's
-- `error.kind` slug, e.g. "server_error", "daemon_not_running") plus a
-- human-readable `message`. Every Client method that can fail returns
-- `nil, Error` rather than a bare string, so callers can branch on `kind`
-- without substring matching.

local Error = {}
Error.__index = Error

Error.__tostring = function(self)
  return string.format("beachcomber: %s: %s", self.kind or "error", self.message or "")
end

--- @param kind string      Machine-readable slug (see ABI `error.kind`, or
--                          "unsupported" for a capability this transport lacks).
--- @param message string   Human-readable description.
--- @return Error
function Error.new(kind, message)
  return setmetatable({ kind = kind or "error", message = message or "" }, Error)
end

--- Build an Error from a decoded envelope's `error` sub-table
--- (`{kind=..., message=...}`), or from a bare string for transports that
--- can only produce prose.
--- @param err table|string|nil
--- @return Error
function Error.from(err)
  if type(err) == "table" then
    return Error.new(err.kind, err.message)
  elseif type(err) == "string" then
    return Error.new("error", err)
  end
  return Error.new("error", "unknown error")
end

--- Build an "unsupported" Error naming the capability and the transport
--- that lacks it — used whenever a backend omits an optional op.
--- @param capability string
--- @param transport string
--- @return Error
function Error.unsupported(capability, transport)
  return Error.new(
    "unsupported",
    string.format("%s is not supported over the %s transport", capability, transport)
  )
end

return Error

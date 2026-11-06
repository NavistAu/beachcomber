--- The error a beachcomber Lua SDK method returns: `nil, Error`.
--
-- An Error carries the SDK API table's fields (docs/sdk-api.md): `kind`, a
-- machine-readable slug from `Error.KINDS`; `message`, the ABI's message as
-- it came (or this SDK's own, for a kind it raises itself); and
-- `library_version`, the loaded library's `bc_version()` — nil over the
-- subprocess transport, which loads no library. Branch on `kind`, never on
-- the message.

local Error = {}
Error.__index = Error

--- Every kind an Error can have, the table's `error_kinds`: the ABI's ten,
-- then the three this SDK raises itself.
Error.KINDS = {
  "daemon_not_running",
  "connection_failed",
  "io_error",
  "parse_error",
  "server_error",
  "timeout",
  "busy",
  "panic",
  "library_not_found",
  "library_symbol",
  "unsupported",
}

Error.__tostring = function(self)
  return string.format("beachcomber: %s: %s", self.kind, self.message)
end

--- @param kind string               One of Error.KINDS.
--- @param message string            The ABI's message, or this SDK's own.
--- @param library_version string?   The loaded library's version.
--- @return Error
function Error.new(kind, message, library_version)
  return setmetatable({ kind = kind, message = message or "", library_version = library_version }, Error)
end

--- An Error from a decoded envelope's `error` table (`{kind=..., message=...}`),
--- or one a backend built already. Anything else is a malformed envelope.
--- @param err table|nil
--- @param library_version string?
--- @return Error
function Error.from(err, library_version)
  if type(err) == "table" then
    return Error.new(err.kind, err.message, err.library_version or library_version)
  end
  return Error.new("parse_error", "malformed error envelope: " .. tostring(err), library_version)
end

--- An "unsupported" Error naming the capability and the transport that
--- lacks it — used whenever a backend omits an optional op.
--- @param capability string
--- @param transport string
--- @param library_version string?
--- @return Error
function Error.unsupported(capability, transport, library_version)
  return Error.new(
    "unsupported",
    string.format("%s is not supported over the %s transport", capability, transport),
    library_version
  )
end

return Error

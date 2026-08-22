--- Minimal JSON encoder/decoder for the beachcomber protocol.
--
-- Handles the subset of JSON used by the daemon:
-- objects, arrays, strings, numbers, booleans, and null.
--
-- Supports Lua 5.1+ and LuaJIT.

local M = {}

-- A plain empty Lua table cannot record whether it came from JSON `{}` or
-- `[]` — both decode to a table with no keys, and Lua has no separate empty
-- vector/map runtime types the way Python (dict{} vs list[]) or Go
-- (map[string]interface{}{} vs []interface{}{}) do. decode_object tags an
-- empty result with this sentinel metatable so a consumer that needs to
-- recover the JSON type (e.g. the conformance runner's data_type check) can
-- via M.is_empty_object(); a non-empty table stays distinguishable by shape
-- (sequential integer keys from 1) without any tagging. Encoding does not
-- need to consult this — encode_table already defaults an empty table to a
-- JSON object (its is_array check requires count > 0), matching this tag.
local EMPTY_OBJECT_MT = {}

--- True when `t` is an empty table that was decoded from a JSON object
-- (`{}`), as opposed to a plain empty table or one decoded from `[]`.
function M.is_empty_object(t)
  return type(t) == 'table' and getmetatable(t) == EMPTY_OBJECT_MT
end

-- A plain Lua `nil` cannot be stored as a table value — `t[k] = nil` deletes
-- `k` rather than recording "present with value null" — so a JSON `null`
-- nested inside an object or array is indistinguishable from that key/index
-- being entirely absent once it has passed through a plain-nil decode. That
-- collapse happens *before* any caller-visible put/get logic runs (e.g. a
-- conformance fixture's own `{"data":{"v":null}}` setup payload loses the
-- `v` key while the fixture file itself is being parsed), so callers can
-- never legitimately construct or observe a null-valued field at all.
--
-- M.NULL is a unique sentinel table standing in for JSON null at any
-- nesting level, including a bare top-level `"null"` document: decode
-- produces it, encode turns it back into the literal `null` token, and it
-- is stored/iterated like any other non-nil table value. It is exported
-- (re-exported as `beachcomber.null` from init.lua) so a caller can test a
-- decoded field with `v == json.NULL` / `json.is_null(v)`.
--
-- This sentinel must NOT leak into miss-detection. The wire protocol's own
-- "no cached value" encoding is the literal shape `"data":null` in a
-- get/watch response — identical on the wire to a genuine stored null —
-- so a transport that decoded that field with M.decode would see M.NULL
-- for a miss too. The daemon has no representation for an actual stored
-- null (src/provider/mod.rs converts a put null to an empty string before
-- caching it — see tests/conformance/mapping/
-- null_value_becomes_empty_string.json), so that specific "data" field can
-- only ever mean "miss" and is the one place a sentinel must be collapsed
-- back to plain Lua nil: see ffi_backend.lua's unwrap_get and
-- Watch:next_event. Nowhere else should perform that collapse.
local NULL = setmetatable({}, { __tostring = function() return "null" end })
M.NULL = NULL

--- True when `v` is the JSON-null sentinel (see M.NULL above).
function M.is_null(v)
  return v == NULL
end

-- ── Decoder ──────────────────────────────────────────────────────────────────

local function decode_error(s, pos, msg)
  error(string.format("JSON decode error at position %d: %s (near %q)", pos, msg, s:sub(pos, pos + 10)), 3)
end

local function skip_ws(s, pos)
  while pos <= #s do
    local c = s:sub(pos, pos)
    if c == ' ' or c == '\t' or c == '\n' or c == '\r' then
      pos = pos + 1
    else
      break
    end
  end
  return pos
end

local decode_value -- forward declaration

local ESCAPE_MAP = {
  ['"']  = '"',
  ['\\'] = '\\',
  ['/']  = '/',
  ['b']  = '\b',
  ['f']  = '\f',
  ['n']  = '\n',
  ['r']  = '\r',
  ['t']  = '\t',
}

local function decode_string(s, pos)
  -- pos points at the opening '"'
  pos = pos + 1 -- skip '"'
  local parts = {}
  while pos <= #s do
    local c = s:sub(pos, pos)
    if c == '"' then
      return table.concat(parts), pos + 1
    elseif c == '\\' then
      pos = pos + 1
      local esc = s:sub(pos, pos)
      if ESCAPE_MAP[esc] then
        parts[#parts + 1] = ESCAPE_MAP[esc]
        pos = pos + 1
      elseif esc == 'u' then
        -- Basic BMP unicode: read 4 hex digits, convert to UTF-8
        local hex = s:sub(pos + 1, pos + 4)
        if #hex < 4 then
          decode_error(s, pos, "incomplete \\u escape")
        end
        local codepoint = tonumber(hex, 16)
        if not codepoint then
          decode_error(s, pos, "invalid \\u escape")
        end
        if codepoint < 0x80 then
          parts[#parts + 1] = string.char(codepoint)
        elseif codepoint < 0x800 then
          parts[#parts + 1] = string.char(
            0xC0 + math.floor(codepoint / 64),
            0x80 + (codepoint % 64)
          )
        else
          parts[#parts + 1] = string.char(
            0xE0 + math.floor(codepoint / 4096),
            0x80 + math.floor((codepoint % 4096) / 64),
            0x80 + (codepoint % 64)
          )
        end
        pos = pos + 5
      else
        decode_error(s, pos, "unknown escape \\" .. esc)
      end
    else
      parts[#parts + 1] = c
      pos = pos + 1
    end
  end
  decode_error(s, pos, "unterminated string")
end

local function decode_number(s, pos)
  local num_str = s:match("^-?%d+%.?%d*[eE]?[+-]?%d*", pos)
  if not num_str then
    decode_error(s, pos, "invalid number")
  end
  local n = tonumber(num_str)
  if n == nil then
    decode_error(s, pos, "invalid number: " .. num_str)
  end
  return n, pos + #num_str
end

local function decode_array(s, pos)
  -- pos points at '['
  pos = pos + 1
  local arr = {}
  local n = 0  -- explicit counter: a JSON null decodes to M.NULL (non-nil), so
               -- a null element is stored (and counted) like any other value,
               -- but the counter still guards against pairs()/# relying on
               -- table-length inference over a sparse table
  pos = skip_ws(s, pos)
  if s:sub(pos, pos) == ']' then
    return arr, pos + 1
  end
  while true do
    pos = skip_ws(s, pos)
    local val
    val, pos = decode_value(s, pos)
    n = n + 1
    rawset(arr, n, val)
    pos = skip_ws(s, pos)
    local c = s:sub(pos, pos)
    if c == ']' then
      return arr, pos + 1
    elseif c == ',' then
      pos = pos + 1
    else
      decode_error(s, pos, "expected ',' or ']' in array")
    end
  end
end

local function decode_object(s, pos)
  -- pos points at '{'
  pos = pos + 1
  local obj = {}
  pos = skip_ws(s, pos)
  if s:sub(pos, pos) == '}' then
    setmetatable(obj, EMPTY_OBJECT_MT)
    return obj, pos + 1
  end
  while true do
    pos = skip_ws(s, pos)
    if s:sub(pos, pos) ~= '"' then
      decode_error(s, pos, "expected string key in object")
    end
    local key
    key, pos = decode_string(s, pos)
    pos = skip_ws(s, pos)
    if s:sub(pos, pos) ~= ':' then
      decode_error(s, pos, "expected ':' after object key")
    end
    pos = pos + 1
    pos = skip_ws(s, pos)
    local val
    val, pos = decode_value(s, pos)
    obj[key] = val
    pos = skip_ws(s, pos)
    local c = s:sub(pos, pos)
    if c == '}' then
      return obj, pos + 1
    elseif c == ',' then
      pos = pos + 1
    else
      decode_error(s, pos, "expected ',' or '}' in object")
    end
  end
end

decode_value = function(s, pos)
  pos = skip_ws(s, pos)
  if pos > #s then
    decode_error(s, pos, "unexpected end of input")
  end
  local c = s:sub(pos, pos)
  if c == '"' then
    return decode_string(s, pos)
  elseif c == '{' then
    return decode_object(s, pos)
  elseif c == '[' then
    return decode_array(s, pos)
  elseif c == 't' then
    if s:sub(pos, pos + 3) == 'true' then
      return true, pos + 4
    end
    decode_error(s, pos, "invalid token")
  elseif c == 'f' then
    if s:sub(pos, pos + 4) == 'false' then
      return false, pos + 5
    end
    decode_error(s, pos, "invalid token")
  elseif c == 'n' then
    if s:sub(pos, pos + 3) == 'null' then
      return NULL, pos + 4 -- M.NULL represents JSON null (see M.NULL above)
    end
    decode_error(s, pos, "invalid token")
  elseif c == '-' or (c >= '0' and c <= '9') then
    return decode_number(s, pos)
  else
    decode_error(s, pos, "unexpected character: " .. c)
  end
end

--- Decode a JSON string into a Lua value.
-- JSON null becomes the M.NULL sentinel (see above), at any nesting level
-- including a bare top-level "null" document. JSON objects become tables
-- with string keys. JSON arrays become tables with integer keys.
-- @param s string  JSON text
-- @return value, nil on success; nil, error_message on failure
function M.decode(s)
  if type(s) ~= 'string' then
    return nil, "json.decode: expected string, got " .. type(s)
  end
  local ok, result_or_err, _ = pcall(function()
    local val, pos = decode_value(s, 1)
    pos = skip_ws(s, pos)
    if pos <= #s then
      decode_error(s, pos, "trailing garbage after JSON value")
    end
    return val
  end)
  if ok then
    return result_or_err
  else
    return nil, result_or_err
  end
end

-- ── Encoder ──────────────────────────────────────────────────────────────────

local STRING_ESCAPE = {
  ['"']  = '\\"',
  ['\\'] = '\\\\',
  ['\b'] = '\\b',
  ['\f'] = '\\f',
  ['\n'] = '\\n',
  ['\r'] = '\\r',
  ['\t'] = '\\t',
}

local function encode_string(s)
  s = s:gsub('[%z\1-\31"\\]', function(c)
    return STRING_ESCAPE[c] or string.format('\\u%04x', c:byte())
  end)
  return '"' .. s .. '"'
end

local encode_value -- forward declaration

local function encode_table(t)
  -- Determine if this is an array (consecutive integer keys from 1)
  local max_n = 0
  local count = 0
  for k, _ in pairs(t) do
    count = count + 1
    if type(k) == 'number' and k == math.floor(k) and k >= 1 then
      if k > max_n then max_n = k end
    end
  end

  local is_array = (max_n == count) and count > 0

  if is_array then
    local parts = {}
    for i = 1, max_n do
      parts[i] = encode_value(t[i])
    end
    return '[' .. table.concat(parts, ',') .. ']'
  else
    local parts = {}
    local keys = {}
    for k in pairs(t) do
      keys[#keys + 1] = k
    end
    table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
    for _, k in ipairs(keys) do
      local v = t[k]
      parts[#parts + 1] = encode_string(tostring(k)) .. ':' .. encode_value(v)
    end
    return '{' .. table.concat(parts, ',') .. '}'
  end
end

encode_value = function(v)
  local t = type(v)
  if t == 'nil' then
    return 'null'
  elseif v == NULL then
    return 'null' -- M.NULL round-trips back to the literal JSON null token
  elseif t == 'boolean' then
    return v and 'true' or 'false'
  elseif t == 'number' then
    if v ~= v then return 'null' end -- NaN
    if v == math.huge or v == -math.huge then return 'null' end
    -- Use integer representation when safe
    if v == math.floor(v) and math.abs(v) < 2^53 then
      return string.format('%d', v)
    end
    return string.format('%.17g', v)
  elseif t == 'string' then
    return encode_string(v)
  elseif t == 'table' then
    return encode_table(v)
  else
    error("json.encode: unsupported type: " .. t)
  end
end

--- Encode a Lua value to a JSON string.
-- @param v  Lua value (table, string, number, boolean, or nil)
-- @return string JSON text, or nil, error_message on failure
function M.encode(v)
  local ok, result = pcall(encode_value, v)
  if ok then
    return result
  else
    return nil, result
  end
end

return M

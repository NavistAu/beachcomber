--- Minimal JSON encoder/decoder for the beachcomber protocol.
--
-- Handles the subset of JSON used by the daemon:
-- objects, arrays, strings, numbers, booleans, and null.
--
-- Supports Lua 5.1+ and LuaJIT.

local M = {}

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
  local n = 0  -- explicit counter so null values (nil) are stored correctly
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
      return nil, pos + 4 -- nil represents JSON null
    end
    decode_error(s, pos, "invalid token")
  elseif c == '-' or (c >= '0' and c <= '9') then
    return decode_number(s, pos)
  else
    decode_error(s, pos, "unexpected character: " .. c)
  end
end

--- Decode a JSON string into a Lua value.
-- JSON null becomes nil. JSON objects become tables with string keys.
-- JSON arrays become tables with integer keys.
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

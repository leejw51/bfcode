-- Minimal JSON encoder/decoder for Love2D
-- Based on rxi/json.lua (MIT License)
local json = { _version = "0.1.0" }

-- Encode
local encode

local escape_char_map = {
    ["\\"] = "\\\\", ["\""] = "\\\"", ["\b"] = "\\b",
    ["\f"] = "\\f",  ["\n"] = "\\n",  ["\r"] = "\\r",
    ["\t"] = "\\t",
}

local function escape_char(c)
    return escape_char_map[c] or string.format("\\u%04x", c:byte())
end

local function encode_nil()
    return "null"
end

local function encode_string(val)
    return '"' .. val:gsub('[%z\1-\31\\"]', escape_char) .. '"'
end

local function encode_number(val)
    if val ~= val or val <= -math.huge or val >= math.huge then
        error("unexpected number value '" .. tostring(val) .. "'")
    end
    return string.format("%.14g", val)
end

local type_func_map = {
    ["nil"]     = encode_nil,
    ["boolean"] = tostring,
    ["number"]  = encode_number,
    ["string"]  = encode_string,
}

encode = function(val, stack)
    local t = type(val)
    local f = type_func_map[t]
    if f then return f(val) end

    if t ~= "table" then
        error("unexpected type '" .. t .. "'")
    end

    stack = stack or {}
    if stack[val] then error("circular reference") end
    stack[val] = true

    local res = {}
    -- Check if array
    local is_array = true
    local n = 0
    for k in pairs(val) do
        if type(k) ~= "number" or k <= 0 or math.floor(k) ~= k then
            is_array = false
            break
        end
        n = math.max(n, k)
    end
    if n ~= #val then is_array = false end

    if is_array then
        for i = 1, #val do
            table.insert(res, encode(val[i], stack))
        end
        stack[val] = nil
        return "[" .. table.concat(res, ",") .. "]"
    else
        for k, v in pairs(val) do
            if type(k) ~= "string" then
                error("invalid table key type: " .. type(k))
            end
            table.insert(res, encode_string(k) .. ":" .. encode(v, stack))
        end
        stack[val] = nil
        return "{" .. table.concat(res, ",") .. "}"
    end
end

json.encode = encode

-- Decode
local decode

local literal_map = {
    ["true"]  = true,
    ["false"] = false,
    ["null"]  = nil,
}

local function create_set(...)
    local s = {}
    for i = 1, select("#", ...) do s[select(i, ...)] = true end
    return s
end

local space_chars   = create_set(" ", "\t", "\r", "\n")
local delim_chars   = create_set(" ", "\t", "\r", "\n", "]", "}", ",")
local escape_chars  = create_set("\\", "/", '"', "b", "f", "n", "r", "t", "u")

local function next_char(str, idx, set)
    for i = idx, #str do
        if not set[str:sub(i, i)] then return i end
    end
    return #str + 1
end

local function decode_error(str, idx, msg)
    local line_count = 1
    local col_count = 1
    for i = 1, idx - 1 do
        col_count = col_count + 1
        if str:sub(i, i) == "\n" then
            line_count = line_count + 1
            col_count = 1
        end
    end
    error(string.format("%s at line %d col %d", msg, line_count, col_count))
end

local escape_char_map_inv = {
    ["/"]  = "/", ["\\"] = "\\", ['"'] = '"',
    ["b"]  = "\b", ["f"] = "\f", ["n"] = "\n",
    ["r"]  = "\r", ["t"] = "\t",
}

local function parse_unicode_escape(s)
    local n = tonumber(s:sub(1, 4), 16)
    if not n then return "" end
    if n < 0x80 then
        return string.char(n)
    elseif n < 0x800 then
        return string.char(0xC0 + math.floor(n / 64), 0x80 + (n % 64))
    else
        return string.char(0xE0 + math.floor(n / 4096),
                           0x80 + math.floor((n % 4096) / 64),
                           0x80 + (n % 64))
    end
end

local function decode_string(str, i)
    local res = {}
    local j = i + 1
    while j <= #str do
        local c = str:sub(j, j)
        if c == '"' then
            return table.concat(res), j + 1
        elseif c == "\\" then
            j = j + 1
            c = str:sub(j, j)
            if c == "u" then
                local hex = str:sub(j + 1, j + 4)
                table.insert(res, parse_unicode_escape(hex))
                j = j + 5
            else
                if not escape_chars[c] then
                    decode_error(str, j, "invalid escape char '" .. c .. "'")
                end
                table.insert(res, escape_char_map_inv[c])
                j = j + 1
            end
        else
            table.insert(res, c)
            j = j + 1
        end
    end
    decode_error(str, i, "expected closing quote")
end

local function decode_number(str, i)
    local x = next_char(str, i, delim_chars)
    local s = str:sub(i, x - 1)
    local n = tonumber(s)
    if not n then decode_error(str, i, "invalid number '" .. s .. "'") end
    return n, x
end

local function decode_literal(str, i)
    local x = next_char(str, i, delim_chars)
    local word = str:sub(i, x - 1)
    if not (word == "true" or word == "false" or word == "null") then
        decode_error(str, i, "invalid literal '" .. word .. "'")
    end
    return literal_map[word], x
end

local function decode_array(str, i, state)
    local res = {}
    local n = 0
    i = i + 1
    while true do
        i = next_char(str, i, space_chars)
        if str:sub(i, i) == "]" then return res, i + 1 end
        if n > 0 then
            if str:sub(i, i) ~= "," then decode_error(str, i, "expected ','") end
            i = next_char(str, i + 1, space_chars)
        end
        local val
        val, i = decode(str, i)
        res[n + 1] = val
        n = n + 1
    end
end

local function decode_object(str, i, state)
    local res = {}
    i = i + 1
    local first = true
    while true do
        i = next_char(str, i, space_chars)
        if str:sub(i, i) == "}" then return res, i + 1 end
        if not first then
            if str:sub(i, i) ~= "," then decode_error(str, i, "expected ','") end
            i = next_char(str, i + 1, space_chars)
        end
        first = false
        if str:sub(i, i) ~= '"' then decode_error(str, i, "expected string for key") end
        local key
        key, i = decode_string(str, i)
        i = next_char(str, i, space_chars)
        if str:sub(i, i) ~= ":" then decode_error(str, i, "expected ':'") end
        i = next_char(str, i + 1, space_chars)
        local val
        val, i = decode(str, i)
        res[key] = val
    end
end

local char_func_map = {
    ['"'] = decode_string,
    ["0"] = decode_number, ["1"] = decode_number, ["2"] = decode_number,
    ["3"] = decode_number, ["4"] = decode_number, ["5"] = decode_number,
    ["6"] = decode_number, ["7"] = decode_number, ["8"] = decode_number,
    ["9"] = decode_number, ["-"] = decode_number,
    ["t"] = decode_literal, ["f"] = decode_literal, ["n"] = decode_literal,
    ["["] = decode_array,   ["{"] = decode_object,
}

decode = function(str, idx)
    idx = idx or 1
    idx = next_char(str, idx, space_chars)
    local c = str:sub(idx, idx)
    local f = char_func_map[c]
    if f then return f(str, idx) end
    decode_error(str, idx, "unexpected character '" .. c .. "'")
end

function json.decode(str)
    if type(str) ~= "string" then
        error("expected string, got " .. type(str))
    end
    local result, _ = decode(str, 1)
    return result
end

return json

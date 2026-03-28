-- WebSocket client for Love2D using threads
-- Connects to the gateway WebSocket endpoint (/v1/ws)
local ws = {}

local connected = false
local thread = nil
local sendChannel = nil
local recvChannel = nil
local pending = {}
local nextId = 1

local threadCode = [[
local socket = require("socket")
local sendCh = love.thread.getChannel("ws_send")
local recvCh = love.thread.getChannel("ws_recv")

-- Minimal WebSocket frame helpers
local function mask_data(data, mask_key)
    local masked = {}
    for i = 1, #data do
        local j = ((i - 1) % 4) + 1
        masked[i] = string.char(bit.bxor(data:byte(i), mask_key:byte(j)))
    end
    return table.concat(masked)
end

local function send_frame(tcp, text)
    local len = #text
    local header
    if len < 126 then
        header = string.char(0x81, bit.bor(len, 0x80))
    elseif len < 65536 then
        header = string.char(0x81, bit.bor(126, 0x80),
            bit.rshift(len, 8), bit.band(len, 0xFF))
    else
        return false, "message too large"
    end
    local mask_key = string.char(
        math.random(0, 255), math.random(0, 255),
        math.random(0, 255), math.random(0, 255))
    local masked = mask_data(text, mask_key)
    local ok, err = tcp:send(header .. mask_key .. masked)
    return ok, err
end

local function read_frame(tcp)
    local b1, err = tcp:receive(2)
    if not b1 then return nil, err end

    local opcode = bit.band(b1:byte(1), 0x0F)
    local payload_len = bit.band(b1:byte(2), 0x7F)
    local is_masked = bit.band(b1:byte(2), 0x80) ~= 0

    if payload_len == 126 then
        local ext, err2 = tcp:receive(2)
        if not ext then return nil, err2 end
        payload_len = bit.lshift(ext:byte(1), 8) + ext:byte(2)
    elseif payload_len == 127 then
        local ext, err2 = tcp:receive(8)
        if not ext then return nil, err2 end
        payload_len = 0
        for i = 1, 8 do
            payload_len = payload_len * 256 + ext:byte(i)
        end
    end

    local mask_key = nil
    if is_masked then
        mask_key, err = tcp:receive(4)
        if not mask_key then return nil, err end
    end

    local payload = ""
    if payload_len > 0 then
        payload, err = tcp:receive(payload_len)
        if not payload then return nil, err end
        if mask_key then
            payload = mask_data(payload, mask_key)
        end
    end

    return opcode, payload
end

-- Parse URL
local url = sendCh:demand()
if type(url) ~= "string" then
    recvCh:push({type = "error", error = "Invalid URL"})
    return
end

-- Extract host, port, path from ws://host:port/path
local host, port, path = url:match("^ws://([^:/]+):?(%d*)(.*)")
if not host then
    recvCh:push({type = "error", error = "Invalid WebSocket URL: " .. url})
    return
end
port = tonumber(port) or 80
if path == "" then path = "/" end

-- TCP connect
local tcp = socket.tcp()
tcp:settimeout(10)
local ok, err = tcp:connect(host, port)
if not ok then
    recvCh:push({type = "error", error = "Connection failed: " .. tostring(err)})
    return
end

-- WebSocket handshake
local key = ""
for i = 1, 16 do key = key .. string.char(math.random(0, 255)) end
-- Simple base64 for the key
local b64chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
local function base64(data)
    local result = {}
    for i = 1, #data, 3 do
        local b0 = data:byte(i)
        local b1 = i + 1 <= #data and data:byte(i + 1) or 0
        local b2 = i + 2 <= #data and data:byte(i + 2) or 0
        local n = b0 * 65536 + b1 * 256 + b2
        table.insert(result, b64chars:sub(bit.rshift(n, 18) % 64 + 1, bit.rshift(n, 18) % 64 + 1))
        table.insert(result, b64chars:sub(bit.rshift(n, 12) % 64 + 1, bit.rshift(n, 12) % 64 + 1))
        if i + 1 <= #data then
            table.insert(result, b64chars:sub(bit.rshift(n, 6) % 64 + 1, bit.rshift(n, 6) % 64 + 1))
        else
            table.insert(result, "=")
        end
        if i + 2 <= #data then
            table.insert(result, b64chars:sub(n % 64 + 1, n % 64 + 1))
        else
            table.insert(result, "=")
        end
    end
    return table.concat(result)
end

local ws_key = base64(key)
local handshake = "GET " .. path .. " HTTP/1.1\r\n" ..
    "Host: " .. host .. ":" .. port .. "\r\n" ..
    "Upgrade: websocket\r\n" ..
    "Connection: Upgrade\r\n" ..
    "Sec-WebSocket-Key: " .. ws_key .. "\r\n" ..
    "Sec-WebSocket-Version: 13\r\n" ..
    "\r\n"

tcp:send(handshake)

-- Read handshake response
tcp:settimeout(10)
local response = ""
while true do
    local line, err2 = tcp:receive("*l")
    if not line then
        recvCh:push({type = "error", error = "Handshake failed: " .. tostring(err2)})
        tcp:close()
        return
    end
    response = response .. line .. "\n"
    if line == "" or line == "\r" then break end
end

if not response:match("101") then
    recvCh:push({type = "error", error = "WebSocket handshake rejected"})
    tcp:close()
    return
end

recvCh:push({type = "connected"})

-- Main loop: read from server + check send channel
tcp:settimeout(0.05)

local running = true
while running do
    -- Check for outgoing messages
    local msg = sendCh:pop()
    if msg then
        if msg == "close" then
            -- Send close frame
            local close_frame = string.char(0x88, 0x80,
                math.random(0, 255), math.random(0, 255),
                math.random(0, 255), math.random(0, 255))
            tcp:send(close_frame)
            running = false
        else
            local ok2, err2 = send_frame(tcp, msg)
            if not ok2 then
                recvCh:push({type = "error", error = "Send failed: " .. tostring(err2)})
                running = false
            end
        end
    end

    -- Read incoming
    local opcode, payload = read_frame(tcp)
    if opcode then
        if opcode == 0x01 then -- text
            recvCh:push({type = "message", data = payload})
        elseif opcode == 0x08 then -- close
            running = false
        elseif opcode == 0x09 then -- ping
            -- Send pong (opcode 0x0A)
            local pong = string.char(0x8A, bit.bor(#payload, 0x80),
                math.random(0, 255), math.random(0, 255),
                math.random(0, 255), math.random(0, 255))
            if #payload > 0 then
                local mk = pong:sub(3, 6)
                pong = pong .. mask_data(payload, mk)
            end
            tcp:send(pong)
        end
    end

    -- Small sleep to avoid busy loop
    socket.sleep(0.01)
end

tcp:close()
recvCh:push({type = "disconnected"})
]]

function ws.connect(url, callbacks)
    if thread then
        ws.disconnect()
    end

    -- Convert http:// to ws://
    local ws_url = url:gsub("^http://", "ws://"):gsub("^https://", "wss://")
    ws_url = ws_url:gsub("/+$", "") .. "/v1/ws"

    pending = callbacks or {}
    connected = false

    sendChannel = love.thread.getChannel("ws_send")
    recvChannel = love.thread.getChannel("ws_recv")

    -- Clear channels
    while sendChannel:getCount() > 0 do sendChannel:pop() end
    while recvChannel:getCount() > 0 do recvChannel:pop() end

    thread = love.thread.newThread(threadCode)
    thread:start()

    -- Send URL as first message to the thread
    sendChannel:push(ws_url)
end

function ws.send(data)
    if not thread or not sendChannel then return false end
    sendChannel:push(data)
    return true
end

function ws.sendJson(tbl)
    local json = require("json")
    return ws.send(json.encode(tbl))
end

function ws.isConnected()
    return connected
end

function ws.update()
    if not recvChannel then return end

    while recvChannel:getCount() > 0 do
        local msg = recvChannel:pop()
        if not msg then break end

        if msg.type == "connected" then
            connected = true
            if pending.onConnect then pending.onConnect() end
        elseif msg.type == "disconnected" then
            connected = false
            if pending.onDisconnect then pending.onDisconnect() end
        elseif msg.type == "error" then
            if pending.onError then pending.onError(msg.error) end
        elseif msg.type == "message" then
            if pending.onMessage then pending.onMessage(msg.data) end
        end
    end

    -- Check thread errors
    if thread then
        local err = thread:getError()
        if err then
            print("WebSocket thread error: " .. err)
            if pending.onError then pending.onError(err) end
            thread = nil
            connected = false
        end
    end
end

function ws.disconnect()
    if thread and sendChannel then
        sendChannel:push("close")
        -- Give thread a moment to close gracefully
        thread:wait()
    end
    thread = nil
    connected = false
    pending = {}
end

return ws

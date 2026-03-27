-- Async HTTP client using Love2D threads
local http = {}

local pending = {}
local nextId = 1

local threadCode = [[
local args = love.thread.getChannel("http_request")
local results = love.thread.getChannel("http_response")

while true do
    local req = args:demand()
    if req == "quit" then break end

    local id = req.id
    local url = req.url
    local method = req.method or "GET"
    local body = req.body
    local headers = req.headers or {}

    local ok, result = pcall(function()
        local ltn12 = require("ltn12")
        local socket = require("socket")

        local http_mod
        if url:match("^https") then
            http_mod = require("ssl.https")
        else
            http_mod = require("socket.http")
        end

        -- Set a long timeout for chat requests (they shell out to AI)
        http_mod.TIMEOUT = 300

        local response_body = {}
        local request_table = {
            url = url,
            method = method,
            sink = ltn12.sink.table(response_body),
            headers = {},
            create = function()
                local tcp = socket.tcp()
                tcp:settimeout(300)
                return tcp
            end,
        }

        for k, v in pairs(headers) do
            request_table.headers[k:lower()] = v
        end

        if body then
            local source = ltn12.source.string(body)
            request_table.source = source
            request_table.headers["content-length"] = tostring(#body)
            request_table.headers["content-type"] = request_table.headers["content-type"] or "application/json"
        end

        local _, status_code = http_mod.request(request_table)

        return {
            status = status_code,
            body = table.concat(response_body),
        }
    end)

    if ok then
        results:push({id = id, success = true, data = result})
    else
        results:push({id = id, success = false, error = tostring(result)})
    end
end
]]

local thread = nil

function http.init()
    if thread then return end
    thread = love.thread.newThread(threadCode)
    thread:start()
end

function http.request(method, url, body, headers, callback)
    http.init()

    local id = nextId
    nextId = nextId + 1

    pending[id] = callback

    local req = {
        id = id,
        url = url,
        method = method,
        body = body,
        headers = headers or {},
    }

    love.thread.getChannel("http_request"):push(req)
    return id
end

function http.get(url, headers, callback)
    return http.request("GET", url, nil, headers, callback)
end

function http.post(url, body, headers, callback)
    return http.request("POST", url, body, headers, callback)
end

function http.update()
    local ch = love.thread.getChannel("http_response")
    while ch:getCount() > 0 do
        local resp = ch:pop()
        if resp and pending[resp.id] then
            local cb = pending[resp.id]
            pending[resp.id] = nil
            cb(resp)
        end
    end

    -- Check for thread errors
    if thread then
        local err = thread:getError()
        if err then
            print("HTTP thread error: " .. err)
            thread = nil
        end
    end
end

function http.shutdown()
    if thread then
        love.thread.getChannel("http_request"):push("quit")
        thread = nil
    end
end

return http

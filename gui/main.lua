-- BFCode GUI - Love2D Client for Gateway Server
local http = require("http")
local login = require("login")
local chat = require("chat")
local ui = require("ui")

local state = {
    screen = "login",  -- "login" or "chat"
    gateway_url = "http://127.0.0.1:8642",
    api_key = "",
    session_id = nil,
    connected = false,
    error_msg = nil,
}

function love.load()
    love.keyboard.setKeyRepeat(true)
    ui.load()
    login.load(state)
    chat.load(state)
end

function love.update(dt)
    http.update()
    if state.screen == "login" then
        login.update(dt, state)
    elseif state.screen == "chat" then
        chat.update(dt, state)
    end
end

function love.draw()
    ui.drawBackground()
    if state.screen == "login" then
        login.draw(state)
    elseif state.screen == "chat" then
        chat.draw(state)
    end
end

function love.textinput(t)
    if state.screen == "login" then
        login.textinput(t, state)
    elseif state.screen == "chat" then
        chat.textinput(t, state)
    end
end

function love.keypressed(key)
    if state.screen == "login" then
        login.keypressed(key, state)
    elseif state.screen == "chat" then
        chat.keypressed(key, state)
    end
end

function love.mousepressed(x, y, button)
    if state.screen == "login" then
        login.mousepressed(x, y, button, state)
    elseif state.screen == "chat" then
        chat.mousepressed(x, y, button, state)
    end
end

function love.wheelmoved(x, y)
    if state.screen == "chat" then
        chat.wheelmoved(x, y, state)
    end
end

function love.resize(w, h)
    if state.screen == "chat" then
        chat.resize(w, h, state)
    end
end

-- Chat screen - 8-bit game world style
local ui = require("ui")
local http = require("http")
local json = require("json")
local ws = require("ws")

local chat = {}
local useWebSocket = false  -- set true once WS connects

-- Assets
local bgImage = nil
local heroUserSheet = nil
local heroBotSheet = nil
local userQuads = {}
local botQuads = {}

-- Game state
local inputText = ""
local inputFocused = true
local sending = false
local sendAnim = 0

-- Character state
local SPRITE_SIZE = 64
local SPRITE_SCALE = 2
local CHAR_W = SPRITE_SIZE * SPRITE_SCALE  -- 128px rendered
local WALK_SPEED = 180

local user = {
    x = 0, y = 0, targetX = 0,
    frame = 0, animTimer = 0, walking = false,
    facing = -1,
    bobTimer = 0, speaking = false, speakTimer = 0,
}
local bot = {
    x = 0, y = 0, targetX = 0,
    frame = 0, animTimer = 0, walking = false,
    facing = 1,
    bobTimer = math.pi, speaking = false, speakTimer = 0,
    thinking = false, thinkTimer = 0,
}

-- Layout zones
local gameAreaH = 0  -- top portion for game world
local logAreaY = 0   -- bottom portion for message log
local groundY = 0

-- Speech bubble (shows above character in game area)
local activeBubble = nil
local bubbleQueue = {}

-- Message log
local messageLog = {}
local logScrollY = 0
local logMaxScroll = 0
local autoScrollPending = false
local initialized = false

function chat.load(state)
    local ok, img = pcall(love.graphics.newImage, "assets/chat_bg.jpg")
    if ok then bgImage = img end

    ok, img = pcall(love.graphics.newImage, "assets/hero_user_sheet.png")
    if ok then
        heroUserSheet = img
        heroUserSheet:setFilter("nearest", "nearest")
        for i = 0, 3 do
            userQuads[i] = love.graphics.newQuad(i * SPRITE_SIZE, 0, SPRITE_SIZE, SPRITE_SIZE, heroUserSheet:getDimensions())
        end
    end

    ok, img = pcall(love.graphics.newImage, "assets/hero_bot_sheet.png")
    if ok then
        heroBotSheet = img
        heroBotSheet:setFilter("nearest", "nearest")
        for i = 0, 3 do
            botQuads[i] = love.graphics.newQuad(i * SPRITE_SIZE, 0, SPRITE_SIZE, SPRITE_SIZE, heroBotSheet:getDimensions())
        end
    end

    -- Fallback single sprites
    if not heroUserSheet then
        ok, img = pcall(love.graphics.newImage, "assets/hero_user.png")
        if ok then heroUserSheet = img; heroUserSheet:setFilter("nearest", "nearest") end
    end
    if not heroBotSheet then
        ok, img = pcall(love.graphics.newImage, "assets/hero_bot.png")
        if ok then heroBotSheet = img; heroBotSheet:setFilter("nearest", "nearest") end
    end

    messageLog = {}
    inputText = ""
    logScrollY = 0
    activeBubble = nil
    bubbleQueue = {}
    initialized = false
    useWebSocket = false

    -- Try to establish WebSocket connection
    ws.connect(state.gateway_url, {
        onConnect = function()
            useWebSocket = true
            state.ws_connected = true
            local sessionInfo = ""
            if state.session_id then
                sessionInfo = " (session: " .. state.session_id:sub(1, 16) .. "...)"
            end
            table.insert(messageLog, {
                role = "system",
                text = "WebSocket connected!" .. sessionInfo .. " Type a message to start coding.",
            })
            autoScrollPending = true
        end,
        onDisconnect = function()
            useWebSocket = false
            state.ws_connected = false
            table.insert(messageLog, {
                role = "system",
                text = "WebSocket disconnected. Falling back to HTTP REST API.",
            })
            autoScrollPending = true
        end,
        onError = function(err)
            -- Fall back to HTTP REST API
            useWebSocket = false
            state.ws_connected = false
            table.insert(messageLog, {
                role = "system",
                text = "WebSocket unavailable, using HTTP REST API.",
            })
            autoScrollPending = true
            print("WebSocket error: " .. tostring(err))
        end,
        onMessage = function(data)
            local ok2, msg = pcall(json.decode, data)
            if ok2 and type(msg) == "table" then
                if msg.type == "thinking" then
                    -- Server acknowledged, chat is processing
                    print("[WS] Server is thinking...")
                elseif msg.type == "response" then
                    sending = false
                    local W = love.graphics.getWidth()

                    bot.speaking = true
                    bot.speakTimer = 0

                    local tokenInfo = nil
                    if msg.total_tokens then
                        tokenInfo = tostring(msg.total_tokens) .. " tokens"
                        if msg.cost then
                            tokenInfo = tokenInfo .. string.format(" ($%.4f)", msg.cost)
                        end
                    end
                    if msg.session_id and not state.session_id then
                        state.session_id = msg.session_id
                    end
                    addBotMsg(msg.response or "", msg.model, tokenInfo, state)
                    returnToIdle(W)
                elseif msg.type == "error" then
                    sending = false
                    local W = love.graphics.getWidth()
                    addBotMsg("Error: " .. (msg.error or "Unknown error"), nil, nil, state)
                    returnToIdle(W)
                end
            end
        end,
    })

    local connectMsg = "Connecting..."
    if state.gateway_version then
        connectMsg = connectMsg .. " (gateway v" .. state.gateway_version .. ", " .. (state.gateway_mode or "local") .. " mode)"
    end
    table.insert(messageLog, {
        role = "system",
        text = connectMsg,
    })
end

function chat.update(dt, state)
    local W, H = love.graphics.getWidth(), love.graphics.getHeight()

    -- Poll WebSocket messages
    ws.update()

    -- Layout: top 55% = game world, bottom 45% = log + input
    gameAreaH = math.floor(H * 0.55)
    logAreaY = gameAreaH
    groundY = gameAreaH - 20  -- ground near bottom of game area

    -- Initialize character positions once
    if not initialized then
        bot.x = W * 0.3 - CHAR_W / 2
        bot.targetX = bot.x
        user.x = W * 0.7 - CHAR_W / 2
        user.targetX = user.x
        initialized = true
    end

    -- Y positions on ground
    user.y = groundY - CHAR_W
    bot.y = groundY - CHAR_W

    updateCharacter(user, dt)
    updateCharacter(bot, dt)

    if sending then
        sendAnim = sendAnim + dt
        bot.thinking = true
        bot.thinkTimer = bot.thinkTimer + dt
    else
        bot.thinking = false
        bot.thinkTimer = 0
    end

    -- Bubble animation: if a new bubble is queued, show it immediately
    if #bubbleQueue > 0 then
        activeBubble = table.remove(bubbleQueue, 1)
        -- Clear any remaining queue (show only latest)
        if #bubbleQueue > 0 then
            activeBubble = bubbleQueue[#bubbleQueue]
            bubbleQueue = {}
        end
    end

    if activeBubble then
        activeBubble.timer = activeBubble.timer + dt
        activeBubble.alpha = math.min(1, activeBubble.alpha + dt * 5)
        -- Track character position
        if activeBubble.role == "user" then
            activeBubble.anchorX = user.x + CHAR_W / 2
            activeBubble.anchorY = user.y
        else
            activeBubble.anchorX = bot.x + CHAR_W / 2
            activeBubble.anchorY = bot.y
        end
        -- Fade out after time
        if activeBubble.timer > 8 then
            activeBubble.alpha = activeBubble.alpha - dt * 3
            if activeBubble.alpha <= 0 then
                activeBubble = nil
            end
        end
    end
end

function updateCharacter(char, dt)
    local dx = char.targetX - char.x
    if math.abs(dx) > 4 then
        char.walking = true
        local dir = dx > 0 and 1 or -1
        char.facing = dir
        char.x = char.x + dir * WALK_SPEED * dt
        if (dir > 0 and char.x > char.targetX) or (dir < 0 and char.x < char.targetX) then
            char.x = char.targetX
        end
    else
        char.walking = false
    end

    if char.walking then
        char.animTimer = char.animTimer + dt
        if char.animTimer > 0.15 then
            char.animTimer = 0
            char.frame = (char.frame + 1) % 4
        end
    else
        char.frame = 0
        char.animTimer = 0
    end

    char.bobTimer = char.bobTimer + dt

    if char.speaking then
        char.speakTimer = char.speakTimer + dt
        if char.speakTimer > 0.6 then
            char.speaking = false
            char.speakTimer = 0
        end
    end
end

function chat.draw(state)
    local W, H = love.graphics.getWidth(), love.graphics.getHeight()

    -- === GAME AREA (top) ===
    love.graphics.setScissor(0, 0, W, gameAreaH)

    -- Background
    if bgImage then
        local iw, ih = bgImage:getDimensions()
        local scale = math.max(W / iw, gameAreaH / ih)
        love.graphics.setColor(1, 1, 1, 1)
        love.graphics.draw(bgImage, (W - iw * scale) / 2, (gameAreaH - ih * scale) / 2, 0, scale, scale)
    else
        love.graphics.setColor(0.15, 0.3, 0.7, 1)
        love.graphics.rectangle("fill", 0, 0, W, gameAreaH)
        love.graphics.setColor(0.2, 0.55, 0.15, 1)
        love.graphics.rectangle("fill", 0, groundY, W, gameAreaH - groundY)
    end

    -- Draw characters
    drawCharacter(bot, heroBotSheet, botQuads)
    drawCharacter(user, heroUserSheet, userQuads)

    -- Thinking dots above bot
    if bot.thinking then
        local cx = bot.x + CHAR_W / 2
        local cy = bot.y - 10
        love.graphics.setFont(ui.fonts.title)
        for i = 0, 2 do
            local dotAlpha = (i < math.floor(sendAnim * 3) % 4) and 1 or 0.25
            love.graphics.setColor(1, 1, 1, dotAlpha)
            love.graphics.print(".", cx - 18 + i * 14, cy - 15 + math.sin(sendAnim * 4 + i * 1.2) * 4)
        end
    end

    -- Speech bubble
    if activeBubble and activeBubble.alpha > 0 then
        drawSpeechBubble(activeBubble, W)
    end

    love.graphics.setScissor()

    -- === HUD HEADER (overlay on game area) ===
    love.graphics.setColor(0, 0, 0, 0.6)
    love.graphics.rectangle("fill", 0, 0, W, 36)
    love.graphics.setColor(0.4, 0.6, 1.0, 0.7)
    love.graphics.rectangle("fill", 0, 35, W, 1)

    love.graphics.setFont(ui.fonts.normal)
    love.graphics.setColor(1, 1, 1, 0.95)
    love.graphics.print("BFCode Agent", 10, 8)

    -- Connection mode indicator
    local modeLabel = useWebSocket and "WS" or "HTTP"
    local modeColor = useWebSocket and {0.3, 1.0, 0.5, 0.8} or {1.0, 0.8, 0.3, 0.8}
    love.graphics.setFont(ui.fonts.small)
    love.graphics.setColor(modeColor)
    love.graphics.print(modeLabel, 130, 10)

    -- EXIT button
    local dcW, dcH = 60, 22
    local dcX, dcY = W - 72, 7
    local dcHover = ui.isHover(dcX, dcY, dcW, dcH)
    love.graphics.setColor(dcHover and {0.9, 0.3, 0.3, 0.9} or {0.5, 0.15, 0.15, 0.7})
    love.graphics.rectangle("fill", dcX, dcY, dcW, dcH)
    love.graphics.setColor(0.8, 0.3, 0.3, 0.8)
    love.graphics.rectangle("line", dcX, dcY, dcW, dcH)
    love.graphics.setFont(ui.fonts.small)
    love.graphics.setColor(1, 1, 1, 0.9)
    local dcLabel = "EXIT"
    love.graphics.print(dcLabel, dcX + (dcW - ui.fonts.small:getWidth(dcLabel)) / 2, dcY + 3)
    chat._dcBtn = {x = dcX, y = dcY, w = dcW, h = dcH}

    -- === MESSAGE LOG (bottom panel, RPG dialog box style) ===
    local logPad = 8
    local inputTotalH = 46
    local logX = logPad
    local logY2 = logAreaY
    local logW = W - logPad * 2
    local logH = H - logAreaY - inputTotalH - logPad

    -- Log background
    love.graphics.setColor(0.02, 0.03, 0.08, 0.92)
    love.graphics.rectangle("fill", 0, logAreaY, W, H - logAreaY)

    -- Decorative border (RPG dialog box)
    love.graphics.setColor(0.35, 0.55, 0.9, 0.7)
    love.graphics.setLineWidth(2)
    love.graphics.rectangle("line", logX, logY2 + 2, logW, logH)
    -- Corner dots
    local cd = 3
    love.graphics.setColor(0.5, 0.7, 1.0, 0.9)
    love.graphics.rectangle("fill", logX - 1, logY2 + 1, cd, cd)
    love.graphics.rectangle("fill", logX + logW - cd + 1, logY2 + 1, cd, cd)
    love.graphics.rectangle("fill", logX - 1, logY2 + logH - cd + 2, cd, cd)
    love.graphics.rectangle("fill", logX + logW - cd + 1, logY2 + logH - cd + 2, cd, cd)

    -- Draw log entries
    love.graphics.setScissor(logX + 4, logY2 + 6, logW - 12, logH - 8)
    love.graphics.setFont(ui.fonts.normal)
    local ly = logY2 + 8 - logScrollY
    local lineH = ui.fonts.normal:getHeight()
    local wrapW = logW - 28

    for _, msg in ipairs(messageLog) do
        local safeText = ui.sanitize(msg.text)
        local _, lines = ui.fonts.normal:getWrap(safeText, wrapW)

        if msg.role == "user" then
            love.graphics.setColor(0.3, 1.0, 0.3, 0.95)
            local prefix = "> "
            for li, line in ipairs(lines) do
                local p = li == 1 and prefix or "  "
                love.graphics.print(p .. line, logX + 10, ly)
                ly = ly + lineH
            end
        elseif msg.role == "assistant" then
            love.graphics.setColor(0.65, 0.82, 1.0, 0.95)
            local prefix = "* "
            for li, line in ipairs(lines) do
                local p = li == 1 and prefix or "  "
                love.graphics.print(p .. line, logX + 10, ly)
                ly = ly + lineH
            end
            if msg.tokens then
                love.graphics.setFont(ui.fonts.small)
                love.graphics.setColor(0.4, 0.45, 0.55, 0.6)
                love.graphics.print("  [" .. ui.sanitize(msg.tokens) .. "]", logX + 10, ly)
                ly = ly + ui.fonts.small:getHeight()
                love.graphics.setFont(ui.fonts.normal)
            end
        else
            love.graphics.setColor(0.5, 0.5, 0.45, 0.6)
            for li, line in ipairs(lines) do
                love.graphics.print(line, logX + 10, ly)
                ly = ly + lineH
            end
        end
        ly = ly + 4
    end

    local totalLogH = ly + logScrollY - logY2 - 8
    logMaxScroll = math.max(0, totalLogH - (logH - 8))

    -- Auto-scroll to bottom when new messages arrive
    if autoScrollPending then
        logScrollY = logMaxScroll
        autoScrollPending = false
    end

    love.graphics.setScissor()

    -- Scrollbar
    if logMaxScroll > 0 and totalLogH > 0 then
        local visH = logH - 8
        local sbH = math.max(20, visH * (visH / totalLogH))
        local sbY = logY2 + 4 + (logScrollY / logMaxScroll) * (visH - sbH)
        love.graphics.setColor(0.4, 0.6, 1.0, 0.35)
        love.graphics.rectangle("fill", logX + logW - 6, sbY, 3, sbH, 1, 1)
    end

    -- === INPUT BAR ===
    local inputBarY = H - inputTotalH - 2
    local inputFieldX = logPad + 4
    local inputFieldW = W - logPad * 2 - 96
    local inputFieldH = 32
    local inputFieldY = inputBarY + 7

    -- Input background
    love.graphics.setColor(0.04, 0.06, 0.12, 0.95)
    love.graphics.rectangle("fill", inputFieldX, inputFieldY, inputFieldW, inputFieldH)
    love.graphics.setColor(0.35, 0.55, 0.9, inputFocused and 0.8 or 0.35)
    love.graphics.setLineWidth(1)
    love.graphics.rectangle("line", inputFieldX, inputFieldY, inputFieldW, inputFieldH)

    -- Input text
    love.graphics.setFont(ui.fonts.normal)
    love.graphics.setScissor(inputFieldX + 4, inputFieldY + 2, inputFieldW - 8, inputFieldH - 4)
    if #inputText == 0 then
        love.graphics.setColor(0.35, 0.4, 0.5, 0.45)
        love.graphics.print("Type a message...", inputFieldX + 8, inputFieldY + 7)
    else
        love.graphics.setColor(0.9, 0.95, 1.0, 0.95)
        local safeInput = ui.sanitize(inputText)
        local tw = ui.fonts.normal:getWidth(safeInput)
        local maxTW = inputFieldW - 16
        local tx = inputFieldX + 8
        if tw > maxTW then tx = tx - (tw - maxTW) end
        love.graphics.print(safeInput, tx, inputFieldY + 7)
    end
    if inputFocused and math.floor(love.timer.getTime() * 3) % 2 == 0 then
        local safeInput = ui.sanitize(inputText)
        local curX = inputFieldX + 8 + ui.fonts.normal:getWidth(safeInput)
        if curX > inputFieldX + inputFieldW - 12 then curX = inputFieldX + inputFieldW - 12 end
        love.graphics.setColor(0.5, 0.8, 1.0, 1)
        love.graphics.rectangle("fill", curX, inputFieldY + 5, 2, inputFieldH - 10)
    end
    love.graphics.setScissor()
    chat._inputBox = {x = inputFieldX, y = inputFieldY, w = inputFieldW, h = inputFieldH}

    -- Send button
    local sendW, sendH = 80, inputFieldH
    local sendX = inputFieldX + inputFieldW + 8
    local sendY = inputFieldY
    local sendHover = ui.isHover(sendX, sendY, sendW, sendH)
    local canSend = not sending and #inputText > 0

    love.graphics.setColor(canSend and (sendHover and {0.25, 0.6, 1.0, 0.9} or {0.15, 0.4, 0.8, 0.75}) or {0.15, 0.15, 0.2, 0.4})
    love.graphics.rectangle("fill", sendX, sendY, sendW, sendH)
    love.graphics.setColor(0.35, 0.55, 0.9, 0.6)
    love.graphics.rectangle("line", sendX, sendY, sendW, sendH)
    love.graphics.setFont(ui.fonts.normal)
    love.graphics.setColor(1, 1, 1, canSend and 1 or 0.3)
    local sl = sending and "..." or "SEND"
    love.graphics.print(sl, sendX + (sendW - ui.fonts.normal:getWidth(sl)) / 2, sendY + 7)
    chat._sendBtn = {x = sendX, y = sendY, w = sendW, h = sendH}
end

function drawCharacter(char, sheet, quads)
    if not sheet then return end

    local drawY = char.y
    if not char.walking then
        drawY = drawY + math.sin(char.bobTimer * 2) * 2
    end
    if char.speaking then
        drawY = drawY - math.abs(math.sin(char.speakTimer * 12)) * 8
    end
    if char.thinking then
        drawY = drawY + math.sin(char.thinkTimer * 3) * 3
    end

    -- Shadow
    love.graphics.setColor(0, 0, 0, 0.25)
    love.graphics.ellipse("fill", char.x + CHAR_W / 2, char.y + CHAR_W - 2, 30, 8)

    love.graphics.setColor(1, 1, 1, 1)
    if quads[0] then
        local sx = char.facing < 0 and -SPRITE_SCALE or SPRITE_SCALE
        local ox = char.facing < 0 and SPRITE_SIZE or 0
        love.graphics.draw(sheet, quads[char.frame], char.x + (char.facing < 0 and CHAR_W or 0), drawY, 0, sx, SPRITE_SCALE, ox, 0)
    else
        love.graphics.draw(sheet, char.x, drawY, 0, SPRITE_SCALE, SPRITE_SCALE)
    end
end

function drawSpeechBubble(bubble, W)
    if not bubble or not bubble.anchorX then return end

    local font = ui.fonts.large
    love.graphics.setFont(font)
    local safeText = ui.sanitize(bubble.text)
    -- Truncate for bubble display (full text is in message log)
    if #safeText > 300 then
        safeText = safeText:sub(1, 297) .. "..."
    end

    local maxBubbleW = math.min(W * 0.65, 500)
    local minBubbleW = 120
    local pad = 16
    local _, lines = font:getWrap(safeText, maxBubbleW - pad * 2)

    -- Limit bubble height
    local maxLines = 5
    if #lines > maxLines then
        local trimmed = {}
        for i = 1, maxLines do trimmed[i] = lines[i] end
        trimmed[maxLines] = trimmed[maxLines] .. "..."
        lines = trimmed
    end

    local lineH = font:getHeight()
    local textH = #lines * (lineH + 2)
    local bubbleW = pad * 2
    for _, line in ipairs(lines) do
        bubbleW = math.max(bubbleW, font:getWidth(line) + pad * 2)
    end
    bubbleW = math.max(minBubbleW, math.min(bubbleW, maxBubbleW))
    local bubbleH = textH + pad * 2

    -- Position bubble directly above character head
    local bx = bubble.anchorX - bubbleW / 2
    local by = bubble.anchorY - bubbleH - 8

    -- Clamp to screen
    bx = math.max(8, math.min(bx, W - bubbleW - 8))
    by = math.max(8, by)

    local a = bubble.alpha

    -- Shadow
    love.graphics.setColor(0, 0, 0, 0.3 * a)
    love.graphics.rectangle("fill", bx + 3, by + 3, bubbleW, bubbleH, 8, 8)

    -- Background
    love.graphics.setColor(1, 1, 1, 0.95 * a)
    love.graphics.rectangle("fill", bx, by, bubbleW, bubbleH, 8, 8)

    -- Border (thicker, colored by role)
    local bc = bubble.role == "user" and {0.1, 0.6, 0.2} or {0.2, 0.4, 0.85}
    love.graphics.setColor(bc[1], bc[2], bc[3], 0.9 * a)
    love.graphics.setLineWidth(3)
    love.graphics.rectangle("line", bx, by, bubbleW, bubbleH, 8, 8)

    -- Role label
    love.graphics.setFont(ui.fonts.small)
    local label = bubble.role == "user" and "YOU" or "BOT"
    love.graphics.setColor(bc[1], bc[2], bc[3], 0.7 * a)
    love.graphics.print(label, bx + pad, by + 4)

    -- Text
    love.graphics.setFont(font)
    love.graphics.setColor(0.05, 0.05, 0.1, a)
    for li, line in ipairs(lines) do
        love.graphics.print(line, bx + pad, by + pad + 6 + (li - 1) * (lineH + 2))
    end
end

function chat.textinput(t, state)
    if inputFocused and not sending then
        inputText = inputText .. t
    end
end

function chat.keypressed(key, state)
    if key == "return" and not love.keyboard.isDown("lshift", "rshift") then
        chat.sendMessage(state)
    elseif key == "backspace" then
        if love.keyboard.isDown("lgui", "rgui") then
            inputText = ""
        else
            inputText = inputText:sub(1, -2)
        end
    elseif key == "v" and love.keyboard.isDown("lgui", "rgui") then
        local clip = love.system.getClipboardText() or ""
        clip = clip:gsub("[\r\n]", " ")
        inputText = inputText .. clip
    end
end

function chat.mousepressed(x, y, button, state)
    if button ~= 1 then return end

    local s = chat._sendBtn
    if s and x >= s.x and x <= s.x + s.w and y >= s.y and y <= s.y + s.h then
        chat.sendMessage(state)
        return
    end

    local d = chat._dcBtn
    if d and x >= d.x and x <= d.x + d.w and y >= d.y and y <= d.y + d.h then
        ws.disconnect()
        useWebSocket = false
        state.screen = "login"
        state.connected = false
        state.ws_connected = false
        state.session_id = nil
        state.session_user = nil
        state.error_msg = nil
        state.gateway_mode = nil
        state.gateway_version = nil
        state.gateway_sessions = nil
        messageLog = {}
        initialized = false
        return
    end

    local ib = chat._inputBox
    if ib then
        inputFocused = (x >= ib.x and x <= ib.x + ib.w and y >= ib.y and y <= ib.y + ib.h)
    end
end

function chat.wheelmoved(x, y, state)
    logScrollY = logScrollY - y * 30
    logScrollY = math.max(0, math.min(logScrollY, logMaxScroll))
end

function chat.resize(w, h, state)
    initialized = false  -- recalculate positions
end

function chat.sendMessage(state)
    if sending or #inputText == 0 then return end

    local W = love.graphics.getWidth()
    local msgText = inputText
    inputText = ""

    -- User walks toward center, faces bot
    user.targetX = W * 0.55
    user.facing = -1
    user.speaking = true
    user.speakTimer = 0

    -- Show user speech bubble (immediately replace any existing)
    activeBubble = {
        text = msgText,
        anchorX = user.x + CHAR_W / 2,
        anchorY = user.y,
        alpha = 0, role = "user", timer = 0,
    }
    bubbleQueue = {}

    table.insert(messageLog, { role = "user", text = msgText })
    autoScrollPending = true

    sending = true
    sendAnim = 0

    -- Bot walks toward user
    bot.targetX = W * 0.25
    bot.facing = 1

    if useWebSocket and ws.isConnected() then
        -- Send via WebSocket (persistent connection, single session)
        local wsMsg = {
            type = "chat",
            message = msgText,
        }
        if state.session_id then
            wsMsg.session_id = state.session_id
        end
        print("[WS] Sending chat, session_id=" .. tostring(state.session_id))
        ws.sendJson(wsMsg)
    else
        -- Fallback to HTTP
        local url = state.gateway_url
        local headers = {["Content-Type"] = "application/json"}
        if state.api_key and #state.api_key > 0 then
            headers["Authorization"] = "Bearer " .. state.api_key
        end

        local body = json.encode({
            message = msgText,
            session_id = state.session_id,
        })

        http.post(url .. "/v1/chat", body, headers, function(resp)
            sending = false
            bot.speaking = true
            bot.speakTimer = 0

            if not resp.success then
                addBotMsg("Error: " .. (resp.error or "Connection failed"), nil, nil, state)
                returnToIdle(W)
                return
            end

            local statusCode = tonumber(resp.data.status) or 0
            if statusCode ~= 200 then
                local errMsg = "Error (HTTP " .. tostring(statusCode) .. ")"
                local rawBody = (resp.data.body or ""):gsub("^%s+", ""):gsub("%s+$", "")
                local ok2, errData = pcall(json.decode, rawBody)
                if ok2 and type(errData) == "table" and errData.error then
                    errMsg = errData.error
                end
                addBotMsg(errMsg, nil, nil, state)
                returnToIdle(W)
                return
            end

            local rawBody = (resp.data.body or ""):gsub("^%s+", ""):gsub("%s+$", "")
            if rawBody:sub(1, 3) == "\xEF\xBB\xBF" then rawBody = rawBody:sub(4) end

            local ok2, data = pcall(json.decode, rawBody)
            if ok2 and type(data) == "table" and data.response then
                local tokenInfo = nil
                if data.total_tokens then
                    tokenInfo = tostring(data.total_tokens) .. " tokens"
                    if data.cost then
                        tokenInfo = tokenInfo .. string.format(" ($%.4f)", data.cost)
                    end
                end
                if data.session_id and not state.session_id then
                    state.session_id = data.session_id
                end
                addBotMsg(data.response, data.model, tokenInfo, state)
            else
                local extracted = rawBody:match('"response"%s*:%s*"(.-)"')
                if extracted then
                    extracted = extracted:gsub('\\"', '"'):gsub('\\n', '\n'):gsub('\\\\', '\\')
                    addBotMsg(extracted, nil, nil, state)
                else
                    addBotMsg(rawBody, nil, nil, state)
                end
            end

            returnToIdle(W)
        end)
    end
end

function returnToIdle(W)
    user.targetX = W * 0.65
    user.facing = -1
    bot.targetX = W * 0.15
    bot.facing = 1
end

function addBotMsg(text, model, tokenInfo, state)
    -- Show bot speech bubble (immediately replace any existing)
    activeBubble = {
        text = text,
        anchorX = bot.x + CHAR_W / 2,
        anchorY = bot.y,
        alpha = 0, role = "assistant", timer = 0,
    }
    bubbleQueue = {}

    table.insert(messageLog, {
        role = "assistant", text = text,
        model = model, tokens = tokenInfo,
    })
    autoScrollPending = true
end

return chat

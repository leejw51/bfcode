-- Login screen - The Never Ending Coding
local ui = require("ui")
local http = require("http")
local json = require("json")

local login = {}
local SPRITE_SIZE = 64

local titleBg = nil
local titleLogo = nil
local bannerImg = nil
local heroUser = nil
local heroBot = nil

local fields = {
    { key = "gateway_url", label = "GATEWAY URL",  placeholder = "http://127.0.0.1:8642", password = false },
    { key = "api_key",     label = "API KEY",       placeholder = "Enter your API key (optional)", password = true },
}
local focusedField = 1
local connecting = false
local connectAnim = 0
local timer = 0
local particles = {}
local stars = {}

-- Fade-in timing
local PANEL_DELAY = 3.0      -- seconds before panel appears
local PANEL_FADE_DUR = 2.0   -- seconds for fade-in animation

-- Easing: smooth ease-out cubic
local function easeOutCubic(t)
    t = math.max(0, math.min(1, t))
    return 1 - (1 - t) ^ 3
end

local function initEffects()
    particles = {}
    for i = 1, 40 do
        table.insert(particles, {
            x = math.random() * 900,
            y = math.random() * 650,
            size = math.random() * 2.5 + 0.5,
            speed = math.random() * 20 + 8,
            phase = math.random() * math.pi * 2,
            color = math.random() < 0.5 and "green" or "gold",
        })
    end
    stars = {}
    for i = 1, 60 do
        table.insert(stars, {
            x = math.random() * 900,
            y = math.random() * 650,
            size = math.random() * 1.5 + 0.3,
            twinkle = math.random() * math.pi * 2,
            speed = math.random() * 2 + 1,
        })
    end
end

function login.load(state)
    local ok, img

    ok, img = pcall(love.graphics.newImage, "assets/title_bg.jpg")
    if ok then titleBg = img end

    ok, img = pcall(love.graphics.newImage, "assets/title_logo.png")
    if ok then titleLogo = img; titleLogo:setFilter("nearest", "nearest") end

    ok, img = pcall(love.graphics.newImage, "assets/banner.jpg")
    if ok then bannerImg = img end

    ok, img = pcall(love.graphics.newImage, "assets/hero_user.png")
    if ok then heroUser = img; heroUser:setFilter("nearest", "nearest") end

    ok, img = pcall(love.graphics.newImage, "assets/hero_bot.png")
    if ok then heroBot = img; heroBot:setFilter("nearest", "nearest") end

    initEffects()
end

function login.update(dt, state)
    timer = timer + dt
    if connecting then connectAnim = connectAnim + dt end

    local W, H = love.graphics.getWidth(), love.graphics.getHeight()
    for _, p in ipairs(particles) do
        p.y = p.y - p.speed * dt
        if p.y < -10 then
            p.y = H + 10
            p.x = math.random() * W
        end
    end
end

function login.draw(state)
    local W, H = love.graphics.getWidth(), love.graphics.getHeight()

    -- Full-screen background
    if titleBg then
        local iw, ih = titleBg:getDimensions()
        local scale = math.max(W / iw, H / ih)
        love.graphics.setColor(1, 1, 1, 1)
        love.graphics.draw(titleBg, (W - iw * scale) / 2, (H - ih * scale) / 2, 0, scale, scale)
        -- Slight darkening
        love.graphics.setColor(0, 0, 0, 0.3)
        love.graphics.rectangle("fill", 0, 0, W, H)
    else
        love.graphics.setColor(0.05, 0.07, 0.15, 1)
        love.graphics.rectangle("fill", 0, 0, W, H)
    end

    -- Twinkling stars
    for _, s in ipairs(stars) do
        local a = 0.3 + 0.7 * math.abs(math.sin(timer * s.speed + s.twinkle))
        love.graphics.setColor(1, 1, 0.85, a * 0.6)
        love.graphics.circle("fill", s.x % W, s.y % H, s.size)
    end

    -- Floating particles (green/gold sparks)
    for _, p in ipairs(particles) do
        local a = 0.2 + 0.5 * math.abs(math.sin(timer * 2.5 + p.phase))
        if p.color == "green" then
            love.graphics.setColor(0.2, 0.9, 0.4, a)
        else
            love.graphics.setColor(1.0, 0.85, 0.3, a)
        end
        love.graphics.circle("fill", p.x, p.y, p.size)
    end

    local cx = W / 2

    -- === TITLE LOGO - drops in from top after 1s ===
    local LOGO_DELAY = 1.0
    local LOGO_FADE_DUR = 2.0
    local logoFadeT = (timer - LOGO_DELAY) / LOGO_FADE_DUR
    local logoAlpha = easeOutCubic(logoFadeT)

    -- Starts off-screen above, drops to final position
    local logoRestY = 15
    local logoStartY = -200
    local logoY = logoStartY + (logoRestY - logoStartY) * logoAlpha

    local logoBottomY = 20
    if titleLogo and timer > LOGO_DELAY then
        local lw, lh = titleLogo:getDimensions()
        local logoMaxW = math.min(W * 0.55, 450)
        local logoScale = logoMaxW / lw
        local logoH = lh * logoScale
        local lx = cx - logoMaxW / 2
        local floatY = 0
        if logoAlpha >= 1 then
            floatY = math.sin(timer * 1.2) * 3
        end
        local pulse = 0.85 + 0.15 * math.sin(timer * 1.8)
        love.graphics.setColor(pulse, pulse, pulse, logoAlpha)
        love.graphics.draw(titleLogo, lx, logoY + floatY, 0, logoScale, logoScale)
        logoBottomY = logoRestY + logoH + 10
    end

    -- Subtitle - fades in after logo lands (delay 2.5s)
    local subFadeT = (timer - 2.5) / 1.0
    local subAlpha = easeOutCubic(subFadeT)
    love.graphics.setFont(ui.fonts.small)
    local sub = "- A Coding Agent Adventure -"
    local sw = ui.fonts.small:getWidth(sub)
    local subGlow = 0.6 + 0.3 * math.sin(timer * 2)
    love.graphics.setColor(0.6, 0.85, 0.5, subGlow * subAlpha)
    love.graphics.print(sub, cx - sw / 2, logoBottomY + (1 - subAlpha) * 15)
    logoBottomY = logoBottomY + 22

    -- === PANEL (below title) - fades in after delay ===
    local panelFadeT = (timer - PANEL_DELAY) / PANEL_FADE_DUR
    local panelAlpha = easeOutCubic(panelFadeT)

    -- Don't draw panel at all before delay
    if timer < PANEL_DELAY then
        -- Show "PRESS START" hint instead
        if math.floor(timer * 1.5) % 2 == 0 then
            love.graphics.setFont(ui.fonts.normal)
            love.graphics.setColor(0.8, 0.85, 0.5, 0.7)
            local hint = "PRESS ENTER TO CONNECT"
            love.graphics.print(hint, cx - ui.fonts.normal:getWidth(hint) / 2, H - 50)
        end
        return
    end

    local panelW = math.min(400, W - 80)
    local panelH = 230
    local px = (W - panelW) / 2
    local py = logoBottomY + 5
    if py + panelH > H - 50 then py = H - panelH - 50 end

    -- Slide up effect: panel starts 30px lower and rises into place
    local slideOffset = (1 - panelAlpha) * 30
    py = py + slideOffset

    -- === LOGIN PANEL with banner frame ===
    if bannerImg then
        local bw, bh = bannerImg:getDimensions()
        local bscaleX = (panelW + 20) / bw
        local bscaleY = (panelH + 20) / bh
        love.graphics.setColor(1, 1, 1, 0.9 * panelAlpha)
        love.graphics.draw(bannerImg, px - 10, py - 10, 0, bscaleX, bscaleY)
    end

    -- Dark inner area
    love.graphics.setColor(0.03, 0.04, 0.08, 0.88 * panelAlpha)
    love.graphics.rectangle("fill", px + 8, py + 8, panelW - 16, panelH - 16, 4, 4)

    -- Panel content
    local contentX = px + 30
    local contentW = panelW - 60
    local cy = py + 20

    -- Input fields
    local fieldH = 34

    for i, field in ipairs(fields) do
        love.graphics.setFont(ui.fonts.small)
        love.graphics.setColor(0.5, 0.75, 0.45, 0.9 * panelAlpha)
        love.graphics.print(field.label, contentX, cy)
        cy = cy + 16

        local text = state[field.key] or ""
        local focused = (focusedField == i)

        if field.password then
            ui.drawPasswordBox(contentX, cy, contentW, fieldH, text, focused, field.placeholder)
        else
            ui.drawInputBox(contentX, cy, contentW, fieldH, text, focused, field.placeholder)
        end

        field._x, field._y, field._w, field._h = contentX, cy, contentW, fieldH
        cy = cy + fieldH + 12
    end

    -- Connect button
    local btnW = contentW
    local btnH = 40
    local btnX = contentX
    local btnY = cy + 8
    local btnHover = ui.isHover(btnX, btnY, btnW, btnH)

    -- Button glow
    if btnHover and not connecting then
        love.graphics.setColor(0.2, 0.8, 0.3, 0.15 * panelAlpha)
        ui.drawRoundRect("fill", btnX - 3, btnY - 3, btnW + 6, btnH + 6, 8)
    end

    -- Custom green-themed button
    if connecting then
        love.graphics.setColor(0.15, 0.3, 0.15, 0.8 * panelAlpha)
    elseif btnHover then
        love.graphics.setColor(0.15, 0.7, 0.25, 0.9 * panelAlpha)
    else
        love.graphics.setColor(0.1, 0.5, 0.2, 0.8 * panelAlpha)
    end
    ui.drawRoundRect("fill", btnX, btnY, btnW, btnH, 5)
    love.graphics.setColor(0.3, 0.8, 0.4, 0.7 * panelAlpha)
    love.graphics.setLineWidth(1)
    ui.drawRoundRect("line", btnX, btnY, btnW, btnH, 5)

    love.graphics.setFont(ui.fonts.normal)
    local btnLabel = connecting and ("CONNECTING" .. string.rep(".", math.floor(connectAnim * 3) % 4)) or "START ADVENTURE"
    local blw = ui.fonts.normal:getWidth(btnLabel)
    love.graphics.setColor(1, 1, 1, (connecting and 0.6 or 1) * panelAlpha)
    love.graphics.print(btnLabel, btnX + (btnW - blw) / 2, btnY + (btnH - ui.fonts.normal:getHeight()) / 2)

    login._btn = {x = btnX, y = btnY, w = btnW, h = btnH}

    -- Error message
    if state.error_msg then
        love.graphics.setFont(ui.fonts.small)
        love.graphics.setColor(1, 0.3, 0.3, 0.95 * panelAlpha)
        local errText = ui.sanitize(state.error_msg)
        local ew = ui.fonts.small:getWidth(errText)
        love.graphics.print(errText, cx - ew / 2, btnY + btnH + 8)
    end

    -- "PRESS START" blinking text at bottom
    local blinkAlpha = (0.4 + 0.6 * math.abs(math.sin(timer * 2.5))) * panelAlpha
    love.graphics.setFont(ui.fonts.normal)
    love.graphics.setColor(0.8, 0.85, 0.5, blinkAlpha)
    local pressStart = "PRESS ENTER TO CONNECT"
    local psw = ui.fonts.normal:getWidth(pressStart)
    love.graphics.print(pressStart, cx - psw / 2, H - 35)

    -- Version
    love.graphics.setFont(ui.fonts.small)
    love.graphics.setColor(0.3, 0.35, 0.4, 0.5)
    love.graphics.print("v0.1.0", 8, H - 18)
end

function login.textinput(t, state)
    if connecting or timer < PANEL_DELAY then return end
    local field = fields[focusedField]
    if field then
        state[field.key] = (state[field.key] or "") .. t
    end
end

function login.keypressed(key, state)
    if connecting or timer < PANEL_DELAY then return end

    if key == "tab" then
        if love.keyboard.isDown("lshift", "rshift") then
            focusedField = focusedField - 1
            if focusedField < 1 then focusedField = #fields end
        else
            focusedField = focusedField + 1
            if focusedField > #fields then focusedField = 1 end
        end
    elseif key == "backspace" then
        local field = fields[focusedField]
        if field then
            local text = state[field.key] or ""
            if love.keyboard.isDown("lgui", "rgui") then
                state[field.key] = ""
            else
                state[field.key] = text:sub(1, -2)
            end
        end
    elseif key == "return" then
        login.doConnect(state)
    elseif key == "v" and love.keyboard.isDown("lgui", "rgui") then
        local field = fields[focusedField]
        if field then
            local clip = love.system.getClipboardText() or ""
            clip = clip:gsub("[\r\n]", "")
            state[field.key] = (state[field.key] or "") .. clip
        end
    end
end

function login.mousepressed(x, y, button, state)
    if button ~= 1 or timer < PANEL_DELAY then return end

    for i, field in ipairs(fields) do
        if field._x and x >= field._x and x <= field._x + field._w
           and y >= field._y and y <= field._y + field._h then
            focusedField = i
            return
        end
    end

    local b = login._btn
    if b and x >= b.x and x <= b.x + b.w and y >= b.y and y <= b.y + b.h then
        if not connecting then
            login.doConnect(state)
        end
    end
end

function login.doConnect(state)
    if connecting then return end

    local url = state.gateway_url
    if not url or #url == 0 then
        state.error_msg = "Please enter a gateway URL"
        return
    end

    url = url:gsub("/+$", "")
    state.gateway_url = url

    connecting = true
    connectAnim = 0
    state.error_msg = nil

    local headers = {}
    if state.api_key and #state.api_key > 0 then
        headers["Authorization"] = "Bearer " .. state.api_key
    end

    http.get(url .. "/v1/health", headers, function(resp)
        connecting = false
        if not resp.success then
            state.error_msg = "Connection failed: " .. (resp.error or "unknown error")
            return
        end

        local statusCode = tonumber(resp.data.status) or 0
        if statusCode == 401 then
            state.error_msg = "Invalid API key"
            return
        end

        if statusCode ~= 200 then
            state.error_msg = "Server error (HTTP " .. tostring(resp.data.status) .. ")"
            return
        end

        local ok, data = pcall(json.decode, resp.data.body)
        if ok and data and data.status == "ok" then
            state.connected = true
            state.session_id = nil
            state.screen = "chat"
        else
            state.error_msg = "Unexpected server response"
        end
    end)
end

return login

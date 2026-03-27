-- UI utilities and theming
local ui = {}

-- Strip invalid UTF-8 bytes to prevent Love2D crashes
function ui.sanitize(str)
    if type(str) ~= "string" then return "" end
    -- Replace any non-ASCII bytes that aren't valid UTF-8 with '?'
    local result = {}
    local i = 1
    local len = #str
    while i <= len do
        local b = str:byte(i)
        if b < 128 then
            -- ASCII
            table.insert(result, str:sub(i, i))
            i = i + 1
        elseif b >= 194 and b <= 223 then
            -- 2-byte sequence
            if i + 1 <= len then
                local b2 = str:byte(i + 1)
                if b2 >= 128 and b2 <= 191 then
                    table.insert(result, str:sub(i, i + 1))
                    i = i + 2
                else
                    i = i + 1
                end
            else
                i = i + 1
            end
        elseif b >= 224 and b <= 239 then
            -- 3-byte sequence
            if i + 2 <= len then
                local b2, b3 = str:byte(i + 1), str:byte(i + 2)
                if b2 >= 128 and b2 <= 191 and b3 >= 128 and b3 <= 191 then
                    table.insert(result, str:sub(i, i + 2))
                    i = i + 3
                else
                    i = i + 1
                end
            else
                i = i + 1
            end
        elseif b >= 240 and b <= 244 then
            -- 4-byte sequence
            if i + 3 <= len then
                local b2, b3, b4 = str:byte(i + 1), str:byte(i + 2), str:byte(i + 3)
                if b2 >= 128 and b2 <= 191 and b3 >= 128 and b3 <= 191 and b4 >= 128 and b4 <= 191 then
                    table.insert(result, str:sub(i, i + 3))
                    i = i + 4
                else
                    i = i + 1
                end
            else
                i = i + 1
            end
        else
            -- Invalid leading byte, skip
            i = i + 1
        end
    end
    -- Strip non-ASCII unicode that the font likely can't render (emoji, symbols)
    local filtered = {}
    local s = table.concat(result)
    local j = 1
    local slen = #s
    while j <= slen do
        local byte = s:byte(j)
        if byte < 128 then
            table.insert(filtered, s:sub(j, j))
            j = j + 1
        elseif byte >= 194 and byte <= 223 then
            -- 2-byte: keep common Latin/accented chars (U+0080-U+07FF)
            table.insert(filtered, s:sub(j, j + 1))
            j = j + 2
        elseif byte >= 224 and byte <= 239 then
            -- 3-byte: skip most (emoji, CJK, symbols above U+2000)
            j = j + 3
        elseif byte >= 240 and byte <= 244 then
            -- 4-byte: always emoji/supplementary, skip
            j = j + 4
        else
            j = j + 1
        end
    end
    return table.concat(filtered)
end

-- Color palette (dark theme)
ui.colors = {
    bg         = {0.11, 0.12, 0.14, 1},
    panel      = {0.15, 0.16, 0.19, 1},
    panel_alt  = {0.18, 0.20, 0.23, 1},
    input_bg   = {0.08, 0.09, 0.10, 1},
    input_focus= {0.10, 0.12, 0.16, 1},
    border     = {0.25, 0.27, 0.30, 1},
    border_focus={0.35, 0.55, 0.95, 1},
    text       = {0.90, 0.91, 0.92, 1},
    text_dim   = {0.55, 0.57, 0.60, 1},
    text_label = {0.70, 0.72, 0.75, 1},
    accent     = {0.35, 0.55, 0.95, 1},
    accent_hover={0.45, 0.65, 1.0, 1},
    success    = {0.30, 0.75, 0.45, 1},
    error      = {0.90, 0.35, 0.35, 1},
    user_msg   = {0.20, 0.22, 0.28, 1},
    bot_msg    = {0.15, 0.16, 0.19, 1},
    scrollbar  = {0.30, 0.32, 0.35, 0.6},
}

ui.fonts = {}

function ui.load()
    local fontPath = "assets/myfont.ttf"
    ui.fonts.normal = love.graphics.newFont(fontPath, 18)
    ui.fonts.small = love.graphics.newFont(fontPath, 15)
    ui.fonts.large = love.graphics.newFont(fontPath, 22)
    ui.fonts.title = love.graphics.newFont(fontPath, 34)
    ui.fonts.mono = love.graphics.newFont(fontPath, 16)
end

function ui.drawBackground()
    love.graphics.setColor(ui.colors.bg)
    love.graphics.rectangle("fill", 0, 0, love.graphics.getWidth(), love.graphics.getHeight())
end

function ui.drawRoundRect(mode, x, y, w, h, r)
    r = r or 6
    love.graphics.rectangle(mode, x, y, w, h, r, r)
end

function ui.drawInputBox(x, y, w, h, text, focused, placeholder)
    local c = focused and ui.colors.input_focus or ui.colors.input_bg
    love.graphics.setColor(c)
    ui.drawRoundRect("fill", x, y, w, h, 5)

    local bc = focused and ui.colors.border_focus or ui.colors.border
    love.graphics.setColor(bc)
    love.graphics.setLineWidth(focused and 2 or 1)
    ui.drawRoundRect("line", x, y, w, h, 5)

    love.graphics.setFont(ui.fonts.normal)
    local padding = 12
    love.graphics.setScissor(x + padding, y, w - padding * 2, h)

    if #text == 0 and not focused then
        love.graphics.setColor(ui.colors.text_dim)
        love.graphics.print(placeholder or "", x + padding, y + (h - ui.fonts.normal:getHeight()) / 2)
    else
        love.graphics.setColor(ui.colors.text)
        local displayText = ui.sanitize(text)
        -- Scroll text if too wide
        local tw = ui.fonts.normal:getWidth(displayText)
        local maxW = w - padding * 2
        local tx = x + padding
        if tw > maxW then
            tx = x + padding - (tw - maxW)
        end
        love.graphics.print(displayText, tx, y + (h - ui.fonts.normal:getHeight()) / 2)

        -- Cursor
        if focused then
            local cursorX = tx + tw
            if cursorX > x + w - padding then cursorX = x + w - padding end
            local blink = math.floor(love.timer.getTime() * 2) % 2
            if blink == 0 then
                love.graphics.setColor(ui.colors.accent)
                love.graphics.rectangle("fill", cursorX, y + 6, 2, h - 12)
            end
        end
    end

    love.graphics.setScissor()
end

function ui.drawPasswordBox(x, y, w, h, text, focused, placeholder)
    local masked = string.rep("*", #text)
    ui.drawInputBox(x, y, w, h, masked, focused, placeholder)
end

function ui.drawButton(x, y, w, h, label, hovered, disabled)
    if disabled then
        love.graphics.setColor(0.2, 0.22, 0.25, 1)
    elseif hovered then
        love.graphics.setColor(ui.colors.accent_hover)
    else
        love.graphics.setColor(ui.colors.accent)
    end
    ui.drawRoundRect("fill", x, y, w, h, 5)

    love.graphics.setFont(ui.fonts.normal)
    local tw = ui.fonts.normal:getWidth(label)
    local tc = disabled and ui.colors.text_dim or ui.colors.text
    love.graphics.setColor(tc)
    love.graphics.print(label, x + (w - tw) / 2, y + (h - ui.fonts.normal:getHeight()) / 2)
end

function ui.isHover(x, y, w, h)
    local mx, my = love.mouse.getPosition()
    return mx >= x and mx <= x + w and my >= y and my <= y + h
end

return ui

local function push(spans, text, color, bold)
    if text == nil or text == "" then
        return
    end
    table.insert(spans, {
        text = text,
        color = color,
        bold = bold == true,
    })
end

local function sep(spans)
    if #spans > 0 then
        table.insert(spans, { text = " · ", color = "gray" })
    end
end

local function health_color(pct)
    if pct == nil then
        return "yellow"
    end

    if pct >= 70 then
        return "green"
    elseif pct >= 35 then
        return "yellow"
    else
        return "red"
    end
end

local function is_critical_health(pct)
    return pct ~= nil and pct < 15
end

local function health_bar(pct)
    if pct == nil then
        return "[????]"
    end

    local filled = math.floor(math.max(0, math.min(100, pct)) / 25)
    local cells = {}
    for i = 1, 4 do
        if i <= filled then
            table.insert(cells, "=")
        else
            table.insert(cells, "-")
        end
    end
    return "[" .. table.concat(cells) .. "]"
end

local function compact_number(n)
    if n == nil then
        return nil
    end

    local abs_n = math.abs(n)
    if abs_n >= 1000000 then
        return string.format("%.1fM", n / 1000000)
    elseif abs_n >= 1000 then
        return string.format("%.1fk", n / 1000)
    else
        return tostring(n)
    end
end

local function positive_compact_number(n)
    if n == nil or n <= 0 then
        return nil
    end
    return compact_number(n)
end

local function sandbox_label(sandbox)
    if sandbox == nil or sandbox == "" then
        return nil
    elseif sandbox == "read-only" then
        return "RO"
    elseif sandbox == "workspace-write" then
        return "RW"
    elseif sandbox == "danger-full-access" then
        return "GOD"
    else
        return string.upper(sandbox)
    end
end

local function sandbox_color(sandbox)
    if sandbox == "read-only" then
        return "cyan"
    elseif sandbox == "workspace-write" then
        return "yellow"
    elseif sandbox == "danger-full-access" then
        return "red"
    else
        return "white"
    end
end

local function approval_label(approval)
    if approval == nil or approval == "" then
        return nil
    elseif approval == "untrusted" then
        return "LOCK"
    elseif approval == "interactive" then
        return "ASK"
    elseif approval == "on-failure" then
        return "FAIL"
    elseif approval == "on-request" then
        return "ASK"
    elseif approval == "never" then
        return "FREE"
    else
        return string.upper(approval)
    end
end

chaos.statusline(function(ctx)
    local spans = {}

    local hp_pct = nil
    if ctx.context ~= nil then
        hp_pct = ctx.context.remaining_pct
    end
    local hp_color = health_color(hp_pct)
    local hp_label = hp_pct ~= nil and tostring(hp_pct) .. "%" or "???%"

    local model = ctx.model or "unknown"
    local effort = ctx.reasoning_effort
    local weapon_label = model
    if effort ~= nil and effort ~= "" and effort ~= "default" then
        weapon_label = weapon_label .. " " .. effort
    end

    local map_label = ctx.branch or ctx.project_root or ctx.cwd_display or ctx.cwd
    local dir_label = ctx.cwd_display or ctx.cwd
    local armor_label = sandbox_label(ctx.sandbox)
    local armor_color = sandbox_color(ctx.sandbox)
    local approval = approval_label(ctx.approval)

    local ctx_load = nil
    local ammo = nil
    local ammo_ctx = nil
    if ctx.tokens ~= nil and ctx.tokens.available == true then
        ctx_load = compact_number(ctx.tokens.context_effective or 0)
        ammo = positive_compact_number(ctx.tokens.last_output)
        if ctx.tokens.has_prior_context == true then
            ammo_ctx = positive_compact_number(ctx.tokens.last_effective)
        end
    end

    push(spans, "HUD", "magenta", true)

    sep(spans)
    push(spans, "HP ", "white", true)
    if is_critical_health(hp_pct) then
        push(spans, "CRIT ", "red", true)
    end
    push(spans, health_bar(hp_pct), hp_color, true)
    push(spans, " ", nil, false)
    push(spans, hp_label, hp_color, true)

    sep(spans)
    push(spans, "WPN ", "white", true)
    push(spans, weapon_label, "cyan", false)

    if map_label ~= nil and map_label ~= "" then
        sep(spans)
        push(spans, "MAP ", "white", true)
        push(spans, map_label, "yellow", false)
    end

    if armor_label ~= nil or approval ~= nil then
        sep(spans)
        push(spans, "ARM ", "white", true)
        if armor_label ~= nil then
            push(spans, armor_label, armor_color, true)
        end
        if armor_label ~= nil and approval ~= nil then
            push(spans, "/", "gray", false)
        end
        if approval ~= nil then
            push(spans, approval, "blue", false)
        end
    end

    if ctx_load ~= nil then
        sep(spans)
        push(spans, "CTX ", "white", true)
        push(spans, ctx_load, "magenta", true)
    end

    if ammo ~= nil then
        sep(spans)
        push(spans, "AMMO ", "white", true)
        push(spans, ammo, "magenta", true)
        if ammo_ctx ~= nil then
            push(spans, " ", nil, false)
            push(spans, "(+" .. ammo_ctx .. " ctx)", "gray", false)
        end
    end

    if dir_label ~= nil and dir_label ~= "" and dir_label ~= map_label then
        sep(spans)
        push(spans, "DIR ", "white", true)
        push(spans, dir_label, "gray", false)
    end

    return spans
end)

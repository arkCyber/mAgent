-- datalogger.lua — environmental data logger with persistent NVRAM config.
--
-- Use case: a remote weather station takes sensor samples, applies a moving
-- average filter, and only forwards a BLE summary when the value changes
-- significantly (event-driven reporting, not periodic — saves radio power).
-- Configuration (sample rate, filter window, change threshold) is stored in
-- NVRAM so it survives a reboot; defaults are used on first boot.
--
-- Hardware used:
--   * sensors : "temp", "battery", "heart_rate"
--   * storage : hardware.nvram_get / hardware.nvram_set (flash-backed KV)
--   * radio   : hardware.ble_send (BLE payload)
--
-- All nvram writes are bounded by the host's own length caps.

------------------------------------------------------------
-- 1.  Config helpers — read a key with a baked-in default, persist on
--     first touch so subsequent boots can see what was chosen.
------------------------------------------------------------
local function cfg_get(key, default)
    local v = hardware.nvram_get(key)
    if v == nil or v == "" then
        hardware.nvram_set(key, default)
        return default
    end
    return v
end

local function cfg_number(key, default)
    local n = tonumber(cfg_get(key, tostring(default)))
    if n == nil then return default end
    return n
end

------------------------------------------------------------
-- 2.  Module state — running window for the moving-average filter.
------------------------------------------------------------
local SAMPLE_COUNT    = 0
local TEMP_SUM        = 0.0
local LAST_REPORTED   = nil -- last value we BLE'd, for change detection

------------------------------------------------------------
-- 3.  Boot — pull the config (and persist defaults if first boot).
------------------------------------------------------------
-- Mutable config state: each value is read at boot time but can be updated
-- at runtime via `on_event("SET key value")`. Storing them in module-level
-- `local`s makes them invisible to `on_event`; using a small table keeps
-- them mutable AND lets us pass them through by reference.
local CFG = {
    window    = cfg_number("cfg.window",     5),   -- sample count
    threshold = cfg_number("cfg.threshold",  0.5), -- °C change required
    rate_ms   = cfg_number("cfg.rate_ms",    500), -- poll period
    device_id = cfg_get    ("cfg.device_id", "magent-01"),
}

------------------------------------------------------------
-- 4.  The tick handler — invoked by AppRuntime each loop iteration.
------------------------------------------------------------
function on_tick(now_ms)
    -- (a) Rate limit (would normally be enforced by the runtime; double-
    --     gated here so the script is also standalone-runnable).
    if (now_ms % CFG.rate_ms) > 50 then
        return ""
    end

    local temp = hardware.sensor_read("temp")
    local batt = hardware.sensor_read("battery")

    -- (b) Moving average over the last CFG.window readings.
    SAMPLE_COUNT = SAMPLE_COUNT + 1
    TEMP_SUM     = TEMP_SUM + temp
    local avg = TEMP_SUM / SAMPLE_COUNT
    -- When the window is full, drop the oldest half to keep memory bounded
    -- and track a true moving average from then on.
    if SAMPLE_COUNT >= CFG.window then
        SAMPLE_COUNT = math.floor(CFG.window / 2)
        TEMP_SUM     = avg * SAMPLE_COUNT
    end

    -- (c) Change detection — only report if the smoothed value moved.
    if LAST_REPORTED ~= nil
       and math.abs(avg - LAST_REPORTED) < CFG.threshold then
        return ""
    end
    LAST_REPORTED = avg

    -- (d) Build the BLE payload: "id,temp,battery".
    local payload = string.format(
        "%s,%.2f,%.2f",
        CFG.device_id, avg, batt
    )

    -- (e) Forward over BLE. `ble_send` is bound to MAX_PAYLOAD_LEN so a
    --     misformed payload is rejected rather than wedging the radio.
    local ok, err = pcall(hardware.ble_send, payload)
    if not ok then
        return "BLE_ERR"
    end

    -- (f) Surface the report via the action dispatch table.
    return string.format("BLE_SEND:%s", payload)
end

------------------------------------------------------------
-- 5.  on_event — hot-reloadable config update from a remote console.
--     Allows the operator to tune the filter window without a reboot.
------------------------------------------------------------
function on_event(cmd)
    if cmd == nil or cmd == "" then return "NOOP" end
    if cmd == "RESET_CONFIG" then
        hardware.nvram_set("cfg.window",    "5")
        hardware.nvram_set("cfg.threshold", "0.5")
        hardware.nvram_set("cfg.rate_ms",   "500")
        -- Also reset the live config table so the running loop picks up the
        -- defaults immediately rather than waiting for a reboot.
        CFG.window    = 5
        CFG.threshold = 0.5
        CFG.rate_ms   = 500
        return "RESET"
    end
    if cmd == "STATUS" then
        return string.format(
            "SAMPLE_COUNT=%d LAST_REPORTED=%s WINDOW=%d THRESHOLD=%.2f RATE_MS=%d",
            SAMPLE_COUNT,
            tostring(LAST_REPORTED),
            CFG.window,
            CFG.threshold,
            CFG.rate_ms
        )
    end
    -- "SET window 10" → split on whitespace; persist the first token only
    -- if it names a known config key. We avoid `string.match` / `string.find`
    -- so the script runs on engines with a minimal `string` stdlib (piccolo
    -- only ships `string.len` / `string.sub`).
    --
    -- Manual `find ' '` implementation: scan byte-by-byte with `string.sub`.
    local len = string.len(cmd)
    local first_space = nil
    local i = 5  -- skip the leading "SET "
    while i <= len do
        if string.sub(cmd, i, i) == " " then
            first_space = i
            break
        end
        i = i + 1
    end
    if first_space ~= nil then
        local k = string.sub(cmd, 5, first_space - 1)
        local v = string.sub(cmd, first_space + 1)
        if k == "window" or k == "threshold" or k == "rate_ms" then
            local cfg_key = "cfg." .. k
            -- Validate the new value as a number (rejects garbage) and
            -- apply non-negative floors for each parameter so a mis-typed
            -- value can never crash the running loop.
            local n = tonumber(v)
            if n == nil then
                return "BAD_VALUE"
            end
            if k == "window" or k == "rate_ms" then
                if n < 1 then n = 1 end
                if n > 65535 then n = 65535 end
                n = math.floor(n)
            else
                if n < 0 then n = 0 end
                if n > 100 then n = 100 end
            end
            hardware.nvram_set(cfg_key, tostring(n))
            -- Apply the change to the live config table so the next tick
            -- uses the new value without a reboot.
            CFG[k] = n
            return string.format("STORED:%s=%s", cfg_key, tostring(n))
        end
    end
    return "UNKNOWN"
end

-- alarm.lua — dynamic-threshold alarm system with state machine.
--
-- Use case: a wearable safety device monitors a vital sign (heart rate) and
-- fires escalating alarms (info → warn → critical) on persistent threshold
-- violations. The thresholds themselves are *not* hard-coded: the AI agent
-- adapts them based on activity context (resting vs exercising).
--
-- State machine (pure Lua, no host state):
--   IDLE  → OK        : HR in normal band, all clear
--   OK    → INFO      : one reading outside band
--   INFO  → OK        : reading returned to band
--   INFO  → WARN      : N consecutive violations
--   WARN  → ALARM     : one more violation in WARN
--   ALARM → OK        : reading returned to band
--
-- Hardware used:
--   * sensor : "heart_rate" (bpm)
--   * LED    : pin 4 (status: off=IDLE, low=OK, high=WARN, blink=ALARM)
--   * BLE    : notify base station on each state transition

------------------------------------------------------------
-- 1.  Default thresholds (bpm); can be retuned by the agent.
------------------------------------------------------------
local HR_REST_LOW   = 50
local HR_REST_HIGH  = 100
local HR_ACT_LOW    = 90
local HR_ACT_HIGH   = 160
local WINDOW        = 3  -- consecutive readings to escalate WARN
local STATE_VAR     = "STATE"
local MODE_VAR      = "MODE"

local STATE = "IDLE"
local MODE  = "rest"        -- "rest" | "active"
local COUNT = 0            -- consecutive violations in current state
local LAST_HR = nil

local function set_state(next_state, hr)
    if STATE == next_state then
        return
    end
    local prev = STATE
    STATE = next_state
    COUNT = 0
    -- LED pattern encodes the state on pin 4.
    if STATE == "IDLE" then
        hardware.gpio_write(4, 0)
    elseif STATE == "OK" then
        hardware.gpio_write(4, 1)   -- solid low
    elseif STATE == "INFO" then
        hardware.gpio_write(4, 1)   -- pulse pattern via next tick
    elseif STATE == "WARN" then
        hardware.pwm_set(4, 50)
    elseif STATE == "ALARM" then
        hardware.pwm_set(4, 100)   -- buzzer full
    end
    -- Notify base station via BLE; payload is "ALARM:prev->next hr=<bpm>".
    local payload = string.format("%s:%s->%s hr=%.0f", "ALARM", prev, STATE, hr or 0)
    pcall(hardware.ble_send, payload)
end

local function thresholds_for_mode()
    if MODE == "active" then
        return HR_ACT_LOW, HR_ACT_HIGH
    end
    return HR_REST_LOW, HR_REST_HIGH
end

------------------------------------------------------------
-- 2.  on_event — switch mode (rest ↔ active) or silence the alarm.
------------------------------------------------------------
function on_event(cmd)
    if cmd == "MODE_ACTIVE" then
        MODE = "active"
        return "MODE_ACTIVE"
    elseif cmd == "MODE_REST" then
        MODE = "rest"
        return "MODE_REST"
    elseif cmd == "SILENCE" then
        set_state("OK", LAST_HR or 0)
        hardware.pwm_set(4, 0)
        return "SILENCED"
    elseif cmd == "STATUS" then
        return string.format("STATE=%s MODE=%s HR=%s", STATE, MODE, tostring(LAST_HR))
    end
    return "UNKNOWN"
end

------------------------------------------------------------
-- 3.  on_tick — read HR, advance the state machine.
------------------------------------------------------------
function on_tick(now_ms)
    local hr = hardware.sensor_read("heart_rate")
    LAST_HR = hr

    local lo, hi = thresholds_for_mode()
    local in_band = (hr >= lo and hr <= hi)

    if STATE == "IDLE" then
        if in_band then
            set_state("OK", hr)
        end
    elseif STATE == "OK" then
        if in_band then
            return ""
        else
            set_state("INFO", hr)
        end
    elseif STATE == "INFO" then
        if in_band then
            set_state("OK", hr)
            return ""
        else
            COUNT = COUNT + 1
            if COUNT >= WINDOW then
                set_state("WARN", hr)
                return "BUZZER:50"
            end
            return ""
        end
    elseif STATE == "WARN" then
        if in_band then
            set_state("OK", hr)
            hardware.pwm_set(4, 0)
            return ""
        else
            set_state("ALARM", hr)
            return "BUZZER:100"
        end
    elseif STATE == "ALARM" then
        if in_band then
            set_state("OK", hr)
            hardware.pwm_set(4, 0)
            return "SILENCED"
        end
        return "BUZZER:100"
    end

    return ""
end

-- greenhouse.lua — intelligent greenhouse climate controller.
--
-- Use case: a low-power greenhouse / cold-chain monitor runs an enterprise
-- Lua app on the chip. The deterministic rules own the safety policy
-- (read sensors, threshold-check, never drive fan PWM > 100 %); the agent
-- is consulted only for the *fuzzy* control question ("with current humidity
-- and battery, should we run the fan at full bore, half, or just trickle?").
--
-- Hardware used:
--   * sensors  : "temp" (die / ambient), "battery" (volts)
--   * actuator : fan PWM on pin 1 (0..100 %)
--   * alarm    : buzzer on pin 3 (0..100 %) via BUZZER
--
-- All numeric parsing is defensive: bad agent output never crashes the loop.

------------------------------------------------------------
-- 1.  Configuration (would normally be stored in NVRAM)
------------------------------------------------------------
local THERMAL_HIGH     = 35.0  -- °C : agent's "turn on cooling" threshold
local BATTERY_LOW      = 3.30  -- V  : under this, do not exceed 30 % PWM
local CRITICAL_TEMP    = 55.0  -- °C : hard safety limit, no agent needed
local SAFE_PWM_MAX     = 80    -- %  : never drive the fan harder than this
local POLL_INTERVAL_MS = 1000  -- only call agent every Nth tick

------------------------------------------------------------
-- 2.  Tick handler — what `AppRuntime::tick` invokes.
------------------------------------------------------------
function on_tick(now_ms)
    local temp    = hardware.sensor_read("temp")
    local battery = hardware.sensor_read("battery")

    -- (a) Hard safety check: no agent, no judgment — alarm + full fan.
    if temp > CRITICAL_TEMP then
        hardware.pwm_set(1, 100)
        hardware.pwm_set(3, 80) -- buzzer at 80 %
        return string.format("CRITICAL:%d", math.floor(temp))
    end

    -- (b) No need to ask the agent for nominal conditions.
    if temp <= THERMAL_HIGH then
        -- Idle: make sure the fan isn't stuck on from a prior tick.
        hardware.pwm_set(1, 0)
        return "IDLE"
    end

    -- (c) Fuzzy case: temperature is between THERMAL_HIGH and CRITICAL_TEMP.
    --     Ask the AI to pick a duty between 0..SAFE_PWM_MAX.
    --
    --     Throttle: ask the agent at most once a second.
    if (now_ms % POLL_INTERVAL_MS) > 50 then
        return ""
    end

    local ctx = string.format(
        "temp=%.1fC battery=%.2fV now_ms=%d",
        temp, battery, now_ms
    )
    local suggestion = agent.reason(
        ctx,
        "Pick a fan duty 0..80 for cooling this greenhouse."
    )

    -- (d) Defensive parse: "SET_COOLING_PULSE:NN" → NN; anything else = 0.
    local duty = tonumber(string.match(suggestion, "SET_COOLING_PULSE:(%d+)"))
    if duty == nil then
        duty = 0
    end
    if duty < 0 then duty = 0 end
    if duty > SAFE_PWM_MAX then duty = SAFE_PWM_MAX end

    -- (e) Battery guard: low battery → cap duty further.
    if battery < BATTERY_LOW and duty > 30 then
        duty = 30
    end

    hardware.pwm_set(1, duty)

    -- (f) Returned action is recognised by `apply_action` and will be
    --     applied to hardware (here we returned it as a string anyway).
    return string.format("SET_COOLING_PULSE:%d", duty)
end

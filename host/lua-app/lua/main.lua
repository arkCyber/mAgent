-- main.lua — "user App as brain, AI agent as brain-trust" enterprise demo.
--
-- Deterministic logic owns the control flow; the embedded agent is consulted
-- only when the data crosses a threshold that pure rules can't arbitrate.

local temp = hardware.sensor_read("temp")
print(string.format("temp = %.1f C", temp))

-- I2C sensor demo: write a config byte then read back 2 bytes sequentially.
hardware.i2c_write(0x48, 0x01, 'AB')
local sensor = hardware.i2c_read(0x48, 0x01, 2)
print("i2c[0x48] reg 0x01 = " .. tostring(sensor))

if temp > 30.0 then
    -- Fuzzy / natural-language decision: delegate to the AI agent.
    local action = agent.reason(
        "Device temperature is high and vibration is abnormal.",
        "What control parameter should I apply to prevent a shutdown?"
    )
    print("agent suggests: " .. tostring(action))

    if string.match(action, "SET_COOLING_PULSE:(%d+)") then
        local duty = tonumber(string.match(action, "SET_COOLING_PULSE:(%d+)"))
        hardware.pwm_set(1, duty or 50)
        print("fan PWM set to " .. tostring(duty))
    elseif string.match(action, "COOL") or string.match(action, "SET_COOLING") then
        hardware.gpio_write(1, 1)
        print("fan ON (gpio 1 high)")
    else
        print("no cooling action matched; leaving fan as-is")
    end
else
    print("temp nominal; no agent call needed")
end

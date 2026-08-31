-- selftest.lua — mAgent Lua hardware self-test app (test case).
--
-- A deterministic, PASS/FAIL application that exercises every `hardware.*`
-- binding the ESP32-S3 Lua host exposes, so an operator can verify the
-- firmware-to-Lua bridge on a fresh board (or after a flash). Load it by
-- writing this source to the `main.lua` NVS key (see `set_lua_app_source` /
-- `AT+LUAAPP=<base64>`), or by building with it as the embedded DEFAULT_MAIN_LUA.
--
-- NOTE: written against the `piccolo` stdlib — use `..` for concatenation
-- (piccolo does not implement `string.format`).
--
-- Status: this board (ESP32-S3, 4 MB flash / quad PSRAM) currently cannot spawn
-- the `lua-thread` alongside the agent + ingress + Wi-Fi (internal DRAM
-- exhausted; see docs/FIRMWARE_TLS_BLE_GAP_ANALYSIS.md). This app is the test
-- case to run once internal DRAM is freed (or on a platform that fits the VM).

local pass = 0
local fail = 0

local function check(name, ok, detail)
    if ok then
        pass = pass + 1
        print("[PASS] " .. name)
    else
        fail = fail + 1
        print("[FAIL] " .. name .. (detail and (": " .. detail) or ""))
    end
end

-- 1. Temperature sensor (internal). Expect a number.
local temp = hardware.sensor_read("temp")
check("sensor_read temp", type(temp) == "number", tostring(temp))
print("  temp = " .. tostring(temp))

-- 2. Free-heap sensor.
local heap = hardware.sensor_read("mem")
check("sensor_read mem", type(heap) == "number", tostring(heap))
print("  heap = " .. tostring(heap))

-- 3. GPIO write + read back (if the board maps a loopback; else just write).
local wr = pcall(function() hardware.gpio_write(2, 1) end)
check("gpio_write", wr)

-- 4. PWM duty (0..100 on a pin).
local pwm = pcall(function() hardware.pwm_set(1, 50) end)
check("pwm_set 50%", pwm)

-- 5. Persistent flash write/read round-trip via NVS.
local fw = pcall(function()
    hardware.flash_write(0x1000, "selftest-v1")
    local got = hardware.flash_read(0x1000, 16)
    if got ~= "selftest-v1" then error("roundtrip mismatch: " .. tostring(got)) end
end)
check("flash write/read roundtrip", fw)

-- 6. Agent consultation (local heuristic; no cloud in safe mode).
local action = agent.reason("selftest", "respond with STATUS_OK")
check("agent.reason", type(action) == "string" and #action > 0, tostring(action))

print("")
print("=== SELFTEST RESULT: " .. tostring(pass) .. " pass, " .. tostring(fail) .. " fail ===")
if fail == 0 then print("SELFTEST: ALL PASS") else print("SELFTEST: FAILURES PRESENT") end

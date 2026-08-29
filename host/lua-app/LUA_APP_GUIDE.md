# Writing a Lua app for mAgent (host dev guide)

This is the **host-side "Lua working environment"**: iterate on `main.lua` from
the shell against the RAM-backed simulator, then the same script runs on the
ESP32-S3 firmware (gated behind the `lua` feature) once a Xtensa-compatible Lua
engine is used. All runtime logic is host-verified (61 tests).

## 1. The `hardware.*` API your script can call

| Lua | Meaning |
|---|---|
| `hardware.sensor_read(name)` | `temp`/`temperature`/`die`, `heart_rate`/`hr`/`pulse`, `hrv`, `stress`, `glucose`, `battery`, `memory`/`free_heap` |
| `hardware.gpio_write(pin, level)` / `gpio_read(pin)` | digital I/O (level 0/1) |
| `hardware.i2c_read(addr, reg, len)` / `i2c_write(addr, reg, data)` | I2C sequential |
| `hardware.i2c_transfer(addr, reg, tx, rx_len)` | I2C write-then-read |
| `hardware.adc_read(pin)` | raw voltage (volts) |
| `hardware.pwm_set(pin, duty)` | duty 0–100 |
| `hardware.flash_read/write/erase_sector(addr, …)` | persistent flash |
| `hardware.nvram_get(key)` / `nvram_set(key, value)` | persistent KV (config) |
| `hardware.ble_send(data)` | BLE payload |
| `hardware.power_set(profile)` | 0=Active,1=Idle,2=LowPower,3=DeepSleep |
| `agent.reason(context, prompt)` | delegate to the embedded agent (returns an action string) |

## 2. Action grammar

`agent.reason()` returns a command the app can act on:

```text
COMMAND            e.g. "SET_COOLING"
COMMAND:value      e.g. "SET_COOLING_PULSE:80"
```

`string.match(action, "SET_COOLING_PULSE:(%d+)")` is the idiomatic Lua way to
pick a value; the `magent_lua::action` module parses the same grammar on the
Rust side.

## 3. Run it (no Rust changes)

```sh
# Boot the bundled demo main.lua
cargo run -p magent-lua --bin lua-run -- --ticks 5

# Your own script, a warm die, a mock agent decision
cargo run -p magent-lua --bin lua-run -- --script ./my_app.lua \
    --temp 60 --action SET_COOLING_PULSE:80 --ticks 10
```

You'll see each tick's result, a health snapshot (uptime/ticks/errors/stale),
and a few observable hardware effects (fan PWM, GPIO, ADC).

## 4. Sandbox limits (your script is sandboxed)

- Stdlibs: `table` / `string` / `math` only — `os`/`io`/`debug`/`package`/`ffi`
  are **absent**.
- Heap: **512 KiB** cap (a big allocation errors, not hangs).
- Per-script instruction budget: **2,000,000** (an infinite loop is stopped).
- Binding-layer caps: BLE ≤4096 B, I2C ≤256 B, `agent.reason` text ≤2048 B.

## 5. Iterating

- `--temp` / `--action` let you exercise different branches without editing
  the script.
- `AppRuntime::reload` (Rust) swaps `main.lua` at runtime; a firmware OTA can
  hot-reload the app without reboot.

## 6. Going to the ESP32-S3

The host simulator and the real chip share the exact `HardwareBackend` trait, so
a script that works here runs unchanged on hardware. See
`docs/LUA_SCRIPTING_S3.md` for the firmware integration (currently gated behind
the `lua` Cargo feature; `mlua` does not yet build for Xtensa).

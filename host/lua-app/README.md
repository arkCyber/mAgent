# magent-lua — aerospace-grade Lua 5.4 scripting host for mAgent

`magent-lua` implements the **"user App as brain, AI agent as brain-trust"**
architecture on top of `magent-core` + `magent-hal`:

* An enterprise developer writes a `main.lua` that owns deterministic control
  flow (sensor polling, networking, display).
* When it hits fuzzy analysis / natural-language decision points, it calls
  `agent.reason(context, prompt)` to delegate to the embedded `MiniAgent`
  (which can reach an LLM over Wi‑Fi on a memory-rich chip such as the
  ESP32‑S3).
* All hardware is reached through `hardware.*` — a narrow [`HardwareBackend`]
  trait so the identical script runs on the host simulator or a real chip.

```lua
-- main.lua
local temp = hardware.sensor_read("temp")
if temp > 85.0 then
    local action = agent.reason("Temp is high.", "What control?")
    if string.match(action, "COOL") then hardware.gpio_write(1, 1) end
end
```

## Build & test (host)

```sh
# Run the end-to-end demo (boots a warm simulated die, runs lua/main.lua)
cargo run -p magent-lua --example demo

# Run the aerospace-grade test suite (61 behavior tests)
cargo test -p magent-lua

# Lint (this crate is clean under clippy)
cargo clippy -p magent-lua --all-targets -- -D warnings

# One-command regression gate (clippy + tests + CLI build + fmt)
bash scripts/check-lua.sh

# Developer CLI: iterate on any main.lua against the simulator
cargo run -p magent-lua --bin lua-run -- --script my_app.lua --temp 60 \
    --action SET_COOLING_PULSE:80 --ticks 5

# Pure-Rust `piccolo` engine (optional, for the ESP32-S3 which cannot build
# `mlua`'s vendored Lua C). `PiccoloVm` covers the full HardwareBackend surface
# and `AppRuntime<PiccoloVm>` runs the full runtime (6 tests).
cargo test -p magent-lua --features piccolo --test piccolo_tests
```

See [`LUA_APP_GUIDE.md`](LUA_APP_GUIDE.md) for how to write and run a Lua app.

## What's bound into Lua

| Lua API | Rust backing |
|---|---|
| `hardware.gpio_write(pin, level)` | `HardwareBackend::gpio_write` |
| `hardware.gpio_read(pin)` | `HardwareBackend::gpio_read` |
| `hardware.sensor_read(name)` | `HardwareBackend::sensor_read` |
| `hardware.flash_read(addr, len)` | `HardwareBackend::flash_read` |
| `hardware.flash_write(addr, data)` | `HardwareBackend::flash_write` |
| `hardware.i2c_read(addr, reg, len)` | `HardwareBackend::i2c_read` |
| `hardware.i2c_write(addr, reg, data)` | `HardwareBackend::i2c_write` |
| `hardware.i2c_transfer(addr, reg, tx, rx_len)` | `HardwareBackend::i2c_transfer` (write-then-read) |
| `hardware.adc_read(pin)` | `HardwareBackend::adc_read` (volts) |
| `hardware.pwm_set(pin, duty)` | `HardwareBackend::pwm_set` |
| `hardware.ble_send(data)` | `HardwareBackend::ble_send` |
| `hardware.power_set(profile)` | `HardwareBackend::power_set` |
| `hardware.nvram_get(key)` | [`nvram`](src/nvram.rs) `get` (persistent) |
| `hardware.nvram_set(key, value)` | [`nvram`](src/nvram.rs) `set` (persistent) |
| `agent.reason(context, prompt)` | `MiniAgent::run` (blocked on a mutex) |

For event-driven scripts, [`LuaVm::call`](src/vm.rs) invokes a named global
function (e.g. `on_tick`, `on_event`) on a timer / interrupt without re-running
the whole chunk:

```rust
vm.run_script("function on_tick(x) return 'tick:' .. x end")?;
vm.call("on_tick", &["42".to_string()])?; // "tick:42"
```

## Supervised app runtime (`AppRuntime`)

The firmware-facing runtime: **boot once, tick forever**, with a heartbeat and
action dispatch. This is the "App as brain" loop:

```rust
use magent_lua::runtime::AppRuntime;

let mut app = AppRuntime::new(vm, hardware);
app.boot(include_str!("lua/main.lua"))?;                 // one-time init
app.boot("function on_tick(ms) return 'FAN_ON' end")?;   // per-tick handler
let t = app.tick()?;                                      // one loop iteration
// t.result == "FAN_ON", t.dispatched == true (applied to hardware)
app.tick_count(); // heartbeat: monotonic tick index
```

* `AppRuntime::tick` calls Lua `on_tick(now_ms)` and, when the returned string
  is a recognised command (see [`action`](src/action.rs)), dispatches it to
  hardware via [`apply_action`].
* A missing handler yields an empty tick; a per-tick handler error is returned
  but does not corrupt the loop (next tick still runs).
* `tick_count` / `uptime_ms` are the heartbeat a watchdog / supervisor polls;
  [`AppRuntime::is_stale`] reports whether the loop has stopped being driven.
* **Graceful stop:** [`AppRuntime::stop_flag`] returns a cross-thread handle a
  supervisor can set; [`AppRuntime::run_until_stop`] drives the loop until it
  is set (or a `max_ticks` cap), which is exactly the firmware's main-loop
  pattern.
* **Persistence:** [`nvram`](src/nvram.rs) is a small KV over the flash backend
  (`hardware.nvram_get` / `nvram_set`), so a script can store config that
  survives a reboot.
* **Host testing without a network:** [`install_mock_agent`] plugs a
  [`MockLlmBackend`] into `agent.reason()` so the whole "agent decides →
  hardware acts" path runs on a desktop (see
  `agent_decision_drives_pwm` test).

Try `cargo run -p magent-lua --example event_loop`.

## Design-assurance properties

See [`AUDIT.md`](./AUDIT.md) for the full aerospace-grade audit. In short:

1. **No panics in production** — every fallible path returns
   [`LuaHostError`]; `clippy::panic_in_result_fn` is denied.
2. **Sandboxed VM** — only `table`/`string`/`math` stdlibs; `os`, `io`,
   `debug`, `package`, `ffi` are absent.
3. **Heap + instruction caps** — `set_memory_limit` and a count hook stop a
   runaway script (tested against `while true do end`).
4. **Caught longjmp** — Lua errors become `Result` at the VM boundary; they
   never unwind across the Rust stack.
5. **Swappable hardware** — `HardwareBackend` keeps scripts portable between
   the host simulator and a chip.

## Going to hardware (ESP32‑S3)

See [`docs/LUA_SCRIPTING_S3.md`](../../docs/LUA_SCRIPTING_S3.md) for the
two-stage plan and how to wire `HardwareBackend` / `SharedAgent` into
`firmware/esp32-app` on a dedicated FreeRTOS worker thread.

# Lua scripting on the ESP32‑S3 — two-stage integration plan

The `host/lua-app` crate (`magent-lua`) already provides a **host-validated,
aerospace-grade Lua 5.4 host** with `hardware.*` and `agent.reason()` bound
onto `magent-hal` / `magent-core`. This document is the second stage: wiring
the *same* bindings into `firmware/esp32-app` (S3).

> **Status (2026-08):** the S3 Lua wiring skeleton lives in
> `firmware/esp32-app/src/lua_task.rs`, gated behind a separate **`lua` Cargo
> feature** (OFF by default). The `board-s3` firmware itself builds and boots.
> `mlua` cannot build for Xtensa ("don't know how to build Lua for
> xtensa-esp32s3-espidf"), so the S3 Lua VM must use a **pure-Rust engine.
> `piccolo`** is now integrated as an optional `piccolo` Cargo feature, with a
> **Status (2026-08):** the S3 Lua path is now **build-verified end-to-end**:
> `cargo +esp build --target xtensa-esp32s3-espidf --no-default-features
> --features board-s3,wifi,uart,lua --release` compiles the firmware with
> `PiccoloVm` (pure-Rust `piccolo`, **no `mlua`**) + `Esp32Hardware` +
> `AppRuntime<PiccoloVm>` into an Xtensa ELF. `mlua` is an optional feature of
> `magent-lua` (default on); the firmware `lua` feature builds `magent-lua` with
> `default-features = false, features = ["piccolo"]`. `PiccoloVm` covers the
> full `HardwareBackend` surface + `agent.reason` + `call`/`has`, and the whole
> runtime is host-verified (mlua 61 tests, mlua+piccolo 67, pure-piccolo 6).
> Remaining: real S3 board validation (the `Esp32Hardware` drivers and the
> on-device Lua app).

## Why S3 and not C61

| Chip | Internal dynamic SRAM | PSRAM | Verdict |
|---|---|---|---|
| ESP32‑C61 | ~134 KB | 2 MB | Too tight for WiFi + BLE + agent + a Lua VM |
| **ESP32‑S3** | **~390 KB** | **8 MB** | Comfortably runs WiFi(DeepSeek) + BLE + agent + Lua |

On the S3 the firmware already uses `esp-idf-svc + std`, and `std::alloc` is
backed by the 8 MB PSRAM — exactly the heap a Lua VM needs. This is the
target board for this feature.

## How the pieces fit

```
┌────────────────────────────── firmware/esp32-app (S3) ─────────────────────┐
│  main()                                                                     │
│   └─ spawn "lua-thread" (FreeRTOS task, e.g. stack_size(32*1024))           │
│        └─ LuaVm::new(hardware_backend, shared_agent)                       │
│             ├─ hardware  → Esp32Hardware implements magent_lua::HardwareBackend
│             │              (esp-hal GPIO / I2C / SPI flash / BLE)          │
│             ├─ agent     → Arc<Mutex<MiniAgent>> with set_llm_backend(DeepSeek)
│             │              (the firmware's existing DeepSeek HTTP client)   │
│             └─ run_script(include_bytes main.lua)  (boot-time main loop)    │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Wiring skeleton (design, not yet compiled for Xtensa)

```rust
// firmware/esp32-app/src/lua_task.rs  (sketch)
use magent_lua::{HardwareBackend, LuaVm};
use magent_core::MiniAgent;

struct Esp32Hardware { /* esp-hal pins, i2c bus, nvs, ble */ }
impl HardwareBackend for Esp32Hardware {
    fn gpio_write(&mut self, pin: u8, level: u8) -> Result<(), String> {
        // gpio: &mut OutputPin; level 0→Low, else High
        self.pins.write(pin, level).map_err(|e| e.to_string())
    }
    // ... gpio_read, sensor_read (ADC/I2C), flash_read/write (NVS), ble_send, power_set
}

fn spawn_lua_thread() {
    // Reuse the agent already owned by the firmware (Arc<Mutex<MiniAgent>>),
    // with set_llm_backend(deepseek) already installed.
    let hw: magent_lua::vm::SharedHardware = Arc::new(Mutex::new(Esp32Hardware::new()));
    let task = ThreadSpawnConfiguration {
        name: "lua-thread".into(),
        stack_size: 32 * 1024,
        priority: 10,
        ..Default::default()
    };
    task.spawn(move || {
        let vm = match LuaVm::new(hw, shared_agent) {
            Ok(v) => v,
            Err(e) => { log::error!("lua vm init: {e}"); return; }
        };
        // The enterprise main.lua lives in a Flash partition / bundled include.
        let script = include_str!("../../lua/main.lua");
        if let Err(e) = vm.run_script(script) {
            log::error!("main.lua: {e}"); // bounded, never panics
        }
    }).ok();
}
```

> **Implemented in `src/lua_task.rs` (board-s3 gate):** `Esp32Hardware`
> implements `HardwareBackend`; `start_lua_task()` builds the non-`Send`
> `AppRuntime` **inside** the spawned thread closure (so `std::thread` only
> captures `Send` values — no `mlua` `send` feature needed). **Wired (raw
> ESP-IDF C API, same as `local_tools.rs`):**
> - GPIO I/O, internal die temperature (mirrors `local_tools::read_sensor`), free heap;
> - PWM via **LEDC** (timer 0, 8-bit, 1 kHz; channels 0..7 allocated lazily per GPIO; duty 0..=100 %);
> - ADC via **oneshot** on **ADC1** (GPIO1..=10 → channels 0..=9, 12-bit, `DB_11`; ADC2 is excluded because it conflicts with the Wi-Fi the firmware uses);
> - I2C **master** on `I2C_NUM_0` (register read/write via repeated start; SDA/SCL pins are `I2C_SDA_PIN`/`I2C_SCL_PIN` constants, default GPIO9/GPIO8);
> - persistent **flash** backed by ESP-IDF **NVS** (`flash_<addr:08x>` keyed blobs in the `magent_lua` namespace; NVS already initialised by `main::init_default_nvs()`);
> - **BLE TX** (`ble_send`) pushes to the connected client's SYS_RSP characteristic via the firmware `ble_config` GATT server (enabled with `--features ble`).
>
> The default boot script (`DEFAULT_MAIN_LUA`) is a hardware **self-test** that
> probes every wired driver inside `pcall` and prints `[lua] <driver> ok/err` to
> the console. Every `HardwareBackend` method is now wired (BLE returns an error
> only when no BLE client is connected / the `ble` feature is off).
>
> **App source loading:** at boot `start_lua_task` prefers an operator-provided
> `main.lua` stored in NVS (key `main.lua`, written via `set_lua_app_source`),
> falling back to the embedded `DEFAULT_MAIN_LUA` — so the app can be updated
> without reflashing the firmware.
>
> `PiccoloVm` uses `Lua::full()` so the **io stdlib (`print`)** is available for
> logging. Note: piccolo does **not** implement `string.format` — scripts must
> use `..` concatenation (the self-test is written this way).

## Flashing & validating on the linked S3

Hardware is connected. Build the Lua-enabled firmware and flash it:

```sh
# 1. Build (board-s3 + wifi + uart + lua; piccolo engine, no mlua)
./build-s3.sh

# 2. Flash and monitor (install `cargo-espflash` if needed)
espflash flash --monitor target/xtensa-esp32s3-espidf/release/magent-esp32-app
```

On boot the `lua-thread` runs `DEFAULT_MAIN_LUA` — a hardware self-test that
prints one line per wired driver. Expected console output:

```
[lua] temp  ok  <die temp °C>
[lua] adc   ok  <volts on GPIO1>      # wire a divider / pot to GPIO1
[lua] pwm   ok  duty=50%              # scope GPIO1 or wire an LED/fan
[lua] i2c   ok  <1 byte from 0x38>    # only if a device (e.g. TMP102/SSD1306) is on GPIO9/GPIO8
[lua] gpio  ok  p2=1                  # measure GPIO2 high
[lua] flash ok  HELLO                 # NVS round-trip at address 0x100
[lua] ble   ok  sent                  # connect a BLE client; else err "no connected BLE client"
```

A driver prints `err` (not a crash) when its hardware isn't wired — the
`pcall`-wrapped probe contains it. `i2c` is expected to print `err` unless a
device is present on the configured SDA/SCL pins (`I2C_SDA_PIN`/`I2C_SCL_PIN`,
default GPIO9/GPIO8 — change to match your wiring). `flash`/`ble` return an
explicit `Err` (not yet wired).

### Updating the app without reflashing (`AT+LUAAPP`)

The boot app loads from NVS (`main.lua`) with the embedded script as fallback.
Operators can push a new app over the UART ingress with `AT+LUAAPP`:

```sh
# 1. Turn your Lua file into an AT command (URL-safe base64):
scripts/luaapp-encode.sh path/to/main.lua
#    → AT+LUAAPP=<url-safe-base64-of-the-file>

# 2. Paste that line into the device console (115200). The firmware decodes it
#    and persists it as the boot `main.lua`.
#    AT+LUAAPP?            → +LUAAPP:<bytes>  (0 if only the embedded app)

# 3. Reset / reboot the device — it boots the new app. (`AppRuntime::reload`
#    can also hot-swap it at runtime without a reboot.)
```

The payload is URL-safe base64 (no `+`/`/`) so arbitrary Lua (commas, quotes,
newlines) survives AT argument parsing losslessly.

## Firmware-specific pitfalls (captured from the host build)

1. **C cross-compile.** `mlua`'s `lua54`+`vendored` feature builds the bundled
   Lua C. `build-s3.sh` already cross-compiles C deps (`secp256k1_sys`) with
   `xtensa-esp32s3-elf-gcc`, so Lua 5.4 compiles the same way. Keep `CC`/
   `CXX` pointed at the Xtensa toolchain.
2. **Thread model.** The VM + its `Arc<Mutex<MiniAgent>>` run on one dedicated
   FreeRTOS task (single-threaded, like the host). Do **not** share the VM
   across tasks, and keep `agent.reason`'s `block_on` off the RTOS tick.
3. **LLM decisions.** `start_lua_task` now installs the firmware's DeepSeek
   backend (`MiniAgent::set_llm_backend`, via `Box::leak`) when a model + API
   key are configured in NVS (`AT+LLMCFG`), so `agent.reason()` returns real
   decisions on the S3 instead of the canned heuristic answer. It also installs
   the real `Esp32ToolHandler` so agent tool calls drive actual GPIO / sensors.
4. **Sandbox stays on.** `LuaVm::new` enforces the memory cap and instruction
   budget on the S3 exactly as on the host; a hostile `main.lua` cannot hang
   the device. For the piccolo engine this is enforced in
   `PiccoloVm::execute_bounded` around every `run_script`/`call`:
   - a bounded `Fuel` budget (instruction budget) — `while true do end` returns a
     Lua error instead of wedging the `lua-thread`;
   - an 8 MB `Lua::total_memory()` cap — a script that grows the heap unboundedly
     is contained instead of exhausting PSRAM.
   (The mlua sandbox's `set_memory_limit`/instruction hook is mlua-only; the
   piccolo path gets the same guarantees from `Fuel` + `total_memory`.)
5. **Stack.** The Lua interpreter's own C stack needs room; give the task a
   32 KiB stack (the agent thread already uses `32 * 1024`).
6. **Wire the tested runtime.** Drive [`AppRuntime`](../host/lua-app/src/runtime.rs)
   (39-tested on the host): boot `main.lua` once, then call `tick()` from the
   worker thread and poll `tick_count()` / `uptime_ms()` / `is_stale(...)` as
   the watchdog heartbeat. The `HardwareBackend` impl (esp-hal GPIO / I2C /
   PWM / BLE / flash) is the only S3-specific code you write — the loop,
   sandbox, action dispatch, and per-tick error containment are already
   proven. Map [`nvram`](../host/lua-app/src/nvram.rs) `get`/`set` onto
   ESP-IDF NVS (or a flash partition) so `hardware.nvram_*` persists across a
   real power cycle.

## Acceptance checklist for real S3 hardware

- [ ] Firmware boots, `AT+SYSRAM?` shows large free heap (8 MB PSRAM visible).
- [ ] `lua-thread` starts and `main.lua` runs without error.
- [ ] `hardware.gpio_write/read` toggles a real LED; `sensor_read` reads a real
      sensor; `flash_write/read` round-trips through NVS.
- [ ] `agent.reason()` returns a DeepSeek decision over Wi‑Fi and the script
      applies `SET_COOLING:*`.
- [ ] `os.execute`, `io.open`, and `while true do end` are all rejected /
      stopped without a device reset (heartbeat stays fresh).

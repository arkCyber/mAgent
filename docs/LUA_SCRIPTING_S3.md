# Lua scripting on the ESP32‑S3 — two-stage integration plan

The `host/lua-app` crate (`magent-lua`) already provides a **host-validated,
aerospace-grade Lua 5.4 host** with `hardware.*` and `agent.reason()` bound
onto `magent-hal` / `magent-core`. This document is the second stage: wiring
the *same* bindings into `firmware/esp32-app` (S3).

> **Status (2026-09):** the S3 Lua wiring lives in
> `firmware/esp32-app/src/lua_task.rs`, gated behind a separate **`lua` Cargo
> feature** (OFF by default). It is off not because of the build but because the
> Lua VM **crashes Core0 on hardware** (see `firmware/esp32-app/build-s3.sh`).
> `mlua` cannot build for Xtensa ("don't know how to build Lua for
> xtensa-esp32s3-espidf"), so the S3 Lua VM uses a **pure-Rust engine**,
> `piccolo`, integrated as the `piccolo` Cargo feature of `magent-lua`.
> **The S3 Lua path is build-verified end-to-end**:
> `cargo +esp build --target xtensa-esp32s3-espidf --no-default-features
> --features board-s3,wifi,uart,lua --release` compiles the firmware with
> `PiccoloVm` (pure-Rust `piccolo`, **no `mlua`**) + `Esp32Hardware` +
> `AppRuntime<PiccoloVm>` into an Xtensa ELF. `mlua` is an optional feature of
> `magent-lua` (default on); the firmware `lua` feature builds `magent-lua` with
> `default-features = false, features = ["piccolo"]`. `PiccoloVm` covers the
> full `HardwareBackend` surface + `agent.reason` + `call`/`has`, and the whole
> runtime is host-verified (mlua 61 tests, mlua+piccolo 67, pure-piccolo 6).
> Remaining: **root-cause the on-device Core0 crash** (see `build-s3.sh`), then
> real S3 board validation (the `Esp32Hardware` drivers and the on-device Lua
> app). Enabling `lua` without `board-s3` is now a compile error in `main.rs`,
> so a C61 image cannot silently start the Lua host.
>
> The Lua thread now logs `internal_free` / `psram_free` / `stack_min_free` at
> thread start, before and after `PiccoloVm` construction, and after
> `main.lua` boot — a hardware boot log therefore shows the last phase reached
> and the headroom left before a crash. The boot path is also smaller: the Lua
> host shares the main agent's single Core-0 LLM worker (no second mbedTLS
> client), and the default app is passive (no flash/ADC/I2C/GPIO touch until
> an explicit `selftest()`).

## Why S3 and not C61

| Chip | Internal dynamic SRAM | PSRAM | Verdict |
|---|---|---|---|
| ESP32‑C61 | ~134 KB | 2 MB | Too tight for WiFi + agent + a Lua VM |
| **ESP32‑S3** | **~390 KB** | **2 MB quad (shipped dev board) / 8 MB octal (N8R8, `s3-8mb-psram`)** | Comfortably runs WiFi(DeepSeek) + agent + Lua |

On the S3 the firmware already uses `esp-idf-svc + std`, and `std::alloc` is
backed by the external PSRAM — 2 MB on the shipped quad dev board, 8 MB on the
N8R8 module when built with `s3-8mb-psram` — which is exactly the heap a Lua
VM needs. The quad board is the target `build-s3.sh` flashes today.

## How the pieces fit

```
┌────────────────────────────── firmware/esp32-app (S3) ─────────────────────┐
│  main()                                                                     │
│   └─ spawn "lua-thread" (FreeRTOS task, e.g. stack_size(32*1024))           │
│        └─ LuaVm::new(hardware_backend, shared_agent)                       │
│             ├─ hardware  → Esp32Hardware implements magent_lua::HardwareBackend
│             │              (ESP-IDF gpio / I2C / NVS; LEDC PWM)            │
│             ├─ agent     → Arc<Mutex<MiniAgent>> with set_llm_backend(DeepSeek)
│             │              (the firmware's existing DeepSeek HTTP client)   │
│             └─ run_script(include_bytes main.lua)  (boot-time main loop)    │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Wiring skeleton (build-verified for Xtensa; on-device run pending)

```rust
// firmware/esp32-app/src/lua_task.rs  (sketch)
// NOTE: the real firmware drives `AppRuntime<PiccoloVm>` — `LuaVm` is the
// mlua-backed engine, which cannot build for Xtensa. Shown here for shape only.
use magent_lua::{HardwareBackend, LuaVm};
use magent_core::MiniAgent;

struct Esp32Hardware { /* ESP-IDF gpio/pins, i2c bus, nvs */ }
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
> - I2C **master** on `I2C_NUM_0` (register read/write via repeated start; SDA/SCL pins are `I2C_SDA_PIN`/`I2C_SCL_PIN` constants, default GPIO5 (SDA) / GPIO4 (SCL));
> - persistent **flash** backed by ESP-IDF **NVS** (`flash_<addr:08x>` keyed blobs in the `magent_lua` namespace; NVS already initialised by `main::init_default_nvs()`).
>
> `hardware.ble_send` always returns `Err`: BLE was removed from the firmware on
> 2026-09-20 (see the *BLE removed outright* entry in `docs/BACKLOG.md`), so
> there is no GATT server to push to. It errors rather than silently dropping the
> payload, so a Lua app that depends on the channel fails loudly.
>
> The default boot script (`DEFAULT_MAIN_LUA`) is a hardware **self-test** that
> probes every wired driver inside `pcall` and prints `[lua] <driver> ok/err` to
> the console. Every `HardwareBackend` method is wired except `ble_send`.
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
# 1. Build (board-s3 + wifi + uart + lua; piccolo engine, no mlua).
#    NOTE: ./build-s3.sh does NOT enable `lua` (it builds board-s3,wifi,uart),
#    because the Lua VM currently crashes Core0 on hardware — see build-s3.sh.
#    Add `lua` to the feature list there while root-causing, or build directly:
cd firmware/esp32-app
MCU=ESP32S3 ESP_IDF_SDKCONFIG_DEFAULTS=sdkconfig.defaults \
  BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf" \
  CC=$HOME/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-gcc \
  CXX=$HOME/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-g++ \
  RUSTC_BOOTSTRAP=1 cargo +esp build --target xtensa-esp32s3-espidf \
  --no-default-features --features board-s3,wifi,uart,lua --release

# 2. Flash and monitor (install `cargo-espflash` if needed)
espflash flash --monitor target/xtensa-esp32s3-espidf/release/magent-esp32-app
```

On boot the `lua-thread` runs `DEFAULT_MAIN_LUA`, which is deliberately
**passive**:

```
[lua] app booted (passive); call selftest() to probe hardware
```

Bringing the Lua host up while Wi-Fi/TLS is still starting is the risky
window; a flash write or an ADC/I2C/GPIO reconfiguration there is exactly the
kind of operation that can fault the other core. The default app therefore
touches no peripheral at boot and defines `selftest()` instead. Upload a small
app that calls it (or call it from your own `on_tick`) once the link is up:

```lua
selftest()
```

Expected output from `selftest()`:

```
[lua] temp  ok  <die temp °C>
[lua] adc   ok  <volts on GPIO1>      # wire a divider / pot to GPIO1
[lua] pwm   ok  duty=50%              # scope GPIO1 or wire an LED/fan
[lua] i2c   ok  <1 byte from 0x38>    # only if a device (e.g. TMP102/SSD1306) is on GPIO5/GPIO4
[lua] gpio  ok  p2=1                  # measure GPIO2 high
[lua] flash ok  HELLO                 # NVS round-trip at address 0x100
```

A driver prints `err` (not a crash) when its hardware isn't wired — the
`pcall`-wrapped probe contains it. `i2c` is expected to print `err` unless a
device is present on the configured SDA/SCL pins (`I2C_SDA_PIN`/`I2C_SCL_PIN`,
default GPIO5 (SDA) / GPIO4 (SCL) — change to match your wiring). Every wired driver returns
an explicit `Err` (never a crash) when its hardware is absent; `hardware.ble_send`
is the one method that is *not* wired — it always returns `Err` because BLE was
removed from the firmware (see above).

### Updating the app without reflashing (`AT+LUAAPP`)

The boot app loads from NVS (`main.lua`) with the embedded script as fallback.
Operators can push a new app over the UART ingress with `AT+LUAAPP`:

```sh
# 1. Turn your Lua file into a sequence of AT commands (URL-safe base64,
#    chunked on UTF-8 character boundaries because one AT line is <= 256 B):
scripts/luaapp-encode.sh path/to/main.lua > upload.txt
#    → AT+LUAAPP=<b64 of chunk 0>      (replaces the stored app)
#      AT+LUAAPP=+<b64 of chunk 1>     (appends)
#      ...

# 2. Send the lines in order (e.g. `cat upload.txt > /dev/cu.usbmodem*`).
#    The firmware decodes each chunk and persists it as the boot `main.lua`.
#    AT+LUAAPP?            → +LUAAPP:<bytes>  (0 if only the embedded app)

# 3. Reset / reboot the device — it boots the new app. (`AppRuntime::reload`
#    can also hot-swap it at runtime without a reboot.)
```

The payload is URL-safe base64 (no `+`/`/`) so arbitrary Lua (commas, quotes,
newlines) survives AT argument parsing losslessly. A leading `+` on the value
is outside that alphabet and means "append this chunk"; the first (replace)
chunk must arrive before any append, and the stored source is capped at 16 KiB.
Each chunk is a read-modify-write plus NVS commit — an occasional
operator-update path, not a hot loop.

## Firmware-specific pitfalls (captured from the host build)

1. **C cross-compile.** `mlua`'s `lua54`+`vendored` feature builds the bundled
   Lua C. `build-s3.sh` already cross-compiles C deps (`secp256k1_sys`) with
   `xtensa-esp32s3-elf-gcc`, so Lua 5.4 compiles the same way. Keep `CC`/
   `CXX` pointed at the Xtensa toolchain.
2. **Thread model.** The VM + its `Arc<Mutex<MiniAgent>>` run on one dedicated
   FreeRTOS task (single-threaded, like the host). Do **not** share the VM
   across tasks, and keep `agent.reason`'s `block_on` off the RTOS tick.
3. **LLM decisions.** `start_lua_task` installs a `ChannelLlmBackend` over the
   **same single Core-0 worker the main agent created** (`llm::shared_sender`),
   so `agent.reason()` returns real decisions without a second mbedTLS client
   on Core 0. If the main agent has no model/API key (`AT+LLMCFG`), the Lua
   host uses the local heuristic. It also installs the real
   `Esp32ToolHandler` so agent tool calls drive actual GPIO / sensors.
4. **Sandbox stays on.** `LuaVm::new` enforces the memory cap and instruction
   budget on the S3 exactly as on the host; a hostile `main.lua` cannot hang
   the device. For the piccolo engine this is enforced in
   `PiccoloVm::execute_bounded` around every `run_script`/`call`:
   - a bounded `Fuel` budget (instruction budget) — `while true do end` returns a
     Lua error instead of wedging the `lua-thread`;
   - a board-sized `Lua::total_memory()` cap — 512 KiB on the quad-PSRAM dev
     board (2 MB total, shared with Wi-Fi/TLS/the agent) and 2 MiB under the
     N8R8 `s3-8mb-psram` profile — so a script that grows the heap unboundedly
     is contained instead of exhausting PSRAM. (It used to be a flat 8 MiB,
     which was above the quad board's entire pool.)
   (The mlua sandbox's `set_memory_limit`/instruction hook is mlua-only; the
   piccolo path gets the same guarantees from `Fuel` + `total_memory`.)
5. **Stack.** The Lua interpreter's own C stack needs room; give the task a
   32 KiB stack (the agent thread already uses `32 * 1024`).
6. **Wire the tested runtime.** Drive [`AppRuntime`](../host/lua-app/src/runtime.rs)
   (39-tested on the host): boot `main.lua` once, then call `tick()` from the
   worker thread and poll `tick_count()` / `uptime_ms()` / `is_stale(...)` as
   the watchdog heartbeat. The `HardwareBackend` impl (ESP-IDF gpio / I2C /
   LEDC PWM / NVS flash) is the only S3-specific code you write — the loop,
   sandbox, action dispatch, and per-tick error containment are already
   proven. Map [`nvram`](../host/lua-app/src/nvram.rs) `get`/`set` onto
   ESP-IDF NVS (or a flash partition) so `hardware.nvram_*` persists across a
   real power cycle.

## Acceptance checklist for real S3 hardware

- [ ] Firmware boots, `AT+SYSRAM?` shows the expected free heap (2 MB PSRAM on the quad dev board; 8 MB on N8R8 with `s3-8mb-psram`).
- [ ] `lua-thread` starts and `main.lua` runs without error.
- [ ] `hardware.gpio_write/read` toggles a real LED; `sensor_read` reads a real
      sensor; `flash_write/read` round-trips through NVS.
- [ ] `agent.reason()` returns a DeepSeek decision over Wi‑Fi and the script
      applies `SET_COOLING:*`.
- [ ] `os.execute`, `io.open`, and `while true do end` are all rejected /
      stopped without a device reset (heartbeat stays fresh).

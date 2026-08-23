# ESP32-C61 Build & Operations Guide

TRACE: REQ-DOC-001 — this document is the single source of truth for
building the mAgent firmware on the **ESP32-C61-DevKitC-1-N8R2** board
(N = 8 MB Flash, R = 2 MB PSRAM). Earlier revisions of this document
referenced a non-existent `tick_hal` task and the wrong RISC-V target
(`riscv32imc`); both have been removed. If you find a lingering reference
to `tick_hal` or `riscv32imc` in the source tree, that is a bug — see
`docs/SRS.md` REQ-DOC-001.

## Why we chose ESP-IDF (not bare-metal esp-hal)

The ESP32-C61 is a RISC-V device with **2 MB of PSRAM** soldered onto the
dev kit. That extra memory is decisive:

* ESP-IDF brings a battle-tested Wi-Fi + mbedTLS + event-loop stack
  written in C, which is far smaller than porting everything to Rust.
* `esp-idf-svc 0.52` (paired with `esp-idf-sys 0.37`) ships first-class
  Rust wrappers for ESP-IDF v6.0 and **fully supports the C61**.
* We can keep the chip-agnostic `magent-core` in `no_std` form **and**
  link the firmware binary against `std::alloc`, which is now backed by
  the 2 MB PSRAM (`CONFIG_SPIRAM_USE_MALLOC=y`).

The trade-off: the build needs the ESP-IDF SDK installed locally. The
`esp-idf` toolchain is installed by `espup`; see step 1 below.

## 1. Toolchain setup

```bash
# 1.1 Install espup (one-time)
cargo install espup --locked
espup install

# 1.2 Source the env file every new shell session
source ~/export-esp.sh

# 1.3 Install Rust target for the C61
rustup target add riscv32imac-unknown-none-elf

# 1.4 Install espflash (flashing / monitoring)
cargo install espflash --locked
```

The C61 is a single-core RISC-V with the `imac` extension (hardware CAS).
This is critical: do **not** use the `riscv32imc-unknown-none-elf` target
(the old ESP32-C3 / C6 default) — that target lacks atomic-CAS and would
force `embassy-executor` to fall back to single-core atomic emulation, which
hides data races the C61 hardware can actually detect.

## 2. SDK configuration

`firmware/esp32-app/sdkconfig.defaults` (committed alongside the firmware
crate) is the single source of truth for PSRAM, Wi-Fi, TLS, and OTA. The
most important keys:

```text
# PSRAM (N8R2 variant)
CONFIG_SPIRAM=y
CONFIG_SPIRAM_TYPE_AUTO=y
CONFIG_SPIRAM_USE_MALLOC=y
CONFIG_SPIRAM_BOOT_INIT=y

# Wi-Fi
CONFIG_ESP_WIFI_ENABLED=y
CONFIG_ESP_WIFI_STATIC_RX_BUFFER_NUM=10
CONFIG_ESP_WIFI_DYNAMIC_RX_BUFFER_NUM=32

# TLS (mbedTLS, no rustls)
CONFIG_ESP_TLS_USING_MBEDTLS=y

# OTA with rollback
CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y
CONFIG_OTA_ALLOW_HTTP=y
```

After the first `cargo build`, the esp-idf-svc build script will generate
`sdkconfig` from `sdkconfig.defaults`. Re-run `cargo build` if you change
either file.

## 3. Build

```bash
cargo build -p magent-esp32-app --release
```

The build invokes `build.rs` (which delegates to `embuild::build()`) and
links against the ESP-IDF binaries installed under `~/esp/esp-idf/`. The
result is a `target/riscv32imac-esp-espidf/release/magent-esp32-app`
binary.

## 4. Flash & monitor

```bash
cargo run -p magent-esp32-app --release
```

This will:

1. Build the firmware.
2. Convert the ELF to the ESP32 boot image format.
3. Flash it over USB.
4. Open a serial monitor at 115 200 baud.

On first boot you should see something like:

```
I (312) boot:  ESP-IDF v6.0  2nd stage bootloader
I (412) cpu_start:  Pro cpu start user code
I (530) spiram:  SPI RAM enabled, 2 MB at 0x3C000000
I (612) wifi:  wifi driver task: 0x3fc8a1c0, prio:23, stack:6144
I (740) net:  got ip: 192.168.1.42
I (812) magent:  ingress gateway online
```

## 5. What's wired up

The default `firmware/esp32-app/src/main.rs` builds a working firmware that:

1. Brings up the SoC (clocks, GPIO, timer).
2. Installs the ESP-IDF event loop and the global `EspLogger`.
3. Connects to the Wi-Fi STA network (credentials are read from NVS; flash
   them with `espflash write-nvs --partition-table ...`).
4. Spawns two threads:
   * `agent-thread` — drives the chip-agnostic `MiniAgent` ReAct loop.
   * `ingress-thread` — drains UART/SPI/Button adapters via
     `IngressGateway` (see `firmware/esp32-app/src/link_adapters.rs`).

## 6. Common issues

### `error: 'riscv32imac' is not a recognized target`

```bash
rustup target add riscv32imac-unknown-none-elf
```

### `linker 'xtensa-esp-elf-gcc' not found`

You forgot to `source ~/export-esp.sh` (or the equivalent path on your
system). Run `espup install` and source the env file it prints at the
end.

### `failed to select a version for esp-idf-svc`

The SDK moves fast. If `cargo build` complains about `esp-idf-svc` /
`esp-idf-sys` versions, bump the entries in
`firmware/esp32-app/Cargo.toml` together — they need to stay in lockstep.

### `heap corruption on connect`

Increase `CONFIG_ESP_MAIN_TASK_STACK_SIZE` in `sdkconfig.defaults`. The
default is 16 KiB; the Wi-Fi event handler can recurse deeply on join.

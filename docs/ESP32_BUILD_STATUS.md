# ESP32 Build Status

> **Last updated**: 2026-09-14

## Current State

Both ESP32 targets in this repository are **buildable and hardware-verified**:

| Target | Board feature | Rust target | Toolchain | Status |
|--------|---------------|-------------|-----------|--------|
| **ESP32-C61** | `board-c61` (default) | `riscv32imac-esp-espidf` | `esp` (espup) via `cargo +esp` / `RUSTC_BOOTSTRAP=1` | ✅ Builds; verified on HW |
| **ESP32-S3** | `board-s3` | `xtensa-esp32s3-espidf` | `esp` (espup, Xtensa LLVM) via `cargo +esp` | ✅ Builds; verified stable on HW |

The ESP32 std targets (`riscv32imac-esp-espidf`, `xtensa-esp32s3-espidf`, …)
are **not** part of upstream Rust — they ship with Espressif's `esp` toolchain.

## Required toolchain

```bash
cargo install espup --locked
espup install                 # installs the `esp` toolchain + targets
source ~/export-esp.sh        # activate it in every new shell
rustup toolchain list         # should list: esp
```

### Historical note (toolchain install)

Early bring-up hit network/proxy failures during `espup install`
(`Failed to Install RISC-V targets`). Workarounds that resolve it:

```bash
# Option 1: clear proxies and retry
unset all_proxy socks5_proxy https_proxy http_proxy
espup install

# Option 2: official Espressif Docker image (nice on macOS)
docker run -it espressif/idf-rust:latest

# Option 3: build on Linux / WSL
```

## Build commands

```bash
# ESP32-C61 (default board)
cd firmware/esp32-app
source ~/export-esp.sh
./build-c61.sh                # → target/riscv32imac-esp-espidf/release/magent-esp32-app

# ESP32-S3 (Xtensa)
cd firmware/esp32-app
source ~/export-esp.sh
./build-s3.sh                 # → target/xtensa-esp32s3-espidf/release/magent-esp32-app
```

Both scripts wrap the exact env the ESP toolchain needs (`MCU`, `CC`/`CXX`,
`BINDGEN_EXTRA_CLANG_ARGS`, board sdkconfig). See
[`../firmware/esp32-app/BOARDS.md`](../firmware/esp32-app/BOARDS.md) and
[`ESP32.md`](ESP32.md) for details.

## What works now

- ✅ `magent-core` host tests (1200+ across unit + integration targets)
- ✅ `magent` CLI, `magent-simulator`, `magent-tools`, MCP executors
- ✅ `magent-esp32-app` for **both** C61 and S3
- ✅ ESP32-C61 on-HW: AT/UART, Wi-Fi, BLE, HTTP, TLS, OTA, web_admin
- ✅ ESP32-S3 on-HW: Wi-Fi STA, HTTP dashboard, DeepSeek cloud LLM, PSRAM

## Build/tool warnings

- **`patch was not used in the crate graph`** — expected; the patched crate
  versions aren't always selected by the resolver.
- **TLS on 32-bit RISC-V** — the build uses `aws-lc-rs` instead of `ring`
  (`ring` does not build cleanly for the ESP RISC-V targets).

## Flashing / monitoring

```bash
cd firmware/esp32-app
./flash-c61.sh                    # C61: bootloader + partitions + app
./flash-s3.sh --port /dev/cu.usbserial-XXX
espflash monitor                  # serial console @ 115200
```

> **Note**: on the C61, flash through the **CP2102 USB-UART bridge**, not the
> native USB-JTAG (see
> [`ESP32_C61_BOARD_BOOT_FAILURE.md`](ESP32_C61_BOARD_BOOT_FAILURE.md)).

## GCC tools installed (this host)

- `riscv32-esp-elf-gcc`
- `xtensa-esp32s3-elf-gcc` (in `~/.platformio/packages/toolchain-xtensa-esp-elf/`)
- ESP-IDF v6.0 SDK (via PlatformIO / `~/.espressif/esp-idf/`)

## Related documentation

- [ESP32-C61 Build Guide](ESP32_C61_BUILD.md)
- [ESP32-C61 Build & Operations](ESP32.md)
- [ESP32-S3 Port Success](ESP32_S3_PORT_SUCCESS.md)
- [Dual-Core Scheduling (S3)](DUAL_CORE_SCHEDULING.md)
- [Tier-6 On-Hardware Validation](TIER6_VALIDATION.md)
- [Main README](../README.md)

# mAgent ESP32 Firmware (ESP32-C61 + ESP32-S3)

Rust firmware for Espressif ESP32 boards running the mAgent embedded AI system.
One source tree, two compile-time board targets selected by Cargo features
(`board-c61` default / `board-s3`). See [BOARDS.md](BOARDS.md) for the full
switch matrix.

| Board | Feature | Target | Flash | PSRAM | Status |
|-------|---------|--------|-------|-------|--------|
| **ESP32-C61** (default) | `board-c61` | `riscv32imac-esp-espidf` | 8 MB | 2 MB | ✅ verified on HW |
| **ESP32-S3** (quad dev board, shipped) | `board-s3` | `xtensa-esp32s3-espidf` | 4 MB | 2 MB quad | ✅ verified on HW (flagship) |
| **ESP32-S3-WROOM-1-N8R8** (octal) | `board-s3,s3-8mb-psram` | `xtensa-esp32s3-espidf` | 8 MB | 8 MB octal | build + `sdkconfig.s3.defaults`; ceiling profile 4 MiB/2 MiB |

## Building

### Prerequisites

1. **ESP toolchain** (`espup` provides the `esp` Rust toolchain with the
   RISC-V and Xtensa targets):
```bash
cargo install espup --locked
espup install
source ~/export-esp.sh        # activate in every new shell
```

2. **ESP-IDF toolchain** (via PlatformIO — installed automatically by the build)

3. **espflash** (flashing / monitoring):
```bash
cargo install espflash
```

### Build Commands

**ESP32-C61 (default board)** — use the helper script, which swaps the C61
sdkconfig in and restores the S3 one afterwards:
```bash
cd firmware/esp32-app
./build-c61.sh
# Output: target/riscv32imac-esp-espidf/release/magent-esp32-app
```

**ESP32-S3 (Xtensa)** — requires the `esp` toolchain (Xtensa LLVM backend):
```bash
cd firmware/esp32-app
./check-s3.sh            # compile-only check (no flash)
./check-s3.sh --lua      # also compile the (still unstable) Lua host
./check-s3.sh --octal8mb # N8R8 8 MB octal profile
./build-s3.sh            # release build for the quad-PSRAM dev board
./build-s3.sh --octal8mb # N8R8 8 MB octal module (sdkconfig swapped automatically)
# Output: target/xtensa-esp32s3-espidf/release/magent-esp32-app
```

> **Note**: the raw `cargo build` path needs the board-specific environment
> (`MCU`, `ESP_IDF_SDKCONFIG_DEFAULTS`, `CC`/`CXX`,
> `BINDGEN_EXTRA_CLANG_ARGS`). The `build-*.sh` scripts set all of it — prefer
> them over invoking cargo by hand. On the C61, `MCU=ESP32C61` is required
> because ESP-IDF v6 tools don't yet know about ESP32-C61 (it shares the
> rv32imac architecture with the ESP32-C6).

## 📋 Hardware

### ESP32-C61
- **Board**: ESP32-C61-DevKitC-1-N8R2
- **MCU**: ESP32-C61 (RISC-V 32-bit @ 160 MHz)
- **RAM**: 320 KB SRAM + 2 MB PSRAM (N8R2 in-package)
- **Flash**: 8 MB
- **Connectivity**: Wi-Fi 6 (802.11ax), BLE 5 (LE)

### ESP32-S3
- **Board**: ESP32-S3-WROOM-1 (4 MB flash variant used here; 2 MB quad PSRAM, per `sdkconfig.defaults`)
- **MCU**: ESP32-S3 (dual-core Xtensa LX7 @ 240 MHz)
- **RAM**: 512 KB SRAM + 2 MB quad PSRAM (~2.18 MB free heap after boot)
- **8 MB N8R8 variant**: octal PSRAM; build with `--features board-s3,s3-8mb-psram` and `sdkconfig.s3.defaults`
- **Connectivity**: Wi-Fi 2.4 GHz, BLE 5 (LE); DeepSeek cloud LLM + HTTP dashboard
- **Notes**: BLE and Lua are disabled in the default S3 build (see BOARDS.md)

## 🛠️ Architecture

### Build System

The ESP32 firmware builds with the Espressif `esp` toolchain (`cargo +esp`)
and the following configuration:

| Component | Version | Purpose |
|-----------|---------|---------|
| Rust | `esp` toolchain (espup) | RISC-V **and** Xtensa backends |
| ESP-IDF | v6.0 | Via PlatformIO framework |
| Target (C61) | `riscv32imac-esp-espidf` | 32-bit RISC-V |
| Target (S3) | `xtensa-esp32s3-espidf` | Xtensa LX7 |
| Driver | `pio` | PlatformIO ESP-IDF integration |

### Key Features

- **Board feature switch**: one source tree builds the C61 (`board-c61`) or S3 (`board-s3`)
- **PlatformIO Driver**: `esp-idf-sys` uses PlatformIO's pre-built ESP-IDF v6.0 toolchain
- **magent-core Integration**: ESP32-specific feature `esp32` enables RISC-V HAL support
- **Web3 Support**: `web3` feature provides Ed25519 identity and signing (no reqwest/ring)
- **Ingress Gateway**: UART/RS232/SPI adapters for device communication
- **S3 extras**: DeepSeek cloud LLM backend, `web_admin` HTTP dashboard, dual-core RT scheduling

### magent-core Features

The firmware enables these `magent-core` features:

| Feature | Description |
|---------|-------------|
| `esp32` | RISC-V architecture support, defmt logging |
| `link_adapters` | UART/RS232/SPI ingress adapters |
| `ingress` | Signed message envelopes (Ed25519) |
| `web3` | Identity and signing (no reqwest) |

### Patches Applied

The following patches are applied automatically:

1. **esp-idf-sys-0.37.2**: `spi_transaction_t` layout fix, `timeval`/`timespec` type fixes
2. **esp-idf-svc-0.52.1**: Mutex `tv_nsec` cast fix
3. **esp-idf-hal-0.46.2**: SPI transaction buffer access
4. **rustls patches**: `aws-lc-rs` instead of `ring` (32-bit RISC-V compatibility)

## 📊 Binary Information

| Metric | Debug | Release |
|--------|-------|---------|
| Binary Size | ~25 MB | ~2-3 MB |
| Flash Usage | ~500 KB | ~200 KB |
| RAM Usage | ~300 KB | ~260 KB |

> **Note**: Debug builds include full symbols and are ~25 MB. Release builds are stripped to ~2-3 MB.

### Memory Layout (Release)
```
Flash Sections:
  .iram0.text      48,712 bytes  (Fast instruction RAM)
  .flash.text      74,522 bytes  (Flash code)
  .flash.rodata    37,508 bytes  (Read-only data)
  
RAM Sections:
  .dram0.data       7,292 bytes  (Initialized data)
  .dram0.bss        3,920 bytes  (Uninitialized data)
```

## 🔍 Verification

```bash
# Check architecture
file target/riscv32imac-esp-espidf/release/magent-esp32-app
# Expected: ELF 32-bit LSB executable, UCB RISC-V, RVC, soft-float ABI

# Detailed size analysis
riscv32-esp-elf-size -A target/riscv32imac-esp-espidf/release/build/esp-idf-sys-*/out/esp-idf/.pio/build/release/firmware.elf
```

## 📝 Configuration Files

### Key Files
- `Cargo.toml` - Dependencies and features (incl. `board-c61` / `board-s3`)
- `rust-toolchain.toml` - Pinned stable host toolchain (the S3 build overrides it with `cargo +esp`)
- `.cargo/config.toml` - Build target and linker settings
- `sdkconfig.defaults` - ESP-IDF configuration (S3 quad-PSRAM dev board)
- `sdkconfig.c61.defaults` - ESP-IDF configuration for the ESP32-C61
- `sdkconfig.s3.defaults` - ESP-IDF overlay for the S3 N8R8 (8 MB octal) module
- `build.rs` - Custom build script (linker script patching, Xtensa DROM fix)
- `check-s3.sh` - Cross-target S3 compile check (`--lua`, `--octal8mb`)
- `qemu-s3.sh` - Build + run the S3 firmware under QEMU (`esp32s3`), PSRAM off by default; `--gdb` pauses at reset with a GDB server
- `build-c61.sh` / `build-s3.sh` - Board build wrappers
- `flash-c61.sh` / `flash-s3.sh` / `flash-secure.sh` - Flashing wrappers
- `partitions*.csv` - Partition tables (default / OTA / S3)

### Features
```toml
[features]
default = ["board-c61", "wifi", "uart"]
board-c61 = ["esp-println/esp32c6"]        # ESP32-C61 (RISC-V) board selection
board-s3  = ["esp-println/esp32s3"]        # ESP32-S3 (Xtensa) chip selection
s3-8mb-psram = ["magent-core/s3-8mb-psram"] # N8R8 8 MB octal module only
wifi = []          # Wi-Fi support
ota  = []          # Over-the-air updates (C61, dual-slot)
uart = []          # UART adapter
lua  = ["dep:magent-lua"]  # Lua app host (S3 only; pure-Rust piccolo, no mlua)
# There is deliberately no `ble` feature — BLE was removed 2026-09-20.
```

## 🐛 Troubleshooting

### Build Issues

**Problem**: `patch was not used in the crate graph` warnings
**Solution**: These warnings indicate patches were created for versions not currently used. The actual rustls version in use is being resolved from the registry. This is expected behavior.

**Problem**: `unknown type name 'crypto_word_t'` or `ring` compilation errors
**Solution**: This has been fixed. The build now uses `aws-lc-rs` instead of `ring` for TLS on 32-bit RISC-V targets. If it recurs, check that the `web3` feature is enabled without `std`.

**Problem**: Linker errors with `cc` instead of `riscv32-esp-elf-gcc`
**Solution**: Ensure the workspace `.cargo/config.toml` has the correct `linker` setting for `riscv32imac-esp-espidf` target.

**Problem**: ESP-IDF bindgen fails
**Solution**: Ensure ESP-IDF v6 tools are installed via PlatformIO. The build uses `pio` driver which auto-manages ESP-IDF.

### Flash Issues

**Problem**: Device not found
**Solution**: Check USB connection and list ports:
```bash
ls /dev/cu.usbserial-* /dev/cu.usbmodem*
```

**Problem**: Permission denied (Linux)
**Solution**: Add user to dialout group:
```bash
sudo usermod -a -G dialout $USER
```

## 🖥️ Serial Output Example

After successful boot:
```
ESP-ROM:esp32c6-20220919
Build:Sep 19 2022
rst:0x1 (POWERON),boot:0xc (SPI_FAST_FLASH_BOOT)

[INFO] mAgent ESP32-C61 v0.1.0 starting...
[INFO] Initializing NVS...
[INFO] Initializing WiFi...
[agent] thread starting
[agent] MiniAgent configured: max_iterations=20, max_memory=524288
[ingress] thread starting
[ingress] IngressGateway initialized
```

## 🏗️ Project Structure

```
firmware/esp32-app/
├── Cargo.toml                    # Manifest (board-c61 / board-s3 features)
├── build.rs                      # Build script (linker/segment patching)
├── rust-toolchain.toml           # Toolchain config
├── sdkconfig.defaults            # ESP-IDF config (S3 quad-PSRAM dev board)
├── sdkconfig.c61.defaults        # ESP-IDF config for ESP32-C61
├── sdkconfig.s3.defaults         # S3 N8R8 octal-PSRAM overlay
├── partitions*.csv               # Partition tables (default / OTA / S3)
├── check-s3.sh                   # S3 cross-target compile check
├── qemu-s3.sh                    # S3 QEMU build/run (no hardware)
├── build-c61.sh · build-s3.sh    # Board build wrappers
├── flash-c61.sh · flash-s3.sh · flash-secure.sh
├── BOARDS.md                     # Board / feature switch matrix
├── .cargo/
│   └── config.toml               # Build target and linker
└── src/
    ├── main.rs                   # Application entry point
    ├── at_dispatch.rs            # AT (ESP-AT subset) parser + handlers
    ├── link_adapters.rs          # UART/RS232/SPI/GPIO adapters
    ├── llm.rs                    # DeepSeek backend + cross-core LLM worker
    ├── local_tools.rs            # On-device GPIO / temperature tool handler
    ├── web_admin.rs              # HTTP admin dashboard + /api/status
    ├── ota.rs                    # Over-the-air update (C61, dual-slot)
    ├── core_affinity.rs          # S3 FreeRTOS core + priority profiles
    ├── rt_watchdog.rs            # S3 real-time watchdog isolation
    ├── latency_metrics.rs        # WCET / latency channels
    ├── lua_task.rs               # S3 Lua app host (optional `lua` feature)
    ├── ble_*.rs                  # BLE AT / config / GATT / wallet
    ├── device_key.rs · sntp_sync.rs · ping.rs · blockchain_transport.rs
    └── sysenv_stubs.c            # POSIX compatibility stubs
```

## 🔧 Technical Details

### Build Pipeline
1. Rust compilation via stable `rustc` with `-Z build-std` for tier-3 target
2. ESP-IDF component build via PlatformIO (`pio` driver)
3. C stub compilation (`sysenv_stubs.c`) for POSIX compatibility
4. Final linking with `riscv32-esp-elf-gcc`

### Key Fixes Applied

| Issue | Fix |
|-------|-----|
| `spi_transaction_t` layout | Patched `esp-idf-sys` to use direct `tx_buffer`/`rx_buffer` fields |
| `timespec.tv_nsec` type | Added explicit i64→i32 cast in mutex.rs |
| 32-bit RISC-V TLS | Patched `rustls` to use `aws-lc-rs` instead of `ring` |
| Web3 without reqwest | Separated `web3` feature from `std` requirement |

### Toolchain

| Component | Version |
|-----------|---------|
| Rust | Espressif `esp` toolchain (installed by `espup`) |
| ESP-IDF | v6.0 (via PlatformIO) |
| GCC (C61) | riscv32-esp-elf-gcc 15.2.0 |
| GCC (S3) | xtensa-esp32s3-elf-gcc |
| Target (C61) | `riscv32imac-esp-espidf` (rv32imac) |
| Target (S3) | `xtensa-esp32s3-espidf` (Xtensa LX7) |

## 📚 Documentation

- **Main README**: [../../README.md](../../README.md)
- **Firmware Overview**: [../README.md](../README.md)
- **Board Matrix**: [BOARDS.md](BOARDS.md)
- **ESP32-C61 Build Guide**: [../../docs/ESP32_C61_BUILD.md](../../docs/ESP32_C61_BUILD.md)
- **ESP32-C61 Build & Operations**: [../../docs/ESP32.md](../../docs/ESP32.md)
- **ESP32-S3 Port Success**: [../../docs/ESP32_S3_PORT_SUCCESS.md](../../docs/ESP32_S3_PORT_SUCCESS.md)
- **Dual-Core Scheduling (S3)**: [../../docs/DUAL_CORE_SCHEDULING.md](../../docs/DUAL_CORE_SCHEDULING.md)
- **Lua Scripting on S3**: [../../docs/LUA_SCRIPTING_S3.md](../../docs/LUA_SCRIPTING_S3.md)
- **Tier-6 On-HW Validation**: [../../docs/TIER6_VALIDATION.md](../../docs/TIER6_VALIDATION.md)

## 🔗 External Resources

- [ESP-IDF Programming Guide](https://docs.espressif.com/projects/esp-idf/en/latest/)
- [esp-rs Book](https://esp-rs.github.io/book/)
- [PlatformIO ESP-IDF](https://docs.platformio.org/en/latest/frameworks/esp-idf.html)

---

**Status**: ✅ Production Ready — verified on ESP32-C61-DevKitC-1-N8R2 and ESP32-S3-WROOM-1
**Last Updated**: September 14, 2026

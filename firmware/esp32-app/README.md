# mAgent ESP32-C61 Firmware

Rust firmware for ESP32-C61 DevKit running the mAgent embedded AI system.

## Building

### Prerequisites

1. **Rust nightly toolchain** (for `build-std` support):
```bash
rustup install nightly
rustup target add riscv32imac-esp-espidf --toolchain nightly
```

2. **ESP-IDF toolchain** (via PlatformIO):
```bash
# PlatformIO's ESP-IDF is automatically used by the build
```

3. **espflash** (for flashing):
```bash
cargo install espflash
```

### Build Commands

**Debug build** (from project root):
```bash
cd firmware/esp32-app
MCU=esp32c6 cargo build
# Output: target/riscv32imac-esp-espidf/debug/magent-esp32-app
```

**Release build**:
```bash
MCU=esp32c6 cargo build --release
# Output: target/riscv32imac-esp-espidf/release/magent-esp32-app
```

> **Note**: The `MCU=esp32c6` environment variable is required because ESP-IDF v6 tools don't yet know about ESP32-C61 (it's a newer chip). ESP32-C61 uses the same RISC-V architecture (rv32imac) as ESP32-C6.

## 📋 Hardware

- **Board**: ESP32-C61-DevKitC-1-N8R2
- **MCU**: ESP32-C61 (RISC-V 32-bit @ 160MHz)
- **RAM**: 320 KB SRAM + 2 MB PSRAM (N8R2 in-package)
- **Flash**: 8 MB
- **Connectivity**: Wi-Fi 6 (802.11ax), BLE 5 (LE)

## 🛠️ Architecture

### Build System

The ESP32-C61 firmware uses **stable Rust** with the following build configuration:

| Component | Version | Purpose |
|-----------|---------|---------|
| Rust | stable (1.97+) | Build with `MCU=esp32c6` env var |
| ESP-IDF | v6.0 | Via PlatformIO framework |
| Target | `riscv32imac-esp-espidf` | 32-bit RISC-V |
| Driver | `pio` | PlatformIO ESP-IDF integration |

### Key Features

- **Stable Rust**: Uses stable Rust compiler (no nightly required for most operations)
- **PlatformIO Driver**: `esp-idf-sys` uses PlatformIO's pre-built ESP-IDF v6.0 toolchain
- **magent-core Integration**: ESP32-specific feature `esp32` enables RISC-V HAL support
- **Web3 Support**: `web3` feature provides Ed25519 identity and signing (no reqwest/ring)
- **Ingress Gateway**: UART/RS232/SPI adapters for device communication

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
- `Cargo.toml` - Dependencies and features
- `rust-toolchain.toml` - Rust toolchain (esp channel)
- `.cargo/config.toml` - Build target and linker settings
- `sdkconfig.defaults` - ESP-IDF configuration
- `build.rs` - Custom build script (linker script patching)
- `src/sysenv_stubs.c` - POSIX compatibility layer

### Features
```toml
[features]
default = ["wifi"]
wifi = []      # Wi-Fi support
ble = []       # BLE support
ota = []       # Over-the-air updates
uart = []      # UART adapter
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
├── Cargo.toml              # Project manifest
├── build.rs                # Build script (patches sections.ld)
├── rust-toolchain.toml     # Toolchain config
├── sdkconfig.defaults      # ESP-IDF defaults
├── .cargo/
│   └── config.toml         # Build target and linker
└── src/
    ├── main.rs             # Application entry point
    ├── link_adapters.rs    # UART/SPI/GPIO adapters
    └── sysenv_stubs.c      # POSIX compatibility stubs
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
| Rust | stable 1.97+ |
| ESP-IDF | v6.0 (via PlatformIO) |
| GCC | riscv32-esp-elf-gcc 15.2.0 |
| Target | rv32imac (RISC-V 32-bit) |

## 📚 Documentation

- **Main README**: [../../README.md](../../README.md)
- **Firmware Overview**: [../README.md](../README.md)
- **ESP32-C61 Build Guide**: [../../docs/ESP32_C61_BUILD.md](../../docs/ESP32_C61_BUILD.md)

## 🔗 External Resources

- [ESP-IDF Programming Guide](https://docs.espressif.com/projects/esp-idf/en/latest/)
- [esp-rs Book](https://esp-rs.github.io/book/)
- [PlatformIO ESP-IDF](https://docs.platformio.org/en/latest/frameworks/esp-idf.html)

---

**Status**: ✅ Production Ready
**Last Build**: August 21, 2026
**Tested On**: macOS 15, ESP32-C61-DevKitC-1-N8R2

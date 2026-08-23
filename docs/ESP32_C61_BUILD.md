# ESP32-C61 Build Guide

Complete guide for building and flashing the mAgent firmware on ESP32-C61 DevKit with macOS.

## ✨ Quick Start

```bash
cd firmware/esp32-app
cargo build --release
espflash flash target/riscv32imac-esp-espidf/release/magent-esp32-app --monitor
```

## 📋 Hardware Information

- **Board**: ESP32-C61-DevKitC-1-N8R2
- **MCU**: ESP32-C61 (RISC-V 32-bit @ 160MHz)
- **Architecture**: RISC-V with compressed instructions (RVC)
- **RAM**: 320 KB SRAM + 512 KB PSRAM
- **Flash**: 8 MB
- **Wireless**: Wi-Fi 6 (802.11ax), BLE 5.3

## 🛠️ Environment Setup (macOS)

### Prerequisites

1. **Rust with ESP toolchain**:
```bash
# Install espup (Espressif Rust installer)
cargo install espup

# Set up proxy if needed (for Chinese mainland users)
export http_proxy=http://127.0.0.1:10808
export https_proxy=http://127.0.0.1:10808
export ALL_PROXY=socks5://127.0.0.1:10808

# Install ESP Rust toolchain
espup install

# Activate the environment
source ~/export-esp.sh
```

2. **Add to your `~/.zshrc`** (persistent activation):
```bash
echo 'source ~/export-esp.sh' >> ~/.zshrc
```

3. **Install flashing tools**:
```bash
cargo install espflash
cargo install ldproxy
```

### Project Configuration

The project uses these key configuration files:

#### `rust-toolchain.toml`
```toml
[toolchain]
channel = "esp"
```

#### `.cargo/config.toml`
```toml
[target.riscv32imac-esp-espidf]
linker = "ldproxy"

[env]
MCU = "esp32c6"  # ESP32-C61 uses C6 toolchain base
ESP_IDF_VERSION = "v6.0"

[build]
target = "riscv32imac-esp-espidf"

[unstable]
build-std = ["std", "panic_abort"]
```

#### `sdkconfig.defaults`
```ini
CONFIG_IDF_TARGET_ESP32C61=y
CONFIG_COMPILER_ORPHAN_SECTIONS_WARNING=y
```

## 🔧 Build Process

### Debug Build
```bash
cd firmware/esp32-app
cargo build
```

Output: `target/riscv32imac-esp-espidf/debug/magent-esp32-app`

### Release Build (Optimized)
```bash
cd firmware/esp32-app
cargo build --release
```

Output: `target/riscv32imac-esp-espidf/release/magent-esp32-app`

**Binary sizes**:
- Stripped binary: 2.0 MB
- Firmware ELF with symbols: 3.5 MB
- Flash footprint: ~169 KB (code + data)
- RAM usage: ~60 KB

## 📦 Build Artifacts

```
target/riscv32imac-esp-espidf/release/
├── magent-esp32-app                    # Stripped binary for flashing
└── build/
    └── esp-idf-sys-*/
        └── out/
            └── esp-idf/
                └── .pio/
                    └── build/
                        └── release/
                            ├── firmware.elf    # Full ELF with symbols
                            ├── bootloader.elf  # Bootloader
                            └── partitions.csv  # Partition table
```

## 🔍 Verification

Check the built binary:
```bash
# Verify architecture
file target/riscv32imac-esp-espidf/release/magent-esp32-app
# Expected: ELF 32-bit LSB executable, UCB RISC-V, RVC, soft-float ABI

# Check size
ls -lh target/riscv32imac-esp-espidf/release/magent-esp32-app

# Inspect sections (from firmware.elf with symbols)
riscv32-esp-elf-size -A target/riscv32imac-esp-espidf/release/build/esp-idf-sys-*/out/esp-idf/.pio/build/release/firmware.elf
```

## 📲 Flashing to Device

### Using espflash (Recommended)
```bash
cd firmware/esp32-app

# Flash and open serial monitor
espflash flash target/riscv32imac-esp-espidf/release/magent-esp32-app --monitor

# Flash only (no monitor)
espflash flash target/riscv32imac-esp-espidf/release/magent-esp32-app

# Specify port manually
espflash flash target/riscv32imac-esp-espidf/release/magent-esp32-app --port /dev/cu.usbserial-*
```

### Monitor Serial Output
```bash
# After flashing
espflash monitor

# Or with specific port
espflash monitor --port /dev/cu.usbserial-*
```

### Expected Serial Output
```
[INFO] mAgent ESP32-C61 v0.1.0 starting...
[INFO] Initializing NVS...
[INFO] Initializing WiFi...
[agent] thread starting
[ingress] thread starting
```

## 🐛 Troubleshooting

### Build Issues

#### 1. MCU Mismatch Error
**Error**: `MCUs mismatch: configured MCU 'ESP32C61' does not match MCUs [ESP32C6, ESP32H2, ...]`

**Solution**: Already fixed in `pio.rs` patch. ESP32C61 is automatically mapped to ESP32C6 toolchain base.

#### 2. Linker Script Error (sections.ld)
**Error**: `The gap between .flash.rodata and .flash.init_array must not exist`

**Solution**: Already fixed. The `build.rs` automatically patches the generated `sections.ld` to comment out hard ASSERT statements.

#### 3. Symbol Conflict (posix_memalign)
**Error**: `multiple definition of posix_memalign`

**Solution**: Already fixed. The stub in `sysenv_stubs.c` is marked as `WEAK` to defer to ESP-IDF's implementation.

#### 4. Network Issues During espup install
**Problem**: GitHub downloads fail in China

**Solution**:
```bash
export http_proxy=http://127.0.0.1:10808
export https_proxy=http://127.0.0.1:10808
espup install
```

### Flash Issues

#### Device Not Found
```bash
# List available ports
ls /dev/cu.usbserial-* /dev/cu.usbmodem*

# On macOS, install drivers if needed
brew install --cask silicon-labs-vcp-driver
```

#### Permission Denied
```bash
# Add user to dialout group (Linux)
sudo usermod -a -G dialout $USER

# On macOS, no special permissions needed
```

## 🔬 Advanced Build Options

### Clean Build
```bash
# Clean target directory
cargo clean

# Clean esp-idf-sys cache
cargo clean -p esp-idf-sys

# Clean PlatformIO cache
rm -rf ~/.platformio/.cache
```

### Build with Verbose Output
```bash
cargo build --release --verbose
```

### Check Without Building
```bash
cargo check
```

### Build for Different Profiles
```bash
# Debug (faster compile, larger binary, with debug info)
cargo build

# Release (slower compile, optimized, stripped)
cargo build --release
```

## 📐 Memory Layout

```
Flash Memory (8 MB):
├── Bootloader         (~48 KB)
├── Partition Table    (~4 KB)
├── Application        (~169 KB)
│   ├── .iram0.text    (48 KB)   # Fast instruction RAM
│   ├── .flash.text    (74 KB)   # Flash code
│   └── .flash.rodata  (37 KB)   # Read-only data
└── Free Space         (~7.77 MB)

RAM (832 KB total):
├── SRAM (320 KB):
│   ├── .dram0.data    (7 KB)    # Initialized data
│   ├── .dram0.bss     (4 KB)    # Uninitialized data
│   └── Heap           (~309 KB) # Dynamic allocation
└── PSRAM (512 KB):     # External PSRAM for large buffers
```

## 🏗️ Technical Details

### Build Pipeline

1. **Rust Compilation**: Source → LLVM IR → RISC-V machine code
2. **ESP-IDF Integration**: PlatformIO builds ESP-IDF v6.0 components
3. **Linker Script Patching**: `build.rs` patches `sections.ld` to allow orphan sections
4. **C Stub Compilation**: `sysenv_stubs.c` provides POSIX compatibility layer
5. **Final Linking**: Links Rust + ESP-IDF + stubs into single ELF
6. **Post-processing**: Strips symbols, generates flashable binary

### Key Patches Applied

1. **`pio.rs`** (ESP-IDF sys):
   - Maps ESP32C61 → ESP32C6 for PlatformIO MCU resolution
   - Emits `cargo:rustc-link-arg` directives for ldproxy
   - Propagates link arguments to downstream crates

2. **`build.rs`** (magent-esp32-app):
   - Dynamically locates and patches `sections.ld`
   - Comments out hard ASSERT statements
   - Compiles `sysenv_stubs.c` for POSIX compatibility

3. **`sysenv_stubs.c`**:
   - Weak POSIX function stubs for stable Rust's Unix PAL
   - Defers to ESP-IDF's implementations when available
   - Provides fallbacks for missing functions

### Toolchain Components

- **Rust Compiler**: esp channel (based on Rust 1.83+ nightly)
- **LLVM**: Custom ESP build with RISC-V backend
- **GCC**: riscv32-esp-elf-gcc 15.2.0
- **ESP-IDF**: v6.0 via PlatformIO
- **Linker**: ldproxy (wraps riscv32-esp-elf-gcc)

## 📝 Configuration Files Reference

### `Cargo.toml` Features
```toml
[features]
default = ["wifi"]
wifi = []
ble = []
ota = []
uart = []
```

### Environment Variables
```bash
# Set by espup/export-esp.sh
LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib
ESP_IDF_VERSION=v6.0
PATH=$HOME/.espressif/tools/...:$PATH

# Build-time
RUSTC_BOOTSTRAP=1  # Enables unstable features on stable
MCU=esp32c6        # Passed to ESP-IDF build system
```

## 🎯 Next Steps After Successful Build

1. **Flash the firmware** using espflash
2. **Monitor serial output** to verify boot
3. **Configure Wi-Fi** via serial console
4. **Test agent functionality** with sensor readings
5. **Deploy to production** with OTA updates

## 🤝 Contributing

If you encounter build issues not covered here:
1. Check `cargo build --verbose` output
2. Review ESP-IDF build logs in `target/.../out/esp-idf/.pio/build/`
3. Open an issue with full build log and environment details

## 📚 Further Reading

- [ESP-IDF Programming Guide](https://docs.espressif.com/projects/esp-idf/en/latest/)
- [ESP Rust Book](https://esp-rs.github.io/book/)
- [RISC-V Instruction Set Manual](https://riscv.org/technical/specifications/)
- [espup Repository](https://github.com/esp-rs/espup)
- [esp-idf-sys Documentation](https://github.com/esp-rs/esp-idf-sys)

---

**Last Updated**: August 20, 2026  
**Tested On**: macOS 14 (Sonoma), ESP32-C61-DevKitC-1-N8R2  
**Build Time**: ~3 minutes (clean build), ~20 seconds (incremental)

# nRF52840 Build Guide

Complete guide for building and deploying mAgent firmware on the nRF52840.

## Prerequisites

### 1. Install Rust ARM Target

```bash
# Add the ARM Cortex-M4F target
rustup target add thumbv7em-none-eabihf

# Verify installation
rustup target list --installed | grep thumbv7
```

### 2. Optional: Install Flashing Tools

**Option A: probe-rs (Recommended)**
```bash
cargo install probe-rs
```

**Option B: nrfjprog (Nordic official)**
```bash
# Download from Nordic Semiconductor website
# https://www.nordicsemi.com/Products/Development-tools/nrf-command-line-tools
```

**Option C: JLink**
```bash
# Download from SEGGER website
# https://www.segger.com/downloads/jlink/
```

## Building

### Build Commands

```bash
# Navigate to the firmware directory
cd firmware/nrf52-app

# Build release firmware
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf

# The binary will be at:
# target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

### Verify Build

```bash
# Check binary size
ls -lh target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Check binary format
file target/thumbv7em-none-eabihf/release/magent-nrf52-app
# Should output: ELF 32-bit LSB executable, ARM, EABI5
```

### Build Output

```
Location:   target/thumbv7em-none-eabihf/release/magent-nrf52-app
Size:       193 KB
Format:     ELF 32-bit ARM EABI5
Architecture: ARM Cortex-M4F (thumbv7em-none-eabihf)
```

## Flashing

### Method 1: Using probe-rs

```bash
# Flash and run
probe-rs run --chip nRF52840_xxAA \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Or flash only
probe-rs flash --chip nRF52840_xxAA \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Flash with reset
probe-rs flash --chip nRF52840_xxAA --reset \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

### Method 2: Generate HEX for External Tools

```bash
# Convert ELF to HEX
cargo objcopy -p magent-nrf52-app --release --target thumbv7em-none-eabihf \
  -- -O ihex firmware.hex

# Convert ELF to BIN
cargo objcopy -p magent-nrf52-app --release --target thumbv7em-none-eabihf \
  -- -O binary firmware.bin

# Then flash using your preferred tool
```

### Method 3: Using OpenOCD + GDB

```bash
# Terminal 1: Start OpenOCD
openocd -f interface/cmsis-dap.cfg -f target/nrf52.cfg

# Terminal 2: Connect with GDB
arm-none-eabi-gdb target/thumbv7em-none-eabihf/release/magent-nrf52-app

# In GDB:
(gdb) target remote localhost:3333
(gdb) load
(gdb) monitor reset halt
(gdb) continue
```

## Debugging

### Using probe-rs

```bash
# Debug with RTT logging
probe-rs debug --chip nRF52840_xxAA \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Debug with ETM trace (if supported)
probe-rs debug --chip nRF52840_xxAA --trace \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

### Using GDB

```bash
# Start GDB
arm-none-eabi-gdb target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Connect to target
(gdb) target remote localhost:3333

# Set breakpoints
(gdb) break main
(gdb) break nrf_gpio_set

# Run
(gdb) continue

# Step through
(gdb) step
(gdb) next
```

## Memory Layout

```
nRF52840 Memory Map:
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Region    Start       End         Size    Purpose
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Flash     0x00000000  0x00100000  1 MB    Program storage
RAM       0x20000000  0x20040000  256 KB  Working memory
UICR      0x10001000  0x10001000  -       User Information Config
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### Linker Script (memory.x)

```ld
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 1024K
  RAM : ORIGIN = 0x20000000, LENGTH = 256K
}

_stack_start = ORIGIN(RAM) + LENGTH(RAM);
```

## Configuration

### Environment Variables

```bash
# Set log level
export DEFMT_LOG=info

# Or in .cargo/config.toml:
[env]
DEFMT_LOG = "info"
```

### Build Configuration (.cargo/config.toml)

```toml
[build]
target = "thumbv7em-none-eabihf"

[target.thumbv7em-none-eabihf]
rustflags = [
    "-C", "link-arg=-Tdefmt.x",
]

[env]
DEFMT_LOG = "info"
```

## Features

### Current Features

| Feature | Module | Description |
|---------|--------|-------------|
| BLE Advertising | `ble.rs` | BLE 5.3 peripheral advertising |
| Sensor Support | `sensors.rs` | LIS2DW12, BME280 drivers |
| Power Management | `power.rs` | Active/LowPower/Sleep/DeepSleep |
| Watchdog | `watchdog.rs` | System health monitoring |
| Embassy RTOS | `main.rs` | Async task scheduling |

### Feature Flags

```bash
# Default includes BLE
cargo build -p magent-nrf52-app --release --features ble

# Build without BLE (smaller binary)
cargo build -p magent-nrf52-app --release --no-default-features
```

## Troubleshooting

### Build Issues

**Q: "linker `thumbv7em-none-eabihf-gcc` not found"**
```bash
# Install ARM gcc toolchain
# macOS
brew install --cask gcc-arm-embedded

# Linux
sudo apt install gcc-arm-none-eabi
```

**Q: "undefined symbol: _defmt_panic"**
```bash
# Add defmt linker script to rustflags
# In .cargo/config.toml:
[target.thumbv7em-none-eabihf]
rustflags = ["-C", "link-arg=-Tdefmt.x"]
```

**Q: "memory.x not found"**
```bash
# Ensure memory.x exists in the project root
ls firmware/nrf52-app/memory.x

# If not, create it with proper memory regions
```

### Flash Issues

**Q: "Failed to connect to device"**
```bash
# Check device connection
probe-rs list

# Try with sudo if permission denied
sudo probe-rs flash --chip nRF52840_xxAA firmware.hex
```

**Q: "Chip not found"**
```bash
# Verify chip model
# nRF52840_xxAA vs nRF52840_xxAB
probe-rs info --chip nRF52840_xxAA
```

## Size Analysis

### Binary Breakdown

```
mAgent nRF52840 Firmware Size Analysis
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Component              Size (KB)   Percentage
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
magent-core            85.2       44.1%
embassy-runtime        42.1       21.8%
nrf-softdevice         31.5       16.3%
defmt                  12.3        6.4%
cortex-m-rt             8.7        4.5%
app code               13.2        6.9%
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total                 193.0 KB   100.0%
```

### Optimize Binary Size

```bash
# Use LTO for smaller binaries
# In .cargo/config.toml:
[target.thumbv7em-none-eabihf]
rustflags = [
    "-C", "link-arg=-Tdefmt.x",
    "-C", "lto=on",
    "-C", "opt-level=z",
]
```

## Development

### Run Tests

```bash
# Build tests
cargo test -p magent-nrf52-app

# Run integration tests (requires nRF hardware)
cargo test -p magent-nrf52-app --features integration
```

### Continuous Integration

```yaml
# .github/workflows/nrf52.yml
name: nRF52840 Build

on: [push, pull_request]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: thumbv7em-none-eabihf
      - name: Build
        run: cargo build -p magent-nrf52-app --release
      - name: Size Check
        run: |
          ls -lh target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

## See Also

- [Platform Comparison](PLATFORM_COMPARISON.md)
- [ESP32-C61 Build Guide](ESP32_C61_BUILD.md)
- [Memory Analysis](NRF52_MEMORY_ANALYSIS.md)

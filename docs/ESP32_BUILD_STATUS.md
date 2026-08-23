# ESP32 Build Status

## Current State

The ESP32 firmware **cannot be built** on this system due to a missing Rust target.

## Problem

The ESP-IDF std library (`std` for RISC-V) is only provided by Espressif's **custom Rust fork** called `esp` toolchain.

Standard Rust toolchains (stable, nightly) do **NOT** include:
- `riscv32imac-esp-espidf`
- `riscv32imc-esp-espidf`
- `xtensa-esp32-espidf`

## Required Installation

The `esp` toolchain must be installed via `espup`:

```bash
espup install
rustup toolchain list  # Should show: esp
```

### Current Installation Issues

The `espup install` command is failing to install RISC-V targets due to network/proxy issues:
```
[warn]: Installation for 'RISC-V Rust target' failed, retrying
Error: espup::toolchain::rust::install_riscv_target
  × Failed to Install RISC-V targets for 'stable' toolchain
```

## Workaround Options

### Option 1: Fix Network Issues
Try `espup install` without proxy:
```bash
unset all_proxy socks5_proxy https_proxy http_proxy
espup install
```

### Option 2: Use Docker (Recommended for macOS)
Use the official Espressif Docker container:
```bash
docker run -it espressif/idf-rust:latest
```

### Option 3: Build on Linux
Set up a Linux VM or use WSL with a standard ESP-IDF installation.

## Status After Fix

Once `espup install` succeeds:
```bash
rustup toolchain list
# Should show: esp

source ~/export-esp.sh
cargo build -p magent-esp32-app --release
```

## What Works Now

- ✅ `magent-core` tests: 113+ tests passing
- ✅ `magent-cli`: Compiles successfully
- ✅ `magent-simulator`: Compiles successfully
- ❌ `magent-esp32-app`: Cannot build (missing espidf target)

## GCC Tools Installed

The following GCC tools are installed and available:
- `riscv32-esp-elf-gcc` (in `~/.espressif/tools/`)
- `xtensa-esp32-elf-gcc` (in `~/.espressif/tools/`)
- ESP-IDF v6.0 SDK (in `~/.espressif/esp-idf/`)

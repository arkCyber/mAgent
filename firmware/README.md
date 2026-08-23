# MicroAgent Firmware

Multi-platform embedded firmware for MicroAgent, supporting ARM Cortex-M and RISC-V architectures.

## 📦 Available Platforms

| Platform | Status | Directory | Target | Binary Size | Features |
|----------|--------|-----------|--------|-------------|----------|
| **nRF52840** | ✅ Ready | `nrf52-app/` | `thumbv7em-none-eabihf` | 161 KB | BLE 5.3, magent-core AI Agent, Low Power, Sensors |
| **ESP32-C61** | ✅ Ready | `esp32-app/` | `riscv32imac-esp-espidf` | 607 KB | Wi-Fi 6, BLE 5.0, magent-core AI Agent |

## 🎯 Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        MicroAgent                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────────┐      ┌─────────────────┐                │
│  │   magent-core   │      │    firmware/    │                │
│  │  (chip-agnostic)│ ───▶ │    nrf52-app    │                │
│  │                 │      │                 │                │
│  │  ┌───────────┐  │      │  ┌───────────┐  │                │
│  │  │MiniAgent  │  │      │  │ Embassy   │  │                │
│  │  │ ReAct    │  │      │  │ RTOS      │  │                │
│  │  └───────────┘  │      │  └───────────┘  │                │
│  │                 │      │                 │                │
│  │  ┌───────────┐  │      │  ┌───────────┐  │                │
│  │  │ Skills   │  │      │  │ BLE/GATT  │  │                │
│  │  │ Manager  │  │      │  └───────────┘  │                │
│  │  └───────────┘  │      │                 │                │
│  │                 │      │  ┌───────────┐  │                │
│  │  ┌───────────┐  │      │  │ Sensors  │  │                │
│  │  │ Tools    │  │      │  └───────────┘  │                │
│  │  │Registry  │  │      │                 │                │
│  │  └───────────┘  │      │  ┌───────────┐  │                │
│  │                 │      │  │  Power    │  │                │
│  │  ┌───────────┐  │      │  │Management│  │                │
│  │  │ Safety   │  │      │  └───────────┘  │                │
│  │  │ Budget   │  │      │                 │                │
│  │  └───────────┘  │      │  ┌───────────┐  │                │
│  │                 │      │  │ Watchdog  │  │                │
│  └─────────────────┘      │  └───────────┘  │                │
│                            └─────────────────┘                │
└─────────────────────────────────────────────────────────────────┘
```

## 🚀 Quick Start

### nRF52840 (ARM Cortex-M4F)

```bash
# Install target
rustup target add thumbv7em-none-eabihf

# Build firmware
cd firmware/nrf52-app
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf

# The binary is at:
# target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

**Features**:
- Embassy async runtime
- BLE 5.3 support (nRF SoftDevice S140)
- **MiniAgent with ReAct state machine** (from magent-core)
- **Skills Manager** (flash-based skill storage)
- **Tool Registry** (read_sensor, write_gpio, flash_read/write, etc.)
- Low-power optimized (Active/LowPower/Sleep/DeepSleep)
- Sensor drivers (LIS2DW12, BME280)
- Watchdog and error recovery
- 256 KB RAM, 1 MB Flash
- Hardware floating-point (Cortex-M4F)

**Documentation**: [nRF52840 Build Guide](docs/NRF52_BUILD_GUIDE.md)

---

### ESP32-C61 (RISC-V 32-bit with Wi-Fi 6 + BLE 5.0)

**Build** (uses stable Rust):
```bash
cd firmware/esp32-app
MCU=esp32c6 cargo build --release
```

> **Note**: The `MCU=esp32c6` env var is required because ESP-IDF v6 tools don't yet know about ESP32-C61.

**Features**:
- Wi-Fi 6 (802.11ax) support
- BLE 5.0 support
- ESP-IDF framework (v6.0 via PlatformIO)
- **MiniAgent with ReAct state machine** (from magent-core)
- **Ingress Gateway** with UART/RS232/SPI adapters
- **Web3 Identity** (Ed25519 signing without reqwest)
- 832 KB RAM (320 KB + 512 KB PSRAM)
- 8 MB Flash

**Documentation**: [ESP32-C61 Build Guide](docs/ESP32_C61_BUILD.md)

---

## 🔧 Project Structure

```
firmware/
├── README.md                    # This file
│
├── nrf52-app/                  # nRF52840 firmware
│   ├── src/
│   │   ├── main.rs             # Entry point & tasks
│   │   ├── ble.rs              # BLE advertising & GATT
│   │   ├── sensors.rs          # Sensor drivers
│   │   ├── power.rs           # Power management
│   │   └── watchdog.rs        # Watchdog & error recovery
│   ├── .cargo/
│   │   └── config.toml        # Build configuration
│   ├── Cargo.toml             # Dependencies
│   ├── memory.x               # Memory layout (1024K Flash, 256K RAM)
│   ├── build.rs               # Build script
│   └── README.md              # Detailed guide
│
└── esp32-app/                 # ESP32-C61 firmware
    ├── src/
    │   ├── main.rs             # Entry point
    │   └── sysenv_stubs.c      # C stubs for no_std
    ├── .cargo/
    │   └── config.toml        # Build configuration
    ├── Cargo.toml             # Dependencies
    ├── build.rs               # Build script
    ├── sdkconfig             # ESP-IDF configuration
    └── README.md              # Detailed guide
```

## 📊 Platform Comparison

### When to use nRF52840
- ✅ Battery-powered wearables (smartwatch form factor)
- ✅ BLE-only connectivity
- ✅ Ultra-low power requirements
- ✅ Compact code size (161 KB)
- ✅ Real-time sensor processing

### When to use ESP32-C61
- ✅ Wi-Fi connectivity needed
- ✅ Cloud integration
- ✅ Higher performance (RISC-V 160 MHz)
- ✅ More memory headroom
- ✅ Dual wireless (Wi-Fi + BLE)

| Hardware | nRF52840 | ESP32-C61 |
|---------|----------|------------|
| Architecture | ARM Cortex-M4F | RISC-V 32-bit |
| Clock Speed | 64 MHz | 160 MHz |
| Flash | 1 MB | 8 MB |
| RAM | 256 KB | 832 KB |
| Connectivity | BLE 5.3 | Wi-Fi 6 + BLE 5.0 |
| Power (Active) | ~15 mA | ~80 mA |
| Power (Sleep) | ~0.5 mA | ~10 mA |

| Feature | nRF52840 | ESP32-C61 |
|---------|----------|------------|
| ReAct Agent | ✅ | ✅ |
| Skills Manager | ✅ | ✅ |
| Tool Registry | ✅ | ✅ |
| BLE Communication | ✅ | ✅ |
| Wi-Fi 6 | ❌ | ✅ |
| Health Sensors | ✅ | ✅ |
| Web3 Identity | ✅ | ✅ |
| Ingress Gateway | ✅ | ✅ |

## 🛠️ Development Tools

### Required for nRF52840
```bash
# Rust ARM target
rustup target add thumbv7em-none-eabihf

# Optional: flashing tool
cargo install probe-rs
```

### Required for ESP32-C61
```bash
# Rust nightly + RISC-V target
rustup install nightly
rustup target add riscv32imac-esp-espidf --toolchain nightly

# ESP flashing tool
cargo install espflash
```

---

## 📈 Build Metrics

### nRF52840 (with magent-core)
```
Binary size:    161 KB
Flash usage:    15.7% (1 MB total)
RAM usage:      ~15 KB heap + stack
Build time:     ~15 seconds (release)
Features:       BLE, Embassy, MiniAgent, Skills, Tools
```

### ESP32-C61
```
Binary size:    607 KB
Flash usage:    7.4% (8 MB total)
RAM usage:      ~50 KB heap + BSS
Build time:     ~45 seconds (release)
Features:       Wi-Fi, BLE, MiniAgent, Skills, Tools
```

---

## 🔍 Testing & Verification

### nRF52840
```bash
# Size analysis
cargo size -p magent-nrf52-app --release --target thumbv7em-none-eabihf -- -A

# Check binary format
file target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Generate hex file for flashing
cargo objcopy -p magent-nrf52-app --release --target thumbv7em-none-eabihf -- -O ihex nrf52-app.hex
```

### ESP32-C61
```bash
# Check binary
ls -lh target/riscv32imac-esp-espidf/release/magent-esp32-app

# Generate bin (with bootloader)
esptool.py --chip esp32c61 merge_bin \
  0x0 build/esp32c61/bin/bootloader.bin \
  0x8000 build/esp32c61/bin/partitions.bin \
  0x10000 target/riscv32imac-esp-espidf/release/magent-esp32-app.bin \
  -o firmware.bin
```

---

## 🚧 Roadmap

- [ ] ESP32-C3 support (RISC-V 32-bit, ultra-low power)
- [ ] ESP32-C6 support (RISC-V 32-bit, Wi-Fi 6)
- [ ] ESP32-S3 support (Xtensa LX7, dual-core, AI acceleration)
- [ ] Over-the-air (OTA) updates
- [ ] Power profiling tools
- [ ] Hardware-in-the-loop testing

---

## 📚 Documentation

| Document | Description |
|----------|-------------|
| [This README](README.md) | Firmware overview |
| [nRF52840 Build Guide](docs/NRF52_BUILD_GUIDE.md) | Detailed nRF52840 guide |
| [ESP32-C61 Build Guide](docs/ESP32_C61_BUILD.md) | Detailed ESP32-C61 guide |
| [Platform Comparison](docs/PLATFORM_COMPARISON.md) | Platform analysis |
| [Memory Analysis](docs/NRF52_MEMORY_ANALYSIS.md) | nRF52840 memory breakdown |

### External Resources
- [Embassy Framework](https://embassy.dev/)
- [ESP-IDF Documentation](https://docs.espressif.com/projects/esp-idf/)
- [nRF52840 Product Page](https://www.nordicsemi.com/products/nrf52840)
- [ESP32-C61 Product Page](https://www.espressif.com/en/products/socs/esp32-c61)

---

**Status**: Both platforms are production-ready! 🎉

Choose your platform and start building embedded AI applications.

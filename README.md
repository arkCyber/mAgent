# mAgent - Aerospace-Grade Embedded AI Agent

| Language | Rust |
|----------|------|
| Platform | Embedded / Bare-metal |
| Architecture | ARM Cortex-M4F (nRF52840), RISC-V (ESP32-C61) |

Aerospace-grade AI agent for nRF52840 smartwatches and embedded devices. Built
with Rust: the nRF52840 path uses Embassy (bare-metal, no OS); the ESP32-C61
path uses `esp-idf-svc` (std) with real Wi-Fi 6, local hardware tools, and a
bidirectional UART command interface.

## 🚀 Aerospace-Grade Safety Features

- **Bounded Panics**: All runtime error paths use `Result` types; the only
  remaining `.expect()` calls are on compile-time constants or unreachable
  hardware paths (hardware TRNG / fixed Ed25519 seed), so no runtime condition
  can panic the board.
- **Memory Safety**: Stack depth analysis, heapless data structures, bounded buffers
- **Resource Limits**: Strict memory, time, and iteration budgets
- **Input Validation**: All inputs validated with bounds checking
- **Fault Tolerance**: Graceful degradation, watchdog integration
- **Secure Communication**: Encrypted BLE/Thread with certificate validation
- **Power Management**: Low-power states, battery monitoring
- **Real-time Guarantees**: Bounded execution time for all operations

## 📋 Hardware Requirements

### nRF52840 (Primary Platform)
- **MCU**: nRF52840 (Cortex-M4F @ 64MHz)
- **RAM**: 256 KB
- **Flash**: 1 MB
- **Wireless**: BLE 5.3, Thread, Zigbee
- **Sensors**: Temperature, accelerometer (optional)

### ESP32-C61 (RISC-V Platform)
- **MCU**: ESP32-C61 (RISC-V 32-bit @ 160MHz)
- **RAM**: 320 KB SRAM + 512 KB PSRAM
- **Flash**: 8 MB
- **Wireless**: Wi-Fi 6, BLE 5.3

## ✅ Build Status

| Platform | Architecture | Status | Build Command | Notes |
|----------|--------------|--------|---------------|-------|
| **nRF52840** | ARM Cortex-M4F | ✅ Ready | `cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf` | Primary smartwatch platform, BLE 5.3 |
| **ESP32-C61** | RISC-V 32-bit | ✅ Ready + Verified on HW | `cd firmware/esp32-app && MCU=ESP32C61 cargo build --release` | Wi-Fi 6 + BLE 5.0, std (esp-idf-svc), real local tools, bidirectional UART |
| ESP32-C3/C6 | RISC-V 32-bit | 🔄 Compatible | Use ESP32-C61 config | Same architecture |
| ESP32/S3 | Xtensa LX6/LX7 | 🔄 In Progress | TBD | Requires Xtensa toolchain |

## 🏗️ Project Structure

```
MicroAgent/
├── Cargo.toml                 # Workspace root
├── README.md                  # This file
│
├── magent-core/              # Core AI Agent Library (chip-agnostic)
│   ├── src/
│   │   ├── lib.rs           # Module exports, version constants
│   │   ├── agent.rs         # MiniAgent - ReAct state machine
│   │   ├── agent_runner.rs  # Full agent runner with LLM integration
│   │   ├── error.rs         # Error types and handling
│   │   ├── config.rs        # Agent configuration
│   │   ├── skills.rs        # Skills manager (flash-based)
│   │   ├── tools.rs         # Tool registry and execution
│   │   ├── safety.rs        # Safety checks and budgets
│   │   ├── hardware.rs      # Hardware abstraction
│   │   ├── power.rs         # Power management
│   │   ├── security.rs      # Security and encryption
│   │   ├── monitoring.rs     # Runtime monitoring
│   │   ├── ollama.rs       # LLM backend interface
│   │   ├── storage.rs       # Flash storage
│   │   ├── wear_leveling.rs  # Flash wear leveling
│   │   │
│   │   ├── health_sensors.rs    # Health sensor processing
│   │   ├── sports_coach.rs     # Sports coaching module
│   │   ├── sleep_manager.rs     # Sleep analysis
│   │   ├── early_warning.rs    # Early warning system
│   │   ├── voice_notification.rs # Voice notification
│   │   │
│   │   ├── communication/    # Link-layer adapters
│   │   │   ├── mod.rs
│   │   │   ├── link.rs     # LinkAdapter trait
│   │   │   ├── ble.rs      # BLE adapter
│   │   │   ├── mqtt.rs     # MQTT adapter (host)
│   │   │   └── manual.rs   # Manual stdin adapter
│   │   │
│   │   ├── web3/           # Web3 / blockchain
│   │   │   └── mod.rs
│   │   └── web3_app/       # Web3 app integration
│   └── Cargo.toml
│
├── magent-hal/               # Hardware Abstraction Layer
│   ├── src/
│   │   ├── lib.rs
│   │   └── nrf52/
│   │       └── sim/        # nRF52840 simulator
│   └── Cargo.toml
│
├── firmware/                 # Firmware for embedded targets
│   ├── README.md            # Firmware overview
│   │
│   ├── nrf52-app/          # nRF52840 firmware
│   │   ├── src/
│   │   │   ├── main.rs     # Application entry point
│   │   │   ├── ble.rs      # BLE advertising & GATT
│   │   │   ├── sensors.rs  # Sensor drivers
│   │   │   ├── power.rs    # Power management
│   │   │   └── watchdog.rs # Watchdog & error handling
│   │   ├── Cargo.toml
│   │   ├── memory.x        # Memory layout
│   │   ├── build.rs        # Build script
│   │   ├── .cargo/
│   │   │   └── config.toml # Build config
│   │   └── README.md
│   │
│   └── esp32-app/          # ESP32-C61 firmware
│       ├── src/
│       │   ├── main.rs
│       │   └── sysenv_stubs.c
│       ├── Cargo.toml
│       ├── build.rs
│       ├── sdkconfig
│       └── README.md
│
├── host/                     # Host-side tooling
│   ├── simulator/          # Desktop simulator
│   ├── nrf52-simulator/    # nRF52840 simulator
│   ├── email-mcp/          # Email MCP server
│   ├── mqtt-mcp/           # MQTT MCP server
│   └── mcp-tool-executor/  # MCP tool executor
│
├── cli/                     # Command-line interface
│   └── src/
│       └── main.rs
│
├── tools/                   # Development tools
│   └── src/
│
├── docs/                    # Documentation
│   ├── README.md            # Doc index
│   ├── NRF52_BUILD_GUIDE.md
│   ├── NRF52_MEMORY_ANALYSIS.md
│   ├── ESP32_C61_BUILD.md
│   ├── ESP32_C61_BUILD_TROUBLESHOOTING.md
│   └── PLATFORM_COMPARISON.md
│
└── examples/                # Example applications
```

## 🔋 Power Management

- **Power Modes**: Active, Idle, Low Power, Deep Sleep
- **Battery Monitoring**: Voltage and percentage tracking
- **Low Battery Detection**: Automatic low power mode entry
- **Power Optimization**: Dynamic power state management

## 🔒 Security

- **BLE Encryption**: AES-128/256 CCM encryption
- **Secure Pairing**: Certificate-based authentication
- **Message Authentication**: Authentication tags for all messages
- **Security Levels**: None, Low, Medium, High

## 💾 Flash Management

- **Wear Leveling**: Dynamic, Static, and Hybrid strategies
- **Wear Monitoring**: Real-time wear level calculation
- **Wear Detection**: Automatic worn-out detection
- **Extended Lifetime**: Up to 10x flash lifetime extension

## 🔧 Build Instructions

### Prerequisites

**For nRF52840 (ARM Cortex-M4F):**
- Rust 1.70+ with target `thumbv7em-none-eabihf`
- probe-rs for flashing and debugging
- ARM GCC toolchain for binary analysis

**For ESP32-C61 (RISC-V):**
- Rust stable toolchain (1.97+)
- PlatformIO ESP-IDF framework (auto-managed by build)
- espflash for firmware deployment

### Installation

```bash
# Install Rust targets for the chips you want to build for
rustup target add thumbv7em-none-eabihf       # nRF52840 (ARM Cortex-M4F) ✅ Ready
rustup target add riscv32imac-esp-espidf      # ESP32-C61 (RISC-V) ✅ Ready

# Install essential tools
cargo install cargo-binutils    # For cargo size, cargo objcopy, etc.
cargo install probe-rs         # For nRF52840 flashing (ARM)
cargo install espflash         # For ESP32 flashing (RISC-V/Xtensa)
rustup component add llvm-tools-preview  # For generating hex files

# Clone repository
git clone https://github.com/arkCyber/mAgent.git
cd mAgent
```

### Building

**nRF52840 Firmware:**
```bash
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
```

**ESP32-C61 Firmware:**
```bash
# Build (MUST run from the firmware dir — the workspace root has no [build] target)
cd firmware/esp32-app
source ~/export-esp.sh   # activate the ESP toolchain
export MCU=ESP32C61 RUSTC_BOOTSTRAP=1
cargo build --release
```

**ESP32-C61 Flash & UART command interface:**
```bash
# 1) Build the app bin
cd firmware/esp32-app && cargo build --release
esptool.py --chip esp32c61 elf2image --flash_size 8MB \
  target/riscv32imac-esp-espidf/release/magent-esp32-app \
  -o target/riscv32imac-esp-espidf/release/magent-esp32-app.bin

# 2) Flash bootloader + custom partition table + app (via CP2102 USB-UART bridge,
#    NOT the native USB-JTAG — see docs/ESP32_C61_BOARD_BOOT_FAILURE.md)
P=target/riscv32imac-esp-espidf/release/build/esp-idf-sys-*/out/esp-idf/.pio/build/release
esptool.py --chip esp32c61 --port /dev/cu.usbserial-10 --baud 460800 write_flash \
  0x0 $P/bootloader.bin 0x8000 <custom-partitions.bin> 0x10000 <magent-esp32-app.bin>

# 3) Reset + monitor the serial console at 115200
esptool.py --chip esp32c61 --port /dev/cu.usbserial-10 --after hard_reset run
```

**Send a command to the agent over UART (bidirectional):** any text sent on the
UART is fed to the agent, which executes it with the local hardware tool handler
(GPIO, internal temperature sensor) and replies over the same link:
```
$ printf 'read the temperature\n' > /dev/cu.usbserial-10
RESULT[read the temperature]: Task: Tool result: temperature=34.4 C (err=0)

$ printf 'turn on the led\n' > /dev/cu.usbserial-10
RESULT[turn on the led]: Task: Tool result: GPIO13 set to high (err=0)
```

**Host-Side Tools:**
```bash
cargo build -p magent --release
```

## 📚 Documentation

| Document | Description |
|----------|-------------|
| [This README](README.md) | Project overview |
| [Firmware README](firmware/README.md) | Firmware overview |
| [nRF52840 Build Guide](docs/NRF52_BUILD_GUIDE.md) | nRF52840 detailed guide |
| [nRF52840 Memory](docs/NRF52_MEMORY_ANALYSIS.md) | Memory analysis |
| [ESP32-C61 Build Guide](docs/ESP32_C61_BUILD.md) | ESP32-C61 detailed guide |
| [ESP32-C61 Boot & Hardware Notes](docs/ESP32_C61_BOARD_BOOT_FAILURE.md) | Bring-up diagnosis, fixes, and verification |
| [Platform Comparison](docs/PLATFORM_COMPARISON.md) | Platform analysis |

## 📊 Features Matrix

| Feature | nRF52840 | ESP32-C61 |
|---------|----------|------------|
| ReAct Agent | ✅ | ✅ |
| Skills Manager | ✅ | ✅ |
| Tool Registry | ✅ | ✅ |
| Real Local Tools (GPIO/sensor, no network) | ✅ | ✅ |
| BLE Communication | ✅ | ✅ |
| Wi-Fi 6 | ❌ | ✅ |
| Health Sensors | ✅ | ✅ |
| Sports Coach | ✅ | ✅ |
| Sleep Manager | ✅ | ✅ |
| Early Warning | ✅ | ✅ |
| Web3 Identity (Ed25519) | ✅ | ✅ |
| Ingress Gateway | ✅ | ✅ |
| Bidirectional UART (command → result reply) | ✅ | ✅ |
| Crash-loop detection + safe mode | ✅ | ✅ |
| Health monitoring (heartbeat, free-heap) | ✅ | ✅ |
| OTA Updates | 🔄 | 🔄 |

## 🛡️ Reliability & Error-Handling

The ESP32-C61 firmware and `magent-core` have been hardened for operation in
unattended / low-trust environments:

- **Non-fatal platform bring-up** — event loop, peripherals, NVS, or Wi-Fi
  failure no longer panics the board; the agent + ingress threads keep running
  (local tools and UART don't need network).
- **Crash-loop recovery** — a consecutive-boot counter in NVS detects repeated
  reboots and enters *safe mode* (skips Wi-Fi), keeping the board up for
  diagnosis; the counter resets after a stable 60s boot.
- **Watchdog** — the main loop feeds the TG0 hardware watchdog; ESP-IDF
  auto-reboots on panic.
- **Heartbeat hang detection** — each worker thread beats a shared clock; the
  supervisor flags a thread that stalls >15s (catches hangs, not just panics).
- **Non-blocking UART** — `UartAdapter` polls RX availability so the ingress
  loop never blocks forever on input.
- **`RecoveryManager`** (in `magent-core`) now actually applies exponential
  backoff between retries, with a pluggable delay hook.
- 281 `magent-core` unit tests pass.

## 🤝 Contributing

Contributions welcome! Please read the safety guidelines in `magent-core/src/safety.rs`.

## 📄 License

MIT

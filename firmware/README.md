# MicroAgent Firmware

Multi-platform embedded firmware for MicroAgent, supporting **ARM Cortex-M**
(bare-metal Embassy) and **Espressif ESP32** (ESP-IDF `std`) architectures.

## 📦 Available Platforms

| Platform | Status | Directory | Target | Highlights |
|----------|--------|-----------|--------|------------|
| **nRF52840** | ✅ Ready | `nrf52-app/` | `thumbv7em-none-eabihf` | BLE 5.3, Embassy async, magent-core agent, low power, sensors |
| **ESP32-C61** | ✅ Ready + verified on HW | `esp32-app/` (`board-c61`, default) | `riscv32imac-esp-espidf` | Wi-Fi 6 + BLE 5, real local tools, bidirectional UART + AT, OTA (8 MB dual-slot) |
| **ESP32-S3** | ✅ Verified on HW (flagship) | `esp32-app/` (`board-s3`) | `xtensa-esp32s3-espidf` | Wi-Fi STA + DeepSeek cloud LLM + HTTP admin dashboard, dual-core RT scheduling, PSRAM |
| **integration-test** | ✅ Ready | `integration-test/` | `thumbv7em-none-eabihf` | On-device end-to-end test runner for nRF52840 |

> The ESP32-C61 and ESP32-S3 share **one source tree** (`esp32-app/`) selected at
> compile time by the `board-c61` / `board-s3` Cargo features. See
> [`esp32-app/BOARDS.md`](esp32-app/BOARDS.md) for the board matrix and
> [`../docs/ESP32_S3_PORT_SUCCESS.md`](../docs/ESP32_S3_PORT_SUCCESS.md) for the
> S3 bring-up log.

## 🎯 Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        MicroAgent                               │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐      ┌─────────────────┐                   │
│  │   magent-core   │      │    firmware/    │                   │
│  │ (chip-agnostic) │ ───▶ │  nrf52 / esp32  │                   │
│  │                 │      │                 │                   │
│  │  ┌───────────┐  │      │  ┌───────────┐  │                   │
│  │  │ MiniAgent │  │      │  │ Embassy / │  │                   │
│  │  │  ReAct    │  │      │  │  ESP-IDF  │  │                   │
│  │  └───────────┘  │      │  └───────────┘  │                   │
│  │                 │      │                 │                   │
│  │  ┌───────────┐  │      │  ┌───────────┐  │                   │
│  │  │  Skills   │  │      │  │ BLE / WiFi│  │                   │
│  │  │  Manager  │  │      │  │  / AT     │  │                   │
│  │  └───────────┘  │      │  └───────────┘  │                   │
│  │                 │      │                 │                   │
│  │  ┌───────────┐  │      │  ┌───────────┐  │                   │
│  │  │   Tools   │  │      │  │ Sensors / │  │                   │
│  │  │ Registry  │  │      │  │  Power    │  │                   │
│  │  └───────────┘  │      │  └───────────┘  │                   │
│  └─────────────────┘      └─────────────────┘                   │
└─────────────────────────────────────────────────────────────────┘
```

## 🚀 Quick Start

### nRF52840 (ARM Cortex-M4F)

```bash
# Install target
rustup target add thumbv7em-none-eabihf

# Build firmware (from the workspace root)
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
# Binary: target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

**Features**:
- Embassy async runtime, no OS
- BLE 5.3 support (nRF SoftDevice S140)
- **MiniAgent with ReAct state machine** (from magent-core)
- **Skills Manager** (flash-based skill storage)
- **Tool Registry** (`read_sensor`, `write_gpio`, `flash_read`/`write`, …)
- Low-power optimized (Active/LowPower/Sleep/DeepSleep)
- Sensor drivers (LIS2DW12, BME280)
- Watchdog and error recovery
- 256 KB RAM, 1 MB Flash, hardware FPU

**Documentation**: [nRF52840 Build Guide](../docs/NRF52_BUILD_GUIDE.md)

---

### ESP32-C61 (RISC-V 32-bit, Wi-Fi 6 + BLE 5) — default board

```bash
cd firmware/esp32-app
source ~/export-esp.sh      # activate the ESP-IDF / esp toolchain
./build-c61.sh              # swaps in sdkconfig.c61.defaults, builds, restores
# ELF: target/riscv32imac-esp-espidf/release/magent-esp32-app

./flash-c61.sh              # flash bootloader + partitions + app
```

**Features**:
- Wi-Fi 6 (802.11ax) + BLE 5 (LE)
- ESP-IDF v6.0 (via PlatformIO), RISC-V 160 MHz
- **MiniAgent ReAct agent** + **Skills** + **Tool Registry**
- Bidirectional UART command interface + deterministic **AT (ESP-AT subset)**
- Real local tools (GPIO / internal temperature) — no network required
- Web3 identity (Ed25519), ingress gateway
- OTA updates (8 MB flash, dual-slot + anti-rollback)
- 320 KB SRAM + 2 MB PSRAM

**Documentation**: [ESP32-C61 Build Guide](../docs/ESP32_C61_BUILD.md) ·
[Tier-6 on-HW runbook](../docs/TIER6_VALIDATION.md)

---

### ESP32-S3 (Xtensa LX7, dual-core) — flagship ESP board

```bash
cd firmware/esp32-app
./build-s3.sh                       # uses the `esp` toolchain (cargo +esp)
# ELF: target/xtensa-esp32s3-espidf/release/magent-esp32-app

./flash-s3.sh --port /dev/cu.usbserial-XXX
```

**Features**:
- Dual-core Xtensa LX7 @ 240 MHz; Wi-Fi STA (2.4 GHz) + BLE 5 (LE)
- **DeepSeek cloud LLM** path (S3-only) + **HTTP admin dashboard** (`web_admin`)
- Dual-core real-time scheduling (ReAct FM on Core 1, network workers on Core 0)
- PSRAM-backed heap (quad 4 MB on this board; ~2.18 MB free after boot)
- Shared ReAct agent / AT / UART / local-tool stack with the C61 build
- BLE **off** (internal-DRAM constraint) and Lua **off** in the default S3 build;
  the optional `lua` Cargo feature (pure-Rust `piccolo`, no `mlua`) builds
  end-to-end — see [`../docs/LUA_SCRIPTING_S3.md`](../docs/LUA_SCRIPTING_S3.md)

**Documentation**: [ESP32-S3 Port Success](../docs/ESP32_S3_PORT_SUCCESS.md) ·
[Dual-Core Scheduling](../docs/DUAL_CORE_SCHEDULING.md)

---

## 🔧 Project Structure

```
firmware/
├── README.md                    # This file
│
├── nrf52-app/                   # nRF52840 firmware (Embassy, bare-metal)
│   ├── src/
│   │   ├── main.rs             # Entry point & tasks
│   │   ├── ble.rs              # BLE advertising & GATT
│   │   ├── sensors.rs          # Sensor drivers
│   │   ├── power.rs            # Power management
│   │   └── watchdog.rs         # Watchdog & error recovery
│   ├── .cargo/config.toml      # Build configuration
│   ├── Cargo.toml              # Dependencies
│   ├── memory.x                # Memory layout (1024K Flash, 256K RAM)
│   └── build.rs                # Build script
│
├── esp32-app/                   # ESP32 firmware (esp-idf-svc, std) — C61 + S3
│   ├── src/
│   │   ├── main.rs             # Entry point, event loop, board switch
│   │   ├── at_dispatch.rs      # AT (Hayes / ESP-AT subset) parser + handlers
│   │   ├── ble_at.rs / ble_config.rs / ble_gatt.rs / ble_wallet.rs
│   │   ├── link_adapters.rs    # UART/RS232/SPI ingress adapters
│   │   ├── llm.rs              # DeepSeek backend + cross-core LLM worker
│   │   ├── local_tools.rs      # On-device GPIO / temperature tool handler
│   │   ├── web_admin.rs        # HTTP admin dashboard + /api/status
│   │   ├── latency_metrics.rs  # WCET / latency channels
│   │   ├── core_affinity.rs    # FreeRTOS core + priority profiles (S3)
│   │   ├── rt_watchdog.rs      # Real-time watchdog isolation (S3)
│   │   ├── sntp_sync.rs        # SNTP time synchronisation
│   │   ├── ping.rs / lua_task.rs / blockchain_transport.rs
│   │   └── device_key.rs
│   ├── Cargo.toml · build.rs · rust-toolchain.toml
│   ├── sdkconfig.defaults          # S3 (quad-PSRAM dev board) config
│   ├── sdkconfig.c61.defaults      # C61 config
│   ├── sdkconfig.s3.defaults       # S3 N8R8 (8 MB octal) overlay
│   ├── sdkconfig.prod.defaults · partitions.csv · partitions.ota.csv · partitions.s3.csv
│   ├── build-c61.sh · build-s3.sh · flash-c61.sh · flash-s3.sh · flash-secure.sh
│   └── BOARDS.md                    # Board / feature switch matrix
│
└── integration-test/            # nRF52840 on-device E2E test runner
```

## 📊 Platform Comparison

### When to use nRF52840
- ✅ Battery-powered wearables (smartwatch form factor)
- ✅ BLE-only connectivity, ultra-low power
- ✅ Compact code size (~182 KB, no OS)
- ✅ Real-time sensor processing

### When to use ESP32-C61
- ✅ Wi-Fi + cloud integration with the tightest BOM
- ✅ Dual wireless (Wi-Fi 6 + BLE 5) with OTA in 8 MB flash
- ✅ More memory headroom than the nRF52840 (~134 KB dynamic SRAM + 2 MB PSRAM)

### When to use ESP32-S3
- ✅ Wi-Fi **and** a cloud LLM in the same image (largest internal SRAM)
- ✅ Dual-core deterministic latency (real-time agent + network workers)
- ✅ PSRAM-hungry workloads (large context, HTTP dashboard, Lua host)

| Hardware | nRF52840 | ESP32-C61 | ESP32-S3 |
|----------|----------|-----------|----------|
| Architecture | ARM Cortex-M4F | RISC-V 32-bit | Xtensa LX7 (dual-core) |
| Clock Speed | 64 MHz | 160 MHz | 240 MHz |
| Flash | 1 MB | 8 MB | 4 MB (board variant) |
| Internal SRAM | 256 KB | 320 KB (~134 KB dynamic) | 512 KB (~390 KB dynamic) |
| PSRAM | ❌ | 2 MB | 4 MB quad (verified) |
| Connectivity | BLE 5.3 | Wi-Fi 6 + BLE 5.0 | Wi-Fi 2.4 GHz + BLE 5 (LE) |
| RTOS | Embassy (no OS) | ESP-IDF (std) | ESP-IDF (std) |
| Cloud LLM | ➖ | local | ✅ DeepSeek |
| OTA | ➖ | ✅ dual-slot | ➖ (4 MB constraint) |

| Feature | nRF52840 | ESP32-C61 | ESP32-S3 |
|---------|----------|-----------|----------|
| ReAct Agent | ✅ | ✅ | ✅ |
| Skills Manager | ✅ | ✅ | ✅ |
| Tool Registry | ✅ | ✅ | ✅ |
| Real local tools (GPIO/sensor) | ✅ | ✅ | ✅ |
| BLE Communication | ✅ | ✅ | ❌ (internal-DRAM) |
| Wi-Fi STA | ❌ | ✅ | ✅ (verified on HW) |
| HTTP admin dashboard | ❌ | ✅ | ✅ (verified on HW) |
| Web3 Identity | ✅ | ✅ | ✅ |
| Ingress Gateway / UART / AT | ✅ | ✅ | ✅ |

## 🛠️ Development Tools

### Required for nRF52840
```bash
rustup target add thumbv7em-none-eabihf   # ARM Cortex-M4F
cargo install probe-rs                    # flashing / debugging
```

### Required for ESP32-C61 (RISC-V)
```bash
cargo install espup --locked && espup install   # provides `cargo +esp`
source ~/export-esp.sh
rustup target add riscv32imac-esp-espidf
cargo install espflash
```

### Required for ESP32-S3 (Xtensa LX7)
```bash
cargo install espup --locked && espup install   # Xtensa LLVM backend (`cargo +esp`)
source ~/export-esp.sh
cargo install espflash
```

---


## 📈 Build Metrics

Reference sizes of the release ELF on a current build (approximate — they vary
with toolchain and feature set):

### nRF52840 (with magent-core)
```
Binary size:    ~182 KB
Flash usage:    ~18% (1 MB total)
RAM usage:      bounded heapless buffers + stack
Features:       BLE, Embassy, MiniAgent, Skills, Tools
```

### ESP32-S3
```
Free heap after boot:   ~2.18 MB (quad PSRAM in use)
Features:               WiFi STA, DeepSeek cloud LLM, web_admin, AT/UART, local tools
Status:                 verified stable on real hardware (long-run soak, no leak)
```

> Exact sizes are toolchain-dependent; regenerate with `cargo size` /
> `ls -lh target/...` after a local build.

---

## 🔍 Testing & Verification

### nRF52840
```bash
# Size analysis
cargo size -p magent-nrf52-app --release --target thumbv7em-none-eabihf -- -A

# On-device E2E runner
cargo build -p magent-integration-test --release --target thumbv7em-none-eabihf
```

### ESP32 (C61 / S3)
```bash
cd firmware/esp32-app
./build-c61.sh            # or ./build-s3.sh
ls -lh target/*/release/magent-esp32-app          # check the ELF

# On-hardware validation runbooks:
#   docs/TIER6_VALIDATION.md      (C61: AT/BLE/HTTP/TLS/OTA/secure-boot)
#   docs/ESP32_S3_PORT_SUCCESS.md (S3: WiFi/dashboard/PSRAM)
```

---

## 🚧 Roadmap

- [x] ESP32-C3 / C6 support (same RISC-V config as the C61)
- [x] **ESP32-S3 support (Xtensa LX7, dual-core)** — verified on HW
- [x] Over-the-air (OTA) updates (ESP32-C61, dual-slot)
- [x] Dual-core real-time scheduling (ESP32-S3)
- [x] AT (ESP-AT subset) provisioning + signed device identity
- [ ] Hardware-in-the-loop test automation on CI runners
- [ ] Power profiling tooling
- [ ] ESP32-S3 Lua app sandbox — build-verified, real-board validation pending
- [ ] ESP32-S3 OTA (blocked by the 4 MB flash variant)

---

## 📚 Documentation

| Document | Description |
|----------|-------------|
| [This README](README.md) | Firmware overview |
| [Board Matrix](esp32-app/BOARDS.md) | ESP32 C61 / S3 compile-time switch |
| [nRF52840 Build Guide](../docs/NRF52_BUILD_GUIDE.md) | Detailed nRF52840 guide |
| [nRF52840 Memory Analysis](../docs/NRF52_MEMORY_ANALYSIS.md) | Memory breakdown |
| [ESP32-C61 Build Guide](../docs/ESP32_C61_BUILD.md) | Detailed ESP32-C61 guide |
| [ESP32‑C61 Build & Ops](../docs/ESP32.md) | Toolchain, build, operations |
| [ESP32-S3 Port Success](../docs/ESP32_S3_PORT_SUCCESS.md) | S3 bring-up, fixes, on-HW verification |
| [Dual-Core Scheduling](../docs/DUAL_CORE_SCHEDULING.md) | S3 real-time scheduling (REQ-SCHED-001) |
| [Lua Scripting on S3](../docs/LUA_SCRIPTING_S3.md) | Lua host integration plan / status |
| [Tier-6 Validation](../docs/TIER6_VALIDATION.md) | On-hardware validation runbook |
| [AT Command Reference](../docs/AT_COMMAND_REFERENCE.md) | AT provisioning subset |
| [Platform Comparison](../docs/PLATFORM_COMPARISON.md) | Platform analysis |
| [Documentation Index](../docs/README.md) | All docs |

### External Resources
- [Embassy Framework](https://embassy.dev/)
- [ESP-IDF Documentation](https://docs.espressif.com/projects/esp-idf/)
- [esp-rs Book](https://esp-rs.github.io/book/)
- [nRF52840 Product Page](https://www.nordicsemi.com/products/nrf52840)
- [ESP32-C61 Product Page](https://www.espressif.com/en/products/socs/esp32-c61)
- [ESP32-S3 Product Page](https://www.espressif.com/en/products/socs/esp32-s3)

---

**Status**: nRF52840, ESP32-C61 and ESP32-S3 are production-ready; the S3 is the
current flagship ESP target. 🎉


# Documentation Index

Complete documentation for the MicroAgent project.

## 📚 Getting Started

| Document | Description |
|----------|-------------|
| [Main README](../README.md) | Project overview, features, and quick start |
| [Firmware README](../firmware/README.md) | Firmware overview for all platforms |
| [Project Overview](PROJECT_OVERVIEW.md) | High-level project narrative |
| [Executive Summary](EXECUTIVE_SUMMARY.md) | One-page summary |
| [Architecture](ARCHITECTURE.md) | Architecture overview |
| [Hardware](HARDWARE.md) | Hardware notes |

## 🔧 Platform Guides

### nRF52840 (ARM Cortex-M4F)
| Document | Description |
|----------|-------------|
| [Build Guide](NRF52_BUILD_GUIDE.md) | Complete build and deployment guide |
| [Build Status](NRF52_BUILD_STATUS.md) | Build history and current status |
| [Memory Analysis](NRF52_MEMORY_ANALYSIS.md) | Detailed memory usage breakdown |

### ESP32-C61 (RISC-V, default board)
| Document | Description |
|----------|-------------|
| [Build Guide](ESP32_C61_BUILD.md) | Complete build and deployment guide |
| [ESP32-C61 Build & Operations](ESP32.md) | Toolchain setup, build, and operations (REQ-DOC-001) |
| [Build History](ESP32_C61_BUILD_HISTORY.md) | Build troubleshooting history |
| [Troubleshooting](ESP32_C61_BUILD_TROUBLESHOOTING.md) | Common issues and solutions |
| [Board Boot Failure](ESP32_C61_BOARD_BOOT_FAILURE.md) | Bring-up diagnosis, fixes, verification |
| [Build Status](ESP32_BUILD_STATUS.md) | ESP32 build status & toolchain notes |
| [Tier-6 Validation](TIER6_VALIDATION.md) | On-hardware validation runbook (AT/BLE/HTTP/TLS/OTA/secure-boot) |

### ESP32-S3 (Xtensa LX7, flagship)
| Document | Description |
|----------|-------------|
| [S3 Port Success](ESP32_S3_PORT_SUCCESS.md) | Bring-up log, fixes, on-HW verification, S3 vs C61 matrix |
| [Dual-Core Scheduling](DUAL_CORE_SCHEDULING.md) | Real-time core/priority/watchdog design (REQ-SCHED-001) |
| [Lua Scripting on S3](LUA_SCRIPTING_S3.md) | Lua host integration plan + status |
| [Firmware TLS/BLE Gap Analysis](FIRMWARE_TLS_BLE_GAP_ANALYSIS.md) | TLS + BLE gaps and plans |

## 📊 Comparison & Analysis

| Document | Description |
|----------|-------------|
| [Platform Comparison](PLATFORM_COMPARISON.md) | Feature and performance comparison |
| [Simulator Dedup Plan](SIMULATOR_DEDUP_PLAN.md) | Host simulator consolidation plan |

## 🛡️ Features & Reference

| Document | Description |
|----------|-------------|
| [AT Command Reference](AT_COMMAND_REFERENCE.md) | AT (Hayes / ESP-AT) provisioning subset |
| [LLM Backends](LLM_BACKENDS.md) | DeepSeek / Ollama provider wiring |
| [Summary Store](SUMMARY_STORE.md) | Run-summary schema & CLI |
| [Prompt Store](PROMPT_STORE.md) | Stored system-prompt management |
| [Context Management](CONTEXT_MANAGEMENT.md) | Bounded-context windowing |
| [MQTT MCP](MQTT_MCP.md) | MQTT MCP server |
| [Config](CONFIG.md) | Config file reference |
| [API](API.md) | API reference |

## 🛡️ Reliability & Audits

| Document | Description |
|----------|-------------|
| [Aerospace Code Audit](AUDIT_AEROSPACE_2026.md) | Panic-freedom / bounded-memory audit |
| [Audit Report](AUDIT_REPORT.md) | Audit report |
| [SRS](SRS.md) | Software requirements specification |
| [SRS Traceability](SRS_TRACE.md) | Requirements traceability |
| [Backlog](BACKLOG.md) | Open backlog items |

## 📁 File Structure

```
docs/
├── README.md                        # This file (index)
│
├── NRF52_BUILD_GUIDE.md            # nRF52840 build guide
├── NRF52_BUILD_STATUS.md           # nRF52840 status
├── NRF52_MEMORY_ANALYSIS.md        # Memory breakdown
│
├── ESP32.md                        # ESP32-C61 build & operations
├── ESP32_C61_BUILD.md              # ESP32-C61 build guide
├── ESP32_C61_BUILD_HISTORY.md      # Build history
├── ESP32_C61_BUILD_TROUBLESHOOTING.md  # Troubleshooting
├── ESP32_C61_BOARD_BOOT_FAILURE.md # Boot / hardware bring-up notes
├── ESP32_BUILD_STATUS.md           # ESP32 toolchain & build status
├── ESP32_S3_PORT_SUCCESS.md        # ESP32-S3 bring-up + verification
├── DUAL_CORE_SCHEDULING.md         # S3 real-time scheduling (REQ-SCHED-001)
├── LUA_SCRIPTING_S3.md             # Lua host on S3
├── FIRMWARE_TLS_BLE_GAP_ANALYSIS.md # TLS / BLE gap analysis
├── TIER6_VALIDATION.md             # On-hardware validation runbook
│
├── AT_COMMAND_REFERENCE.md         # AT (Hayes / ESP-AT) provisioning subset
├── LLM_BACKENDS.md                 # DeepSeek / Ollama provider wiring
├── SUMMARY_STORE.md                # Run-summary schema & CLI
├── PROMPT_STORE.md                 # Stored system-prompt management
├── CONTEXT_MANAGEMENT.md           # Bounded-context windowing
├── MQTT_MCP.md                     # MQTT MCP server
├── CONFIG.md                       # Config file reference
├── API.md                          # API reference
│
├── ARCHITECTURE.md                 # Architecture overview
├── PROJECT_OVERVIEW.md             # Project overview
├── EXECUTIVE_SUMMARY.md            # Executive summary
├── HARDWARE.md                     # Hardware notes
├── SIMULATOR_DEDUP_PLAN.md         # Host simulator consolidation
│
├── AUDIT_AEROSPACE_2026.md         # Panic-freedom / bounded-memory audit
├── AUDIT_REPORT.md                 # Audit report
├── SRS.md                          # Software requirements spec
├── SRS_TRACE.md                    # Requirements traceability
├── BACKLOG.md                      # Open backlog
│
└── PLATFORM_COMPARISON.md          # Platform comparison
```

## 🔍 Quick Reference

### Build Commands

**nRF52840:**
```bash
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
```

**nRF52840 integration-test (on-device E2E runner):**
```bash
cargo build -p magent-integration-test --release --target thumbv7em-none-eabihf
```

**ESP32-C61** (must run from the firmware dir — the workspace root has no
`[build] target`):
```bash
cd firmware/esp32-app
source ~/export-esp.sh   # activate the ESP toolchain
./build-c61.sh           # C61 board (swaps sdkconfig.defaults in/out)
```

**ESP32-S3** (Xtensa; needs the `esp` toolchain from `espup`):
```bash
cd firmware/esp32-app
source ~/export-esp.sh
./build-s3.sh            # `board-s3` feature, Xtensa target
```

### Binary Sizes

Reference sizes of the release ELF on a current build (approximate — they vary
with the toolchain and feature set):

| Platform | Size | Location |
|----------|------|----------|
| nRF52840 | ~182 KB | `target/thumbv7em-none-eabihf/release/magent-nrf52-app` |
| nRF52840 integration-test | ~193 KB | `target/thumbv7em-none-eabihf/release/magent-integration-test` |
| ESP32-C61 | ~2.4 MB (ELF) | `target/riscv32imac-esp-espidf/release/magent-esp32-app` |
| ESP32-S3 | toolchain-dependent | `target/xtensa-esp32s3-espidf/release/magent-esp32-app` |

## 🆘 Troubleshooting

If you encounter issues:

1. **Build errors**: Check [ESP32-C61 Troubleshooting](ESP32_C61_BUILD_TROUBLESHOOTING.md), [ESP32 Build Status](ESP32_BUILD_STATUS.md), or the [nRF52840 Build Guide](NRF52_BUILD_GUIDE.md)
2. **Flashing issues**: Verify your hardware connection and tool installation
3. **Runtime issues**: Check serial output (`espflash monitor` / `defmt` logging)

## 🤝 Contributing

When updating documentation:
1. Update the relevant platform guide
2. Update this index
3. Update the main README if needed


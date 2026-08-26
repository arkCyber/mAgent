# Documentation Index

Complete documentation for the MicroAgent project.

## 📚 Getting Started

| Document | Description |
|----------|-------------|
| [Main README](../README.md) | Project overview, features, and quick start |
| [Firmware README](../firmware/README.md) | Firmware overview for both platforms |

## 🔧 Platform Guides

### nRF52840 (ARM Cortex-M4F)
| Document | Description |
|----------|-------------|
| [Build Guide](NRF52_BUILD_GUIDE.md) | Complete build and deployment guide |
| [Build Status](NRF52_BUILD_STATUS.md) | Build history and current status |
| [Memory Analysis](NRF52_MEMORY_ANALYSIS.md) | Detailed memory usage breakdown |

### ESP32-C61 (RISC-V)
| Document | Description |
|----------|-------------|
| [Build Guide](ESP32_C61_BUILD.md) | Complete build and deployment guide |
| [Build History](ESP32_C61_BUILD_HISTORY.md) | Build troubleshooting history |
| [Troubleshooting](ESP32_C61_BUILD_TROUBLESHOOTING.md) | Common issues and solutions |

## 📊 Comparison & Analysis

| Document | Description |
|----------|-------------|
| [Platform Comparison](PLATFORM_COMPARISON.md) | Feature and performance comparison |

## 🛡️ Reliability & Audits

| Document | Description |
|----------|-------------|
| [Aerospace Code Audit](AUDIT_AEROSPACE_2026.md) | Panic-freedom / bounded-memory audit |
| [LLM Backends](LLM_BACKENDS.md) | DeepSeek / Ollama provider wiring |
| [Summary Store](SUMMARY_STORE.md) | Run-summary schema & CLI |
| [AT Command Reference](AT_COMMAND_REFERENCE.md) | AT (Hayes / ESP-AT) provisioning subset |

## 📁 File Structure

```
docs/
├── README.md                        # This file (index)
│
├── NRF52_BUILD_GUIDE.md           # nRF52840 build guide
├── NRF52_BUILD_STATUS.md          # nRF52840 status
├── NRF52_MEMORY_ANALYSIS.md      # Memory breakdown
│
├── ESP32_C61_BUILD.md             # ESP32-C61 build guide
├── ESP32_C61_BUILD_HISTORY.md     # Build history
├── ESP32_C61_BUILD_TROUBLESHOOTING.md  # Troubleshooting
├── ESP32_C61_BOARD_BOOT_FAILURE.md      # Boot / hardware bring-up notes
├── ESP32_BUILD_STATUS.md          # ESP32 build status
│
├── AT_COMMAND_REFERENCE.md        # AT (Hayes / ESP-AT) provisioning subset
├── LLM_BACKENDS.md                # DeepSeek / Ollama provider wiring
├── SUMMARY_STORE.md               # Run-summary schema & CLI
├── PROMPT_STORE.md                # Stored system-prompt management
├── CONTEXT_MANAGEMENT.md          # Bounded-context windowing
├── MQTT_MCP.md                    # MQTT MCP server
├── CONFIG.md                      # Config file reference
├── API.md                         # API reference
│
├── ARCHITECTURE.md                # Architecture overview
├── PROJECT_OVERVIEW.md            # Project overview
├── EXECUTIVE_SUMMARY.md           # Executive summary
├── HARDWARE.md                    # Hardware notes
│
├── AUDIT_AEROSPACE_2026.md        # Panic-freedom / bounded-memory audit
├── AUDIT_REPORT.md                # Audit report
├── SRS.md                         # Software requirements spec
├── SRS_TRACE.md                   # Requirements traceability
│
└── PLATFORM_COMPARISON.md         # Platform comparison
```

## 🔍 Quick Reference

### Build Commands

**nRF52840:**
```bash
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
```

**ESP32-C61 (must run from the firmware dir — the workspace root has no
`[build] target`):**
```bash
cd firmware/esp32-app
source ~/export-esp.sh   # activate the ESP toolchain (PlatformIO / ESP-IDF)
cargo build --release    # stable toolchain + `-Z build-std` (see ../README.md)
```

**nRF52840 integration-test (on-device E2E runner):**
```bash
cargo build -p magent-integration-test --release --target thumbv7em-none-eabihf
```

### Binary Sizes

Reference sizes of the release ELF on a current build (approximate — they vary
with the toolchain and feature set):

| Platform | Size | Location |
|----------|------|----------|
| nRF52840 | ~182 KB | `target/thumbv7em-none-eabihf/release/magent-nrf52-app` |
| nRF52840 integration-test | ~193 KB | `target/thumbv7em-none-eabihf/release/magent-integration-test` |
| ESP32-C61 | ~2.4 MB (ELF) | `target/riscv32imac-esp-espidf/release/magent-esp32-app` |

## 🆘 Troubleshooting

If you encounter issues:

1. **Build errors**: Check [ESP32-C61 Troubleshooting](ESP32_C61_BUILD_TROUBLESHOOTING.md) or [nRF52840 Build Guide](NRF52_BUILD_GUIDE.md)
2. **Flashing issues**: Verify your hardware connection and tool installation
3. **Runtime issues**: Check serial output with `defmt` logging enabled

## 🤝 Contributing

When updating documentation:
1. Update the relevant platform guide
2. Update this index
3. Update the main README if needed

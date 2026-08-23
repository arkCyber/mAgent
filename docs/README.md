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

## 📁 File Structure

```
docs/
├── README.md                        # This file
│
├── NRF52_BUILD_GUIDE.md           # nRF52840 build guide
├── NRF52_BUILD_STATUS.md          # nRF52840 status
├── NRF52_MEMORY_ANALYSIS.md      # Memory breakdown
│
├── ESP32_C61_BUILD.md             # ESP32-C61 build guide
├── ESP32_C61_BUILD_HISTORY.md     # Build history
├── ESP32_C61_BUILD_TROUBLESHOOTING.md  # Troubleshooting
│
└── PLATFORM_COMPARISON.md         # Platform comparison
```

## 🔍 Quick Reference

### Build Commands

**nRF52840:**
```bash
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
```

**ESP32-C61:**
```bash
cargo +nightly build -p magent-esp32-app --release
```

### Binary Sizes

| Platform | Size | Location |
|----------|------|----------|
| nRF52840 | 193 KB | `target/thumbv7em-none-eabihf/release/magent-nrf52-app` |
| ESP32-C61 | 607 KB | `target/riscv32imac-esp-espidf/release/magent-esp32-app` |

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

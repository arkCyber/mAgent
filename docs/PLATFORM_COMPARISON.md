# MicroAgent Platform Build Summary

## 📊 Overall Build Status

Last Updated: **2026-09-14**

| Platform | Architecture | Status | Binary Size | Flash Usage | RAM Usage | Notes |
|----------|--------------|--------|-------------|-------------|-----------|-------|
| **nRF52840** | ARM Cortex-M4F | ✅ Ready | ~182 KB | ~18% (1 MB) | bounded heapless + stack | Primary smartwatch platform |
| **ESP32-C61** | RISC-V 32-bit | ✅ Ready + verified on HW | 607 KB | 7.4% (607/8192 KB) | ~134 KB dynamic + 2 MB PSRAM | Wi-Fi 6 + BLE 5; OTA dual-slot |
| **ESP32-S3** | Xtensa LX7 (dual-core) | ✅ Verified stable on HW (flagship) | toolchain-dependent | 4 MB board variant | ~390 KB dynamic + 4 MB PSRAM | Wi-Fi STA + DeepSeek cloud LLM + HTTP dashboard |
| ESP32-C3 | RISC-V 32-bit | 🔄 Compatible | TBD | TBD | TBD | Use ESP32-C61 config |
| ESP32-C6 | RISC-V 32-bit | 🔄 Compatible | TBD | TBD | TBD | Use ESP32-C61 config |
| ESP32 | Xtensa LX6 | 🔄 Planned | TBD | TBD | TBD | Requires Xtensa toolchain |

---

## 🎯 Platform Comparison

### nRF52840 (ARM Cortex-M4F)
**Best for**: Wearables, low-power IoT devices, BLE-centric applications

**Specifications**:
- CPU: ARM Cortex-M4F @ 64 MHz
- RAM: 256 KB SRAM
- Flash: 1 MB internal
- Wireless: BLE 5.3, 802.15.4, NFC
- Power: Ultra-low power modes (< 1 µA sleep)
- FPU: Hardware floating-point (FPv4-SP)

**Pros**:
- ✅ Excellent power efficiency
- ✅ Mature toolchain (probe-rs, ARM ecosystem)
- ✅ Small binary size (194 KB)
- ✅ Rich peripheral set (SPI, I2C, UART, ADC, PWM)
- ✅ Strong BLE stack support

**Cons**:
- ❌ No Wi-Fi (BLE only)
- ❌ Limited processing power vs RISC-V
- ❌ Smaller flash (1 MB vs 8 MB)

**Use Cases**:
- Smartwatches and fitness trackers
- Medical wearables
- Asset trackers
- Wireless sensors
- Battery-powered IoT nodes

---

### ESP32-C61 (RISC-V 32-bit)
**Best for**: Connected devices, Wi-Fi 6 applications, higher performance needs

**Specifications**:
- CPU: RISC-V 32-bit @ 160 MHz
- RAM: 320 KB SRAM (~134 KB dynamic) + 2 MB PSRAM
- Flash: 8 MB external
- Wireless: Wi-Fi 6 (802.11ax), BLE 5.3
- Power: Multiple power modes
- Extensions: RVC (compressed instructions)

**Pros**:
- ✅ Wi-Fi 6 support (modern connectivity)
- ✅ Higher CPU frequency (160 MHz vs 64 MHz)
- ✅ More memory (8 MB flash, 832 KB RAM total)
- ✅ Dual connectivity (Wi-Fi + BLE)
- ✅ Growing RISC-V ecosystem

**Cons**:
- ❌ Higher power consumption
- ❌ Larger binary size (607 KB vs 194 KB)
- ❌ Less mature toolchain
- ❌ More complex build setup (ESP-IDF required)

**Use Cases**:
- Smart home devices
- Industrial IoT gateways
- Networked sensors
- Edge computing nodes
- Voice assistants

---

---

### ESP32-S3 (Xtensa LX7, dual-core)
**Best for**: Always-connected agents that need Wi-Fi **and** a cloud LLM in one image

**Specifications**:
- CPU: Xtensa LX7 dual-core @ 240 MHz (PRO = Core 0, APP = Core 1)
- RAM: 512 KB SRAM (~390 KB dynamic) + PSRAM
- Flash: 4 MB (this board variant)
- Wireless: Wi-Fi 2.4 GHz + BLE 5 (LE)
- PSRAM: 4 MB quad (verified working; ~2.18 MB free heap after boot)

**Pros**:
- ✅ Wi-Fi + BLE + agent + cloud LLM all fit (largest internal SRAM)
- ✅ Dual-core deterministic latency (real-time agent on Core 1, network on Core 0)
- ✅ PSRAM-backed heap for large context / HTTP dashboard
- ✅ Verified stable on real hardware (long-run soak, no leak)

**Cons**:
- ❌ BLE disabled in the default build (internal-DRAM constraint)
- ❌ Lua disabled in the default build (`esp_timer` UAF via the WiFi PHY)
- ❌ No OTA on the 4 MB flash variant
- ❌ Xtensa toolchain (via `espup`) required

**Use Cases**:
- Cloud-connected embedded agents (DeepSeek / hosted LLM)
- Edge devices exposing an HTTP admin dashboard
- Latency-sensitive edge reasoning on a dual-core MCU

**Documentation**: [ESP32-S3 Port Success](ESP32_S3_PORT_SUCCESS.md) ·
[Dual-Core Scheduling](DUAL_CORE_SCHEDULING.md)

---

## 🔧 Build Tool Comparison

### nRF52840 Toolchain
```bash
# Required tools
rustup target add thumbv7em-none-eabihf
cargo install probe-rs
cargo install cargo-binutils
rustup component add llvm-tools-preview

# Build command
cd firmware/nrf52-app
cargo build --release

# Flash command
cargo run --release
```

**Pros**: Simple, pure Rust toolchain, fast builds  
**Cons**: None

---

### ESP32 (C61 / S3) Toolchain
```bash
# Required tools
cargo install espup --locked && espup install   # provides the `esp` toolchain
source ~/export-esp.sh
cargo install espflash

# Build command (C61 default / S3)
cd firmware/esp32-app
./build-c61.sh                                  # RISC-V C61
./build-s3.sh                                   # Xtensa S3

# Flash command
./flash-c61.sh                                  # or ./flash-s3.sh
```

**Pros**: Rich Wi-Fi/BLE stack, extensive ESP ecosystem, pinned toolchain via scripts  
**Cons**: Complex setup, requires ESP-IDF, slower builds

---

## 📈 Performance Metrics

### Boot Time (Estimated)
- nRF52840: ~50 ms (bare metal)
- ESP32-C61: ~500 ms (with Wi-Fi init)
- ESP32-S3: ~500 ms (with Wi-Fi init)

### Power Consumption (Typical)
- nRF52840 Active (BLE): 5-15 mA @ 3V
- nRF52840 Sleep: < 1 µA
- ESP32-C61 Active (Wi-Fi): 80-120 mA @ 3.3V
- ESP32-C61 Light Sleep: 1-5 mA
- ESP32-C61 Deep Sleep: 10-50 µA
- ESP32-S3 Active (Wi-Fi): 100-150 mA @ 3.3V

### Code Execution Speed
- nRF52840: 64 MHz ARM (efficient instruction set)
- ESP32-C61: 160 MHz RISC-V (2.5x clock advantage)
- ESP32-S3: 240 MHz dual-core Xtensa LX7 (deterministic dual-core split)

---

## 🛠️ Development Workflow

### nRF52840 Quick Start
```bash
# 1. Setup (one-time)
rustup target add thumbv7em-none-eabihf
cargo install probe-rs

# 2. Build
cd firmware/nrf52-app
cargo build --release

# 3. Flash and monitor
cargo run --release

# 4. Check binary size
cargo size --release
```

### ESP32 (C61 / S3) Quick Start
```bash
# 1. Setup (one-time)
cargo install espup --locked && espup install   # `esp` toolchain
source ~/export-esp.sh
cargo install espflash

# 2. Build
cd firmware/esp32-app
./build-c61.sh          # or ./build-s3.sh

# 3. Flash and monitor
./flash-c61.sh          # or ./flash-s3.sh --port /dev/cu.usbserial-XXX
espflash monitor

# 4. Check binary size
ls -lh target/*/release/magent-esp32-app
```

---

## 📚 Documentation Links

### Platform-Specific Guides
- [nRF52840 Build Status](NRF52_BUILD_STATUS.md)
- [ESP32-C61 Build Guide](ESP32_C61_BUILD.md)
- [ESP32-C61 Build & Operations](ESP32.md)
- [ESP32-C61 Build History](ESP32_C61_BUILD_HISTORY.md)
- [ESP32-S3 Port Success](ESP32_S3_PORT_SUCCESS.md)
- [Dual-Core Scheduling (S3)](DUAL_CORE_SCHEDULING.md)

### Framework Documentation
- [Embassy Framework (nRF52)](https://embassy.dev/)
- [ESP-IDF (ESP32)](https://docs.espressif.com/projects/esp-idf/en/latest/)
- [esp-rs Book](https://esp-rs.github.io/book/)
- [probe-rs (nRF52 flashing)](https://probe.rs/)
- [espflash (ESP32 flashing)](https://github.com/esp-rs/espflash)

### Hardware Datasheets
- [nRF52840 Product Spec](https://www.nordicsemi.com/products/nrf52840)
- [ESP32-C61 Datasheet](https://www.espressif.com/en/products/socs/esp32-c61)
- [ESP32-S3 Datasheet](https://www.espressif.com/en/products/socs/esp32-s3)

---

## 🎯 Recommended Platform Selection

### Choose nRF52840 if you need:
- ✅ Long battery life (months to years)
- ✅ BLE-only connectivity
- ✅ Compact code size
- ✅ Wearable/portable devices
- ✅ Low-latency sensor processing

### Choose ESP32-C61 if you need:
- ✅ Wi-Fi connectivity (especially Wi-Fi 6)
- ✅ Cloud integration
- ✅ Higher processing power
- ✅ More memory headroom
- ✅ Dual wireless (Wi-Fi + BLE)

### Choose ESP32-S3 if you need:
- ✅ Wi-Fi + BLE + agent + **cloud LLM** in one firmware image
- ✅ Dual-core deterministic latency (real-time + network isolation)
- ✅ PSRAM for large context / an on-device HTTP dashboard
- ✅ The largest internal SRAM of the supported ESP targets

### Use Both if you're building:
- 🎯 Multi-tier IoT system (nRF52 for sensors, ESP32 for gateway)
- 🎯 Hybrid wearable (nRF52 for watch, ESP32 for companion device)
- 🎯 Learning platform (compare architectures and trade-offs)

---

## 🚀 Next Steps

1. **nRF52840**: Hardware-in-the-loop automation, power profiling
2. **ESP32-C61**: Expand on-HW validation coverage (Tier-6 runbook)
3. **ESP32-C3/C6**: Port ESP32-C61 config (minor adjustments)
4. **ESP32-S3**: Real-board validation of the Lua app sandbox; OTA is gated by the 4 MB flash variant
5. **Integration**: Build agent protocol for cross-platform communication

---

**Status**: nRF52840, ESP32-C61 and ESP32-S3 are production-ready — the S3 is the
current flagship ESP target. 🎉

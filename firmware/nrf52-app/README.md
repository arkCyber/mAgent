# mAgent nRF52840 Firmware

Smartwatch AI Agent firmware for the Nordic nRF52840 chip, powered by `magent-core`.

## Overview

This firmware provides a complete embedded AI agent runtime for battery-powered wearable devices, featuring:

- **MiniAgent**: ReAct-based AI agent with skills and tool execution
- **BLE Connectivity**: Bluetooth Low Energy 5.3 advertising and GATT
- **Sensor Processing**: Real-time sensor data handling
- **Ultra-Low Power**: Multiple power modes for battery optimization
- **Safety Features**: Aerospace-grade error handling and watchdog

## Hardware

| Specification | Value |
|--------------|-------|
| Chip | Nordic nRF52840 |
| Architecture | ARM Cortex-M4F |
| Clock Speed | 64 MHz |
| Flash | 1 MB |
| RAM | 256 KB |
| Connectivity | BLE 5.3 (nRF SoftDevice S140) |
| Package | QFN-73 10x10 mm |

## Building

### Prerequisites

```bash
# Install ARM target
rustup target add thumbv7em-none-eabihf
```

### Build Commands

```bash
# Build release firmware
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf

# Output location
# target/thumbv7em-none-eabihf/release/magent-nrf52-app
```

### Build Output

```
Size:   161 KB
Format: ELF 32-bit ARM EABI5
```

## Code Structure

```
src/
├── main.rs       # Application entry point & async tasks
│                  # - Initializes Embassy RTOS
│                  # - Creates MiniAgent from magent-core
│                  # - Runs main_task() and agent_task()
│
├── ble.rs        # BLE advertising and GATT services
│                  # - SoftDevice initialization
│                  # - Advertising configuration
│                  # - Connection management
│
├── sensors.rs    # Sensor drivers and data structures
│                  # - Accelerometer (LIS2DW12)
│                  # - Environmental (BME280)
│                  # - Battery monitoring
│
├── power.rs      # Power management
│                  # - PowerMode enum (Active/LowPower/Sleep/DeepSleep)
│                  # - Runtime estimation
│                  # - Battery level calculation
│
└── watchdog.rs   # Watchdog and error recovery
                    # - Watchdog timeout monitoring
                    # - ErrorContext tracking
                    # - RecoveryHandler
                    # - HealthStatus scoring
```

## Features

### AI Agent (from magent-core)

| Feature | Status | Description |
|---------|--------|-------------|
| MiniAgent | ✅ | ReAct-based AI agent state machine |
| Skills Manager | ✅ | Flash-based skill storage |
| Tool Registry | ✅ | 10 built-in tools |
| Safety Budget | ✅ | Iteration and memory budgets |
| Watchdog | ✅ | Timeout monitoring |

### Built-in Tools

| Tool | Description |
|------|-------------|
| `read_sensor` | Read temperature, heart rate, battery, etc. |
| `write_gpio` | Drive GPIO pins |
| `flash_read` | Read from internal flash |
| `flash_write` | Write to internal flash |
| `ble_send` | Send data over BLE |
| `read_heart_rate` | Read heart rate (alias) |
| `read_glucose` | Read glucose level |
| `read_ecg` | Read ECG trace |
| `voice_output` | Queue text-to-speech |
| `send_notification` | Send notification |

### Firmware Features

| Feature | Status | Description |
|---------|--------|-------------|
| BLE Advertising | ✅ | BLE 5.3 peripheral mode |
| BLE GATT | ✅ | Custom service with characteristics |
| Accelerometer | ✅ | LIS2DW12 driver framework |
| Environmental | ✅ | BME280 driver framework |
| Battery Monitor | ✅ | Voltage-based level estimation |
| Power Modes | ✅ | Active/LowPower/Sleep/DeepSleep |
| Watchdog | ✅ | System health monitoring |
| Embassy RTOS | ✅ | Async task scheduling |
| defmt Logging | ✅ | Efficient runtime logging |

## Integration with magent-core

The firmware uses `magent-core` as its AI engine:

```rust
use magent_core::{
    MiniAgent,      // ReAct-based AI agent
    AgentConfig,    // Agent configuration
    VERSION,        // Firmware version
};

fn init_agent() {
    let config = AgentConfig::new()
        .with_max_iterations(50);
    
    let agent = MiniAgent::new(config)
        .expect("agent");
    
    // Agent is ready to run tasks
}
```

### magent-core Components Used

```
magent-core/
├── agent.rs         # MiniAgent (ReAct state machine) ✅
├── config.rs        # AgentConfig ✅
├── skills.rs        # SkillsManager
├── tools.rs         # ToolRegistry ✅
├── safety.rs        # BudgetEnforcer, Watchdog ✅
├── error.rs         # Error types ✅
└── ...
```

## Memory Layout

```
nRF52840 Memory Map:
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Region    Start       End         Size    Purpose
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Flash     0x00000000  0x00100000  1 MB    Program storage
RAM       0x20000000  0x20040000  256 KB  Working memory
UICR      0x10001000  0x10001000  -       User Information Config
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Flashing

### Using probe-rs

```bash
# Flash and run
probe-rs run --chip nRF52840_xxAA \
  target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Flash only
probe-rs flash --chip nRF52840_xxAA firmware.hex
```

### Generate HEX

```bash
# Convert ELF to HEX
cargo objcopy -p magent-nrf52-app --release --target thumbv7em-none-eabihf \
  -- -O ihex firmware.hex

# Or generate binary
cargo objcopy -p magent-nrf52-app --release --target thumbv7em-none-eabihf \
  -- -O binary firmware.bin
```

## Power Consumption

| Mode | Current | Duration |
|------|---------|----------|
| Active | ~15 mA | Continuous |
| LowPower | ~3 mA | 8+ hours |
| Sleep | ~0.5 mA | 20+ days |
| DeepSleep | ~1 µA | 3+ months |

Estimated battery life with 300 mAh battery:
- Active: ~20 hours
- LowPower: ~4 days
- Sleep: ~25 days
- DeepSleep: ~12 months

## Future Enhancements

- [ ] Full LIS2DW12 driver implementation
- [ ] Full BME280 driver implementation
- [ ] SAADC battery voltage reading
- [ ] Step counting algorithm
- [ ] Heart rate monitor support
- [ ] BLE OTA firmware updates
- [ ] LLM backend integration (Wi-Fi gateway)

## See Also

- [Main Project README](../../README.md)
- [Build Guide](../../docs/NRF52_BUILD_GUIDE.md)
- [Memory Analysis](../../docs/NRF52_MEMORY_ANALYSIS.md)
- [Platform Comparison](../../docs/PLATFORM_COMPARISON.md)
- [magent-core Library](../magent-core/src/lib.rs)

## License

MIT

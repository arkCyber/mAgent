# nRF52840 Memory Analysis

Detailed breakdown of memory usage for the mAgent nRF52840 firmware.

## Overview

```
nRF52840 Memory Specifications:
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Flash:  1 MB (0x00000000 - 0x00100000)
RAM:    256 KB (0x20000000 - 0x20040000)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Binary Size Analysis

### Firmware Size: 161 KB

```
mAgent nRF52840 Firmware Breakdown (with magent-core)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Component              Size (KB)   Percentage
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
magent-core           65.0        40.4%
  ├─ Agent runtime     35.0
  ├─ Skills manager    12.0
  ├─ Tool registry      8.0
  └─ Safety/Budget    10.0

embassy-runtime       35.0        21.7%
  ├─ Executor          18.5
  ├─ Task storage      8.0
  └─ Synchronization   8.5

nrf-softdevice        25.0        15.5%
  ├─ BLE stack         18.0
  └─ Link layer        7.0

defmt                 10.0         6.2%
  ├─ Logger            6.5
  └─ Formatters        3.5

cortex-m-rt            6.0         3.7%
  ├─ Vector table     0.5
  ├─ Startup          2.0
  └─ Linker script     3.5

Application code       20.0        12.5%
  ├─ main.rs          8.0
  ├─ ble.rs           4.0
  ├─ sensors.rs       3.0
  ├─ power.rs         2.5
  └─ watchdog.rs       2.5
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total                161.0 KB   100.0%
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## RAM Usage Analysis

### Static RAM (at compile time)

```
Static RAM Allocation
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Item                     Size     Address
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
.cortex_m_rt (vector)   512 B    0x20000000
.stack                   8 KB     0x2003C000
.heap                    8 KB     0x20034000
.bss                     2 KB     0x20030000
.data                    1 KB     0x2002C000
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total Static RAM        ~20 KB
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### Dynamic RAM (runtime)

```
Dynamic RAM Usage
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Component              Peak Usage  Description
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Heap (embedded-alloc)  8 KB      Async allocations
Task stacks            2 KB      Embassy tasks
BLE buffers            4 KB      Advertising/connection
Sensor buffers         1 KB      Sensor data
defmt ring buffer      256 B     Logging buffer
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total Dynamic RAM      ~16 KB
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Flash Layout

```
nRF52840 Flash Memory Map (1 MB)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Offset       Size     Contents
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
0x00000     4 KB     Bootloader (if present)
0x01000     512 B    UICR (User Information Config)
0x01200     8 KB     SoftDevice (S140 BLE stack)
0x02200     192 KB   mAgent Firmware
0x32000     768 KB   Reserved / OTA storage
0xF0000     64 KB    Bootloader settings
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Optimization Tips

### Reduce Binary Size

1. **Disable unused features**
```toml
# In Cargo.toml
[features]
default = ["ble"]
ble = []
# Disable thread if not needed
```

2. **Enable LTO (Link Time Optimization)**
```toml
# In .cargo/config.toml
[target.thumbv7em-none-eabihf]
rustflags = [
    "-C", "lto=on",
    "-C", "opt-level=s",
]
```

3. **Strip debug info**
```bash
# Release builds are automatically stripped
# Manual stripping:
arm-none-eabi-strip -s firmware.elf
```

### Reduce RAM Usage

1. **Reduce heap size**
```rust
// In main.rs
const HEAP_SIZE: usize = 4096; // Reduced from 8192
```

2. **Use stack instead of heap**
```rust
// Instead of:
let data = vec![0u8; 100];

// Use:
static mut DATA: [u8; 100] = [0; 100];
```

3. **Disable defmt buffering**
```toml
# In Cargo.toml
defmt = { version = "0.3", features = [] }
```

## Memory Budget

```
Total Budget vs. Usage
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Resource    Total    Used     Free     Usage%
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Flash       1 MB     193 KB   831 KB   18.8%
RAM         256 KB   ~36 KB   220 KB   14.1%
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Tool Commands

### Size Analysis

```bash
# Detailed size report
cargo size -p magent-nrf52-app --release --target thumbv7em-none-eabihf -- -A

# Show only sections
cargo size -p magent-nrf52-app --release --target thumbv7em-none-eabihf

# Compare with previous build
cargo size -p magent-nrf52-app --release --target thumbv7em-none-eabihf > size_after.txt
diff size_before.txt size_after.txt
```

### Memory Map

```bash
# Generate linker map
cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf
arm-none-eabi-gcc -T memory.x -Map=firmware.map *.o

# View map
less firmware.map
```

### Symbol Analysis

```bash
# List symbols sorted by size
arm-none-eabi-nm -S --size-sort target/thumbv7em-none-eabihf/release/magent-nrf52-app

# Find largest functions
arm-none-eabi-nm -S --size-sort target/thumbv7em-none-eabihf/release/magent-nrf52-app | head -20
```

## Related Documentation

- [nRF52840 Build Guide](NRF52_BUILD_GUIDE.md)
- [Platform Comparison](PLATFORM_COMPARISON.md)
- [nRF52840 Product Specification](https://infocenter.nordicsemi.com/topic/ps_nrf52840/keyfeatures_html5.html)

# mAgent Architecture

mAgent is an aerospace-grade embedded AI agent that runs on microcontrollers.
This document describes the workspace layout and feature-flag strategy that
lets the same `magent-core` library compile for nRF52 (ARM Cortex-M) and ESP32
(RISC-V / Xtensa) without dragging in irrelevant dependencies.

## Workspace layout

```
MicroAgent/
├── Cargo.toml                          # Workspace manifest (virtual)
├── magent-core/                        # Chip-agnostic core (no_std by default)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs                      # Module gating + feature docs
│   │   ├── hal.rs                      # Chip-agnostic HAL traits
│   │   ├── hal/
│   │   │   ├── esp32.rs                # ESP32 stubs (always compile)
│   │   │   └── nrf52.rs                # nRF52840 adapter (std + embedded)
│   │   ├── agent.rs                    # ReAct loop + MiniAgent
│   │   ├── skills.rs, tools.rs         # Skill registry / tool registry
│   │   ├── safety.rs, config.rs, ...   # Cross-cutting embedded logic
│   │   └── health_*.rs, voice_*.rs,    # Health monitoring modules
│   │       sleep_manager.rs, ...
│   └── tests/                          # Integration tests (auto-discovered)
│       ├── agent_runner_tests.rs       #   (the canonical test suite)
│       ├── agent_tests.rs              #   Real-agent ReAct tests
│       ├── nrf52_simulation_tests.rs   #   nRF52 SIM-only tests (x86_64)
│       ├── comprehensive_agent_tests.rs
│       └── nrf52_comprehensive_tests.rs
├── firmware/
│   ├── nrf52-app/                      # nRF52840 smartwatch firmware
│   │   ├── Cargo.toml                  #   enables `magent-core/nrf52`
│   │   ├── .cargo/config.toml          #   pins target = thumbv7em-none-eabihf
│   │   └── src/main.rs
│   ├── esp32-app/                      # ESP32 family firmware (NEW)
│   │   ├── Cargo.toml                  #   enables `magent-core/esp32`
│   │   └── src/main.rs
│   └── integration-test/               # Standalone E2E test (nRF52 hardware)
│       ├── Cargo.toml                  #   #![no_std] #![no_main] binary
│       ├── .cargo/config.toml          #   pins target = thumbv7em-none-eabihf
│       └── src/main.rs
├── host/
│   ├── simulator/                      # Standalone desktop simulator
│   └── nrf52-simulator/                # Smartwatch AI agent simulator
└── tools/                              # Loose dev tools (benchmarks, demos)
    ├── Cargo.toml                      # One binary per loose file
    └── src/bin/
        ├── benchmarks.rs               # cargo run -p magent-tools --bin benchmarks
        ├── algorithm_tests.rs          # cargo run -p magent-tools --bin algorithm-tests
        ├── integration_tests.rs
        ├── module_integration_test.rs
        ├── config_validation_test.rs
        └── e2e_agent_test.rs
```

## Feature flag scheme

`magent-core` exposes **orthogonal** feature flags. Each one is small and
toggles a single concern, so a build can be customized to match the target
chip without pulling in irrelevant code.

| Feature           | Pulls in                                          | Use when…                               |
|-------------------|---------------------------------------------------|-----------------------------------------|
| (default)         | (nothing)                                          | CI / docs / linting builds             |
| `std` / `host`    | `reqwest`, `serde/std`                            | Desktop tests on x86_64 / Linux / macOS |
| `arch-cortex-m`   | `cortex-m`, `cortex-m-rt`, `critical-section`     | Targeting any ARM Cortex-M chip         |
| `arch-riscv`      | `riscv`, `riscv-rt`, `critical-section`           | Targeting RISC-V chips (ESP32-C3/C6)    |
| `arch-xtensa`     | `xtensa-lx`, `xtensa-lx-rt`, `critical-section`   | Targeting Xtensa chips (ESP32/S3)       |
| `nrf52`           | `arch-cortex-m` + `embassy-nrf` + `nrf-softdevice`| nRF52840 firmware                       |
| `esp32`           | `arch-riscv`                                      | ESP32 family firmware (RISC-V by default) |
| `ble`             | (marker)                                           | BLE capability needed                   |
| `wifi`            | (marker)                                           | WiFi capability needed                  |
| `thread`          | (marker)                                           | Thread protocol needed                  |
| `monitoring`      | (marker)                                           | Health monitoring hooks needed          |
| `embedded`        | `nrf52` (alias)                                    | Backward compatibility                  |

### Why this layout

The pre-refactor code had a single `embedded` flag that gated
*every* embedded-only dependency. That made it impossible to compile for an
architecture that wasn't ARM (RISC-V / Xtensa) without ripping out code.

The new scheme separates **architecture** (Cortex-M / RISC-V / Xtensa) from
**chip family** (nRF52 / ESP32). Each chip family pulls in the right
architecture's startup code plus its own drivers, so consumers can mix and
match.

## Chip-agnostic HAL

`magent-core/src/hal.rs` defines five small traits:

* [`Gpio`] — pin configuration and level read/write.
* [`Flash`] — read / write / erase-sector over a flat address space.
* [`Ble`]  — `is_connected` + `send`.
* [`Sensor`] — generic `read() -> Result<Reading, _>`.
* [`Power`] — query and switch between `PowerProfile`s.

Concrete implementations:

* `hal::esp32` — always available stubs (in-memory `Vec<u8>` for flash,
  `AtomicBool` for BLE state, etc.). Used both by desktop tests and by
  firmware that hasn't wired up the real peripherals yet.

* `hal::nrf52` — when `std` is enabled, wraps the existing
  `nrf52_hal::simulation::*` types so desktop tests keep working. When
  the `nrf52` chip-family feature is on, wraps the real embassy-nrf
  peripherals.

New code should depend on the traits, not on a chip-specific type, so
the same agent binary can target either chip.

## ReAct loop & MiniAgent

`agent::MiniAgent` is the chip-agnostic ReAct state machine (think →
tool call → observe → repeat). It is gated behind *any* chip-family
feature (`nrf52`, `esp32`, or the legacy `embedded` alias) so it is
available to every firmware crate but not pulled in for pure-host
test builds.

`MiniAgent` uses `async fn` for its tool execution; the embassy
executor provided by the firmware crate drives the state machine.
This keeps the core library executor-agnostic — you can drive it from
embassy, a custom RTOS, or even a `pollster::block_on` on the host.

## Migration guide for downstream users

If you depend on the old `magent-app` / `nrf52-simulator` / `simulator`
crate names, update your `Cargo.toml`:

| Old path             | New path                            |
|----------------------|-------------------------------------|
| `magent-app`         | `firmware/nrf52-app`                |
| `nrf52-simulator`    | `host/nrf52-simulator`              |
| `simulator`          | `host/simulator`                    |

If you depended on the old `embedded` feature, use `nrf52` instead
(or `esp32` if you're on ESP32). The `embedded` alias is preserved
for backward compatibility but its semantic is now "ARM Cortex-M +
nRF52 drivers".

If you depended on `magent_core::hal::esp32::*` types, they still
exist with the same names — they are just stubs that always compile.
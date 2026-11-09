# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

### Host-side (default members)
- **Full host check** (fmt→clippy→test→feature matrix): `./test-workspace.sh`
- **Clippy all default members**: `cargo clippy --all-targets -- -D warnings`
- **Run host tests**: `cargo test`
- **Run single test**: `cargo test -p magent-core --test agent_runner_tests -- <test_name>`
- **Run specific crate tests**: `cargo test -p magent-core` / `cargo test -p magent`
- **Feature-gated CLI tests**: `cargo test -p magent --features web3,blockchain --lib`
- **S3 memory profile tests**: `cargo test -p magent-core --features std,s3-8mb-psram --lib`
- **Feature matrix check**: `cargo check -p magent-core --no-default-features --features <feat> --all-targets`

### Firmware builds (cross-compilation)
- **ESP32-C61**: `cd firmware/esp32-app && ./build-c61.sh`
- **ESP32-S3**: `cd firmware/esp32-app && ./build-s3.sh`
- **nRF52840**: `cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf`

### Static analysis
- **Format check**: `cargo fmt --all -- --check`
- **Clippy specific**: `cargo clippy -p magent-core --all-targets -- -D warnings`
- **Coverage**: `scripts/check-coverage.sh`
- **Dangerous patterns**: `tools/ci/dangerous_patterns.sh`

## Architecture

### Crate dependency graph
```
magent-core (no_std, chip-agnostic)
  ├── magent-hal (std, trait surface + host simulators)
  ├── magent (CLI) — agent runner, REPL, line editor
  ├── magent-nrf52-app — nRF52840 firmware (Embassy, bare-metal)
  ├── magent-esp32-app — ESP32-C61/S3 firmware (esp-idf-svc, std)
  ├── magent-lua — Lua 5.4 scripting host
  └── mcp-tool-executor — async MCP tool executor
```

- **`magent-core`**: The core AI agent library — `MiniAgent` (ReAct state machine), `AgentRunner`, bounded conversation, AT parser, tool registry, safety/sleep/health modules. Feature-flagged for `nrf52`, `esp32`, `std`, `web3`, etc. Default features = empty for no_std.
- **`magent-hal`**: Chip-agnostic HAL traits (`Gpio`, `Flash`, `Ble`, `Sensor`, `Power`) + host-side nRF52840 simulator.
- **`firmware/esp32-app/src/`**: Shared source tree for C61 (RISC-V) and S3 (Xtensa), selected at compile time via `board-c61`/`board-s3` features.

### Key design patterns
- **Panic-free**: Zero `.unwrap()`/`.expect()`/`.panic!()` in production code. Use `Result` everywhere. `panic_in_result_fn = "deny"` lint enforces this.
- **Bounded data structures**: Use `heapless` (stack-allocated, fixed-capacity) instead of `std::vec::Vec` in the agent core. `Vec` only in host-side (std) code.
- **Feature gate everything**: No module pulls in chip-specific or std-only code without a cargo feature flag. `nrf52`/`esp32` imply `arch-cortex-m`/`arch-xtensa`.
- **DO-178C/ISO 26262/IEC 61508 design philosophy**: Resource budgets on every operation, input validation at every boundary, graceful degradation, watchdog integration.

### Aerospace-grade lint rules (Cargo.toml workspace lints)
```
unsafe_op_in_unsafe_fn = "deny"
missing_docs = "warn"
panic_in_result_fn = "deny"
undocumented_unsafe_blocks = "deny"
mutex_atomic = "deny"
```

### Hardware platforms
| Platform | Arch | Runtime | Status |
|---|---|---|---|
| nRF52840 | ARM Cortex-M4F | Embassy (no_std) | Compiles, scaffold only — not a working agent |
| ESP32-C61 | RISC-V 32-bit | esp-idf-svc (std) | Verified on HW, Wi-Fi 6 |
| ESP32-S3 | Xtensa LX7 | esp-idf-svc (std) | Flagship, verified stable, DeepSeek LLM |

## Important gotchas

- **Never use `cargo test --workspace`**: Firmware crates use `#[link_section]` attributes that fail on Mach-O (macOS). Firmware has dedicated CI jobs.
- **Firmware tests cannot run on host**: They require chip-specific toolchains. Always use `--target` or the dedicated build scripts.
- **`#[cfg(test)]` modules compile only in test targets**: Always use `--all-targets` when checking feature compilation (`cargo check -p X --features Y --all-targets`).
- **ESP32-C61 is the default board** in `firmware/esp32-app/.cargo/config.toml`. Build with `board-s3` feature for S3.
- **BLE is removed** from both ESP32 targets (internal DRAM constraint). `AT+BLE=` still parses and returns `+CMDER:9`.
- **Patches in `.cargo-patches/`**: 15 vendored patches for ESP-IDF/RISC-V compatibility. Don't remove them.
- **CI currently has `continue-on-error: true` for fmt** — hundreds of pre-existing formatting diffs. Format normalization is a separate standalone PR.
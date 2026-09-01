# mAgent - Aerospace-Grade Embedded AI Agent

[![CI](https://img.shields.io/github/actions/workflow/status/arkCyber/mAgent/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/arkCyber/mAgent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square)](LICENSE)
[![Workspace version](https://img.shields.io/badge/workspace-v0.1.0-informational.svg?style=flat-square)](Cargo.toml)
[![Audit status](https://img.shields.io/badge/audit-internal%20self--audit-orange.svg?style=flat-square)](SECURITY_AUDIT.md)

> **Confidentiality notice**: This repository is the open-source codebase of
> the **mAgent** project (target commercial brand: **arkChip-mAgent**). The
> most recent security audit is an **internal AI-assisted self-audit**,
> not a third-party independent audit; the audit timeline commitment is
> tracked in [`SECURITY_AUDIT.md`](SECURITY_AUDIT.md) and the commercial
> pitch deck. Do not represent this codebase as certified to DO-178C,
> ISO 26262, or IEC 61508 unless a third-party report says so.

| Language | Rust |
|----------|------|
| Platform | Embedded / Bare-metal |
| Architecture | ARM Cortex-M4F (nRF52840), RISC-V (ESP32-C61), Xtensa LX7 (ESP32-S3) |

Aerospace-grade AI agent for nRF52840 smartwatches and embedded devices. Built
with Rust: the nRF52840 path uses Embassy (bare-metal, no OS); the ESP32-C61
and ESP32-S3 paths use `esp-idf-svc` (std) with real Wi-Fi, local hardware
tools, and a bidirectional UART command interface.

## 🚀 Aerospace-Grade Safety Features

- **Bounded Panics**: All runtime error paths use `Result` types; the firmware
  production path has **zero** `.unwrap()`/`.expect()` (only `#[cfg(test)]`
  code uses them), so no runtime condition can panic the board.
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
- **RAM**: 320 KB SRAM + 2 MB PSRAM (N8R2 in-package)
- **Flash**: 8 MB
- **Wireless**: Wi-Fi 6, BLE 5 (LE)

#### Memory utilization
The 2 MB in-package PSRAM is enabled and backs the `std::alloc` heap
(`CONFIG_SPIRAM=y` + `CONFIG_SPIRAM_USE_MALLOC=y`, pinned to 80 MHz to match
flash), so dynamic allocations (serde_json, larger context, the ReAct runner's
conversation history) live on the 2 MB heap. The embedded `MiniAgent` itself
uses bounded `heapless` buffers by design (aerospace-grade), so its own
footprint is a small fixed amount of the 320 KB SRAM. The configured agent
memory budget defaults to 512 KiB and is configurable up to
`MAX_CONFIGURABLE_MEMORY` (1 MiB) — a safety ceiling, not the available RAM.
Free-heap is logged periodically by the health monitor and triggers a warning
below 64 KiB.

### ESP32-S3 (Xtensa LX7)
- **MCU**: ESP32-S3 (dual-core Xtensa LX7, AI acceleration)
- **Flash**: 4 MB (this board's variant)
- **PSRAM**: 4 MB quad PSRAM (verified working; free-heap ~2.18 MB after boot)
- **Wireless**: Wi-Fi 2.4 GHz + BLE 5 (LE); also supports Wi-Fi 6 / BLE 5.3 in higher bins
- **Status**: ✅ **Verified stable on real hardware** — boot, agent thread, local
  tools, Wi-Fi STA, and the HTTP admin dashboard all work on the board (long-run
  soak with no leak). This is the current flagship ESP target.

**Bring-up notes** (see [`docs/ESP32_S3_PORT_SUCCESS.md`](docs/ESP32_S3_PORT_SUCCESS.md)):
- Built with the ESP-rs toolchain (`cargo +esp`), Xtensa target
  `xtensa-esp32s3-espidf`, the `board-s3` Cargo feature, and the S3 sdkconfig.
- BLE is **off** (internal-DRAM constraint) and Lua is **off** (WiFi PHY
  `esp_timer` use-after-free) on this firmware; DeepSeek cloud LLM, Wi-Fi,
  the HTTP status dashboard, and bidirectional UART are the working paths.
- TLS T1/T2 now pass with the SSL buffers moved to PSRAM.

## ✅ Build Status

| Platform | Architecture | Status | Build Command | Notes |
|----------|--------------|--------|---------------|-------|
| **nRF52840** | ARM Cortex-M4F | ✅ Ready | `cargo build -p magent-nrf52-app --release --target thumbv7em-none-eabihf` | Primary smartwatch platform, BLE 5.3 |
| **ESP32-C61** | RISC-V 32-bit | ✅ Ready + Verified on HW | `cd firmware/esp32-app && ./build-c61.sh` | Wi-Fi 6 + BLE 5.0, std (esp-idf-svc), real local tools, bidirectional UART |
| ESP32-C3/C6 | RISC-V 32-bit | 🔄 Compatible | Use ESP32-C61 config | Same architecture |
| **ESP32-S3** | Xtensa LX7 | ✅ Verified on HW (stable, flagship) | `cd firmware/esp32-app && ./build-s3.sh` | 4MB flash + 4MB quad PSRAM; WiFi STA + DeepSeek cloud LLM + HTTP dashboard verified stable on HW (long-run soak, no leak); TLS T1/T2 (SSL buffers→PSRAM); BLE off (internal-DRAM); Lua off (WiFi PHY esp_timer UAF) |

## 🏗️ Project Structure

```
MicroAgent/
├── Cargo.toml                 # Workspace root (members, patches, lints)
├── README.md                  # This file
│
├── magent-core/              # Core AI Agent Library (chip-agnostic, no_std)
│   ├── src/
│   │   ├── lib.rs           # Module exports, version constants
│   │   ├── agent.rs         # MiniAgent — ReAct state machine
│   │   ├── agent_runner.rs  # Host runner with LLM (Ollama/DeepSeek) integration
│   │   ├── at.rs            # AT (Hayes / ESP-AT) command parser (no_std)
│   │   ├── at_validate.rs   # Host-tested AT value validators
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
│   │   ├── ingress.rs       # IngressGateway (link adapters)
│   │   ├── recovery.rs      # Exponential-backoff retry manager
│   │   ├── conversation.rs  # Bounded conversation history
│   │   ├── time_sync.rs     # SNTP time synchronisation (no_std)
│   │   ├── boot_key.rs      # Boot-key derivation
│   │   ├── simulator.rs     # Host simulator
│   │   ├── real_tools.rs    # Host SimulatorExecutor (tool backend)
│   │   ├── wifi_pass_seal.rs / wifi_pass_seal_v2.rs  # NVS secret sealing (DBO2)
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
│   │   ├── summary/        # Run-summary store (record schema + CLI helpers)
│   │   ├── web3/           # Web3 / blockchain (feature-gated)
│   │   │   ├── mod.rs
│   │   │   ├── identity.rs / did.rs / signature.rs
│   │   │   ├── verifiable_credentials.rs
│   │   │   ├── blockchain/ # Secp256k1, transaction, HTTP client, tools
│   │   │   └── wallet/     # BIP-39 / BIP-32 + encrypted keystore
│   │   └── web3_app/       # Signed run-report / prompt envelopes
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
│   ├── nrf52-app/          # nRF52840 firmware (Embassy, bare-metal)
│   │   ├── src/main.rs      # Entry point, tasks
│   │   ├── src/{ble,sensors,power,watchdog}.rs
│   │   ├── Cargo.toml · memory.x · build.rs · .cargo/config.toml
│   ├── esp32-app/          # ESP32 firmware (esp-idf-svc, std) — C61 + S3 boards
│   │   ├── src/main.rs      # Entry point, event loop
│   │   ├── src/{at_dispatch,ble_at,ble_config,ble_gatt,ble_wallet,
│   │   │        device_key,link_adapters,llm,local_tools,sntp_sync,
│   │   │        web_admin,latency_metrics,core_affinity,rt_watchdog,
│   │   │        ping,lua_task,blockchain_transport}.rs
│   │   ├── Cargo.toml · build.rs · sdkconfig.defaults · sdkconfig.s3.defaults
│   │   │        · sdkconfig.c61.defaults · partitions.s3.csv
│   │   │        · build-c61.sh · build-s3.sh · flash-c61.sh · flash-s3.sh
│   │   │        · .cargo/config.toml
│   └── integration-test/    # nRF52840 on-device E2E test runner
│
├── host/                     # Host-side tooling
│   ├── simulator/          # Desktop simulator
│   ├── nrf52-simulator/    # nRF52840 simulator
│   ├── email-mcp/          # Email MCP server (IMAP/SMTP)
│   ├── mqtt-mcp/           # MQTT MCP server
│   ├── mcp-tool-executor/  # MCP tool executor
│   ├── lua-app/            # Lua 5.4 scripting host: hardware.* + agent.reason + AppRuntime (61 tests)
│   └── magent-man/         # Tauri desktop "Device Manager" app (BLE config)
│
├── cli/                     # `magent` command-line tool
│   └── src/{main,cli,runner,config,prompt,summary,web3,
│           web3_blockchain,email_executor,blockchain_executor,
│           scheduler,doctor,output}.rs
│
├── tools/                   # Development tools (benchmarks / algorithm demos)
│   └── src/bin/{benchmarks,algorithm-tests,integration-tests,
│                module-integration-test,config-validation-test,
│                e2e-agent-test}.rs
│
├── examples/                # Example applications (workspace glob)
│
└── docs/                    # Documentation (see index below)
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

**For ESP32-S3 (Xtensa LX7):**
- ESP-rs toolchain via `cargo install espup && espup install` (provides `cargo +esp`)
- PlatformIO ESP-IDF framework (auto-managed by build)
- espflash / esptool for firmware deployment

### Installation

```bash
# Install Rust targets for the chips you want to build for
rustup target add thumbv7em-none-eabihf       # nRF52840 (ARM Cortex-M4F) ✅ Ready
rustup target add riscv32imac-esp-espidf      # ESP32-C61 (RISC-V) ✅ Ready
# ESP32-S3 (Xtensa LX7) uses the ESP-rs toolchain, not rustup targets:
cargo install espup && espup install          # provides `cargo +esp`

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

**ESP32-S3 Firmware (build & flash):**
```bash
# Build the S3 image (compile-time board switch via the `board-s3` feature)
cd firmware/esp32-app
./build-s3.sh                 # Xtensa target; 4MB flash + quad PSRAM config

# Flash bootloader + partition table + app (esptool elf2image fails on Xtensa,
# so espflash generates the app image first)
./flash-s3.sh --port /dev/cu.usbserial-XXX
```

The S3 firmware runs the same agent/ingress threads as the C61 build and adds
the **DeepSeek cloud LLM** path plus the **HTTP admin dashboard**. After the
board joins Wi-Fi, the dashboard is reachable directly from a host on the same
subnet:

```bash
curl http://<board-ip>/          # HTML dashboard
curl http://<board-ip>/api/status # JSON status (incl. wifi_reason field)
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

**Provision the device with AT commands:** text starting with `AT` is
intercepted by the parser and dispatched deterministically — no LLM, no token
budget, no ReAct loop. This is the recommended path for factory provisioning,
field maintenance, and crash-loop recovery. See
[`docs/AT_COMMAND_REFERENCE.md`](docs/AT_COMMAND_REFERENCE.md) for the full
subset. Quick taste:
```
$ printf 'AT+GMR\r\n'                  > /dev/cu.usbserial-10
+GMR:mAgent v0.1.0 / AT v0.2 / esp32-c61
OK
$ printf 'AT+CWJAP="HomeWifi","hunter2"\r\n' > /dev/cu.usbserial-10
OK                                                # credentials saved to NVS
$ printf 'AT+IDENT?\r\n'                > /dev/cu.usbserial-10
+IDENT:7e3b9c4a13d6a...                               # device's did:key pubkey
OK
```

**Host-Side Tools:**
```bash
cargo build -p magent --release
```

## ☁️ Cloud LLM Backends & Web Tools

The agent runner talks to "the LLM" through a small trait, so you can switch
providers without touching the ReAct loop. See [docs/LLM_BACKENDS.md](docs/LLM_BACKENDS.md).

**Local (Ollama):**
```bash
magent run "Read the temperature"                       # localhost:11434, llama3.2
magent run --ollama http://gpu:11434 --model qwen2.5:7b "Summarise the logs"
```

**Hosted (DeepSeek, OpenAI-compatible):**
```bash
DEEPSEEK_API_KEY=sk-... magent run --provider deepseek "Query the weather"
magent run --provider deepseek --model deepseek-reasoner "Solve this PDE"
```

**Web / weather tools** (host-side, require internet) are exposed to the LLM
as ordinary tools and work end-to-end:

```bash
# Search the web, then read a page
magent run --provider deepseek "用 web_search 搜索 Rust 2026 新特性，再用 fetch_url 打开最相关链接"

# Weather via a dedicated compact tool (Open-Meteo, no API key)
magent run --provider deepseek "用 get_weather 查上海天气"
```

The ReAct loop is fault-tolerant about how the model formats its output: it
accepts strict JSON, fenced code blocks, JSON wrapped in prose, and Anthropic
`<invoke>` tool calls, and strips code fences so code answers are delivered
verbatim.

## 📚 Documentation

| Document | Description |
|----------|-------------|
| [This README](README.md) | Project overview |
| [Firmware README](firmware/README.md) | Firmware overview |
| [nRF52840 Build Guide](docs/NRF52_BUILD_GUIDE.md) | nRF52840 detailed guide |
| [nRF52840 Memory](docs/NRF52_MEMORY_ANALYSIS.md) | Memory analysis |
| [ESP32-C61 Build Guide](docs/ESP32_C61_BUILD.md) | ESP32-C61 detailed guide |
| [ESP32-C61 Boot & Hardware Notes](docs/ESP32_C61_BOARD_BOOT_FAILURE.md) | Bring-up diagnosis, fixes, and verification |
| [ESP32-S3 Port Success](docs/ESP32_S3_PORT_SUCCESS.md) | S3 bring-up, fixes, on-HW verification, S3 vs C61 matrix |
| [ESP32 Build Status](docs/ESP32_BUILD_STATUS.md) | ESP32-C6x / S3 build status & board notes |
| [ESP32 Notes](docs/ESP32.md) | General ESP32 target notes |
| [Platform Comparison](docs/PLATFORM_COMPARISON.md) | Platform analysis |
| [AT Command Reference](docs/AT_COMMAND_REFERENCE.md) | AT (Hayes / ESP-AT) provisioning subset |
| [LLM Backends](docs/LLM_BACKENDS.md) | DeepSeek / Ollama provider wiring |
| [Summary Store](docs/SUMMARY_STORE.md) | Run-summary schema & CLI |
| [Aerospace Code Audit](docs/AUDIT_AEROSPACE_2026.md) | Panic-freedom / bounded-memory audit (internal self-audit — see [`SECURITY_AUDIT.md`](SECURITY_AUDIT.md) for scope & limitations) |
| [Security Policy](SECURITY.md) | Vulnerability disclosure & coordinated disclosure timeline |
| [Security Audit Baseline](SECURITY_AUDIT.md) | Internal AI-assisted self-audit (NOT a third-party audit) |
| [Contributing](CONTRIBUTING.md) | How to file issues, open PRs, run the local toolchain |
| [Code of Conduct](CODE_OF_CONDUCT.md) | Community standards (Contributor Covenant 2.1) |
| [License](LICENSE) | MIT License |

## 🧠 Run Summaries

`magent run` can persist a compact head/tail "compression window" of the
conversation for later reuse — handy for sketching context into a fresh
session without replaying the whole transcript. Summaries are stored as
one JSON file per topic under the user's XDG data directory (override with
`$MAGENT_SUMMARIES_DIR` or `--dir <PATH>`); the file layout and schema are
documented in [docs/SUMMARY_STORE.md](docs/SUMMARY_STORE.md).

Save a summary for the current run:

```bash
magent run "Fix the boot hang" --save-summary boot-hang
magent run "Fix the boot hang" --save-summary boot-hang --save-summary-overwrite
```

Load it back into a later run:

```bash
magent run "Continue the boot-hang work" --load-summary boot-hang
```

The summaries are also managed directly via the `magent summary` subcommand:

```bash
magent summary save    boot-hang --from run.txt   # save from a file
magent summary show    boot-hang                   # pretty-print the window
magent summary list                                # all stored topics
magent summary load    boot-hang                   # print as a JSON array
magent summary export  boot-hang > out.json        # raw JSON to stdout
magent summary delete  boot-hang                   # remove a topic
magent summary rollback boot-hang 2                # promote history[2] to active
```

## 📊 Features Matrix

| Feature | nRF52840 | ESP32-C61 | ESP32-S3 |
|---------|----------|------------|----------|
| ReAct Agent | ✅ | ✅ | ✅ |
| Skills Manager | ✅ | ✅ | ✅ |
| Tool Registry | ✅ | ✅ | ✅ |
| Real Local Tools (GPIO/sensor, no network) | ✅ | ✅ | ✅ |
| BLE Communication | ✅ | ✅ | ❌ (off: internal-DRAM) |
| Wi-Fi STA (2.4 GHz) | ❌ | ✅ | ✅ (verified on HW) |
| Wi-Fi 6 | ❌ | ✅ | ➖ (2.4 GHz only) |
| Health Sensors | ✅ | ✅ | ✅ |
| Sports Coach | ✅ | ✅ | ✅ |
| Sleep Manager | ✅ | ✅ | ✅ |
| Early Warning | ✅ | ✅ | ✅ |
| Web3 Identity (Ed25519) | ✅ | ✅ | ✅ |
| Ingress Gateway | ✅ | ✅ | ✅ |
| Bidirectional UART (command → result reply) | ✅ | ✅ | ✅ |
| **AT command subset (ESP-AT compatible provisioning)** | ✅ | ✅ | ✅ |
| **AT-managed secrets stored device-bound sealed (DBO2, HKDF + HMAC)** | ✅ | ✅ | ✅ |
| **`AT+WIFIPASSUPGRADE=1` (DBO1 → DBO2 in-place migration)** | ✅ | ✅ | ✅ |
| Crash-loop detection + safe mode | ✅ | ✅ | ✅ |
| Health monitoring (heartbeat, free-heap) | ✅ | ✅ | ✅ |
| **`AT+PING` (IPv4 ICMP via esp_ping; hostname DNS)** | ✅ | ✅ | ✅ |
| OTA Updates (`AT+OTA` + anti-rollback confirm) | 🔄 | 🔄 | ❌ (4MB flash constraint) |
| **Cloud LLM backends (DeepSeek / Ollama, pluggable)** | ✅ | ✅ (local) | ✅ (DeepSeek, verified) |
| **HTTP admin dashboard** (`web_admin`) | ❌ | ✅ | ✅ (verified on HW) |
| **Web browsing (`web_search` / `fetch_url` / `webpage_summary`)** | ✅ (host) | ✅ (host) | ✅ (host) |
| **Weather query (`get_weather`, Open-Meteo, no key)** | ✅ (host) | ✅ (host) | ✅ (host) |
| **Blockchain tools (`get_balance` / `send_transaction` / …)** | ✅ | ✅ | ✅ |
| **Email MCP tools (`--email-tools`)** | ✅ (host) | ❌ | ❌ |
| **Run summaries (`--save-summary` / `--load-summary`)** | ✅ | ✅ | ✅ |
| **Web3 signed run reports (`magent run --sign`)** | ✅ | ✅ | ✅ |

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
- **Format / code fault tolerance** — the ReAct loop parses malformed or mixed
  LLM output (fenced code blocks, prose-wrapped JSON, Anthropic `<invoke>` tool
  calls, plain-text answers) instead of looping or dropping the answer.
- **Panic-cascade safety** — the trace-sink and boot-key paths never `panic!`
  inside a `Result`; `#![deny(clippy::panic_in_result_fn)]` is enforced under
  CI to keep it that way.
- **Test coverage** — `magent-core` ships **1200+ tests** (931 unit lib tests plus
  integration targets: AT parser, web3/blockchain, ingress gateway, time-sync,
  property tests, nRF52 sim, security, power, mock-HTTP e2e) across all feature
  flags, all green with 0 failures. The host crates (`magent` CLI, MCP executors,
  simulators, `magent-tools`) add another ~460. All firmware targets (nRF52840,
  nRF52840 integration-test, ESP32-C61, ESP32-S3) compile cleanly; the S3 is
  verified stable on hardware (WiFi + agent + HTTP dashboard, long-run soak).

## 🤝 Contributing

Contributions welcome! Please read the safety guidelines in `magent-core/src/safety.rs`.

## 📄 License

MIT

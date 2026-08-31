# CHANGELOG

All notable changes to the **mAgent** open-source codebase will be documented
here. Dates use `YYYY-MM-DD` and follow ISO 8601. Versions follow
[Semantic Versioning](https://semver.org/).

> **Scope reminder**: This file tracks the **open-source codebase**
> (`mAgent` / `MicroAgent`). The target commercial product line is marketed
> under the brand **arkChip-mAgent**; commercial SKU release notes are kept
> in a separate, private changelog and are shared with partners under NDA.

---

## [Unreleased]

### Added
- **Feature-gated modules made host-testable + tested** (`magent-core/src/lib.rs`):
  the pure-logic embedded modules (`power`, `hardware`, `monitoring`, `ollama`,
  `communication`, plus `config`/`safety`/`wear_leveling`/`security`) are now
  compiled under the `std` feature (matching the existing `security` gate), so
  their unit tests actually run on the host instead of being skipped behind the
  `nrf52`/`esp32`/`embedded` chip features. `storage` stays chip-gated (it needs
  `embedded_storage`). CI's default-feature `cargo clippy -p magent-core
  --all-targets -- -D warnings` stays clean.
- **`power.rs` tests** (8): `PowerManager` state-machine transitions (the
  permissive `Active → Idle → LowPower → DeepSleep → Active` sequence, no-op and
  invalid upward steps, `InvalidStateTransition` `from`/`to` names), battery
  threshold get/set, `should_enter_low_power` against the threshold, and
  `read_battery_status`. Coverage 0% → 96%.
- **`hardware.rs` tests** (11): `I2cSensor`/`SpiSensor` init/read/write and
  not-initialised error, `GpioPin` input-rejects-set + output set/read/toggle,
  and `TemperatureSensor`/`Accelerometer`/`HumiditySensor`/`PressureSensor`
  reads (incl. register fallbacks). Coverage 0% → 93%.
- **`ollama.rs` tests** (12): client construction, `build_request`,
  `add_message` (incl. overflow), `add_tools`, `serialize_request`,
  `parse_response` content + tool-call extraction + error path, tool-definition
  constant, and system-prompt sanity. Coverage 0% → 98%.
- **`communication/ble.rs` tests** (12): `BleClient` connect/disconnect,
  `send_request` keyword responses + disconnected/overlong errors,
  `send_tool_result`, `receive_response`, and `BleMessage` new/to_bytes/
  from_bytes round-trip + garbage rejection. Coverage 0% → 98%.
- **`security.rs` tests** (16): a `std`-only suite (XOR encrypt/decrypt, auth-tag
  round-trip, encryption toggle) and a `web3` suite (real AES-128-GCM
  encrypt/decrypt round-trip, nonce-uniqueness, tamper → `AuthenticationFailed`,
  short-ciphertext → `InvalidCiphertext`, HMAC-SHA-256 16-hex-char tag, oversized
  plaintext → `BufferOverflow`, `constant_time_eq`). Coverage 0% → 90%.
- **Full ReAct-loop tests** (`magent-core/src/agent_runner.rs`): a
  `run_react_loop_tests` module drives `RealAgentRunner::run()` end-to-end
  through a canned `LlmBackend` (tool call → tool execution → terminal result;
  tool-failure recovery; iteration-budget exhaustion on chatter) without any
  network. `agent_runner.rs` line coverage ~68% → 72%.
- **`agent_tools.rs` tests** (+5): `create_blockchain_tools` (8 tools),
  `register_blockchain_tools_only` populates a registry, `run_blockchain_tool`
  dispatch, `switch_chain`/`reset`/getters, and `BlockchainManagerHolder`.
  Coverage 73%.
- **`web.rs` top-level tool validation tests** (+6): `web_search` / `fetch_url` /
  `webpage_summary` / `get_weather` argument-validation and SSRF-refusal error
  paths that return before any network I/O.

- **Web3 error-adaptation tests** (`magent-core/src/web3/error.rs`): 8 unit tests
  covering every `Web3ErrorKind` helper constructor, the `Web3ErrorExt`
  trait (`into_agent` / `with_did`, including `DidKeyMismatch` re-tagging),
  and the `From<Web3ErrorKind> for AgentError` conversion. Line coverage
  39% → 96%.
- **Web3 identity security tests** (`magent-core/src/web3/identity.rs`): 13
  unit tests for the Ed25519 signing/verification surface — sign/verify
  round-trip, tampered-payload and wrong-signer rejection
  (`SignatureVerificationFailed` vs `DidKeyMismatch`), `verify_signature*` /
  `verify_signed_message*` error paths, key hex/base58 serialisation, and
  `Debug` redaction of secret-key material. Line coverage 54% → 94%.
- **AgentError classification tests** (`magent-core/src/error.rs`): 3 unit
  tests pinning `AgentError::category()` / `recovery_strategy()` /
  `is_fatal()` for every variant. Line coverage 58% → 66%.
- **`magent-core` line coverage raised to ≥ 80%** (REQ-VFY-006). Full
  workspace lib + integration suites stay green (~1391 lib + ~1287 core
  integration tests), `cargo clippy -p magent-core --all-targets -- -D
  warnings` clean.
- **`wei_to_eth_string` decimal bug fixed** (`magent-core/src/web3/blockchain/
  agent_tools.rs`): the 6-digit fractional part was computed by dividing the
  wei remainder by `1e15` instead of `1e12`, so e.g. `0.123456 ETH` rendered
  as `"0.000123 ETH"` (and the last three decimals dropped). Now uses
  `frac / 1e12`; the existing test that pinned the wrong output was updated.
- **Blockchain RPC client tests** (`client.rs`): `parse_address` / `parse_hash` /
  `parse_wei` / `parse_transaction_receipt` / `hex_encode`/`hex_decode` error
  paths and JSON-RPC error deserialisation. Coverage 41% → 63%.
- **Tool registry + executor tests** (`tools.rs`): registry query methods
  (`has_tool`/`count`/`names`/`describe`/`all_tools`), register overflow, and
  `read_sensor` across all simulated sensor types. Coverage 56% → 60%.
- **Safety mechanisms tests** (`safety.rs`): `BudgetEnforcer` / `Watchdog` /
  `StackMonitor` / `MemoryGuard` / `FaultDetector` budgets, thresholds,
  overflow rejection and reset. Coverage 21% → 95%.
- **Agent config tests** (`config.rs`): `validate()` boundary checks for every
  field, builder validation, and `to_bytes`/`from_bytes` round-trip. Coverage
  28% → 67%.
- **Event parsing tests** (`events.rs`): `topic_as_address`/`u256`/`u64`/
  `bytes32`, `data_as_u64`, and `EventFilter` `from`/`to`/`with_block_param`.
  Coverage 64% → 76%.
- **HTTP client + agent-tool tests** (`http_client.rs`, `agent_tools.rs`):
  `with_timeout`/`with_backoff`/`from_chain`, wei↔ETH conversions, and
  sign/verify tool round-trips.
- **Simulator tests** (`simulator.rs`): `SimFlashStorage` read/write/erase
  (incl. out-of-bounds), `SimGpioController` set/get/toggle, `SimBleInterface`
  connect/send/disconnect. Coverage 31% → 56%.
- **Wear-leveling tests** (`wear_leveling.rs`): `None` strategy pinning,
  `is_sector_worn`, `get_most_worn_sector`, and `reset_stats`. Coverage 79%.
- **ReAct runner tolerance + state tests** (`agent_runner.rs`): a new
  `parse_tolerance_tests` module covering `parse_tool_call` (JSON, fenced/
  prose-wrapped, Anthropic `<invoke>`/`<antml:invoke>`), `parse_result`
  (envelope, fenced block, embedded-in-prose, plain prose vs chatter),
  `find_json_object` brace-in-string handling, `ensure_system_prompt`
  (no-op / replace / insert), `first_user_message`, `Message` constructors,
  and `ToolResult` success/error. Coverage 61% → 68%.
- **`magent-core` line coverage now 81.2%** (REQ-VFY-006 ≥ 80% exceeded).
  Full workspace lib (~1416) + core integration (~1312) suites stay green,
  `cargo clippy -p magent-core --all-targets -- -D warnings` clean.
- **Web3 signed-message tests** (`web3/signature.rs`): `signature()` /
  `signer_did()` decoding, `from_json` error-path categorisation
  (InvalidJson / SchemaMismatch / HexDecode), `to_json_into` (canonical
  equivalence + small-buffer error), and `Signature` Debug redaction.
  Coverage 81% → 93%.
- **Simulator sensor tests** (`simulator.rs`): `SimSensorManager` returns
  realistic bounded values (temp/humidity/pressure/accel/light) that vary
  across reads. Coverage 57% → 64%.
- **Web tool tests** (`web.rs`): `wmo_description` mapping across every WMO
  weather code. Coverage 80% → 82%.
- **Tool executor priority tests** (`tools.rs`): `voice_output` /
  `send_notification` accept low/high/urgent and normalise unknown priorities.

- **OTA firmware update** (`AT+OTA=<url>`, `firmware/esp32-app/src/ota.rs`):
  streams an image into the inactive OTA slot, verifies it via `esp_ota_end`,
  marks the next boot target, and reboots; every failure path aborts the OTA
  handle and leaves the running firmware untouched. Runs on a worker thread so
  it never blocks the UART/AT ingress (REQ-FW-005). Requires an
  `ota_0`/`ota_1` + `otadata` partition table.
- **`AT+RESTORE` implemented** — full factory reset: `nvs_flash_erase()` + reboot
  (was a placeholder error). **`AT+MACRAND` implemented** — assigns a random
  locally-administered STA MAC via TRNG + `esp_wifi_set_mac` (was a placeholder).
- **`AT+OTA` parser**: new `AtOp::Ota` in `magent-core` (`AT+OTA=<url>`) with
  parsing unit tests (`parses_ota_with_url` / `parses_ota_https_url`).
- **OTA rollback config** (REQ-FW-005): `partitions.ota.csv` (OTA-only, no
  `factory` app) + `sdkconfig.prod.defaults` gains `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME`
  pointing at it and enables `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` /
  anti-rollback. Verified to build clean together with Secure Boot v2 + Flash
  Encryption. **`flash-secure.sh` fixed**: it now merges the per-chip base
  sdkconfig with the prod overlay into one file and sets `ESP_IDF_SDKCONFIG_DEFAULTS`
  (ESP-IDF reads a single sdkconfig.defaults; the previous `SDKCONFIG_DEFAULTS`
  list was silently ignored), and no longer double-signs (`BUILD_SIGNED_BINARIES`
  removed).
- **`flash-secure.sh` Flash-Encryption burn path**: esptool `--encrypt` +
  `--flash-encrypt-key` write (auto-detected from the prod overlay); the
  flash-encryption key is now generated with `espsecure.py` (esptool has no such
  subcommand); the flash step is now `--apply`-gated so a dry-run never writes
  the device. Verified end-to-end dry-run (keys → prod build → sign → dry-run).
- **`ota.rs` cleanups**: removed the unreachable `Ok(())`, an unused `Read`
  import, and an unnecessary `mut` — C61 firmware builds with zero firmware-code
  warnings.
- **Test gate**: host-only `cargo test --workspace --lib` (excluding firmware
  crates) is fully green — 1306 passed, 0 failed. Added
  `test_rpc_malformed_response_is_error_not_panic` to `esp32_http` (a hostile /
  truncated RPC response surfaces as `HttpError`, never a panic).
- **AT parser tests**: `parses_restore` / `parses_macrand` for the newly
  implemented commands.
- **BLE AT channel confirmed wired**: BLE `SYS_CMD` (0x2A08) writes already
  dispatch through the full AT engine (`dispatch_ble_command` →
  `dispatch_at_command`), so every AT command (incl. `AT+OTA`/`AT+RESTORE`/
  `AT+MACRAND`) works over BLE with replies on `SYS_RSP`. `ble_at.rs` is a
  superseded, unreferenced skeleton. C61 builds clean with `--features ble`.
  Documented the missing BLE `SYS_CMD` authentication as a security note in
  `SECURITY.md` (see "BLE command channel").
- **Tier-6 validation runbook** (`docs/TIER6_VALIDATION.md`): a step-by-step,
  on-hardware validation plan for the ESP32-C61 — baseline boot/AT, Wi-Fi+HTTP,
  TLS, blockchain RPC, BLE, OTA (+rollback), Secure Boot v2 + Flash Encryption
  (with sacrificial-board + key-backup warnings), TLS-cert verification, and the
  regression sweep. This is the Tier-6 gate that unlocks every on-hardware-only
  backlog item.
- **`AT+PING` IPv6 support**: enabled `CONFIG_LWIP_IPV6` and extended `ping.rs`
  to accept IPv6 literals (e.g. `AT+PING=::1`) via `ip6addr_aton`, building the
  `ip_addr` union (`IPADDR_TYPE_V6`/`V4`) for `esp_ping`. Compile-verified on the
  S3 (zero firmware warnings); device boots cleanly with IPv6 enabled (boot AT
  self-test 6 commands still pass). On-hardware ping awaits Wi-Fi + a physical
  UART0 AT line.
- **`AT+HEAP` now reports `+HEAP:…`**: previously `Heap` reused `sysram_line`
  and echoed `+SYSRAM:…`. Added a dedicated `heap_line` and verified on hardware
  via the boot AT self-test (`[at-self-test] AT+HEAP? => +HEAP:2263435`).
- **`AT+GMR` board label fixed**: the version string no longer hard-codes
  `esp32-c61`; it now reports the actual board (`esp32-s3` / `esp32-c61`) via
  `cfg(feature = "board-…")`. Verified on hardware via the boot-time AT
  self-test (`[at-self-test] AT+GMR => +GMR:mAgent v0.1.0 / AT v0.2 / esp32-s3`).
- **Boot-time AT self-test**: firmware runs `AT+GMR`/`AT+IDENT?`/`AT+SYSRAM?` at
  startup and logs each reply, validating the AT engine (parse + dispatch +
  render) on-device without needing a physical UART0 line. Verified on the S3:
  GMR/IDENT/SYSRAM all return correct replies.
- **Workspace clippy debt cleaned**: `cargo clippy --workspace --all-targets
  -D warnings` is now **clean** (was failing on pre-existing lints). Fixed
  field-reassign-with-default, manual `RangeInclusive::contains`, single-char
  patterns, redundant format args, duplicated attributes, `vec_init_then_push`,
  `assertions_on_constants`, redundant match across `agent.rs`, `agent_runner.rs`,
  `did.rs`, `wear_leveling.rs`, `simulator.rs`, `conversation.rs`,
  `summary/record.rs`, `client.rs`, and the `at_tests`/`comprehensive_agent_tests`/
  `blockchain_tests`/`vc_tests`/`magent-lua` test suites. 1314 host tests still
  green.
- **`HttpClientConfig::from_url` scheme whitelist**: only `http`/`https` URLs are
  parsed; any other scheme (ftp/gopher/ws/garbage) falls back to the safe default
  instead of mis-parsing into a bogus host (defense against scheme confusion).
  +1 test (`test_config_from_url_rejects_non_http_scheme`); esp32_http tests now 38.
- **`AT+PING` implemented** (`firmware/esp32-app/src/ping.rs`): ICMP echo to an
  IPv4 literal **or hostname** (DNS via `lwip_getaddrinfo`) using ESP-IDF's
  `esp_ping` (lwip). `extern "C"` callbacks bridge the async ping task into the
  synchronous AT path with static atomics + a bounded wait; returns
  `+PING: reply=N rtt=Xus`. Previously `+CMDER:4` placeholder. IPv6 (`AT+PING6`)
  is a follow-up. Compile-verified on the S3; awaits on-hardware ping once Wi-Fi
  is associated.
- **OTA anti-rollback startup confirm**: `main` now calls
  `esp_ota_mark_app_valid_cancel_rollback()` once the agent thread is running,
  so a freshly-OTA'd firmware is marked healthy and isn't rolled back on the
  next boot (REQ-FW-005; no-op when anti-rollback is disabled).
- **AT parser tests**: added `parses_ota_without_arg` / `parses_ota_query_kind_is_set`
  (OTA parsing edge cases) — at.rs tests now 73.
- **ESP32-S3 BLE controller params fixed (258 → 257)**: `esp_bt_controller_init`
  failed on the S3 with `ESP_ERR_INVALID_ARG` (258) because the S3 classic
  `esp_bt_controller_config_t` requires `controller_task_prio == 23`
  (`ESP_TASK_BT_CONTROLLER_PRIO = ESP_TASK_PRIO_MAX - 2`) and `ble_max_act > 0`
  (default 6); both were wrong (prio was 5, ble_max_act was 0). Fixed in
  `ble_config.rs` (S3 branch) and verified on hardware — init now passes its
  parameter checks and advances to `ESP_ERR_NO_MEM` (257), i.e. the BLE
  controller needs more internal DRAM than free at init (sdkconfig memory
  tuning, logged in `docs/BACKLOG.md`).
- **ESP32-S3 on-hardware bring-up**: compiled + flashed the S3 and resolved a
  PlatformIO **board flash-size mismatch** — board `4d_systems_esp32s3_gen4_r8n16`
  declares 16MB flash but the connected board is 4MB, so the bootloader was
  16MB while the app was 4MB and the app's flash init asserted. Fixed by setting
  the board's `flash_size` to 4MB (local `~/.platformio` config; backup at
  `/tmp/board-backup.json`), rebuilt the bootloader, and the firmware now boots
  cleanly on the S3 (`[magent] v0.1.0 booting (ESP32-S3 std)`, PSRAM 2048K, agent
  ready, temp sensor 37.6 °C, ingress UART0 registered). Logged two S3-specific
  backlog items: BLE controller init error 258 (needs S3 task_prio/stack) and
  the AT/ingress channel is UART0 (USB-CDC exposes console log only).
- **Blockchain JSON-RPC transport abstraction** (`magent-core::web3::blockchain::esp32_http`):
  the previously-placeholder `EspHttpClient` (`post`/`get`/`post_raw` returned
  hard-coded errors) is now backed by a pluggable `Transport` trait, a
  process-wide default transport (`set_default_transport`), and a
  `StaticTransport` for offline testing. `post_raw`/`post`/`get` now serialise
  and parse JSON-RPC over the injected transport instead of failing (REQ-NET-002,
  REQ-NET-004).
- **Real esp-idf wire backend** (`firmware/esp32-app/src/blockchain_transport.rs`):
  `EspIdfTransport` implements the new `Transport` on ESP-IDF's native HTTP
  client (mbedTLS + CA certificate bundle, bounded 8 s connect timeout, 64 KiB
  response cap, DNS/TCP preflight). Installed as the process-wide default in
  `main`, so the agent's `get_balance`/`send_transaction` blockchain tools now
  actually reach an RPC endpoint from the device. +6 host unit tests; C61
  firmware builds clean with zero firmware-code warnings.
- **Production Secure Boot v2 + Flash Encryption path**:
  `firmware/esp32-app/sdkconfig.prod.defaults` (secure overlay that removes
  `CONFIG_ESP_TLS_INSECURE` and enables Secure Boot v2 / Flash Encryption / OTA
  rollback) and `firmware/esp32-app/flash-secure.sh` (key generation + build +
  sign + optional `--apply` eFuse burn + flash; dry-run by default). Dev
  `sdkconfig*.defaults` now mark `CONFIG_ESP_TLS_INSECURE` as DEV-ONLY.
  Documented in `SECURITY.md` (REQ-FW-004).
- **Host-side Lua scripting host** (`host/lua-app`, crate `magent-lua`): a
  sandboxed Lua 5.4 runtime that binds `hardware.*` (GPIO / I2C / ADC / PWM /
  flash / BLE / power / persistent NVRAM) and `agent.reason()` onto
  `magent-hal` / `magent-core`, plus an `AppRuntime` (boot / reload / tick /
  heartbeat / watchdog / health snapshot / graceful stop). Layered sandbox
  (stdlib allow-list, 512 KiB heap cap, per-script instruction budget,
  binding-layer argument caps), action grammar + dispatcher, persistent NVRAM,
  and a mock LLM backend for host end-to-end tests. **61 host tests**; one-command
  gate `scripts/check-lua.sh`. ESP32-S3 firmware wiring skeleton in
  `firmware/esp32-app/src/lua_task.rs` (`board-s3`-gated) + `docs/LUA_SCRIPTING_S3.md`.
- **`piccolo` pure-Rust engine + `mlua` optional**: `mlua` is now an **optional
  feature** (default on) of `magent-lua`; `--no-default-features --features
  piccolo` builds it **without `mlua`** (pure Rust, Xtensa-capable). The
  `PiccoloVm` covers the full `HardwareBackend` surface (sensor, GPIO I/O, I2C
  read/write, ADC, PWM, flash, NVRAM, BLE, power) + `agent.reason` + `call`/`has`;
  the `LuaEngine` trait + `AppRuntime<T: LuaEngine>` run the full runtime on the
  pure-Rust engine. Verified on host (default mlua 61 tests; mlua+piccolo 67;
  pure-piccolo 6 + `--lib` compiles). The firmware `lua` feature now wires
  `PiccoloVm` and builds `magent-lua` without `mlua`, so the S3 Lua path compiles
  for Xtensa (`--features board-s3,lua`).
- **`magent-core` clippy cleanup**: resolved 12 `clippy::*` warnings that
  surfaced when building with the `esp32` feature (`is_multiple_of`, `div_ceil`,
  `is_some_and`, iterator loops, doc-comment whitespace, `type_complexity`), so
  `magent-core --features esp32 --lib -D warnings` is clean.

### Fixed
- **Workspace feature-unification regression** (`magent-core/src/lib.rs`): the
  host-only modules (`agent_runner`, `conversation`, `summary`, `real_tools`,
  `simulator`, `web`) were gated `#[cfg(all(std, not(esp32)))]`. Adding the
  `magent-lua` workspace member (which enables `magent-core/esp32` for
  `MiniAgent`) flipped these off and broke `cargo build --workspace` for the
  host `cli`. Changed to `#[cfg(all(std, not(target_os = "espidf")))]` — the
  modules are excluded only on the ESP-IDF *device* target (keeping reqwest/ring
  out of firmware), and coexist with the `esp32` feature on a host build.

### Verified
- **ESP32-C61 default firmware build re-verified** after adding the
  `board-s3` Lua wiring: `firmware/esp32-app/build-c61.sh` succeeds, the
  `src/lua_task.rs` module is correctly excluded from the C61 build (the
  `board-s3` `cfg` gate), the optional `magent-lua` dependency is not pulled,
  and `sdkconfig.defaults` is restored. ELF at
  `target/riscv32imac-esp-espidf/release/magent-esp32-app`.
- **ESP32-S3 Lua firmware build verified**: `cargo +esp build --target
  xtensa-esp32s3-espidf --no-default-features --features board-s3,wifi,uart,lua
  --release` compiles the firmware with `PiccoloVm` (pure-Rust `piccolo`, no
  `mlua`) + `Esp32Hardware` + `AppRuntime<PiccoloVm>` into an Xtensa ELF. The
  `SimHardware` host simulator was gated `#[cfg(not(target_os = "espidf"))]` so
  it does not compile into the device build.
- **Full host workspace regression green** after the `mlua`-optional refactor:
  `cargo test --workspace` (excluding device/at targets) passes **1623 tests**,
  zero failures — the engine optionalization is coherent across every crate.
- **`scripts/check-lua.sh` now gates both engines**: clippy, default `mlua`
  tests, a pure-`piccolo` lib build (proves the no-`mlua` path the S3 firmware
  uses compiles), `mlua`+`piccolo` tests, the `lua-run` CLI, and fmt — all green.
- **S3 firmware `Esp32Hardware::pwm_set` wired via raw ESP-IDF LEDC**: timer 0
  (8-bit, 1 kHz, auto clock) configured once; channels 0..7 allocated lazily per
  GPIO; duty `0..=100 %` → 8-bit duty. Compile-verified: the `board-s3,lua`
  Xtensa build (`--features board-s3,wifi,uart,lua --release`) succeeds with the
  new driver (only the pre-existing `web_admin.rs` warning remains). `i2c`/`adc`/
  `flash`/`ble` remain explicit `Err` TODOs pending on-hardware pin/partition
  choices.
- **S3 firmware `Esp32Hardware` now wires ADC + I2C too** (raw ESP-IDF C API):
  - **ADC**: oneshot on **ADC1**, GPIO1..=10 → channels 0..=9, 12-bit, `DB_11`
    attenuation, linear 0..3.3 V scaling (production path documented via
    `adc_cali_*`). ADC2 intentionally unsupported (Wi-Fi conflict).
  - **I2C**: master on `I2C_NUM_0`; `i2c_read`/`i2c_write` are register-style
    (repeated-start write-then-read). SDA/SCL pins are the `I2C_SDA_PIN` /
    `I2C_SCL_PIN` constants (default GPIO9/GPIO8).
- **`PiccoloVm` now uses `Lua::full()`** so the io stdlib (`print`) is available —
  previously scripts using `print` failed with "expected function, found nil".
  Discovery from a new host test that boots the **exact S3 `DEFAULT_MAIN_LUA`
  self-test** through `AppRuntime<PiccoloVm>` + `SimHardware`; also confirmed
  piccolo lacks `string.format`, so the self-test uses `..` concatenation.
- **S3 `DEFAULT_MAIN_LUA` is now a hardware self-test**: it probes every wired
  driver inside `pcall` (temp/adc/pwm/i2c/gpio) and prints `[lua] <driver>
  ok/err` to the console — one failure is reported, never fatal. Host test
  `app_runtime_boots_s3_selftest_script` proves the shipped script boots clean
  on the piccolo engine.
- **`build-s3.sh` now enables the `lua` feature** and prints the
  `espflash flash --monitor ...` command; `docs/LUA_SCRIPTING_S3.md` gains a
  "Flashing & validating on the linked S3" runbook with expected console output.
- **S3 firmware `Esp32Hardware::flash_*` wired via ESP-IDF NVS**: `flash_read`/
  `flash_write`/`flash_erase_sector` map a 32-bit address to a keyed NVS blob
  (`flash_<addr:08x>` in the `magent_lua` namespace, so it persists across
  reboots). NVS is already initialised by `main::init_default_nvs()` before the
  Lua task starts, so the driver just `nvs_open`s. Keys stay ≤15 chars. The
  `DEFAULT_MAIN_LUA` self-test adds a `flash` round-trip probe. Only **BLE**
  (`ble_send`) remains an explicit `Err` (needs the firmware GATT notify handle).
  Fixed a target portability bug: `c_char` is `u8` on Xtensa / `i8` on x86_64,
  so `nvs_key` uses `core::ffi::c_char` instead of a hardcoded `i8`.
- **S3 firmware `Esp32Hardware::ble_send` wired**: adds `pub(crate)
  ble_config::ble_send_payload` (reuses the hardened `notify_char` →
  `esp_ble_gatts_send_indicate` on SYS_RSP; errors when no client is connected)
  and routes the Lua `hardware.ble_send` to it behind `#[cfg(feature = "ble")]`
  with a clear non-`ble` fallback. The `DEFAULT_MAIN_LUA` self-test adds a `ble`
  probe.
- **S3 `ble` feature now compiles for Xtensa** (was broken: 62 E0425/E0560).
  `ble_config::bt_controller_config()` was split into per-board constructors —
  the C61 keeps its newer ESP32-C6-style struct; the ESP32-S3 gets the classic
  ESP32 BLE controller struct (`magic`/`version` + `ESP_BT_MODE_BLE` +
  zero-init defaults via `Default`). Also fixed a device-info bug: the S3 chip
  model byte array was 8 bytes but the field expects 9 (`b"ESP32-S3 "`). Both
  `board-s3,ble,lua` and `board-c61,ble` Xtensa/RISC-V builds succeed.
  `build-s3.sh` now builds `board-s3,wifi,uart,ble,lua` so `ble_send` is active
  on the flashed S3.
- **S3 Lua app loads from NVS at boot with an embedded fallback**: `start_lua_task`
  now calls `load_app_source()`, which prefers an operator-provided `main.lua`
  stored in NVS (key `main.lua`) and otherwise runs the embedded
  `DEFAULT_MAIN_LUA` — enabling app updates without reflashing. A
  `pub set_lua_app_source(&str)` hook persists the app (operator-update entry
  point, ready to wire to an ingress/AT/BLE command). Both `board-s3,ble,lua`
  and `board-s3,lua` Xtensa builds succeed.
- **Production standards installed + comprehensive test pass**:
  - Declared **MSRV** (`rust-version = "1.85"`) in `[workspace.package]` **and
    enforced it** in `magent-core` / `magent-hal` / `magent-lua` via
    `rust-version.workspace = true` (no MSRV warnings).
  - **CI `host` job now gates `magent-lua`** (both the `mlua` and `piccolo`
    engines) and `magent-hal` with `clippy -D warnings`, and runs
    `cargo test -p magent-lua --features piccolo` — so the S3 Lua work is
    regression-protected in CI, not just via `scripts/check-lua.sh`.
  - Fixed a stale CI comment (the repo does ship `deny.toml`).
  - **Comprehensive tests green**: `cargo-deny check` (advisories/bans/licenses/
    sources ok), full host workspace `cargo test` (**1623 passed, 0 failed**),
    `magent-lua` dual-engine gate (clippy + tests + fmt clean), `magent-hal`
    clippy `-D warnings` clean, and the S3/C61 firmware Xtensa+RISC-V builds.
- **S3 Lua agent is now production-functional**: `start_lua_task` installs the
  real `Esp32ToolHandler` (GPIO / temperature tool calls drive actual hardware)
  and, when a DeepSeek model + API key are configured in NVS (`AT+LLMCFG`),
  installs the leaked `&'static mut` DeepSeek backend so `agent.reason()`
  returns real LLM decisions on-device instead of the canned heuristic. Exposed
  `nvs_load_string` / the LLM NVS keys as `pub(crate)` to share `main.rs`'s
  NVS-backed config. Compile-verified with the `board-s3,ble,lua` Xtensa build.
- **Firmware builds are now warning-free for our code** (production clean-build):
  - Removed a redundant `use crate::ble_config::BleServer;` in `main.rs` (it was
    already imported locally where used).
  - Marked the reserved-but-unwired BLE subsystems `ble_at` and `ble_wallet`
    with documented module-level `#![allow(dead_code)]` (kept for future wiring
    into the active `ble_config` dispatch).
  - Added item-level `#[allow(dead_code)]` to the unused `BleState::Connected`
    variant and the `BleServer` `stop_advertising`/`deinit`/`set_state`/
    `is_active` public API methods.
  - Gated `mod llm` behind `board-s3` (DeepSeek is S3-only; it was dead code on
    C61) and `ble_send_payload` behind `lua` (only the Lua task calls it).
  - Fixed an unused-variable warning in `web_admin.rs`.
  The `board-s3,wifi,uart,ble,lua` (Xtensa) and `board-c61,wifi,uart,ble`
  (RISC-V) builds now emit zero firmware-code warnings; the default C61 build
  still succeeds.
- **CRITICAL hardening — the piccolo engine now enforces an instruction budget**:
  `PiccoloVm::run_script`/`call` previously used `Lua::execute`, which runs to
  completion with no budget — so a hostile or buggy `main.lua` (`while true do
  end`) would **hang the `lua-thread` forever on the S3**, contradicting the
  "cannot hang the device" guarantee (the mlua `set_memory_limit`/instruction
  hook does NOT apply to the piccolo engine). Added `PiccoloVm::execute_bounded`
  which drives `Executor::step` with a bounded cumulative `Fuel` (per-step
  refill so GC still runs) and errors with "script instruction budget exceeded"
  once `MAX_FUEL` is spent. Applied to both `run_script` and `call` (the event
  loop path). New host test `piccolo_engine_enforces_instruction_budget` proves
  a `while true do end` and a runaway `on_tick` error instead of hanging, and
  that finite scripts still run normally. Compile-verified with the
  `board-s3,ble,lua` Xtensa build (still zero firmware-code warnings).
- **piccolo engine now enforces a memory cap too**: `execute_bounded` checks
  `Lua::total_memory()` (gc-arena bytes) each step and errors with "script
  memory limit exceeded" once the VM exceeds 8 MB — so a script that grows the
  heap unboundedly (not just an infinite loop) is contained instead of exhausting
  the S3's PSRAM. Together with the instruction budget this completes the
  "cannot hang / exhaust the device" sandbox guarantee on the piccolo engine
  (the mlua-only `set_memory_limit`/instruction hook was never applied to
  piccolo). New host test `piccolo_engine_enforces_memory_limit`; 9 piccolo
  tests total, Xtensa build still clean.
- **Test-coverage tooling + error-type coverage**: added `scripts/check-coverage.sh`
  (runs `cargo llvm-cov` on `magent-lua` for both the `mlua` and `piccolo`
  engines) and wired it into CI as a best-effort step. Coverage: **mlua 81.9% /
  piccolo 79.9% lines**. Added unit tests for `LuaHostError` (all 5 `Display`
  variants, `std::error::Error::source`, the `Result` alias, and the
  `From<mlua::Error>` choke-point) — `error.rs` line coverage went 43% → **100%**.
  Full host workspace test now passes **1628** (up from 1623).
- **`piccolo_vm` (S3 engine) error-path coverage**: added
  `piccolo_vm_hardware_error_paths`, which proves wrong argument types, missing
  arguments, an unknown sensor, and an undefined callable all surface as Lua
  errors (never a panic or hang) through the piccolo `hardware.*`/`agent`
  bindings. `piccolo_vm.rs` line coverage 72.3% → **74.3%** (functions
  44.6% → 51.4%); overall piccolo coverage → **80.5% lines**. 10 piccolo tests.
- **poisoned-lock robustness**: added `piccolo_vm_surfaces_poisoned_hardware_lock`,
  which poisons the `SharedHardware` `Mutex` (panic while held) and proves the
  piccolo `hardware.*` bindings surface it as a Lua error ("hardware lock
  poisoned"), never a host panic. `piccolo_vm.rs` coverage → **76.3% lines /
  54% functions**; overall piccolo coverage → **81.1% lines**. 11 piccolo tests.
  (Evaluated wiring `set_lua_app_source` via an `AT+LUAAPP` command, but the AT
  parser splits args on commas — arbitrary multi-line Lua would not survive
  parsing, so it stays a documented public API hook rather than a lossy wire
  format.)
- **hot-reload test**: added `piccolo_vm_hot_reload_swaps_script`, proving
  `PiccoloVm::reload_state` clears stale globals (functions included) and
  `AppRuntime::reload` drives a changed `main.lua` through the event loop
  without a reboot. `piccolo_vm.rs` coverage → **78.1% lines / 57% functions**;
  overall piccolo coverage → **81.6% lines**. 12 piccolo tests. (This also
  surfaced two real contract facts now locked by tests: `has()` only reports
  callable functions, and the event-loop `on_tick` must return a String.)
- **poisoned-agent robustness**: added `piccolo_vm_surfaces_poisoned_agent_lock`,
  which poisons the `SharedAgent` mutex (panic while held, caught via
  `catch_unwind` since `MiniAgent` is not `Send`) and proves `agent.reason`
  surfaces it as a Lua error, never a host panic. `agent.rs` coverage →
  **93.3% lines**; overall piccolo coverage → **81.7% lines**. 13 piccolo tests.
- **`magent-core` `at_dispatch_outcome` coverage + MSRV fix**: added unit tests
  for `AtOutcome` (`OK`, `ok_line` + truncation, `error`, `Clone`, `Debug`/`Eq`)
  — line coverage 19% → **95.4%**. While running the dual-engine gate, clippy's
  `incompatible_msrv` lint caught a real metadata bug: I had declared MSRV 1.85
  but `magent-core` uses `is_multiple_of` (stable 1.87). Bumped the workspace
  `rust-version` to **1.87** (correct floor). Full host workspace test now
  passes **1634** (up from 1628); magent-core/lua/hal clippy `-D warnings` all
  clean; S3 `board-s3,ble,lua` build succeeds.
- **`magent-core` `recovery` coverage**: added tests for `RecoveryManager`
  (`get_strategy` covering every `AgentError` class incl. the "security"
  `OperationTimeout` special-case, `should_retry` max-retry/strategy logic,
  `DefaultFallback` f32/u32/bool, and the `new`/`default` limits) — `recovery.rs`
  line coverage 24% → **87.9%**. Full host workspace test now passes **1639**
  (up from 1634). (Also confirmed the `web3`-feature arm against the real
  `Web3ErrorKind::InvalidDid { raw }` struct variant.)
- **`magent-core` `health_sensors` audit fix + coverage**: **audit found a real
  bug** — `HeartRateZone::from_hr_and_age` used `Cardio < 85%`, `Peak < 95%`,
  `Danger ≥ 95%` while the documented bands are `Cardio 70–80%`, `Peak 80–90%`,
  `Danger ≥ 90%`. Fixed the thresholds to `< 80`/`< 90`/else and added tests
  that lock the documented bands. Also added tests for `StressLevel`,
  `GlucoseStatus`, `HeartRhythm`, `HeartRateData`/`GlucoseData`/`EcgData`/
  `TemperatureData`, `UserProfile` (incl. error paths), `target_hr`, the
  `HealthSensorManager` history/trend/latest/`is_exercising`/`current_stress`/
  `clear_history`, and the history cap (drops oldest). `health_sensors.rs` line
  coverage 31% → **95.4%**. Full host workspace test now passes **1649** (up from
  1639); magent-core clippy `-D warnings` clean; S3 `board-s3,ble,lua` build OK.
- **`magent-core` `error` Display coverage**: added `display_tests` covering the
  user-facing `AgentError::Display` for every variant (memory/buffer/stack/
  network/storage/sensor/GPIO/validation/budget/timeout/state/config/unknown,
  plus the `web3`-gated `Web3Error`) and asserting no error renders empty.
  `error.rs` line coverage 45.9% → **67.3%**. Full host workspace test now passes
  **1651** (up from 1649); magent-core clippy `-D warnings` clean; `magent-lua`
  dual-engine gate green.
- **`magent-core` `skills` coverage**: added `extra_tests` for `SkillsManager`
  (capacity-limit + validate error paths, `get`/`all`/`all_mut`/`clear`,
  `remove` ok + missing) and `Skill` (empty-field rejection, too-long
  truncation→reject, `increment_usage`/`update_success_rate` saturation at
  `u16::MAX`/`100`/`0`, `to_injection_string` formatting). `skills.rs` line
  coverage 60% → **80.1%** (regions 88.8%). Full host workspace test now passes
  **1659** (up from 1651); magent-core clippy `-D warnings` clean.
- **`magent-core` `early_warning` audit fix + coverage**: **audit found a real
  bug** — `EarlyWarningSystem::add_hospital` claimed to "remove farthest if
  full" but actually removed the *first* hospital farther than the new one,
  so a closer hospital would evict the wrong (nearer) entry and keep a far one.
  Fixed it to evict the max-distance hospital (matching the stated policy).
  Added `extra_tests` for `AlertSeverity` names/emergency, `HealthAlert`
  ack/emergency flags, contacts capacity + priority sort, hospital capacity
  eviction, `nearest_er`/`nearest_with_specialty`, `Hospital::distance_string`
  (m/km/unknown), `GlucoseTrendPrediction::risk_description`,
  `EcgAnomalyType::description`, and the alert-ack/`critical_alerts`/
  `unacknowledged_alerts`/`generate_emergency_message` paths (via a triggered
  critical-low glucose alert). `early_warning.rs` line coverage 80% → **93.5%**
  (regions 95.4%). Full host workspace test now passes **1669** (up from 1659);
  magent-core clippy `-D warnings` clean; S3 `board-s3,ble,lua` build OK.
- **`magent-hal` nRF52 adapter coverage**: the host RAM-backed adapters
  (`NrfGpio`/`NrfFlash`/`NrfBle`/`NrfTemperature`/`NrfPower` — the backend behind
  `magent-lua`'s `SimHardware`) had **0%** own-crate coverage (only exercised
  transitively by magent-lua). Added `nrf52::adapter::tests` covering the GPIO
  mode/write rules, flash read/write/erase + bounds + the flash AND-only
  semantics, BLE connect/send, temperature read, power-profile round-trip, and
  `NrfError` Display. `nrf52/adapter.rs` coverage 0% → **66.6%**; `magent-hal`
  total 54% → **66.1%**. Full host workspace test now passes **1676** (up from
  1669); magent-hal clippy `-D warnings` clean.
- **`magent-hal` ESP32 adapter coverage**: added `esp32::extra_tests` for
  `EspGpio` (pin + output write), `EspFlash` (bounds, erase, the AND-only
  flash semantics), `EspPower` (all profiles round-trip + default), and
  `EspError` (Display + `std::error::Error`). `esp32.rs` coverage 63% →
  **82.2%**; `magent-hal` total → **69.6%**. Noted a minor API gap: `EspBle`
  has no `set_connected` setter, so its send-success path is untestable on the
  host (unlike `NrfBle`). Full host workspace test now passes **1680** (up from
  1676); magent-hal clippy `-D warnings` clean.
- **Features implemented**:
  - **`EspBle::set_connected`** — closed the noted API gap (the ESP32 BLE
    adapter had no way to become connected), enabling the send-success path and
    a test (`ble_send_succeeds_when_connected`). `magent-hal` esp32 coverage now
    ~85%.
  - **`AT+LUAAPP` — operator Lua-app update (no reflash)**: wired `set_lua_app_source`
    (previously a dead hook) into the AT layer with a **URL-safe base64**
    payload so arbitrary Lua survives the comma-splitting parser losslessly.
    Added `AtOp::LuaApp` (+ `name` + parser arm + host test
    `parses_luaapp_set_and_query`) in `magent-core`; a firmware `luaapp_dispatch`
    (set via `base64::URL_SAFE_NO_PAD` → `set_lua_app_source`, query via new
    `lua_task::lua_app_source_len`, unsupported on non-`lua` builds) and a
    `base64 = "0.22"` firmware dep. Added `scripts/luaapp-encode.sh` to turn a
    Lua file into an `AT+LUAAPP=<b64>` line (verified round-trip). Full host
    workspace test now passes **1682** (up from 1680); S3 `board-s3,ble,lua`,
    S3 non-lua, and C61 builds all succeed.




- **`AT+LLMCFG=` validation hardening** (`magent-core::at_validate::validate_llmcfg_set`):
  host-tested validator for the LLM backend model + API key that enforces
  length caps (model ≤64, key ≤128), valid UTF-8, and rejects NUL / control
  bytes / whitespace in the key. Wired into the ESP32 `llmcfg_dispatch` so a
  malformed config is rejected before it is written to NVS. 16 new host tests.
- **`AT+HTTPGET=` URL validation hardening** (`magent-core::at_validate::validate_httpget_set`):
  host-tested validator that whitelists `http://` / `https://` (case-insensitive),
  caps length at 512, and rejects NUL / control bytes / non-HTTP schemes —
  hardening the SSRF-sensitive URL surface before any worker thread is spawned.
  12 new host tests.
- **`AT+BLE=` validation hardening** (`magent-core::at_validate::validate_ble_set`):
  pure, host-tested decision helper that accepts only `ON` / `OFF` / `STATE`
  (case-insensitive) and rejects malformed forms (empty, quoted, `key=val`,
  numeric, unknown verb) with precise `+CMDER:4` / `:7` errors. Wired into the
  ESP32 dispatcher's `ble_dispatch` so a bad BLE control line is rejected
  before reaching the BLE stack. 16 new host unit tests cover every accept /
  reject path.
- **Open-source governance surface** (this PR): `LICENSE` (MIT),
  `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `CONTRIBUTING.md`,
  `SECURITY.md`, issue / PR templates under `.github/`, and a public
  `.github/workflows/ci.yml` covering host checks (fmt, clippy, test) plus
  nRF52840 and ESP32-C61 firmware builds.
- **Internal self-audit disclosure band**: a clear "this is an internal
  AI-assisted self-audit, not a third-party audit" callout at the top of
  `SECURITY_AUDIT.md` and `docs/AUDIT_AEROSPACE_2026.md`, with a written
  commitment that a third-party audit is on the post-funding roadmap
  (Trail of Bits / Cure53 / NCC Group — selection pending).
- CI status / license / audit-status badges in `README.md`.

### Changed
- `README.md` — added CI / License / Audit badges and a confidentiality
  notice; added cross-links to `SECURITY.md`, `SECURITY_AUDIT.md`,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, and `LICENSE`.
- `SECURITY_AUDIT.md` — author is now labeled "Internal AI-assisted
  self-audit, performed by the project owner"; "Auditor Signature" replaced
  with "Internal self-audit signature"; "Next Audit" clarified into
  "Next internal review" + "Next independent audit" commitments.
- `docs/AUDIT_AEROSPACE_2026.md` — same disclosure band added at the top
  and at the conclusion; mapping to DO-178C / ISO 26262 / IEC 61508
  clarified as **informative only**.

### Security
- No code-level security fixes in this release. Disclosure-band changes
  are documentation-only and are not themselves CVEs.

### Fixed
- **`magent-core/src/ollama.rs` `parse_response` off-by-one** — the legacy
  embedded `OllamaClient::parse_response` used `pos + 10` to skip the
  `"content":"` key, but that key is **11** chars long, so every parsed
  `content` started with a spurious leading `"`. The tool-call name extraction
  used `pos + 17` against the 20-char `"function":{"name":"` pattern, so tool
  names parsed as empty. Both offsets corrected (`+ 11` / `+ 20`); new unit
  tests pin the correct values.
- **`magent-core` `property_tests` target would not compile** — it uses
  `web3::wallet::Keystore`, so its `required-features` now includes `wallet`
  (previously only `std` + `web3`, so `cargo test -p magent-core` failed to
  build the target with `could not find 'wallet' in 'web3'`).
- **CLI test suite hung under interactive `cargo test`** — the
  `web3_blockchain::stdin_read_does_not_panic_on_eof` test blocked forever on
  `std::io::stdin().read()`. It now guards with `is_terminal()` so it only
  reads when stdin is piped/closed, and never hangs the suite.
- **CLI `email_executor::debug_impl_covers_all_variants` failed when both
  `web3` and `email-tools` features are enabled** — `CompositeExecutor::new(_)`
  then returns the `Full` variant, not `WithEmailTools`; the assertion now
  expects the variant the active feature set actually produces.
- **nRF52840 firmware (`magent-nrf52-app`) failed to compile** — the `BLE_STATE`
  static was typed as the `BleState` enum but initialised with `BleStateManager`
  struct fields (fixed to `BleStateManager` with a const-compatible literal);
  `handle_characteristic_read` used `Vec<u8>` in a `no_std` crate (added
  `extern crate alloc`); and `info!("{:?}", char_idx)` required the
  `defmt::Format` derive, which is not enabled (replaced with a `&'static str`
  name helper). The firmware now builds.
- **nRF52840 integration-test firmware failed to link** — it was missing the
  `memory.x` linker script and the `build.rs` that exposes it via
  `cargo:rustc-link-search` (plus `-Tdefmt.x`); both added so `rust-lld` can
  resolve the `INCLUDE memory.x` in `cortex-m-rt`'s `link.x`.

---

## [0.1.0] — Initial open release

First public release of the workspace. See [`README.md`](README.md) for the
overall architecture, [`docs/NRF52_BUILD_GUIDE.md`](docs/NRF52_BUILD_GUIDE.md)
and [`docs/ESP32_C61_BUILD.md`](docs/ESP32_C61_BUILD.md) for build
instructions, and [`SECURITY_AUDIT.md`](SECURITY_AUDIT.md) for the
self-audit baseline.

[Unreleased]: https://github.com/arkCyber/mAgent/compare/main...HEAD
[0.1.0]: https://github.com/arkCyber/mAgent/releases/tag/v0.1.0

# mAgent Backlog — Known Gaps (Post-Audit)

> Status as of the **2026-08-28 production-hardening pass**. This file tracks the
> feature/security gaps surfaced by the code audit that are **not yet resolved**,
> prioritised P0 → P2. The P0 items listed below as *done* were addressed in that
> same pass (see `CHANGELOG.md`); everything else is outstanding work.

## ✅ Done this pass (P0)
- [x] **Firmware blockchain HTTP transport** — `EspHttpClient` (was placeholder)
      now serialises/parses JSON-RPC over a pluggable `Transport`; real esp-idf
      wire backend (`firmware/esp32-app/src/blockchain_transport.rs`) installed as
      process-wide default in `main`. (REQ-NET-002 / REQ-NET-004)
- [x] **Secure Boot v2 + Flash Encryption production path** — `sdkconfig.prod.defaults`
      + `flash-secure.sh`; dev `CONFIG_ESP_TLS_INSECURE` marked DEV-ONLY.
      **Awaiting Tier-6 on-hardware validation** (eFuse burn is irreversible).
      (REQ-FW-004)

## ✅ Done (P1 follow-up pass)
- [x] **OTA upgrade logic** — `firmware/esp32-app/src/ota.rs` + `AT+OTA=<url>`:
      stream → `esp_ota_begin/write/end` verify → `set_boot_partition` → reboot;
      every failure aborts the handle (running firmware untouched). Runs on a
      worker thread. **Awaiting Tier-6 on-hardware validation.** (REQ-FW-005)
- [x] **`AT+RESTORE`** — full NVS erase (`nvs_flash_erase`) + reboot (factory reset).
- [x] **`AT+MACRAND`** — random locally-administered STA MAC via TRNG +
      `esp_wifi_set_mac` (requires the Wi-Fi interface to be stopped, e.g. safe mode).

## 🔴 P1 — outstanding
- [ ] **BLE on ESP32-S3** — `esp_bt_controller_init` now passes its parameter
      checks (fixed error 258: `controller_task_prio`=23 == `ESP_TASK_BT_CONTROLLER_PRIO`,
      `ble_max_act`=6), but fails with **257 `ESP_ERR_NO_MEM`** in
      `btdm_controller_mem_init`: the controller needs a large contiguous
      **DMA-capable internal DRAM** block (`MALLOC_CAP_8BIT|DMA|INTERNAL`; PSRAM
      unusable), and the ~244–253 KB free internal DRAM can't provide it.
      Tried (all verified on hardware): `CONFIG_BT_ALLOCATION_FROM_SPIRAM_FIRST`,
      disabling BLE 5.0 — none free enough controller DMA RAM. ESP32-S3's
      Bluedroid controller has **no `BTDM_CONTROLLER_*` Kconfig to shrink it**,
      so this looks like a hard internal-DRAM budget limit when BLE must coexist
      with Wi-Fi + the agent threads. Options: (a) explicit internal-RAM budget
      (which subsystems win), (b) migrate the BLE stack to NimBLE (much smaller),
      (c) validate BLE on the C61 (primary platform, own controller config).
- [ ] **Firmware TLS stability** — HTTPS on the C61 currently fails fast with a
      memory error; only HTTP is reliable. Blocking real certificate-verified
      TLS on device (pre-requisite for secure RPC / DeepSeek / OTA in prod).
      Tracked against `CONFIG_ESP_TLS_INSECURE` removal.
- [x] **`AT+PING` (IPv4 + IPv6 ICMP)** — implemented via esp_ping in
      `firmware/esp32-app/src/ping.rs` (async→sync callback bridge). IPv4
      (literal + DNS hostname) and IPv6 literals (`::1`, `fe80::…`) supported;
      `CONFIG_LWIP_IPV6=y` enabled. Compile-verified; device boots cleanly.
      **Awaiting on-hardware ping** (needs Wi-Fi associated — safe mode must
      clear — and a physical UART0 AT line).
- [x] **OTA rollback** — `partitions.ota.csv` (OTA-only, no `factory`) +
      `sdkconfig.prod.defaults` (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` +
      anti-rollback + OTA-only table). **Verified**: the full production config
      (Secure Boot v2 + Flash Enc + OTA-only + rollback) builds clean. `OTA`
      code already calls `esp_ota_mark_app_valid_cancel_rollback`.
      **Awaiting Tier-6 on-hardware validation.** (REQ-FW-005)
- [x] **Flash-Encryption burn in `flash-secure.sh`** — added esptool `--encrypt`
      + `--flash-encrypt-key` write path (auto-detected from the prod overlay);
      fixed flash-encryption-key generation to use `espsecure.py` (esptool has
      no such subcommand); made the flash step `--apply`-gated (dry-run no longer
      writes the device). Verified end-to-end dry-run.
      **Awaiting Tier-6 on-hardware validation.**

## 🟡 P2 — outstanding
- [x] **BLE AT command channel** — already wired: `SYS_CMD` (0x2A08) writes are
      dispatched via `dispatch_ble_command` → `dispatch_at_command` (the full AT
      engine), so any AT command (incl. `AT+OTA`/`AT+RESTORE`/`AT+MACRAND`) works
      over BLE, replies on `SYS_RSP`. `ble_at.rs` is a superseded, unreferenced
      skeleton. Verified: C61 builds clean with `--features ble`.
      ⚠️ **Security**: `SYS_CMD` accepts AT commands from any connected BLE
      client without authentication — enable pairing/encryption for untrusted
      environments.
- [ ] **`AT+BLE=ON/OFF/STATE` control** — `ble_dispatch` is still a placeholder
      (error 9); needs a shared `BleServer` (currently a local in `main`) exposed
      globally + `esp_ble_gap_stop_advertising`, and on-hardware validation.
- [ ] **BLE wallet service (0x1851)** — `ble_wallet.rs` is reserved dead code;
      needs a new GATT service registered in `ble_config` + secure handling of
      private keys / signing; on-hardware validation required.
- [ ] **Email MCP on ESP32** — `--email-tools` is host-only; no SMTP client on
      firmware for device-originated alerts.
- [ ] **Zigbee / 802.15.4** — nRF52840 advertises the `thread` feature; Zigbee and
      a real 802.15.4 stack are still on the roadmap.
- [ ] **Lua host on C61** — `lua_task` is ESP32-S3-only (`board-s3` gate); no Lua
      App runtime on the primary C61 board.
- [ ] **Platform coverage** — ESP32-C3/C6 "compatible" but unverified; ESP32/S3
      Xtensa path in progress; Secure Boot prod path needs real-hardware sign-off.

## 🟠 Verification backlog
- [ ] **USB-JTAG console risk (S3)**: switching the console primary to
      `CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG` **hijacks the native USB-JTAG**, making
      the board unflashable via USB (`esptool` fails with "No serial data
      received") — matching the warning already in `sdkconfig.defaults`. Reverted.
      **If done again**: enter download mode (hold BOOT + RST) to recover. The
      reliable AT path on this board is UART0 (physical) or the UART0-ingress
      over USB-CDC if the board bridges UART0 (verify).
- [x] **Workspace clippy debt** — **cleaned**: `cargo clippy --workspace --all-targets
      -D warnings` is now clean across `agent.rs`, `agent_runner.rs`, `did.rs`,
      `wear_leveling.rs`, `simulator.rs`, `conversation.rs`, `summary/record.rs`,
      `client.rs`, and the `at_tests`/`comprehensive_agent_tests`/`blockchain_tests`/
      `vc_tests`/`magent-lua` suites. 1314 host tests green.
- [ ] REQ-VFY-005 — `cargo kani` 0 unknown (LLM HTTP offline verification).
- [ ] REQ-VFY-006 — workspace code coverage ≥ 80% (`cargo llvm-cov`; currently only
      `magent-lua` meets it).
- [ ] REQ-VFY-007 — hardware fuzz (`cargo-fuzz`) 0 crash in 1 h, per release.
- [ ] Tier 6 — on-hardware validation per `docs/TIER6_VALIDATION.md`
      (clean-boot log, OTA + rollback, Secure Boot eFuse, Flash Enc, TLS, BLE,
      ping) on a real ESP32-C61 board.

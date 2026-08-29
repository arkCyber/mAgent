# ESP32-C61 Tier-6 On-Hardware Validation Runbook

> **Scope**: validate the firmware features added/fixed across the production
> hardening passes on a **real ESP32-C61-DevKitC-1-N8R2** board. This is the
> Tier-6 gate in `docs/SRS.md` (every merge / per-release) and the prerequisite
> for all on-hardware-only backlog items (OTA, Secure Boot eFuse, Flash
> Encryption, TLS, ping, BLE).

## ⚠️ Safety first

- **Secure Boot + Flash Encryption eFuses are ONE-WAY.** Do **not** run
  `flash-secure.sh --apply` on a board you may want to debug/reflash freely
  later. Use a **sacrificial dev board** for the first provisioning, and back up
  `--keydir` before burning.
- Every destructive AT command (`AT+RESTORE`, `AT+OTA`) is irreversible on the
  device — run them on a development board, not a field unit.
- Keep a serial monitor open (`espflash monitor`) during all steps and capture
  the boot log; every section below gives the exact log line that proves the
  result.

## Preconditions

1. Host with the ESP toolchain: `source ~/export-esp.sh` (espup + espflash +
   esptool + espsecure/espefuse).
2. Build the dev firmware: `cd firmware/esp32-app && cargo build --release`
   (default `wifi,uart,board-c61`) — or `build-c61.sh`.
3. A board wired over USB (default port `/dev/cu.usbserial-10`).
4. A small HTTP file server for OTA (e.g. `python3 -m http.server 8000`) serving
   `magent-esp32-app.bin`.

## Validation matrix

| Area | Step | Pass criterion |
|---|---|---|
| Boot | flash + reset | `[magent] v… booting`, no panic, no crash-loop into safe mode |
| AT / UART | `AT+GMR`, `AT+IDENT?`, `AT+CIPSTAMAC?` | `+GMR`, `+IDENT:<pubkey>`, real MAC |
| BLE | `--features ble` build + scan | scanner sees `mAgent` (0x1850); `SYS_CMD`→`SYS_RSP` round-trip |
| HTTP | `AT+HTTPGET=http://<host>/` | `+CMDER:0`/`OK`, 200 body, no hang |
| TLS | `AT+HTTPGET=https://example.com/` | 200 body over TLS (dev build: insecure certs) |
| Blockchain | agent `get_balance` tool | real RPC response (via `EspIdfTransport`) |
| OTA | `AT+OTA=http://<host>:8000/app.bin` | `+OTA:STARTED` → `[ota] image verified; rebooting` → boots new slot |
| Restore | `AT+RESTORE` | `[at] NVS erased; rebooting` → fresh identity next boot |
| MACRAND | `AT+MACRAND` (safe mode) | `+MACRAND:"aa:…"` |
| **Secure Boot** | `flash-secure.sh` dry-run then `--apply` (sacrificial) | boot log shows `secure boot enabled` |
| **Flash Enc** | same `--apply` run | boot log shows `flash encryption enabled` |
| OTA rollback | boot OTA slot, confirm valid, force bad | bad image rolls back to previous slot |

---

## 1. Baseline boot & AT

```bash
cd firmware/esp32-app && ./flash-c61.sh && espflash monitor
```
- Expect `[magent] v0.1.0 booting (esp-idf-svc 0.52 / ESP32-C61 std)` and **no**
  `PANIC`/`abort()`/repeated reboot.
- Send `AT+GMR` → `+GMR:mAgent…`; `AT+IDENT?` → a hex pubkey; `AT+UPTIME?` → ms.
- If you see crash-loop safe mode: check `AT+SAFEMODE?`, review internal-heap
  logs, and re-check that BLE is initialised before Wi-Fi (per `main.rs`).

## 2. Wi-Fi + HTTP

- Provision via `AT+CWJAP="<ssid>","<pass>"` (persisted to NVS, DBO2-sealed).
- `AT+HTTPGET=http://192.168.1.10:8000/` → 200 + body.
- `AT+HTTPGET=https://example.com/` → 200 over TLS. If it fails with a memory
  error, this is the known C61 TLS issue (see `docs/BACKLOG.md`); record the
  exact error + free-heap.

## 3. Blockchain RPC (real transport)

- With a configured LLM/agent path, invoke the blockchain tool (`get_balance`).
- The new `blockchain_transport.rs` (`EspIdfTransport`) must produce a real RPC
  response, not a placeholder error. Watch the tool result / `[blockchain]` logs.

## 4. BLE (`--features ble`)

```bash
cargo build --release --features ble   # then flash-c61.sh
```
- BLE scanner sees `mAgent`, service `0x1850`.
- Write `AT+IDENT?` to **SYS_CMD (0x2A08)**; read **SYS_RSP (0x2A09)** → the IDENT
  line. This proves the `dispatch_at_command` channel.
- Security note: this channel is unauthenticated today (see `SECURITY.md`).

## 5. OTA

```bash
# host: serve the image
python3 -m http.server 8000   # from the firmware release dir
# device:
AT+OTA=http://192.168.1.10:8000/magent-esp32-app.bin
```
- Expect `+OTA:STARTED`, then `[ota] image verified; rebooting into OTA slot`.
- Confirm the device boots; verify the running OTA slot in the boot log.
- **Rollback**: with `partitions.ota.csv` + anti-rollback, boot a deliberately
  broken image; the watchdog/validation must roll back to the previous slot.
  (Full rollback requires the production build — see §6.)

## 6. Secure Boot v2 + Flash Encryption (sacrificial board)

```bash
./flash-secure.sh --keydir /tmp/prod-keys --chip esp32c61          # dry-run
./flash-secure.sh --keydir /tmp/prod-keys --chip esp32c61 --apply  # ONE-WAY
```
- Dry-run: keys generated + images signed + commands printed, **nothing written**.
- `--apply`: eFuses burned (Secure Boot v2 + flash-enc key, read-protected),
  then signed+encrypted images written with `--encrypt`.
- Boot log must show `secure boot enabled` and `flash encryption enabled`.
- **After this, the board can only run signed/encrypted images** — keep keys safe.

## 7. TLS / certificate verification (prod overlay)

- Build with the production overlay (`sdkconfig.prod.defaults`, which sets
  `CONFIG_ESP_TLS_INSECURE=n`): `AT+HTTPGET=https://…` must verify certs via the
  CA bundle. If the C61 HTTPS memory-error persists, capture it — this is the
  open C61-TLS stability item and blocks secure OTA/DeepSeek in prod.

## 8. Regression sweep

After the above, re-run the full AT smoke (GMR/IDENT/CWJAP/HTTPGET/TIME/NTPSYNC/
SIGN) and the host test gate (`cargo test --workspace --lib --exclude <firmware>`).

## Success definition (Tier-6 gate)

- §1–§4 green on the dev build with zero boot panics.
- §5 green (OTA + verification), §6 green on a sacrificial board (Secure Boot +
  Flash Enc), §7 recorded (TLS result, pass or documented C61 limitation).
- Host test gate green.

## Open items this runbook gates

`AT+PING` (esp_ping), C61 TLS stability, OTA rollback end-to-end, Flash-Enc
encrypted-boot, BLE pairing/encryption, `AT+BLE=ON/OFF/STATE` control, BLE
wallet (0x1851) — all require a passing Tier-6 run.


# mAgent AT Command Reference

> **Purpose.** A deterministic, LLM-free serial command interface for
> the ESP32-C61 firmware. Covers Wi-Fi provisioning, hostname, MAC,
> reconnect policy, safe-mode, identity rotation, and status queries.
>
> **Wire format.** Compatible with the Espressif [ESP-AT] subset for
> Wi-Fi commands so existing factory scripts and tooling can be reused
> without modification. Lines are terminated with `\r\n` (we also accept
> `\n` and bare lines); queries end `?`, sets end `=...`, tests end
> `=?` (the test form is recognised but **not implemented** — it is answered
> `+CMDER:4` rather than executed as a zero-argument set). Likewise, a `?`
> on a command that has no documented query form (e.g. `AT+RST?`,
> `AT+RESTORE?`, `AT+IDENTROT?`) is `+CMDER:4` and **never** runs the action;
> since 2026-11-09 the dispatcher checks the command kind before the per-op
> arms. Command keywords
> are case-insensitive (`at+cwmode=1` is the same
> command as `AT+CWMODE=1`); argument values keep their case. Replies are
> either:
> - `+CMD:<...>` data lines followed by `\r\nOK\r\n` (success); or
> - `+CMDER:<n>\r\nERROR\r\n` (failure).
>
> **Routing.** The first thing the firmware does with any UART text is
> check whether it starts with `AT`. If yes the text is dispatched
> through this surface and *never* seen by the LLM. Natural-language
> text (e.g. `read the temperature`) is routed to the agent's ReAct
> loop as before.

[ESP-AT]: https://docs.espressif.com/projects/esp-at/en/latest/esp32c61/

---

## 1. Aerograde contract

Every command in this document satisfies the following aerograde
discipline (matching the rest of `mAgent`):

| Property | How it's upheld |
|---|---|
| **Zero panic** | The parser returns `Result<AtCommand, AtParseError>`; the dispatcher returns `AtOutcome`. No `unwrap`/`expect` on any hot path; the firmware handler logs and continues on internal error. |
| **Bounded execution time** | All commands complete in O(n) of the input line. Persistent changes go to NVS only; Wi-Fi/identity heavy changes can be refused in `safe mode` (boot-loop protection). |
| **Bounded memory** | All intermediate buffers are `heapless::String` / `heapless::Vec` (max 256/768 bytes). No heap allocation per command. |
| **Crash-loop aware** | Commands that change Wi-Fi credentials while the firmware is in `safe mode` (3 consecutive failed boots) are answered `OK` so scripts don't hang, but deferred — they take effect on the next boot. |
| **Audit log** | Every command produces exactly one `[at] op=… kind=…` log line with the timing tag and outcome. |

---

## 2. Quick-start

**Open the serial port at 115 200 8N1.** The ESP-AT harness on
macOS / Linux:

```sh
# Replace /dev/cu.usbserial-10 with the actual CP210x port.
$ printf 'AT\r\n' > /dev/cu.usbserial-10
OK
$ printf 'AT+GMR\r\n' > /dev/cu.usbserial-10
+GMR:mAgent v0.1.0 / AT v0.2 / esp32-c61
OK
$ printf 'AT+CWJAP="MyHome","hunter2"\r\n' > /dev/cu.usbserial-10
OK
```

**Provision Wi-Fi end-to-end:**

```sh
$ printf 'ATE0\r\n' > /dev/cu.usbserial-10     # turn echo off
OK
$ printf 'AT+CWMODE=1\r\n' > /dev/cu.usbserial-10  # station mode
OK
$ printf 'AT+CWJAP="ssid","pass"\r\n' > /dev/cu.usbserial-10
OK                                            # credentials persisted + live reconnect
$ printf 'AT+CWSTATE?\r\n' > /dev/cu.usbserial-10
+CWSTATE:5,172.20.10.4                         # associated with an IP — no reboot needed
$ printf 'AT+RST\r\n' > /dev/cu.usbserial-10   # optional cold reboot (live esp_restart)
OK
```

Since 2026-08-27, `AT+CWJAP=` reconnects live (the supervisor reloads the
credentials and re-associates immediately), so a reboot is no longer required
to switch networks.

---

## 3. Command reference

The table below maps each command to: its expected reply, what it
touches (NVS, Wi-Fi driver, identity, …), and any aerograde caveat.

### 3.1 Basic

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT` | `OK` | Handshake. | No-op; used by scripts to verify the UART is alive. |
| `ATE0` / `ATE1` | `OK` | Accepted for ESP-AT script compatibility. | **Accepted but inert** — the dispatcher answers `OK` via `AtOutcome::NoReply` (`AtOutcome::OK == AtOutcome::NoReply`) for either form (`AtOp::SetEcho { .. } => AtOutcome::NoReply`) and stores no echo state; the firmware never drives the UART echo register (the host already sees its own bytes). |
| `AT+GMR` | `+GMR:…` `OK` | Reports firmware version, AT version, chip. | No IO. |
| `AT+RST` | `OK` | **Live soft reset** — replies `OK`, then reboots via `esp_restart()` after ~200ms so the reply flushes. | Implemented 2026-08-27 (previously deferred). |
| `AT+SYSRAM?` | `+SYSRAM:<bytes>` `OK` | Reports free heap (bytes). | `esp_get_free_heap_size()`. |
| `AT+SYSLOG?` | `+SYSLOG:<0..5>` `OK` | Current log level. | `log::LevelFilter`. |
| `AT+SYSLOG=<0..5>` | `OK` | Set log level at runtime. | `log::set_max_level`. |
| `AT+SYSSTORE?` / `=0/1` | `+SYSSTORE:<0/1>` `OK` | Persist configuration to NVS on (1) or off (0). Default 1. | NVS key `mag_at:sysstore`. |
| `AT+UPTIME?` | `+UPTIME:<ms>` `OK` | Milliseconds since boot. | `esp_timer_get_time()/1000`. |
| `AT+HEAP?` | `+HEAP:<bytes>` `OK` | Reports free heap (bytes). | `esp_get_free_heap_size()` (2026-08-28: was `+SYSRAM:` alias). |

### 3.2 Wi-Fi

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+CWMODE?` | `+CWMODE:<0..3>` `OK` | Wi-Fi mode. | Default 1 (Station). |
| `AT+CWMODE=1` | `OK` | Persist mode to `mag_at:wifi_mode`. | The only mode this firmware implements; `setup_platform` reads the key at boot. |
| `AT+CWMODE=2` / `=3` | `+CMDER:7` | **Refused** — SoftAP / dual-mode are unimplemented. | Previously returned `OK` while persisting a mode `setup_platform` then rejects, so the device silently came up with no Wi-Fi on the next boot. Now fails fast. |
| `AT+CWJAP="ssid","pwd"` | `OK` | Persist credentials to NVS **and reconnect live** (no reboot) — the supervisor reloads them and re-associates immediately. | **Refused in `safe mode`** `+CMDER:4`. Validates SSID ≤32, pass ≤64, no NUL. Sealed with DBO2. |
| `AT+CWJAP` | `OK` | Re-issue connect with last credentials. | Still deferred to next boot (use `AT+CWJAP=...` for a live connect). |
| `AT+CWQAP` | `OK` | Disconnect **now** — the dispatcher raises a flag the supervisor consumes on its next tick, so the radio goes down and the reconnect path is suppressed until the next `AT+CWJAP=`. | No reboot required. |
| `AT+CWLAP` | `+CWLAP:(<ssid>,<rssi>);(…)…` `OK` | List APs from the supervisor's most recent scan (cached when the STA is disconnected, re-scanned on demand if the cache is empty). | Entries are `;`-separated, capped at 20 APs; the line is bounded by `REPLY_LINE_MAX` (256 B) and a partial list ends with `...`. Falls back to `+CWLAP:scan-started` while the on-demand scan is in flight. |
| `AT+CWSTATE?` | `+CWSTATE:<state>[,<ip>]` `OK` | Real Wi-Fi state + IP from the supervisor's live snapshot. | Codes: 0 idle, 1 connecting, 3 associated, 4 disconnected, 5 got-IP. (Previously hard-coded 4.) |
| `AT+CWHOSTNAME?` / `="name"` | `+CWHOSTNAME:"…"` `OK` | Read/write hostname. | NVS `mag_at:hostname`. Max 32 bytes. |
| `AT+CWAUTOCONN?` / `=0/1` | `+CWAUTOCONN:<0/1>` `OK` | Auto-connect at boot. | NVS `mag_at:autoconn`. |
| `AT+CWRECONNCFG?` / `=<int>,<repeat>[,<now>]` | `+CWRECONNCFG:<int>,<repeat>` `OK` | Reconnection policy. ESP-AT bounds: interval ≤7200s, repeat ≤1000. | NVS `mag_at:reconn_int` + `mag_at:reconn_rep`. **Consumed since 2026-09-23** (previously written but ignored): `interval` caps the reconnect backoff (not the credential-error backoff), `repeat` caps reconnect attempts, after which the supervisor stops until `AT+CWJAP=` or a reboot. Both are read once at supervisor start — a change takes effect on the next reboot. |
| `AT+CWJAP?` | `+CWJAP:"<ssid>",,0,0,<seal-fmt>` `OK` | Currently-joined SSID + **seal format** of the stored password. `<seal-fmt>` is one of `NONE`, `DBO2`, `DBO1_LEGACY`, `PLAINTEXT_LEGACY`. | Read-only; the extra field lets an operator spot un-migrated entries. |
| `AT+WIFIPASSUPGRADE?` | `+WIFIPASSUPGRADE:CURRENT` / `+WIFIPASSUPGRADE:LEGACY` / `+WIFIPASSUPGRADE:NO_ENTRY` `OK` | Reports whether `magent:wifi_pass` is already in the current (DBO2) wire format. | Idempotent; safe to run on any device. |
| `AT+WIFIPASSUPGRADE=1` | `+WIFIPASSUPGRADE:MIGRATED` / `+WIFIPASSUPGRADE:CURRENT` `OK` | Re-seal an existing DBO1 / legacy plaintext entry under **DBO2** in place. Only the literal `=1` argument is accepted. | Idempotent. Re-opens the legacy entry, then writes a fresh DBO2 seal. Refuses with `+CMDER:7` if the stored entry can't be opened. |

### 3.3 MAC / Identity

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+CIPSTAMAC?` | `+CIPSTAMAC:aa:bb:cc:dd:ee:ff` `OK` | Read the **real** station MAC via `esp_wifi_get_mac`. | Implemented 2026-08-27 (previously a hard-coded placeholder). Note: no quotes around the value. |
| `AT+CIPSTAMAC="aa:bb:cc:dd:ee:ff"` | `+CMDER:4` `ERROR` | Set station MAC. | Requires stopping the radio first; runtime MAC change not supported. |
| `AT+MACRAND` / `AT+MACRAND?` | `+MACRAND:aa:bb:cc:dd:ee:ff` `OK` | Execute generates a random, locally-administered STA MAC (TRNG) and applies it via `esp_wifi_set_mac`; the `?` query is read-only and reports the current STA MAC. | Implemented 2026-08-28 (was `+CMDER:9`). Randomisation requires the Wi-Fi interface to be stopped (e.g. safe mode); otherwise `+CMDER:6`. No quotes around the value. Since 2026-11-09 the query form no longer randomises the MAC. |
| `AT+IDENT?` | `+IDENT:<hex-pubkey>` `OK` | Ed25519 public key (did:key material) in hex. Returns `+IDENT:NO_IDENTITY` if absent. | Read-only; the firmware regenerates identity from hardware TRNG on first boot. |
| `AT+IDENTROT` | `+IDENTROT:<hex-pubkey>` `OK` | Generate new Ed25519 seed from TRNG and overwrite NVS. | **Refused in `safe mode`**. Atomically persists hex-encoded 32-byte seed to `magent:dev_identity`. |

### 3.4 Safe-mode / Crash recovery

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+SAFEMODE?` | `+SAFEMODE:<0/1>` `OK` | Current forced-safe-mode flag. | NVS `mag_at:safemode`. |
| `AT+SAFEMODE=1` | `OK` | Force the next boot into safe mode (skip Wi-Fi). | Useful when diagnosing a boot loop. |
| `AT+SAFEMODE=0` | `OK` | Clear the flag. | Wi-Fi behaves normally on next boot. |

### 3.5 Diagnostics

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+IFCONFIG?` | `+IFCONFIG:"<ip>"` `OK` | Report the current STA IP (from the supervisor snapshot). Empty when the STA has no link. | Implemented 2026-08-27 (previously `+IFCONFIG: deferred`). Netmask/gateway still deferred. |
| `AT+PING="host"` | `+PING: reply=N rtt=Xms` `OK` | ICMP echo via esp_ping; IPv4/IPv6 literal or hostname (DNS). | Implemented 2026-08-28; IPv6 (e.g. `::1`) added 2026-08-28. The unit is **milliseconds** (`ESP_PING_PROF_TIMEGAP` reports `elapsed_time_ms`); it was previously labelled `us`. |

### 3.6 Restore / agent passthrough

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+RESTORE` | `OK` (reboots in 200 ms) | Factory reset: erase the **entire** NVS region (`nvs_flash_erase`) then reboot. Destructive and irreversible. | Implemented 2026-08-28 (was refused). The device regenerates a fresh device identity on next boot. |
| `AT+SIGN="text"` | `<signed-message JSON>` `OK` | Sign the payload with the device Ed25519 identity; returns canonical signed-message JSON (signer DID + payload_hex + signature_hex). | Implemented 2026-08-27 (previously `+CMDER:4`). The accepted length is bounded by the 256-byte reply line: the DID (~56 chars) + JSON keys + 128-hex signature leave ~11 payload bytes for a typical `did:key` identity, and `+CMDER:7` names the actual maximum. (The earlier `1..=64` claim promised payloads that could never fit.) |
| `AT+AGENT="<text>"` | `OK` | Escape hatch: hand the rest of the line to the agent's ReAct loop. | Useful when you want to script via AT but ask the LLM. |

### 3.7 OTA firmware update

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+OTA=<url>` | `+OTA:STARTED` `OK`, or `+CMDER:7` if the URL is rejected | Stream the firmware image at `url` into the inactive OTA slot (`esp_ota_begin`/`write`/`end`), verify it, set it as the next boot target, and reboot. Runs on a worker thread. | Implemented 2026-08-28 (REQ-FW-005). Since 2026-09-23 the URL is validated **synchronously** (must be `https://`), so a rejected URL returns `+CMDER:7` instead of a `+OTA:STARTED` that never starts. Remaining failures abort the OTA handle and leave the running firmware untouched. Requires an `ota_0`/`ota_1` + `otadata` partition table. Logs `[ota] OTA failed` on failure; the success arm is unreachable (`esp_restart` never returns). |

### 3.8 Additional implemented commands

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+BLE?` / `AT+BLE=<ON\|OFF\|STATE>` | `+CMDER:9` | BLE was **removed from the firmware on 2026-09-20**. The verb is still parsed for command-surface compatibility, but always reports unsupported — it never answers `OK` for a radio that is not there. | See the *BLE removed outright* entry in `docs/BACKLOG.md`. Provision over UART0 (`AT+CWJAP=`, `AT+LLMCFG=`) or the web admin panel instead. |
| `AT+HTTPGET=<url>` | `OK` / body | Issue an HTTP GET to verify outbound reachability. | TLS via mbedTLS + CA certificate bundle (worker thread). |
| `AT+AGENTCFG?` / `AT+AGENTCFG=<name>,<max_iter>,<max_mem>,<watchdog_secs>,<ble_secs>,<skills>,<max_skills>,<debug>` / `AT+AGENTCFG=RESET` | `+AGENTCFG:…` / `OK` | Query, persist, or reset the core `AgentConfig` the agent boots with. Booleans are `0`/`1`; `name` must not contain a comma (the query reply is comma-separated and unquoted); `RESET` deletes the entry. | Stored through `magent_core::config_store` (versioned, validated) in the `magent_cfg` NVS namespace. Takes effect on the **next boot** (the agent thread reads it once at startup, like `AT+LLMCFG`). Numeric range failures → `+CMDER:7`, missing/malformed fields → `+CMDER:4`, name too long → `+CMDER:8`, NVS failure → `+CMDER:7`. **Currently enforced at runtime:** `name`, `max_iterations`, `max_memory`, `max_skills`; `watchdog_secs` / `ble_secs` / `skills` / `debug` are stored and validated but have no consumer yet (see `AgentConfig`'s field docs). |
| `AT+LLMCFG=<model>,<api_key>` | `OK` | Set / query the LLM backend parameters (DeepSeek). | NVS-backed (`mag_at:llm_model` / `mag_at:llm_api_key`). |
| `AT+FLIGHTCFG=<provider>,<token>` | `OK` / `+FLIGHTCFG:<provider>,<masked>` | Set / query the flight-price provider used by the agent's `get_flight_price` tool. Providers: `travelpayouts` (token as query param) or `kiwi` (Tequila key, sent as an `apikey` header). | NVS-backed (`mag_at:flight_provider` / `mag_at:flight_token`); unknown provider → `+CMDER:7`, missing/short args → `+CMDER:4` / `:8`. The reply only ever shows the masked token. |
| `AT+LUAAPP=<b64>` / `AT+LUAAPP=+<b64>` | `OK` | Set / append the Lua app source (URL-safe base64) for the S3 Lua host; `AT+LUAAPP?` reports length. | `lua` feature (S3 only). The AT line is capped at 256 bytes, so a real app is uploaded in `+` chunks; the stored source is capped at 16 KiB. Every chunk is a read-modify-write + NVS commit — an operator-update path, not a hot loop. |
| `AT+NTPSYNC` | `OK` | Force an SNTP time sync. | Requires network up (skipped in safe mode). |
| `AT+TIME?` | `+TIME:<iso8601>,<wall_secs>,<source>` | Report current wall-clock time. First field is ISO 8601; second is Unix seconds; third is the sync source tag. | Via `sntp_sync`. Reports `+TIME:UNSYNCED` before the first sync, `+TIME:ERROR` if the RTC read fails. |
| `AT+TIMEZONE=<zone>` | `OK` | Set the timezone. | NVS-backed. |

---

## 4. `+CMDER` numeric codes

| Code | Meaning |
|---|---|
| 0 | Empty / not-an-AT |
| 4 | Invalid argument / unparseable |
| 5 | Unknown verb |
| 6 | Internal parser error |
| 7 | Number out of range / NVS write failure |
| 8 | String too long |
| 9 | Invalid MAC |

These match the ESP-AT convention so off-the-shelf scripts do not
need a special branch.

---

## 5. NVS namespace layout

Three namespaces are used (the third, `magent_cfg`, holds the versioned
`config_store` entry; it is deliberately separate from every `magent` /
`mag_at` key):

| Namespace | Key | Purpose | Written by | **Read by (boots that honour it)** |
|---|---|---|---|---|
| `magent` | `wifi_ssid` | Wi-Fi SSID | Boot (env), `AT+CWJAP=` | `connect_wifi` (every boot) |
| `magent` | `wifi_pass` | Wi-Fi password (**DBO2-sealed**, see §7) | `AT+CWJAP=` (after seal), `AT+WIFIPASSUPGRADE=1` (migrates legacy entries), Boot (env→seal) | `connect_wifi` (after open) |
| `magent` | `dev_identity` | Ed25519 seed (hex) | Boot (TRNG), `AT+IDENTROT` | `load_or_create_identity` |
| `magent` | `boot_count` | Crash-loop counter | Boot | `check_and_advance_crash_counter` |
| `mag_at` | `wifi_mode` | Last `AT+CWMODE=` | Dispatcher | `setup_platform` (rejects 2/3) |
| `mag_at` | `hostname` | Last `AT+CWHOSTNAME=` | Dispatcher | `connect_wifi` → `sta_netif_mut().set_hostname()` |
| `mag_at` | `autoconn` | Last `AT+CWAUTOCONN=` | Dispatcher | `setup_platform` (skips blocking connect when 0) |
| `mag_at` | `reconn_int` / `reconn_rep` | Reconnect policy | Dispatcher writes; Wi-Fi supervisor reads (since 2026-09-23) | interval caps backoff, repeat caps attempts |
| `mag_at` | `safemode` | Operator-forced safe mode | `AT+SAFEMODE=` | `read_at_safemode_flag()` (consumed then cleared) |
| `mag_at` | `sysstore` | `AT+SYSSTORE` toggle | `AT+SYSSTORE=` | `provision_and_load_wifi_credentials` (gates env-var provisioning) |
| `mag_at` | `timezone_min` | Display UTC offset (minutes) | `AT+TIMEZONE=` | time display |
| `mag_at` | `llm_model` / `llm_api_key` | LLM backend parameters | `AT+LLMCFG=` | LLM backend construction at boot |
| `mag_at` | `flight_provider` / `flight_token` | Flight-price provider | `AT+FLIGHTCFG=` | `get_flight_price` (at tool-call time) |
| `magent_cfg` | `agent_config` | Versioned `config_store` `AgentConfig` entry | `AT+AGENTCFG=` | agent thread at boot (`config_nvs::load_or_default`) |

The `mag_at` namespace is treated as the *overlay*; the `magent`
namespace keeps its existing schema so factory provisioning is
backwards compatible. `magent_cfg` deliberately bypasses the
`nvs_load_string` / `nvs_save_string` helpers: its entry is a binary,
versioned `config_store` envelope, so `config_nvs.rs` uses the raw NVS blob
API (`nvs_get_blob` / `nvs_set_blob`).

### 5.1 Namespace routing

`nvs_load_string` / `nvs_save_string` accept a `namespace:key`
shorthand so callers don't have to thread a separate namespace
parameter through every call site:

```rust
// "mag_at:wifi_mode"  →  (namespace = "mag_at", key = "wifi_mode")
// "wifi_ssid"         →  (namespace = "magent", key = "wifi_ssid")  // default
```

The dispatcher writes with explicit `ns = "mag_at"` (since it
already has the `NS` constant in scope); the boot readers use the
shorthand. Both paths land at the same `(namespace, key)` pair, so
operators never see a write-then-not-found disconnect.

### 5.2 Read-path close-out (audit point)

A common failure mode in AT implementations is "writes are visible
on the wire but boot ignores them". The following AT commands all
have their read path explicitly wired into the boot sequence:

| AT command | Effective from | Behaviour change |
|---|---|---|
| `AT+CWJAP=` | Immediate on next `connect_wifi` (typically same boot) | New SSID/pass used |
| `AT+CWHOSTNAME=` | Next `connect_wifi` call | DHCP discover carries new hostname |
| `AT+CWAUTOCONN=0` | Next boot | Skips 30 s blocking association |
| `AT+CWMODE=2/3` | Next boot | Skips Wi-Fi init (v0.3 implements AP mode) |
| `AT+SAFEMODE=1` | Next boot only | Skips Wi-Fi init; flag cleared after use |
| `AT+SYSSTORE=0` | Next boot | Skips env-var provisioning; existing NVS values still read |
| `AT+IDENTROT` | Next boot | Fresh identity used; previous one rotated out |
| `AT+AGENTCFG=` | Next boot | Agent thread loads the new name / iteration / memory / skills bounds; `RESET` restores board defaults |

If you find an AT command whose setting persists to NVS but does
**not** appear in this table, that is an audit gap — please file it.

### 5.3 Wi-Fi password sealing (DBO2, with DBO1 migration path)

Every write **made by the firmware** — from `AT+CWJAP=`, from the
env-var provisioning path on first boot, and from
`AT+WIFIPASSUPGRADE=1` — runs the plaintext through
`magent_core::wifi_pass_seal_v2` (the **DBO2** algorithm) first, so a raw
flash dump of NVS does not reveal the WPA2 passphrase.

> **Caveat — a flash image written by hand bypasses this.** The reader
> accepts a legacy plaintext entry (`OpenOutcome::LegacyPlaintext`) so that
> devices provisioned before DBO2 keep working. If you provision by writing
> an NVS partition image directly (see `docs/DEPLOYMENT_GUIDE.md` §6.1
> method 2), the value lands in NVS **unsealed** and stays that way until
> `AT+WIFIPASSUPGRADE=1` re-seals it. Run `AT+WIFIPASSUPGRADE?` after
> field provisioning and confirm it reports `CURRENT`, not `LEGACY`.

#### Wire format

NVS value is a hex-encoded string of the form

```
DBO2:<12-byte nonce><N-byte ciphertext><16-byte MAC>
       |                |                   |
       |                |                   +-- HMAC-SHA256( mac_key, nonce || ciphertext )
       |                +-- plaintext XOR cipher_key (derived per entry via HKDF-SHA256)
       +-- random, drawn from the ESP32 hardware TRNG at write time
```

All three fields are lowercase-ASCII hex (no separators), prefixed by
the version tag `DBO2:`. The full payload is bounded by
`wifi_pass_seal_v2::MAX_ENCODED_LEN = 189` bytes, i.e. prefix 5 +
hex(nonce) 24 + hex(ciphertext) ≤ 128 + hex(MAC) 32 — derived from
`MAX_PLAINTEXT = 64` and `MAC_LEN = 16`.

#### Algorithm

```
device_key  = 32-byte Ed25519 seed from magent:dev_identity
nonce       = 12 fresh random bytes per write (HW TRNG)
prk         = HKDF-Extract(salt = nonce, ikm = device_key)
cipher_key  = HKDF-Expand(prk, info = "magent/dbo2/cipher/v1", L = 32)
mac_key     = HKDF-Expand(prk, info = "magent/dbo2/mac/v1",    L = 32)
cipher[i]   =  plain[i] XOR cipher_key[i % 32]
mac         =  HMAC-SHA256( mac_key, nonce || ciphertext )[0..16]
stored      =  "DBO2:" || hex(nonce) || hex(cipher) || hex(mac)
```

The two `info` strings are distinct domain-separation labels
(`magent/dbo2/cipher/v1` and `magent/dbo2/mac/v1`), so the cipher key and
the MAC key are independent.

DBO2 strengthens DBO1 in two ways:

1. **Per-entry key stretching.** DBO1 reused `device_key` directly as
   the XOR pad, so every entry was an XOR of the same key stream.
   Given two ciphertexts and one known plaintext, the second
   plaintext fell out trivially. DBO2 mixes the device key with a
   per-write nonce through HKDF-SHA256, so each entry uses a unique
   cipher key. Known-plaintext attacks no longer compose across
   entries.
2. **HMAC integrity.** DBO1 had no integrity tag — a single bit-flip
   in NVS would either decode to garbage (loud, via bad UTF-8) or
   pass through silently to the Wi-Fi stack. DBO2 carries a 16-byte
   HMAC-SHA256 tag; on `open_sealed_v2` the tag is verified **before**
   any plaintext is returned, so tampering with NVS (bit flip, prefix
   swap, length change) is rejected with `SealError::BadMac` /
   `BadLength` / `BadPrefix` and the boot path returns `None`.

#### Migration from DBO1 / legacy plaintext

`open_sealed_v2` is a **superseder** of `open_sealed_bytes`:

- Entries prefixed with `DBO2:` are opened as DBO2.
- Entries prefixed with `DBO1:` are opened via the DBO1 path
  (transparent fallback).
- Entries with no recognised prefix are returned as
  `OpenOutcome::LegacyPlaintext` so the boot path can still use them
  during one more cycle before the operator runs the explicit
  migration command.

`AT+WIFIPASSUPGRADE=1` performs an explicit in-place migration: it
opens the legacy entry (DBO1 or plaintext), runs the plaintext back
through DBO2, and writes the new seal. The query form
`AT+WIFIPASSUPGRADE?` reports `CURRENT` (already DBO2) /
`LEGACY` (still on the old format) / `NO_ENTRY` (no wifi_pass at
all) so an operator can audit a fleet with a single scripted loop.

Devices provisioned before DBO2 was added continue to work without
intervention: every read goes through `open_sealed_v2` and falls back
to DBO1 / plaintext automatically. The migration is a one-shot,
idempotent, opt-in upgrade — there is no deadline pressure.

The **device identity seed** (`magent:dev_identity`, wrapped as
`BTDK1:<seal>`) follows the same rule since 2026-11-09: the writer emits
DBO2, the opener accepts DBO2 and DBO1 (re-sealing a DBO1 payload forward
on read), and `DEV_IDENTITY_STORED_MAX` is 195 bytes. This is deliberately
not exposed as an AT command — rotating the seed is `AT+IDENTROT`, which
always writes the current format.

#### Properties

- **No new dependencies** — `magent_core::wifi_pass_seal_v2` adds
  `hmac`, `sha2`, `sha3`, `digest` to the dep graph, all gated under
  the `web3` cargo feature.
- **Zero panic** — every function returns `Result`; on buffer
  overflow the seal refuses rather than truncates.
- **Versioned** — the `DBO2` prefix lets us change the algorithm
  later without misinterpreting old entries. Unknown prefixes are
  surfaced as `OpenOutcome::LegacyPlaintext` so a single
  backwards-incompatible revision doesn't brick already-provisioned
  devices.
- **No silent fallback** — a sealed entry that fails integrity /
  format validation is **not** silently downgraded to "use the
  prefix-stripped bytes as the password". The boot path returns
  `None` and `connect_wifi` is skipped, so a corrupted NVS partition
  produces a clean "no network" boot, not a wrong AP.

#### Threat model & non-goals

- **In scope**: passive flash-dump of a single device. The
  ciphertext is bound to *that* device's `dev_identity` seed, so
  dumping and reading on a different device yields nothing.
- **Out of scope**: an attacker with code-execution on the same
  device (they can call `open_sealed_v2` themselves). The boot path
  must recover plaintext to feed ESP-IDF; that is the whole point.
  If you need stronger guarantees, layer a physical-unlock secret
  in front — but that is a v0.3 concern.
- **Out of scope**: low-entropy Wi-Fi passwords (a 4-digit PIN is
  brute-forceable in seconds regardless of sealing). The operator
  is responsible for choosing a strong passphrase.

#### Audit log

The dispatcher writes the password without logging it. The audit log
line is shaped so an operator can correlate repeat `AT+CWJAP=`
calls (same fingerprint → same plaintext+nonce on the same device)
without ever seeing secret material:

```
[at] CWJAP set: ssid=... pass_len=64 sealed_fp=a1b2c3d4 prefix=DBO2:
```

`sealed_fp` is the first 4 bytes of the **ciphertext** portion of
the NVS entry. It does not leak the password or the nonce itself.
Because the nonce is re-drawn per call, the fingerprint does NOT
survive a reboot — it is only useful for correlating retries within
one boot session (e.g. a misbehaving provisioning script).

---

## 6. Workflow walkthroughs

### 6.1 Factory provisioning (production)

```sh
#!/bin/sh
# Provision one device without rebuilding the firmware.
# Run per board (loop over device nodes for a batch).
DEV=/dev/cu.usbserial-10

# Open the port once and keep it open for both directions. A bare
# `printf '...' > "$DEV"` reopens the tty for every write, which
# discards whatever the device sent in between — so the script
# could never see an error reply, let alone assert on it.
stty -f "$DEV" 115200 cs8 -cstopb -parenb -clocal raw -echo
exec 3<>"$DEV"

# Send a command and require an expected reply. A write-then-sleep
# script that never reads cannot tell "OK" from "+CMDER:8", which
# is how a mis-provisioned fleet ships green.
send() {                    # $1 = command, $2 = expected regex
    printf '%s\r\n' "$1" >&3
    IFS= read -r -t 5 line <&3 || { echo "TIMEOUT: $1" >&2; return 1; }
    printf '>> %-24s => %s\n' "$1" "$line"
    printf '%s' "$line" | grep -Eq "$2" || { echo "UNEXPECTED: $1 => $line" >&2; return 1; }
}

send 'ATE0'                     '^OK'            # echo off (script friendly)
send 'AT+GMR'                   '^\+GMR:mAgent v'  # verify firmware
send 'AT+CWMODE=1'              '^OK'            # station mode
send 'AT+CWAUTOCONN=1'          '^OK'            # auto-connect at next boot
send 'AT+CWHOSTNAME="iot-001"'  '^OK'
send 'AT+CWJAP="HomeWifi","hunter2"' '^OK'       # seals + stores the password
send 'AT+CWRECONNCFG=5,100'     '^OK'            # retry every 5s up to 100 times
send 'AT+SYSSTORE=1'            '^OK'            # persist
send 'AT+IDENT?'                '^\+IDENT:'      # record device key on the server
send 'AT+RST'                   '^OK'            # reboot

# Read back what actually stuck, rather than trusting the writes above.
printf 'AT+CWJAP?\r\n' >&3 ; IFS= read -r -t 5 l <&3 ; echo "$l"   # expect ,DBO2
printf 'AT+CWRECONNCFG?\r\n' >&3 ; IFS= read -r -t 5 l <&3 ; echo "$l"
exec 3<&-
```

> Note: `AT+CWJAP?` reports the seal format as its last field
> (`...,DBO2`). A hand-written NVS image can leave the password in
> plaintext until `AT+WIFIPASSUPGRADE=1` runs — see §5.3.

### 6.2 Field maintenance

```sh
# Customer complains Wi-Fi stopped working. SSID changed?
# Open the serial console (115 200 8N1) and:
> AT+CWJAP?          # what does the device remember?
+CWJAP:"OldWiFi",,0,0,DBO2
OK
> AT+CWJAP="NewWiFi","newpass"
OK
> AT+RST
OK
# Device reboots, joins the new AP. Done.
```

### 6.3 Crash-loop recovery

```sh
# In factory QA: the device keeps rebooting every 5s.
> AT+GMR
+GMR:mAgent v0.1.0 / AT v0.2 / esp32-c61
OK
> AT+SAFEMODE=1
OK                  # next boot will skip Wi-Fi
> AT+RST
OK
# The flag is one-shot: `read_at_safemode_flag()` consumes it and
# `clear_at_safemode_flag()` writes "0" back once it has been
# applied, so Wi-Fi resumes on the *following* boot without any
# operator action. `AT+SAFEMODE=0` before the reboot cancels it.
```

Safe mode is also entered automatically: a boot counter counts
consecutive resets, and after `CRASH_LOOP_THRESHOLD = 3` in a row
the firmware treats it as a crash loop and boots with Wi-Fi
skipped (`main.rs`). That auto-entry stays active for as long as
the loop persists; it is not the same one-shot flag `AT+SAFEMODE=1`
sets.

---

## 7. Test plan (aerograde verification)

| Item | How to verify | Where |
|---|---|---|
| Parser handles all 37 verbs | Unit + integration tests | `magent-core::at`, `tests/at_tests.rs` |
| Dispatcher never panics on any input | Insert 1000 random AT lines | `at_dispatch::dispatch` test surface (planned for v0.3) |
| NVS round-trip survives reboot | Set SSID, reboot, query | Flash + serial test |
| Length caps enforced | SSID 33 bytes → `+CMDER:8` | `at_validate.rs` (`CWJAP_SSID_MAX = 32`) |
| Safe-mode blocks CWJAP set | Set safemode=1, attempt CWJAP → `+CMDER:4` | `at_dispatch::cwjap_dispatch` (safe_mode gate) |
| IDENTROT survives reboot | Rotate, query, reboot, query | Ed25519 round-trip |
| **DBO1 seal round-trips** | 24 unit tests (empty / ASCII / UTF-8 / boundary / wrong-key / corrupt / version-tag / nonce-uniqueness) | `magent_core::wifi_pass_seal::tests` |
| **DBO2 seal round-trips** | 24 unit tests (round-trip / boundary / MAC-tamper / prefix-rejection / length-rejection / DBO1 fallback) | `magent_core::wifi_pass_seal_v2::tests` (requires `--features web3`) |
| **WIFIPASSUPGRADE parses** | `AT+WIFIPASSUPGRADE?` and `AT+WIFIPASSUPGRADE=1` round-trip via the parser | `magent-core::at::tests::parses_wifipassupgrade_*` |
| **DBO1 → DBO2 migration** | Seal under DBO1, call `open_sealed_v2`, recover plaintext, seal under DBO2, call `open_sealed_v2`, recover plaintext, assert equal | `magent_core::wifi_pass_seal_v2::tests::dbo1_to_dbo2_migration_round_trip` |
| **DBO1 not downgraded on error** | Corrupt prefix → `None` returned, not legacy string | `open_stored_wifi_pass` boot path (firmware) |
| **DBO2 not downgraded on MAC mismatch** | Tamper one byte of ciphertext → `BadMac`, no plaintext returned | `magent_core::wifi_pass_seal_v2::tests::tampered_ciphertext_fails_mac` |
| **No plaintext leaks in log** | Run `AT+CWJAP=`, grep serial for password | Manual / serial-capture test |

Currently the parser has 79 unit + 44 integration tests passing
(host-side), covering every command + every error code in the
numerical table above. The DBO1 seal/open layer has 24 unit tests
covering boundary conditions, encoding errors, version-tag drift,
and the nonce-uniqueness property. The DBO2 layer adds 24 unit tests
covering round-trip, MAC-tamper rejection, length-rejection,
hex-validation, and the transparent DBO1 / legacy plaintext fallback
that makes the in-place migration safe. (DBO2 lives behind the
`web3` feature, so `cargo test -p magent-core --features web3` is
what exercises it; the bare default-feature test run does not.)

---

## 8. Compatibility matrix

| Surface | v0.2 (this doc) | v0.3 (planned) |
|---|---|---|
| Basic commands | ✅ | ✅ |
| `AT+CWJAP=` full semantics | ✅ live connect (no reboot) | — |
| `AT+CWLAP` results | ✅ cached scan list | full table |
| `AT+CWSTATE?` | ✅ live snapshot | — |
| `AT+CIPSTAMAC?` | ✅ real MAC | — |
| `AT+CIPSTAMAC` set | refused | live cycle |
| `AT+SIGN` | ✅ implemented | — |
| `AT+RESTORE` | ✅ implemented (NVS wipe + reboot) | — |
| `AT+PING` | ✅ implemented (ICMP via esp_ping, v4 + v6) | — |
| `AT+IFCONFIG` | ✅ live IP (mask/gw pending) | lwip netif dump |
| `AT+MACRAND` | ✅ implemented (wifi stopped) | live cycle |
| `AT+OTA=<url>` | ✅ implemented (stream + verify + reboot) | OTA-only table + rollback |
| **DBO1 password seal** | ✅ | ✅ (kept for read-back compat) |
| **DBO2 password seal** | ✅ (default for new writes) | ✅ |
| **`AT+WIFIPASSUPGRADE?` / `=1`** | ✅ (DBO1 → DBO2 in-place migration) | ✅ |

The `v0.3` column is a backlog. The `v0.2` set is what the firmware
ships today and what the tests cover.

---

*Last reviewed: 2026-09-22 · owner: arksong · doc revision: AT v0.2 (DBO2 + WIFIPASSUPGRADE)*

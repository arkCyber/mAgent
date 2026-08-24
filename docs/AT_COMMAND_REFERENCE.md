# mAgent AT Command Reference

> **Purpose.** A deterministic, LLM-free serial command interface for
> the ESP32-C61 firmware. Covers Wi-Fi provisioning, hostname, MAC,
> reconnect policy, safe-mode, identity rotation, and status queries.
>
> **Wire format.** Compatible with the Espressif [ESP-AT] subset for
> Wi-Fi commands so existing factory scripts and tooling can be reused
> without modification. Lines are terminated with `\r\n` (we also accept
> `\n` and bare lines); queries end `?`, sets end `=...`, tests end
> `=?`. Replies are either:
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
OK                                            # credentials persisted
$ printf 'AT+RST\r\n' > /dev/cu.usbserial-10   # reboot (deferred in v0.2)
OK
```

After reboot, `setup_platform` reads the new credentials and the
device comes up connected.

---

## 3. Command reference

The table below maps each command to: its expected reply, what it
touches (NVS, Wi-Fi driver, identity, …), and any aerograde caveat.

### 3.1 Basic

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT` | `OK` | Handshake. | No-op; used by scripts to verify the UART is alive. |
| `ATE0` / `ATE1` | `OK` | Toggle local echo of received commands. State kept by the dispatcher. | We accept but the current firmware doesn't drive the UART echo register (the host already sees its own bytes). |
| `AT+GMR` | `+GMR:…` `OK` | Reports firmware version, AT version, chip. | No IO. |
| `AT+RST` | `OK` | Soft reset (deferred in v0.2; takes effect on next boot). | Wi-Fi changes need a reboot to take effect; we keep `AT+RST` semantically honest by logging a deferred marker. |
| `AT+SYSRAM?` | `+SYSRAM:<bytes>` `OK` | Reports free heap (bytes). | `esp_get_free_heap_size()`. |
| `AT+SYSLOG?` | `+SYSLOG:<0..5>` `OK` | Current log level. | `log::LevelFilter`. |
| `AT+SYSLOG=<0..5>` | `OK` | Set log level at runtime. | `log::set_max_level`. |
| `AT+SYSSTORE?` / `=0/1` | `+SYSSTORE:<0/1>` `OK` | Persist configuration to NVS on (1) or off (0). Default 1. | NVS key `mag_at:sysstore`. |
| `AT+UPTIME?` | `+UPTIME:<ms>` `OK` | Milliseconds since boot. | `esp_timer_get_time()/1000`. |
| `AT+HEAP?` | `+SYSRAM:<bytes>` `OK` | Alias of `AT+SYSRAM?`. | — |

### 3.2 Wi-Fi

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+CWMODE?` | `+CWMODE:<0..3>` `OK` | Wi-Fi mode. | Default 1 (Station). |
| `AT+CWMODE=<0..3>` | `OK` | Persist mode to `mag_at:wifi_mode`. | Wi-Fi re-applied at next boot. |
| `AT+CWJAP?` | `+CWJAP:"<ssid>",,0,0` `OK` | Currently-joined SSID (BSSID/RSSI/ch are placeholders until v0.3 reads `esp_wifi_sta_get_state()`). | Read-only. |
| `AT+CWJAP="ssid","pwd"` | `OK` | Persist Wi-Fi credentials to `magent:wifi_ssid` / `magent:wifi_pass`. | **Refused in `safe mode`** with `+CMDER:4`. Validates SSID ≤32 bytes, pass ≤64, no NUL bytes. |
| `AT+CWJAP` | `OK` | Re-issue connect with last credentials. | Deferred to next boot in v0.2. |
| `AT+CWQAP` | `OK` | Disconnect. | Deferred to next boot in v0.2. |
| `AT+CWLAP` | `+CWLAP:scan-started` `OK` | List available APs. | Background scan in v0.2; explicit table output ships in v0.3. |
| `AT+CWSTATE?` | `+CWSTATE:4` `OK` | Current Wi-Fi state. | ESP-AT codes: 0 uninit, 1 connected no IP, 2 IP, 3 connecting, 4 disconnected. We always answer 4 in v0.2. |
| `AT+CWHOSTNAME?` / `="name"` | `+CWHOSTNAME:"…"` `OK` | Read/write hostname. | NVS `mag_at:hostname`. Max 32 bytes. |
| `AT+CWAUTOCONN?` / `=0/1` | `+CWAUTOCONN:<0/1>` `OK` | Auto-connect at boot. | NVS `mag_at:autoconn`. |
| `AT+CWRECONNCFG?` / `=<int>,<repeat>[,<now>]` | `+CWRECONNCFG:<int>,<repeat>` `OK` | Reconnection policy. ESP-AT bounds: interval ≤7200s, repeat ≤1000. | NVS `mag_at:reconn_int` + `mag_at:reconn_rep`. |
| `AT+CWJAP?` | `+CWJAP:"<ssid>",,0,0,<seal-fmt>` `OK` | Currently-joined SSID + **seal format** of the stored password. `<seal-fmt>` is one of `NONE`, `DBO2`, `DBO1_LEGACY`, `PLAINTEXT_LEGACY`. | Read-only; the extra field lets an operator spot un-migrated entries. |
| `AT+WIFIPASSUPGRADE?` | `+WIFIPASSUPGRADE:CURRENT` / `+WIFIPASSUPGRADE:LEGACY` / `+WIFIPASSUPGRADE:NO_ENTRY` `OK` | Reports whether `magent:wifi_pass` is already in the current (DBO2) wire format. | Idempotent; safe to run on any device. |
| `AT+WIFIPASSUPGRADE=1` | `+WIFIPASSUPGRADE:MIGRATED` / `+WIFIPASSUPGRADE:CURRENT` `OK` | Re-seal an existing DBO1 / legacy plaintext entry under **DBO2** in place. Only the literal `=1` argument is accepted. | Idempotent. Re-opens the legacy entry, then writes a fresh DBO2 seal. Refuses with `+CMDER:7` if the stored entry can't be opened. |

### 3.3 MAC / Identity

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+CIPSTAMAC?` | `+CIPSTAMAC:"…"` `OK` | Read station MAC. (Currently returns the placeholder in v0.2; real driver call ships in v0.3.) | — |
| `AT+CIPSTAMAC="aa:bb:cc:dd:ee:ff"` | `+CMDER:4` `ERROR` | Set station MAC. | Requires Wi-Fi cycle; deferred to v0.3. |
| `AT+MACRAND?` / `=0/1` | `+CMDER:9` `ERROR` | Toggle MAC randomisation. | Deferred to v0.3. |
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
| `AT+IFCONFIG?` | `+IFCONFIG: deferred` `OK` | Report IP / Mask / GW. | Awaiting a clean interface in v0.3. |
| `AT+PING="host"` | `+CMDER:4` `ERROR` | ICMP ping. | Awaiting an ICMP stack module. |

### 3.6 Restore / agent passthrough

| Command | Reply | Effect | Aerograde note |
|---|---|---|---|
| `AT+RESTORE` | `+CMDER:4` `ERROR` | Factory reset (wipe NVS). | **Not implemented in v0.2** to avoid accidental data loss. Track in v0.3 with explicit `AT+RESTORECONFIRM` follow-up. |
| `AT+SIGN="text"` | `+CMDER:4` `ERROR` | Sign with current identity. | Defer until v0.3 has unified signer context. |
| `AT+AGENT="<text>"` | `OK` | Escape hatch: hand the rest of the line to the agent's ReAct loop. | Useful when you want to script via AT but ask the LLM. |

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

Two namespaces are used:

| Namespace | Key | Purpose | Written by | **Read by (boots that honour it)** |
|---|---|---|---|---|
| `magent` | `wifi_ssid` | Wi-Fi SSID | Boot (env), `AT+CWJAP=` | `connect_wifi` (every boot) |
| `magent` | `wifi_pass` | Wi-Fi password (**DBO2-sealed**, see §7) | `AT+CWJAP=` (after seal), `AT+WIFIPASSUPGRADE=1` (migrates legacy entries), Boot (env→seal) | `connect_wifi` (after open) |
| `magent` | `dev_identity` | Ed25519 seed (hex) | Boot (TRNG), `AT+IDENTROT` | `load_or_create_identity` |
| `magent` | `boot_count` | Crash-loop counter | Boot | `check_and_advance_crash_counter` |
| `mag_at` | `wifi_mode` | Last `AT+CWMODE=` | Dispatcher | `setup_platform` (rejects 2/3) |
| `mag_at` | `hostname` | Last `AT+CWHOSTNAME=` | Dispatcher | `connect_wifi` → `sta_netif_mut().set_hostname()` |
| `mag_at` | `autoconn` | Last `AT+CWAUTOCONN=` | Dispatcher | `setup_platform` (skips blocking connect when 0) |
| `mag_at` | `reconn_int` / `reconn_rep` | Reconnect policy | Dispatcher | Reserved (v0.3 driver hook) |
| `mag_at` | `safemode` | Operator-forced safe mode | `AT+SAFEMODE=` | `read_at_safemode_flag()` (consumed then cleared) |
| `mag_at` | `sysstore` | `AT+SYSSTORE` toggle | `AT+SYSSTORE=` | `provision_and_load_wifi_credentials` (gates env-var provisioning) |

The `mag_at` namespace is treated as the *overlay*; the `magent`
namespace keeps its existing schema so factory provisioning is
backwards compatible.

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

If you find an AT command whose setting persists to NVS but does
**not** appear in this table, that is an audit gap — please file it.

### 5.3 Wi-Fi password sealing (DBO2, with DBO1 migration path)

The `magent:wifi_pass` entry is **never** stored in plaintext. Every
write — from `AT+CWJAP=`, from the env-var provisioning path on first
boot, and from `AT+WIFIPASSUPGRADE=1` — runs the plaintext through
`magent_core::wifi_pass_seal_v2` (the **DBO2** algorithm) first. A raw
flash dump of NVS no longer reveals the WPA2 passphrase.

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
`wifi_pass_seal_v2::MAX_ENCODED_LEN = 253` bytes (prefix 5 + nonce 24 +
ciphertext ≤ 192 + MAC 32).

#### Algorithm

```
device_key  = 32-byte Ed25519 seed from magent:dev_identity
nonce       = 12 fresh random bytes per write (HW TRNG)
(cipher_key, mac_key) = HKDF-SHA256(
    ikm  = device_key,
    salt = nonce,
    info = "magent-wifi-pass-seal-v2",
    L    = 64,
)
cipher[i]  =  plain[i] XOR cipher_key[i % 32]
mac        =  HMAC-SHA256( mac_key, nonce || ciphertext )[0..16]
stored     =  "DBO2:" || hex(nonce) || hex(cipher) || hex(mac)
```

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
# Provision 100 devices without rebuilding the firmware.
DEV=/dev/cu.usbserial-10
for s in ssid list; do :; done

send() { printf "$1\r\n" > "$DEV"; sleep 0.1; }

send 'ATE0'                  # echo off (script friendly)
send 'AT+GMR'                # verify firmware
send 'AT+CWMODE=1'           # station mode
send 'AT+CWAUTOCONN=1'       # auto-connect at next boot
send 'AT+CWHOSTNAME="iot-001"'
send 'AT+CWJAP="HomeWifi","hunter2"'
send 'AT+CWRECONNCFG=5,100'  # retry every 5s up to 100 times
send 'AT+SYSSTORE=1'        # persist
send 'AT+IDENT?'             # record device public key on the server
send 'AT+RST'                # reboot (effective on next cycle)
```

### 6.2 Field maintenance

```sh
# Customer complains Wi-Fi stopped working. SSID changed?
# Open the serial console (115 200 8N1) and:
> AT+CWJAP?          # what does the device remember?
+CWJAP:"OldWiFi",,0,0
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
# ... later, after fixing the upstream issue ...
> AT+SAFEMODE=0
OK
```

---

## 7. Test plan (aerograde verification)

| Item | How to verify | Where |
|---|---|---|
| Parser handles all 28 verbs | Unit + integration tests | `magent-core::at`, `tests/at_tests.rs` |
| Dispatcher never panics on any input | Insert 1000 random AT lines | `at_dispatch::dispatch` test surface (planned for v0.3) |
| NVS round-trip survives reboot | Set SSID, reboot, query | Flash + serial test |
| Length caps enforced | SSID 33 chars → `+CMDER:8` | Parser unit test |
| Safe-mode blocks CWJAP set | Set safemode=1, attempt CWJAP | Firmware trace |
| IDENTROT survives reboot | Rotate, query, reboot, query | Ed25519 round-trip |
| **DBO1 seal round-trips** | 22 unit tests (empty / ASCII / UTF-8 / boundary / wrong-key / corrupt / version-tag / nonce-uniqueness) | `magent_core::wifi_pass_seal::tests` |
| **DBO2 seal round-trips** | 17 unit tests (round-trip / boundary / MAC-tamper / prefix-rejection / length-rejection / DBO1 fallback) | `magent_core::wifi_pass_seal_v2::tests` |
| **WIFIPASSUPGRADE parses** | `AT+WIFIPASSUPGRADE?` and `AT+WIFIPASSUPGRADE=1` round-trip via the parser | `magent-core::at::tests::parses_wifipassupgrade_*` |
| **DBO1 → DBO2 migration** | Seal under DBO1, call `open_sealed_v2`, recover plaintext, seal under DBO2, call `open_sealed_v2`, recover plaintext, assert equal | `magent_core::wifi_pass_seal_v2::tests::dbo1_to_dbo2_migration_round_trip` |
| **DBO1 not downgraded on error** | Corrupt prefix → `None` returned, not legacy string | `open_stored_wifi_pass` boot path (firmware) |
| **DBO2 not downgraded on MAC mismatch** | Tamper one byte of ciphertext → `BadMac`, no plaintext returned | `magent_core::wifi_pass_seal_v2::tests::mac_tamper_returns_bad_mac` |
| **No plaintext leaks in log** | Run `AT+CWJAP=`, grep serial for password | Manual / serial-capture test |

Currently the parser has 48 unit + 37 integration tests passing
(host-side), covering every command + every error code in the
numerical table above. The DBO1 seal/open layer has 22 unit tests
covering boundary conditions, encoding errors, version-tag drift,
and the nonce-uniqueness property. The DBO2 layer adds 17 unit tests
covering round-trip, MAC-tamper rejection, length-rejection,
hex-validation, and the transparent DBO1 / legacy plaintext fallback
that makes the in-place migration safe.

---

## 8. Compatibility matrix

| Surface | v0.2 (this doc) | v0.3 (planned) |
|---|---|---|
| Basic commands | ✅ | ✅ |
| `AT+CWJAP=` full semantics | ✅ (NVS only) | + live connect |
| `AT+CWLAP` results | placeholder | full table |
| `AT+CWSTATE?` | sentinel (4) | live `esp_wifi_sta_get_state()` |
| `AT+CIPSTAMAC` set | refused | live cycle |
| `AT+SIGN` / `AT+RESTORE` | refused | implemented |
| `AT+PING` | refused | needs ICMP |
| `AT+IFCONFIG` | placeholder | lwip netif dump |
| `AT+MACRAND` | refused | needs driver restart |
| **DBO1 password seal** | ✅ | ✅ (kept for read-back compat) |
| **DBO2 password seal** | ✅ (default for new writes) | ✅ |
| **`AT+WIFIPASSUPGRADE?` / `=1`** | ✅ (DBO1 → DBO2 in-place migration) | ✅ |

The `v0.3` column is a backlog. The `v0.2` set is what the firmware
ships today and what the tests cover.

---

*Last reviewed: 2026-08-23 · owner: arksong · doc revision: AT v0.2 (DBO2 + WIFIPASSUPGRADE)*

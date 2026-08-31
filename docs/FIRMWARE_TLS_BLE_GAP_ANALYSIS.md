# Firmware TLS / BLE Gap — Design Analysis

> Status: **T1/T2 implemented & hardware-verified (S3, build44)**; T3 deferred; T4 needs
> physical-UART0 soak (see `tools/s3_tls_soak.py`). Analysis inputs: `docs/BACKLOG.md`
> (P1/P2), `sdkconfig.defaults` / `sdkconfig.prod.defaults`, `firmware/esp32-app/src/llm.rs`,
> `ble_config.rs`, `ble_gatt.rs`, `ble_wallet.rs`, and on-hardware observations captured in
> the audit pass of 2026-08-28. Target platforms: **ESP32-C61** (RISC-V, primary, N8R2 — 2 MB
> PSRAM) and **ESP32-S3** (Xtensa, 8 MB PSRAM).

This document is a *design analysis* for the two outstanding hardware-facing gaps that block
secure production operation: (1) reliable certificate-verified **TLS** on device, and
(2) working + secure **BLE**. Both are memory-bound problems: they share the same scarce
resource — **internal DMA-capable DRAM** — so they must be solved with a single, explicit
memory budget rather than in isolation.

---

## 1. The shared constraint: internal DRAM

Neither issue is primarily a protocol or API bug. Both are **heap** problems.

| Resource | ESP32-C61 (primary) | ESP32-S3 |
|---|---|---|
| Internal SRAM | ~512 KB (partitioned; WiFi/FreeRTOS/lwIP reserve much of it) | ~512 KB |
| PSRAM | 2 MB (`CONFIG_SPIRAM=y`, `SPIRAM_USE_MALLOC=y`) | 8 MB |
| Can WiFi RX use PSRAM? | No — WiFi RX buffers must be DMA-capable **internal** | same |
| Can the BLE **controller** use PSRAM? | No — controller needs `MALLOC_CAP_8BIT\|DMA\|INTERNAL` | same |
| Can TLS/mbedTLS **SSL buffers** use PSRAM? | **Yes** — TLS in/out buffers are not DMA-required | same |

The recurring failure pattern is a request for a large contiguous **internal-DRAM** allocation
that the pool can no longer satisfy once WiFi + the agent threads have claimed their share:

* **TLS (C61):** HTTPS fails fast with a memory error; HTTP works. mbedTLS needs two SSL
  in/out buffers (default 16 KB each in internal DRAM) plus cert-store parse heap.
* **BLE (S3):** `btdm_controller_mem_init` returns **257 `ESP_ERR_NO_MEM`** — the controller
  wants a big contiguous internal-DRAM DMA block (~244–253 KB) that the ~244 KB free pool
  cannot provide with WiFi + agent running.

Because the two subsystems compete for the same pool, a solution that only shrinks one can
just move the failure to the other. **The recommendation below therefore treats DRAM as a
single budget and routes the non-DMA traffic (TLS buffers, BLE host, certs) to PSRAM.**

---

## 2. TLS gap

### 2.1 Symptoms (on C61)
* `AT+HTTPGET=<https-url>` / DeepSeek-over-HTTPS fails **fast with a memory error**.
* Attempted shrink knobs (`CONFIG_MBEDTLS_DYNAMIC_BUFFER`, `CONFIG_MBEDTLS_SSL_KEEP_PEER_CERTIFICATE`)
  made the handshake **HANG** and left lwIP in a bad state → reverted (see `sdkconfig.defaults`).
* HTTP (non-TLS) is reliable.
* Dev config still ships `CONFIG_ESP_TLS_INSECURE=y`; the **prod overlay
  (`sdkconfig.prod.defaults`) already flips it to `n`** and enables the CA bundle — but a build
  that cannot complete a verified handshake can't ship.

### 2.2 Stack (as-built)
* `CONFIG_ESP_TLS_USING_MBEDTLS=y` (mbedTLS is the only TLS stack on firmware; rustls is host-only).
* `CONFIG_MBEDTLS_CERTIFICATE_BUNDLE=y` — compiled-in Mozilla CA bundle used by
  `llm.rs` via `crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach)`.
* `llm.rs` `HttpConfig { timeout: 8s, crt_bundle_attach }` — bounded timeout so a hung TLS
  cannot trip the watchdog.

### 2.3 Root-cause analysis
1. **SSL buffer size.** mbedTLS default in/out content length is 16 KB **each**, allocated from
   internal DRAM. Two 16 KB contiguous internal allocations on a board already holding WiFi +
   the agent threads is precisely the kind of alloc that fails or exhausts the pool at
   handshake time (ClientHello → ServerHello → record processing needs both).
2. **Dynamic-buffer interaction.** The `DYNAMIC_BUFFER` attempt *should* have reduced peak; the
   hang instead points to a second problem — when the SSL record buffers are resized at
   handshake, the lwIP socket read path and mbedTLS desynchronise if a record straddles the
   read. This is a known mbedTLS/lwIP boundary hazard on small-buffer builds, which is why
   reverting "fixed" the hang (at the cost of the memory error).
3. **Cert store.** The full CA bundle inflates the mbedTLS CA table and the parse heap.

### 2.4 Options

| # | Option | Effort | Risk | Impact |
|---|---|---|---|---|
| T1 | Route mbedTLS SSL buffers to **PSRAM** (they are not DMA-required) via heap caps; keep `DYNAMIC_BUFFER` **off** to avoid the lwIP desync | Low | Low | **Primary fix** — removes the internal-DRAM pressure that causes the memory error, without touching the fragile dynamic-buffer path |
| T2 | Right-size `CONFIG_MBEDTLS_SSL_IN/OUT_CONTENT_LEN` to 4–8 KB (DeepSeek replies fit), plus `MBEDTLS_MAX_INPUT_SIZE` | Low | Med | Reduces internal footprint further; do **after** T1, with a handshake soak to catch the lwIP desync |
| T3 | Pin one root/intermediate cert for `api.deepseek.com` and drop the full bundle for that client | Low | Low | Shrinks cert parse heap + table; keep the bundle for OTA/other hosts |
| T4 | (Validate-only) TLS soak harness on C61: repeated handshakes, watch the free-heap low-water mark (already exposed) | Low | — | The acceptance gate; catches the hang variant |

> **Why PSRAM for TLS buffers is safe here:** only the **WiFi RX** buffers and the **BLE
> controller** are DMA-capped to internal DRAM. TLS record buffers are ordinary byte buffers
> consumed by mbedTLS — they can live in PSRAM. This is the standard ESP-IDF mitigation.

### 2.5 Recommendation (TLS)
1. Implement **T1** — allow mbedTLS record buffers to come from PSRAM (heap caps on the SSL
   malloc), leaving `DYNAMIC_BUFFER` off.
2. Apply **T2** (8 KB SSL content len) and **T3** (pinned DeepSeek cert) as incremental
   reductions.
3. Gate the change with **T4** (repeated-handshake soak + free-heap watermark) and keep
   `CONFIG_ESP_TLS_INSECURE=n` (prod overlay) — **REQ-NET-001 is only "done" when the soak
   passes with verification on.**

**Status 2026-08-31:**
* ✅ **T1 done** — `CONFIG_MBEDTLS_EXTERNAL_MEM_ALLOC=y` in `sdkconfig.defaults`; verified in
  the regenerated `sdkconfig.h` (`=1`). SSL in/out buffers now allocate from PSRAM (safe —
  not DMA-required), `DYNAMIC_BUFFER` stays off. Build44 boots stable on the S3.
* ✅ **T2 done** — `CONFIG_MBEDTLS_SSL_IN_CONTENT_LEN=8192` (was 16384), `SSL_OUT_CONTENT_LEN=4096`.
* 🔶 **T3 deferred** — optional on the S3 (8 MB PSRAM makes the full CA bundle cheap);
  pinning a DeepSeek cert still worthwhile later for cert-parse heap + table.
* 🔶 **T4 blocked on hardware access** — the ingress binds physical UART0 (not the USB console),
  so the soak must run against a USB-UART adapter on GPIO43/44. Harness ready:
  `python3 tools/s3_tls_soak.py <physical-uart0> --iterations 10`.



---

## 3. BLE gap

Two independent sub-problems, in priority order.

### 3.1 BLE controller memory on ESP32-S3 (`ESP_ERR_NO_MEM` 257)

**Symptom:** `esp_bt_controller_init` passes parameter checks but fails in
`btdm_controller_mem_init` with 257 — the Bluedroid controller cannot get its large
contiguous **internal-DRAM DMA** block (~244–253 KB). PSRAM is unusable for it.

**What's been tried (verified on hardware):**
* `CONFIG_BT_ALLOCATION_FROM_SPIRAM_FIRST=y` — moves the **host** to PSRAM, not the controller.
* `CONFIG_BT_BLE_50_FEATURES_SUPPORTED=n` — smaller than expected gain.
* `CONFIG_BT_BLE_42_SCAN_EN=n` — a per-`btm_ble_init` allocation; helps but isn't the blocker.
* **On-hardware (2026-08-31)**: at BLE init the S3 reports `internal_free=248 KB`,
  and `esp_bt_controller_init` still returns **ESP_ERR_NO_MEM (257)**. BLE 5.0 is
  already off and the host is PSRAM-first; the *controller's* contiguous internal
  DMA block (~244–253 KB) still doesn't fit alongside the boot path.
* **PATCHED (2026-08-31)**: `CONFIG_BTDM_CTRL_BLE_MAX_CONN` reduced **6 → 1** to shrink
  the controller's link-context pool (one mAgent-Man client suffices). Awaiting
  re-test — if it still fails, the S3 controller's static DMA appetite is a hard
  budget limit and the remaining path is NimBLE (B3).

**Root cause:** the Bluedroid controller on the S3 has a fixed large internal-DRAM DMA appetite
and (unlike some stacks) no `BTDM_CONTROLLER_*` Kconfig to shrink the core pools below what the
default configuration needs. With WiFi + agent threads resident, the free internal pool
(~244 KB) sits right at the boundary — marginal, and it loses.

**Options**

| # | Option | Effort | Risk | Impact |
|---|---|---|---|---|
| B1 | **Explicit internal-RAM budget**: `CONFIG_BTDM_CTRL_BLE_MAX_CONN` 6→1–2, reduce event-task stacks, drop BLE 5.0 extras, confirm `BLE_42_SCAN_EN=n` | Low | Low | May just cross the boundary on the S3; **cheap first try** |
| B2 | **Validate BLE on the C61 first** (primary board, its own controller config) and treat S3 BLE as follow-on | Low | Low | Unblocks the primary platform now; defers the hard S3 case |
| B3 | **Migrate to NimBLE** (`CONFIG_BT_NIMBLE_ENABLED=y`): ~50 KB smaller controller, host can live in PSRAM | Med–High | Med | The robust long-term answer for WiFi+BLE coexistence on the tight S3; **requires reworking `ble_config.rs`/`ble_gatt.rs` because `esp-idf-svc`'s BLE API targets Bluedroid** |

**Recommendation (controller):** Do **B2** (validate C61 BLE) as the immediate path, attempt
**B1** as a low-effort S3 knob, and treat **B3** (NimBLE + a thin GATT shim) as the durable fix
for the S3 — it is the only option that genuinely removes the controller from the internal-DRAM
boundary, which is what lets BLE coexist with WiFi + the agent.

### 3.2 BLE channel security & missing control surface

**Status 2026-08-31 (BLE DISABLED — product decision):**

* 🔴 **BLE is turned OFF** — the Bluedroid BLE controller needs ~244 KB of
  contiguous internal-DMA DRAM (`ESP_ERR_NO_MEM 257` on the S3) and the product
  doesn't use BLE, so `CONFIG_BT_ENABLED=n` in `sdkconfig.defaults` and the
  `ble` cargo feature removed from `build-s3.sh`. This frees internal DRAM for
  the agent + Wi-Fi. `ble_config.rs`/`ble_at.rs`/`ble_wallet.rs` are excluded by
  `#[cfg(feature = "ble")]` (and the AT+BLE dispatch returns `+CMDER:9` when BLE
  is off). The BLE AT+BLE control + pairing/encryption code below remains for
  future re-enablement on a platform that can host the controller.
* ✅ **`AT+BLE=ON/OFF/STATE`** — implemented (shared `BleServer` + `ble_dispatch`),
  available when the `ble` feature is re-enabled.
* ✅ **Pairing/encryption** (`BLE_REQUIRE_ENCRYPTION`) — implemented, opt-in.
* 🔶 **BLE wallet service (0x1851)** — still reserved dead code.

**S3 stability fixes (2026-08-31, verified on hardware — final values):**
* 🔧 **agent-thread stack 96/128 → 32 KiB** — measured `uxTaskGetStackHighWaterMark`
  shows the ReAct `run()` uses only ~5 KiB (high-water ~15–16 KiB), so the
  earlier 96/128 KiB were grossly oversized. 32 KiB frees ~64 KiB of internal
  DRAM. `Box::pin` on the ReAct future is retained. PSRAM stacks are NOT
  viable on the S3 (`esp_task_stack_is_sane_cache_disabled` assert — PSRAM
  shares the flash cache).
* 🔧 **ingress-thread stack 24 → 32 KiB** — fixed the `Guru Meditation:
  Double exception` crash right after boot (the `IngressGateway`/UART setup
  overflowed the 24 KiB ingress stack once the sdkconfig regenerated). Note
  48 KiB *fails* to spawn under Wi-Fi (thread-spawn exhaustion), so 32 KiB is
  the verified balance point.
* 🔧 **Lua Core0 crash root cause (not a Lua bug)** — a debug (symbol) backtrace
  showed the "Lua" panic is actually a **Wi-Fi PHY power-management
  esp_timer use-after-free**: `ppTask → pm_tbtt_process → esp_phy_enable →
  esp_timer_start_periodic → timer_insert` writing to a freed node
  (`0xa5a5a5a5`). Enabling Lua merely adds threads/memory that exposes this
  latent Wi-Fi-subsystem path. Lua is therefore disabled (`board-s3,wifi,uart`
  build) until the esp_timer/Wi-Fi-PHY issue is addressed at the ESP-IDF layer.




---

## 4. Interdependency & single memory budget

TLS and BLE share internal DRAM. The recommended sequence is:

1. **T1/T2/T3 (TLS → PSRAM + shrink)** — removes TLS from the internal-DRAM budget entirely.
2. **B2 (validate C61 BLE)** — the primary platform, on a config the board can actually support.
3. **B1 → B3 (S3 BLE)** — cheap knob first, NimBLE migration as the durable fix.
4. **3.2 (BLE auth + control + wallet)** — a security gate and feature-completion pass, enabled
   only after the controller runs.

This ordering means the two gaps stop competing: TLS goes to PSRAM, BLE is validated on the
platform that can host it, and the S3's internal DRAM is freed for either.

---

## 5. Validation (Tier-6) mapping

| Gap | Tier-6 acceptance (`docs/TIER6_VALIDATION.md`) |
|---|---|
| TLS | Clean-boot log; repeated HTTPS handshakes (DeepSeek + OTA) with **no memory error and no hang**; free-heap low-water mark recorded; `CONFIG_ESP_TLS_INSECURE=n` in prod |
| BLE C61 | `esp_bt_controller_init` succeeds; advertising + connect; SYS_RSP round-trip over SYS_CMD |
| BLE S3 | Controller init succeeds with WiFi + agent (B1/B3); or explicit documented decision to keep S3 BLE off and use C61 |
| BLE auth | Pairing/encryption enforced before SYS_CMD accepted; `AT+BLE` control round-trips |

---

## 6. Open questions for the design review

* Is a pinned DeepSeek cert acceptable for OTA (different host) or must the full bundle stay?
* For the S3, is BLE-on-S3 actually a product requirement, or can BLE be C61-only (which
  de-prioritises B3)?
* If NimBLE (B3) is chosen, does the BLE GATT surface in `magent-hal` / `ble_config` need a
  Bluedroid/NimBLE abstraction, or is a firmware-local shim sufficient?


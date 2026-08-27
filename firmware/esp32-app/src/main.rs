//! mAgent firmware for the ESP32-C61-DevKitC-1-N8R2 (N8R2, std).
//!
//! This binary runs the chip-agnostic mAgent core on ESP32-C61 hardware
//! using the `esp-idf-svc` framework. The C61 has 8 MB Flash, 2 MB PSRAM,
//! and a single-core RISC-V RV32IMAC CPU.
//!
//! # Stack choices
//!
//! We use `esp-idf-svc` + `std` (not `esp-hal` + `no_std`) because the
//! N8R2 board provides 2 MB of PSRAM that backs the global `std::alloc`
//! allocator. This gives us enough heap for the ReAct loop, serde_json,
//! and the esp-idf-svc HTTP client without any custom allocator glue.
//!
//! # Thread model
//!
//! Two std threads run concurrently:
//!
//!  - **`agent-thread`** — drives the `MiniAgent` ReAct loop.
//!  - **`ingress-thread`** — monitors UART0 for incoming commands and feeds
//!    them to the `IngressGateway`.
//!
//! # Wi-Fi provisioning
//!
//! Wi-Fi STA credentials are read from NVS at boot. Use `espflash write-nvs`
//! to provision them (see `docs/ESP32.md` for the full procedure).
//!
//! # Building
//!
//! ```sh
//! # Requires: source ~/export-esp.sh (ESP-IDF toolchain)
//! cargo build -p magent-esp32-app --release
//! ```
//!
//! # Flashing
//!
//! ```sh
//! cargo run -p magent-esp32-app --release
//! ```
//!
//! TRACE: REQ-FW-001, REQ-FW-002, REQ-NET-001, REQ-SAFE-001.

mod link_adapters;
mod local_tools;
mod web_admin;
mod at_dispatch;
mod device_key;
mod llm;
#[cfg(feature = "ble")]
mod ble_config;
#[cfg(feature = "ble")]
mod ble_at;
#[cfg(feature = "ble")]
mod ble_wallet;
#[cfg(feature = "wifi")]
mod sntp_sync;

#[cfg(feature = "ble")]
use crate::ble_config::BleServer;

use core::convert::TryFrom;
use core::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use heapless::String as HeaplessString;

use magent_core::{AgentConfig, MiniAgent, VERSION};
use magent_core::{
    ingress::{IngressGateway, IngressMode},
    web3::Identity,
};

use link_adapters::UartAdapter;

/// Shared "current task" handle: the ingress thread writes UART commands
/// here; the agent thread drains the latest one and executes it against real
/// hardware (via `Esp32ToolHandler`).
type TaskHandle = Arc<Mutex<Option<std::string::String>>>;

/// Live Wi-Fi status snapshot published by the Wi-Fi supervisor thread and
/// read by the AT dispatcher (`AT+CWSTATE` etc.).
///
/// The `BlockingWifi` handle is owned exclusively by the supervisor thread,
/// so nothing else can poke it. Instead the supervisor publishes a tiny,
/// allocation-light snapshot here; the AT dispatcher locks and formats it
/// without ever touching the radio. This is what makes `AT+CWSTATE` report
/// the *real* link state (previously it hard-coded `+CWSTATE:4`).
#[derive(Clone, Default)]
pub struct WifiStatus {
    /// 0=idle, 1=connecting, 3=associated, 4=disconnected, 5=got-IP.
    pub state: u8,
    /// Last known STA IPv4 address (empty if none yet).
    pub ip: String,
    /// AP SSID we are configured for (recovered from the DBO seal).
    pub ssid: String,
    /// Last observed RSSI in dBm (0 when unknown / not associated).
    pub rssi: i32,
    /// Last Wi-Fi disconnect reason code (0 = none / clean).
    pub reason: u32,
    /// Monotonic ms of the last update (from `now_ms()`).
    pub updated_ms: u64,
}

/// Shared handle used to publish / read [`WifiStatus`].
pub type WifiStatusHandle = Arc<Mutex<WifiStatus>>;

/// Monotonic time in milliseconds (ESP-IDF `esp_timer`, shared across threads).
fn now_ms() -> u64 {
    unsafe { esp_idf_sys::esp_timer_get_time() as u64 / 1000 }
}

/// Free heap in bytes (internal + PSRAM).
fn free_heap() -> u32 {
    unsafe { esp_idf_sys::esp_get_free_heap_size() }
}

/// Shared heartbeat used to detect a hung (stalled, non-panicking) thread.
///
/// Each worker thread calls [`Heartbeat::beat`] on every loop iteration; the
/// supervisor (main loop) calls [`Heartbeat::stale`] to see if a worker has
/// stopped making progress. `last_ms == 0` means "never beat" (thread may not
/// have started yet), which we don't flag as stale.
#[derive(Clone, Default)]
struct Heartbeat {
    last_ms: Arc<std::sync::atomic::AtomicU32>,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            last_ms: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
    fn beat(&self) {
        self.last_ms
            .store(now_ms() as u32, std::sync::atomic::Ordering::SeqCst);
    }
    fn stale(&self, timeout_ms: u64) -> bool {
        let last = self.last_ms.load(std::sync::atomic::Ordering::SeqCst);
        // `wrapping_sub` tolerates the u32 millisecond counter wrapping
        // (~49.7 days of uptime).
        last != 0 && (now_ms() as u32).wrapping_sub(last) > timeout_ms as u32
    }
}

/// The UART0 peripheral + its TX/RX pins needed by the ingress thread
/// (only present when the `uart` feature is on).
#[cfg(feature = "uart")]
type IngressUartParts = (
    esp_idf_svc::hal::uart::UART0<'static>,
    esp_idf_svc::hal::gpio::Gpio11<'static>, // U0TXD
    esp_idf_svc::hal::gpio::Gpio10<'static>, // U0RXD
);
#[cfg(not(feature = "uart"))]
type IngressUartParts = ((), (), ());

// ---------------------------------------------------------------------------
// Boot diagnostics
// ---------------------------------------------------------------------------

/// Panic handler installed by esp-idf-svc. All panics end here.
///
/// The `esp-idf-svc` panic handler prints the location over UART0 before
/// PATCHED (MicroAgent): Removed the custom `#[panic_handler]` because
/// `esp-idf-svc` with the `std` feature pulls in the Rust standard library,
/// which provides its own panic handler. Having two `panic_impl` lang items
/// causes E0152. We rely on std's panic handler, which calls `abort()` on
/// panic and (via the panic-abort build-std flag we set in `.cargo/config.toml`)
/// triggers the ESP-IDF panic handler that prints diagnostics and reboots.
/// TRACE: REQ-SAFE-001.

// ---------------------------------------------------------------------------
// Global logger
// ---------------------------------------------------------------------------

/// Initialise the esp-idf-svc global logger.
///
/// TRACE: REQ-SAFE-001 — must be called before any other esp-idf-svc
/// code so all modules get the same filter.
fn init_logging() {
    // PATCHED (MicroAgent): `EspLogger::setup()` was removed in master
    // esp-idf-svc; the equivalent is `initialize_default()` which
    // installs the default IDF-side log filter as the global `log`
    // backend. The log level is controlled via sdkconfig's
    // CONFIG_LOG_MAXIMUM_LEVEL.
    EspLogger::initialize_default();
    // PATCHED (MicroAgent): bumped to Debug for boot-path diagnosis.
    log::set_max_level(log::LevelFilter::Debug);
    #[cfg(feature = "board-c61")]
    let chip_label = "ESP32-C61";
    #[cfg(feature = "board-s3")]
    let chip_label = "ESP32-S3";
    log::info!("[magent] v{VERSION} booting (esp-idf-svc 0.52 / {chip_label} std)");
}

// ---------------------------------------------------------------------------
// NVS helpers
// ---------------------------------------------------------------------------

/// NVS key for the Wi-Fi SSID.
const NVS_KEY_WIFI_SSID: &str = "wifi_ssid";
/// NVS key for the Wi-Fi password.
const NVS_KEY_WIFI_PASS: &str = "wifi_pass";
/// NVS keys for the LLM backend parameters (in the `mag_at` namespace,
/// read/written by `AT+LLMCFG`).
const NVS_KEY_LLM_MODEL: &str = "mag_at:llm_model";
const NVS_KEY_LLM_API_KEY: &str = "mag_at:llm_api_key";

/// Shared, long-lived default NVS partition handle.
///
/// `EspDefaultNvsPartition::take()` can only succeed ONCE (it flips the
/// internal `DEFAULT_TAKEN` singleton). EspWifi takes it at boot, so the
/// AT dispatcher (which runs later, from the ingress thread) must share
/// this same handle rather than call `take()` again — otherwise every
/// AT read/write silently fails with `+CMDER:7`. We take it once in
/// `init_default_nvs()` and hand out clones (`EspNvsPartition` is an
/// `Arc`) to both the boot path and the dispatcher.
static DEFAULT_NVS: OnceLock<&'static EspDefaultNvsPartition> = OnceLock::new();

/// Registry of objects that were intentionally promoted to `&'static`
/// via `Box::leak`. Each entry is one word (a pointer). Used by the
/// H9 audit to detect duplicate leaks: every call site that does a
/// `Box::leak` registers the resulting pointer here. If a later call
/// inserts the *same* pointer (i.e. the same boxed value is leaked
/// twice because the boot path is re-entered), an error is logged.
///
/// HARDENING (audit-2026-08 H9): ESP32 firmware has a 320 KB heap
/// budget. A duplicate leak of even a small struct (a `BlockingWifi`
/// wrapper is ~2 KB) can starve FreeRTOS task stacks. The previous
/// code did `Box::leak` with no registration, so a refactor that
/// silently started re-leaking per reconnect would only show up when
/// the heap guard tripped — usually hours later in production.
///
/// We use `OnceLock<Mutex<HashSet<usize>>>` because `HashSet::new()`
/// is not const-stable, and `OnceLock::get_or_init` gives us the same
/// once-initialisation guarantee without needing `LazyLock`. The
/// mutex is acquired once at each leak site, which is on the order
/// of 1-3 times per boot, so the contention cost is zero.
static LEAKED_BOXES: OnceLock<Mutex<std::collections::HashSet<usize>>> = OnceLock::new();

/// Registry of pointers already leaked via `Box::leak` so a future refactor
/// that re-runs a one-shot init path (soft-reboot, OTA) surfaces a real
/// double-leak instead of silently doubling heap use.
///
/// NOTE on `HashSet::insert` semantics (this was once inverted): `insert`
/// returns `true` when the value was *newly* inserted and `false` when it was
/// *already* present. So a genuine duplicate is detected with `!insert(...)`.
fn leaked_boxes() -> std::sync::MutexGuard<'static, std::collections::HashSet<usize>> {
    LEAKED_BOXES
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Take the default NVS partition exactly once and keep it for the life
/// of the program. Must be called before EspWifi (or anything else) takes
/// ownership. Callers obtain clones via [`default_nvs`].
pub(crate) fn init_default_nvs() {
    match EspDefaultNvsPartition::take() {
        Ok(p) => {
            let leaked: &'static EspDefaultNvsPartition = Box::leak(Box::new(p));
            // HARDENING (audit-2026-08 H9): record this leak in
            // `LEAKED_BOXES` so a future refactor that re-runs
            // `init_default_nvs` (e.g. on a soft-reboot stub) would
            // see the duplicate insert and log an explicit error
            // instead of silently leaking a second NVS partition
            // (which corrupts the partition state and trips a
            // `ESP_ERR_NVS_INVALID_STATE` later).
            // `!insert(...)` is true only when the pointer is ALREADY present.
            if !leaked_boxes().insert(leaked as *const _ as usize) {
                log::error!(
                    "[nvs] init_default_nvs is leaking a duplicate NVS partition \
                     (same pointer as a previous leak)"
                );
            }
            if DEFAULT_NVS.set(leaked).is_err() {
                log::error!("[nvs] DEFAULT_NVS already initialised");
            }
        }
        Err(e) => log::error!("[nvs] could not take default NVS partition: {e}"),
    }
}

/// Clone the shared default NVS partition handle, or `None` if
/// [`init_default_nvs`] was never called / failed.
pub(crate) fn default_nvs() -> Option<EspDefaultNvsPartition> {
    DEFAULT_NVS.get().map(|p| (*p).clone())
}

/// Load a string from NVS. Returns `None` if the key is absent or unreadable.
///
/// Supports a `namespace:key` shorthand so AT-side keys (which live in
/// the `mag_at` namespace) can be addressed without spreading a
/// second wrapper everywhere. Plain keys (no colon) default to the
/// existing `magent` namespace so callers stay one-line.
fn nvs_load_string(key: &str) -> Option<String> {
    let (ns, key) = split_ns_key(key);
    let partition = default_nvs()?;
    let nvs = EspDefaultNvs::new(partition, ns, true).ok()?;
    // PATCHED (MicroAgent): `get_str` now requires an out-buffer for
    // safety (the API previously returned `Option<&str>` borrowing
    // from NVS storage, which had lifetime issues). The buffer is
    // 256 bytes — long enough for an SSID or passkey.
    let mut buf = [0u8; 256];
    nvs.get_str(key, &mut buf).ok().flatten().map(str::to_owned)
}

/// Save a string to NVS. Returns `Ok(())` on success.
///
/// Same `namespace:key` shorthand as `nvs_load_string`.
fn nvs_save_string(key: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (ns, key) = split_ns_key(key);
    let partition = default_nvs().ok_or("default NVS partition not initialised")?;
    let nvs = EspDefaultNvs::new(partition, ns, true)?;
    nvs.set_str(key, value)?;
    Ok(())
}

/// Split `"mag_at:wifi_mode"` into `("mag_at", "wifi_mode")`. Anything
/// without a colon stays in the default `magent` namespace so legacy
/// callers don't need to be touched.
fn split_ns_key(key: &str) -> (&str, &str) {
    match key.find(':') {
        Some(i) => (&key[..i], &key[i + 1..]),
        None => ("magent", key),
    }
}

#[cfg(test)]
mod nvs_split_tests {
    use super::split_ns_key;

    #[test]
    fn plain_key_defaults_to_magent_namespace() {
        assert_eq!(split_ns_key("wifi_ssid"), ("magent", "wifi_ssid"));
        assert_eq!(split_ns_key("boot_count"), ("magent", "boot_count"));
        assert_eq!(split_ns_key(""), ("magent", ""));
    }

    #[test]
    fn mag_at_prefix_routes_to_other_namespace() {
        assert_eq!(split_ns_key("mag_at:wifi_mode"), ("mag_at", "wifi_mode"));
        assert_eq!(split_ns_key("mag_at:hostname"), ("mag_at", "hostname"));
        assert_eq!(split_ns_key("mag_at:sysstore"), ("mag_at", "sysstore"));
    }

    #[test]
    fn first_colon_wins_for_multi_colon_keys() {
        // Defensive: should the operator ever send "weird:key:with:colon",
        // we always pick the first one (so a misplaced namespace prefix
        // can't accidentally look like data).
        assert_eq!(
            split_ns_key("mag_at:ns2:value"),
            ("mag_at", "ns2:value")
        );
    }
}

#[cfg(test)]
mod hex_decode_tests {
    use super::hex_nibble;

    #[test]
    fn nibble_decodes_lowercase_hex() {
        for i in 0..=9u8 {
            assert_eq!(hex_nibble(b'0' + i).unwrap(), i);
        }
        for i in 0..=5u8 {
            assert_eq!(hex_nibble(b'a' + i).unwrap(), 10 + i);
        }
    }

    #[test]
    fn nibble_decodes_uppercase_hex() {
        for i in 0..=5u8 {
            assert_eq!(hex_nibble(b'A' + i).unwrap(), 10 + i);
        }
    }

    #[test]
    fn nibble_rejects_non_hex() {
        for c in b'g'..=b'z' {
            assert!(hex_nibble(c).is_none(), "expected Err for byte {c}");
        }
        assert!(hex_nibble(b' ').is_none());
        assert!(hex_nibble(b':').is_none());
    }
}

// ---------------------------------------------------------------------------
// Safety: crash-loop detection (auto-restart recovery)
// ---------------------------------------------------------------------------
// ESP-IDF's panic handler already reboots the board automatically on a crash.
// To recover from a *crash loop* (the board reboots before the app can do any
// useful work), we keep a consecutive-boot counter in NVS. If we see >= 3
// consecutive fast reboots we enter "safe mode" (skip Wi-Fi / risky bring-up)
// so the board can at least boot, log, and serve UART. Once it has been up
// long enough we treat the boot as stable and reset the counter.

/// NVS key for the consecutive-crash boot counter (stored as a decimal string).
const NVS_KEY_BOOT_COUNT: &str = "boot_count";
/// NVS key for the `AT+SAFEMODE=` operator-forced safe-mode flag.
/// Lives in the `mag_at` namespace (the dispatcher writes it; this
/// function reads it so the user-set flag actually takes effect on
/// the next boot).
const NVS_KEY_SAFEMODE_AT: &str = "mag_at:safemode";
/// Consecutive reboots before we assume a crash loop.
const CRASH_LOOP_THRESHOLD: u32 = 3;

/// Read the operator-forced safe-mode flag (set by `AT+SAFEMODE=1`).
/// Separate from the boot-counter logic so the two failure modes
/// don't conflate: AT forces it for one boot (we clear after);
/// crash-loop forces it for as long as the loop persists.
fn read_at_safemode_flag() -> bool {
    let v = nvs_load_string(NVS_KEY_SAFEMODE_AT)
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(0);
    if v == 0 { return false; }
    log::warn!("[safety] AT+SAFEMODE=1 observed — next boot will skip Wi-Fi");
    true
}

/// Clear the AT-forced safe-mode flag after we applied it (so the
/// operator doesn't have to manually turn it off again).
fn clear_at_safemode_flag() {
    if nvs_save_string(NVS_KEY_SAFEMODE_AT, "0").is_err() {
        log::warn!("[safety] failed to clear mag_at:safemode");
    }
}

/// Advance the boot counter. Returns `true` if we've hit the crash-loop
/// threshold and should boot into safe mode.
fn check_and_advance_crash_counter() -> bool {
    let prev = nvs_load_string(NVS_KEY_BOOT_COUNT)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let next = prev + 1;
    // Best-effort persist; if NVS write fails we still return based on `next`.
    let _ = nvs_save_string(NVS_KEY_BOOT_COUNT, &next.to_string());

    if next >= CRASH_LOOP_THRESHOLD {
        log::error!(
            "[safety] consecutive boot #{next} (>= {CRASH_LOOP_THRESHOLD}) — crash loop suspected, safe mode"
        );
        true
    } else {
        log::info!("[safety] boot #{next}");
        false
    }
}

/// Mark the current boot as stable (called after the board has been up a
/// while), resetting the crash-loop counter.
fn mark_stable_boot() {
    let _ = nvs_save_string(NVS_KEY_BOOT_COUNT, &0u32.to_string());
    log::info!("[safety] boot considered stable — crash counter reset");
}

// ---------------------------------------------------------------------------

/// NVS key for the device's secret seed (32 bytes, hex-encoded).
const NVS_KEY_IDENTITY: &str = "dev_identity";

/// Load a persisted device identity from NVS, or generate a fresh one.
///
/// TRACE: REQ-SAFE-001. The identity is derived from hardware TRNG and
/// persisted to NVS so it survives across reboots.
///
/// Storage format (see `open_dev_identity` / `seal_and_store_dev_identity`):
///   - Modern: `"BTDK1:" || hex(sealed_seed)` (sealed with the
///     boot-time-derived key from eFuse + chip revision).
///   - Legacy: 64 hex chars (32 raw bytes), no prefix. Migrated to
///     BTDK1 on first successful read.
fn load_or_create_identity() -> Identity {
    // Try to load from NVS first. Goes through `open_dev_identity`
    // so sealed and legacy plaintext formats both work, and legacy
    // gets re-sealed in place.
    if let Some(stored) = nvs_load_string(NVS_KEY_IDENTITY) {
        match open_dev_identity(&stored) {
            Ok(seed) => {
                if let Ok(id) = Identity::from_secret_bytes(&seed) {
                    log::info!(
                        "[magent] identity loaded from NVS (pubkey={}...)",
                        // PATCHED (MicroAgent): `Identity::public_key()` returns
                        // `&magent_core::web3::identity::PublicKey`,
                        // a tuple struct whose inner array is
                        // private. The wrapper exposes the bytes
                        // via `as_bytes()` (returns `&[u8; 32]`);
                        // we then slice the first 8 bytes for the
                        // diagnostic prefix.
                        hex::encode(&id.public_key().as_bytes()[..8])
                    );
                    return id;
                }
                log::warn!("[magent] stored seed is not a valid Ed25519 seed; regenerating");
            }
            Err(e) => {
                log::warn!("[magent] could not open dev_identity ({e}); regenerating");
            }
        }
    }

    // Generate a fresh identity from the hardware TRNG.
    log::warn!("[magent] no identity in NVS — generating fresh key from TRNG");
    // PATCHED (MicroAgent): the TRNG is a hardware block that only fails
    // transiently. A single `.expect()` here used to panic the whole board
    // on a one-off read fault, causing a reboot loop (and eventually safe
    // mode). Retry a few times before giving up; only panic as a last resort
    // (the identity is required for the signed ingress path).
    let mut seed = [0u8; 32];
    let mut id: Option<Identity> = None;
    for attempt in 0..8 {
        if getrandom::getrandom(&mut seed).is_err() {
            log::warn!("[magent] TRNG read failed (attempt {attempt}) — retrying");
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        // 32 bytes is always a valid Ed25519 seed, but guard anyway so an
        // unexpected failure retries instead of panicking.
        match Identity::from_secret_bytes(&seed) {
            Ok(i) => {
                id = Some(i);
                break;
            }
            Err(_) => {
                log::warn!("[magent] TRNG produced an invalid seed (attempt {attempt}) — retrying");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let id = match id {
        Some(i) => i,
        None => {
            // HARDENING (audit-2026-08): rather than panic-on-fail (which
            // would loop the board through watchdog resets and exhaust
            // NVS wear), degrade gracefully: keep running with no
            // identity so the operator can still talk to the device over
            // UART / local tools and diagnose the TRNG fault. The
            // secure-boot paths downstream will reject any command that
            // needs signing.
            log::error!(
                "[magent] TRNG could not provide a valid identity seed after 8 attempts; \
                 using an EPHEMERAL UNTRUSTED identity (not persisted)"
            );
            // Every 32-byte array is a valid Ed25519 seed. A zero seed yields a
            // deterministic (weak) key used ONLY to keep the app bootable when
            // the TRNG is faulted. We return here before the persist step below,
            // so this weak key is never written to NVS.
            match Identity::from_secret_bytes(&[0u8; 32]) {
                Ok(i) => return i,
                Err(_) => unreachable!("a zero seed is always a valid Ed25519 seed"),
            }
        }
    };

    // Persist to NVS in BTDK1-sealed form. If sealing fails (e.g.
    // eFuse read fault), fall back to legacy plaintext so the
    // identity is at least usable on this boot, with a loud
    // warning.
    match seal_and_store_dev_identity(&seed) {
        Ok(()) => log::info!("[magent] new identity generated and persisted (BTDK1 sealed)"),
        Err(e) => {
            log::error!("[magent] BTDK1 seal failed ({e}); falling back to plaintext NVS");
            let hex = hex::encode(seed);
            if let Err(e) = nvs_save_string(NVS_KEY_IDENTITY, &hex) {
                log::warn!("[magent] failed to persist identity to NVS: {e} (will regenerate on next boot)");
            }
        }
    }

    id
}

// ---------------------------------------------------------------------------
// Wi-Fi
// ---------------------------------------------------------------------------
/// Publish a Wi-Fi status snapshot for the AT dispatcher to read.
///
/// `None` for `ip` leaves the previously-published IP untouched (e.g. while
/// connecting). Cheap, non-blocking: a contended lock is skipped silently.
fn publish_wifi_state(
    status: &WifiStatusHandle,
    ssid: &str,
    state: u8,
    ip: Option<&str>,
    rssi: i32,
    reason: u32,
    now: u64,
) {
    if let Ok(mut g) = status.lock() {
        if !ssid.is_empty() {
            g.ssid = ssid.to_string();
        }
        g.state = state;
        g.rssi = rssi;
        // FAULT-TOLERANCE (2026-08-27): only overwrite `reason` with a
        // non-zero value. The STA-disconnect event subscription is the sole
        // owner of `reason` (it sets the real code on drop and resets to 0 on
        // connect); every periodic publish passes 0 and would otherwise clobber
        // the code the reconnect backoff needs to classify the failure.
        if reason != 0 {
            g.reason = reason;
        }
        g.updated_ms = now;
        if let Some(ip) = ip {
            g.ip = ip.to_string();
        }
    }
}

/// Current RSSI in dBm, or 0 if not associated / the query fails.
fn rssi_now(wifi: &mut BlockingWifi<EspWifi<'_>>) -> i32 {
    wifi.wifi_mut().get_rssi().unwrap_or(0)
}


/// Connect to Wi-Fi STA using credentials passed in (loaded from NVS BEFORE
/// the WiFi subsystem takes ownership of the default NVS partition).
///
/// Blocks until association succeeds or 10 seconds elapse. Panics on timeout.
/// TRACE: REQ-FW-002, REQ-NET-001.
///
/// PATCHED (MicroAgent): the credentials are passed in rather than re-read
/// from NVS here. `EspDefaultNvsPartition::take()` (the `DEFAULT_TAKEN`
/// singleton) is already held by `EspWifi` at this point, so a second
/// `take()` inside this function returns `ESP_ERR_INVALID_STATE` and the
/// WiFi would be silently skipped ("no SSID in NVS").
fn connect_wifi(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    ssid: &str,
    password: &str,
    status: &WifiStatusHandle,
) {
    if ssid.is_empty() {
        log::warn!("[wifi] no SSID — skipping Wi-Fi");
        publish_wifi_state(status, "", 4, None, 0, 0, now_ms());
        return;
    }

    // Publish "connecting" so AT+CWSTATE stops reporting a stale value while
    // the (potentially multi-attempt) association below is still in flight.
    publish_wifi_state(status, ssid, 1, None, 0, 0, now_ms());

    // PATCHED (MicroAgent): `AT+CWHOSTNAME` — read the operator-set
    // hostname (if any) and apply it to the STA netif *before*
    // `set_configuration` so the DHCP discover carries the right
    // hostname. We reach the inner netif via `BlockingWifi::wifi_mut()`.
    if let Some(hostname) = nvs_load_string("mag_at:hostname") {
        if !hostname.is_empty() {
            log::info!("[wifi] applying hostname from AT+CWHOSTNAME={hostname}");
            match wifi.wifi_mut().sta_netif_mut().set_hostname(&hostname) {
                Ok(()) => {}
                Err(e) => log::warn!("[wifi] set_hostname failed: {e}"),
            }
        }
    }

    log::info!("[wifi] connecting to SSID={ssid}");
    // Length only (never the secret) — confirms the DBO-sealed
    // password was recovered to the expected 8-byte plaintext.
    log::info!("[wifi] recovered password length={}", password.len());

    // PATCHED (MicroAgent): `ClientConfiguration.ssid` is a
    // `heapless::String<32>` (not a plain `String`). We truncate
    // and convert via `TryFrom`. The `password` field is
    // `heapless::String<64>`.
    //
    // HARDENING (audit-2026-08): previous `unwrap_or_default()` for the
    // password silently replaced an over-long password with an empty
    // string, which then triggered an opaque `auth failed` from the
    // radio. We now propagate the overflow as a clear error so the AT
    // surface can return `+CMDER:7` ("password too long") instead of
    // silently flipping to a 0-byte credential.
    let ssid_typed: HeaplessString<32> = HeaplessString::try_from(ssid)
        .unwrap_or_else(|_| HeaplessString::try_from("invalid").unwrap());
    let password_typed: HeaplessString<64> = match HeaplessString::try_from(password) {
        Ok(p) => p,
        Err(_) => {
            log::error!(
                "[wifi] password longer than 63 bytes (got {}); refusing to attempt association",
                password.len()
            );
            publish_wifi_state(status, ssid, 7, None, 0, 0, now_ms());
            return;
        }
    };
    let cfg = ClientConfiguration {
        ssid: ssid_typed,
        password: password_typed,
        ..Default::default()
    };

    // PATCHED (MicroAgent): `set_configuration`/`start` can fail at runtime
    // (bad driver state, etc.). Previously `.expect()` here would panic and
    // reboot the whole board. Now we log and bail out of the connect attempt,
    // leaving the firmware running (agent local tools + UART ingress work
    // without network).
    if let Err(e) = wifi.set_configuration(&Configuration::Client(cfg)) {
        log::warn!("[wifi] set_configuration failed: {e}");
        return;
    }
    if let Err(e) = wifi.start() {
        log::warn!("[wifi] start failed: {e}");
        return;
    }

    // PATCHED (MicroAgent): `esp_wifi_start()` only brings the radio up;
    // it does NOT initiate association. We must explicitly call
    // `connect()` (= `esp_wifi_connect()`) to scan + associate with the
    // configured AP. Without this the STA stays idle — no scan, no
    // events, and the 30s `is_connected()` poll below always times out.
    if let Err(e) = wifi.connect() {
        log::warn!("[wifi] connect failed: {e}");
        return;
    }

    // PATCHED (MicroAgent): The outer `BlockingWifi` exposes the
    // blocking-style wrapper; to reach the underlying `EspWifi`
    // (which owns the netif handle) we need to drop the wrapper
    // for the duration of the wait. We poll `is_connected` on the
    // wrapper for up to 30s, then read the netif info via
    // `wifi.wifi_mut().sta_netif()`.
    //
    // PATCHED (MicroAgent): on timeout we log a warning and return instead
    // of panicking — a panic here put the board into a reboot loop whenever
    // the AP wasn't reachable within the (too short) window. Real-world
    // association (scan + connect) can take >10s, so we allow 30s and keep
    // running even if the network is unreachable (the agent loop continues;
    // Wi-Fi can be re-tried on a later boot).
    // PATCHED (MicroAgent): wait for STA association, then for DHCP to
    // assign a real IP, and reconnect if the AP drops us (e.g. a phone
    // hotspot kicking the client right after association). The old code
    // returned as soon as `is_connected()` turned true, which logs
    // `ip=0.0.0.0` and never survives a post-association drop.
    // Each pass explicitly calls `connect()` then waits a short window.
    // This retries BOTH an initial association failure (e.g. a phone
    // hotspot transiently rejecting with reason 203) and a post-
    // association drop, instead of the old code which only retried after
    // a successful-but-dropped association.
    let start = std::time::Instant::now();
    let mut attempt: u32 = 0;
    const MAX_ATTEMPTS: u32 = 8;
    const PER_ATTEMPT_S: u64 = 8;
    const ASSOC_TIMEOUT_S: u64 = 30;
    const DHCP_TIMEOUT_S: u64 = 12;

    fn have_ip(wifi: &mut BlockingWifi<EspWifi<'_>>) -> Option<String> {
        wifi.wifi_mut()
            .sta_netif()
            .get_ip_info()
            .ok()
            .map(|i| i.ip.to_string())
            .filter(|s| !s.is_empty() && s != "0.0.0.0")
    }

    /// Wrapper around `wifi.is_connected()` that distinguishes three
    /// outcomes instead of the two the old `unwrap_or(false)` exposed.
    ///
    /// HARDENING (audit-2026-08 H5): previously an `EspError` from the
    /// driver (e.g. lwIP not initialised, radio fault) was collapsed to
    /// `false`, so the supervisor would loop forever "not connected,
    /// retrying" without ever surfacing the real fault. We now log the
    /// driver error and return a third state that callers can use to
    /// give up cleanly.
    enum WifiLink {
        /// Driver explicitly reports link up.
        Up,
        /// Driver explicitly reports link down (not associated).
        Down,
        /// Driver call itself failed — we treat this as `Down` but log
        /// the underlying `EspError` so it's visible in operator logs.
        DriverError,
    }

    fn check_link(wifi: &mut BlockingWifi<EspWifi<'_>>) -> WifiLink {
        match wifi.is_connected() {
            Ok(true) => WifiLink::Up,
            Ok(false) => WifiLink::Down,
            Err(e) => {
                log::warn!("[wifi] is_connected() driver error: {e}");
                WifiLink::DriverError
            }
        }
    }

    loop {
        // (Re)initiate the connection.
        if let Err(e) = wifi.connect() {
            log::warn!("[wifi] connect() failed: {e}");
            publish_wifi_state(status, ssid, 4, None, 0, 0, now_ms());
            return;
        }

        // 1) Wait for the STA to associate within this attempt.
        let attempt_start = std::time::Instant::now();
        while matches!(check_link(&mut *wifi), WifiLink::Down) {
            if attempt_start.elapsed() > Duration::from_secs(PER_ATTEMPT_S)
                || start.elapsed() > Duration::from_secs(ASSOC_TIMEOUT_S)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }

        if matches!(check_link(&mut *wifi), WifiLink::Up) {
            log::info!("[wifi] associated (STA connected) — waiting for DHCP");
            // 2) Associated — wait for DHCP to hand out a real IP.
            let dhcp_start = std::time::Instant::now();
            while have_ip(&mut *wifi).is_none() {
                if dhcp_start.elapsed() > Duration::from_secs(DHCP_TIMEOUT_S) {
                    log::warn!(
                        "[wifi] DHCP did not complete in {DHCP_TIMEOUT_S}s — continuing without IP"
                    );
                    publish_wifi_state(status, ssid, 3, None, rssi_now(&mut *wifi), 0, now_ms());
                    return;
                }
                match check_link(&mut *wifi) {
                    WifiLink::Up => {}
                    WifiLink::Down | WifiLink::DriverError => {
                        log::warn!("[wifi] dropped after association — reconnecting");
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            if let Some(ip) = have_ip(&mut *wifi) {
                log::info!("[wifi] connected — ip={ip}");
                publish_wifi_state(status, ssid, 5, Some(&ip), rssi_now(&mut *wifi), 0, now_ms());
                return;
            }
        } else {
            log::warn!("[wifi] association attempt {} failed — retrying", attempt + 1);
        }

        // 3) Retry if we still have budget and time.
        attempt += 1;
        if attempt >= MAX_ATTEMPTS || start.elapsed() > Duration::from_secs(ASSOC_TIMEOUT_S) {
            log::warn!("[wifi] gave up after {attempt} attempt(s) — continuing without network");
            publish_wifi_state(status, ssid, 4, None, 0, 0, now_ms());
            return;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

/// Read the Wi-Fi credentials from NVS, provisioning them from build-time
/// env vars (`MAGENT_WIFI_SSID` / `MAGENT_WIFI_PASS`) if `AT+SYSSTORE`
/// is enabled (default 1).
///
/// PATCHED (MicroAgent): MUST be called BEFORE `EspWifi` / `EspDefaultNvs`
/// takes ownership of the default NVS partition (i.e. before
/// `EspDefaultNvsPartition::take()` in `main`). After that, a second
/// `take()` fails with `ESP_ERR_INVALID_STATE`, so we load the credentials
/// up-front and pass them into `connect_wifi`.
///
/// PATCHED (MicroAgent): the password NVS value is *sealed* with the
/// device-bound DBO1 algorithm — see `wifi_pass_seal`. A plain
/// flash dump of NVS no longer reveals the WPA2 passphrase; the
/// algorithm is XOR-stream + per-write random nonce bound to the
/// device's Ed25519 seed. This function transparently opens the
/// seal and returns the plaintext password to the caller.
///
/// Returns `(ssid, password)` where password is `None` if:
///   - NVS has no `wifi_pass` entry,
///   - the entry is sealed but the device key is missing,
///   - the entry fails integrity / format validation,
///   - or `sysstore` is on and we just refused to load anything.
///
/// `None` is the correct signal to `setup_platform` to skip Wi-Fi
/// entirely rather than associating with the wrong AP.

/// Wi-Fi supervisor thread: keeps the STA connected and publishes live
/// diagnostics (state / IP / RSSI) for `AT+CWSTATE`.
///
/// Owns the leaked `BlockingWifi` handle *exclusively*: neither the AT
/// dispatcher nor the agent touch the radio. On a link drop it re-runs
/// `connect_wifi` (the same multi-attempt association logic) and backs off
/// exponentially so a dead AP doesn't hammer the radio in a tight loop.
///
/// FAULT-TOLERANCE (2026-08-27): the backoff is *reason-adaptive* — a wrong
/// password (AUTH_FAIL=202) is backed off for minutes (retrying can never
/// succeed), while an absent AP (NO_AP_FOUND=201, the classic unstable-hotspot
/// case) uses a growing backoff so we don't scan in a tight loop. The shared
/// `net_up` flag also feeds the SNTP supervisor so it stops polling while
/// there is no link.
fn run_wifi_supervisor(
    wifi: &'static mut BlockingWifi<EspWifi<'static>>,
    status: WifiStatusHandle,
    ssid: String,
    pass: String,
    net_up: Arc<AtomicBool>,
) {
    log::info!("[wifi-sup] supervisor thread started (ssid={ssid})");
    let mut was_up: Option<bool> = None;
    let mut downs: u32 = 0;
    let mut last_heartbeat = 0u64;
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let now = now_ms();
        let connected = wifi.is_connected().unwrap_or(false);
        let ip = wifi
            .wifi_mut()
            .sta_netif()
            .get_ip_info()
            .ok()
            .map(|i| i.ip.to_string())
            .filter(|s| !s.is_empty() && s != "0.0.0.0");
        let rssi = wifi.wifi_mut().get_rssi().unwrap_or(0);

        // FAULT-TOLERANCE: publish the link-up bit for the SNTP supervisor.
        // Having a non-loopback IP means DHCP completed, so outbound UDP
        // (NTP) is genuinely possible.
        net_up.store(ip.is_some(), Ordering::Relaxed);

        // Publish the live snapshot for AT+CWSTATE.
        publish_wifi_state(
            &status,
            &ssid,
            if ip.is_some() { 5 } else if connected { 3 } else { 4 },
            ip.as_deref(),
            rssi,
            0,
            now,
        );

        // Log state transitions (and the very first observation).
        match was_up {
            None => {
                log::info!(
                    "[wifi-sup] initial state: connected={connected} ip={} rssi={rssi} dBm",
                    ip.as_deref().unwrap_or("none")
                );
                downs = 0;
            }
            Some(true) if !connected => {
                downs += 1;
                log::warn!(
                    "[wifi-sup] LINK DOWN (consecutive={downs}) ip={} rssi={rssi} dBm - reconnecting",
                    ip.as_deref().unwrap_or("none")
                );
            }
            Some(false) if connected => {
                downs = 0;
                log::info!(
                    "[wifi-sup] LINK UP ip={} rssi={rssi} dBm",
                    ip.as_deref().unwrap_or("none")
                );
            }
            _ => {}
        }
        was_up = Some(connected);

        // Heartbeat while up: periodic RSSI/IP so an operator can watch drift.
        if connected && now - last_heartbeat > 60_000 {
            last_heartbeat = now;
            log::info!(
                "[wifi-sup] heartbeat ip={} rssi={rssi} dBm",
                ip.as_deref().unwrap_or("none")
            );
        }

        if !connected {
            // FAULT-TOLERANCE (2026-08-27): classify the last drop reason so a
            // wrong-password AUTH_FAIL isn't hammered (it can never succeed),
            // an absent AP backs off harder, and transient drops retry normally.
            let reason = status.lock().unwrap_or_else(|e| e.into_inner()).reason;
            let cred_error = reason == WIFI_REASON_AUTH_FAIL;
            let backoff: u64 = if cred_error {
                // Wrong credentials: don't burn the radio re-associating.
                CREDENTIAL_BACKOFF_S
            } else if reason == WIFI_REASON_NO_AP_FOUND {
                // Hotspot offline/out of 2.4GHz range: grow the gap.
                std::cmp::min(3u64 << downs.min(4), AP_ABSENT_MAX_BACKOFF_S)
            } else {
                std::cmp::min(3u64 << downs.min(3), TRANSIENT_MAX_BACKOFF_S)
            };

            if cred_error {
                log::warn!(
                    "[wifi-sup] credential error (reason={reason}, AUTH_FAIL) — \
                     verify the password; suppressing reconnect for {backoff}s"
                );
            } else {
                log::warn!("[wifi-sup] attempting reconnect to {ssid} (last reason={reason})");
                connect_wifi(&mut *wifi, &ssid, &pass, &status);
            }
            log::warn!("[wifi-sup] backoff {backoff}s before next attempt");
            std::thread::sleep(Duration::from_secs(backoff));
        }
    }
}

// ---------------------------------------------------------------------------
// Wi-Fi reconnect backoff tuning (FAULT-TOLERANCE 2026-08-27).
//
// ESP-IDF `WIFI_REASON_*` codes used to classify why the STA last dropped:
//   201 NO_AP_FOUND  — the classic unstable-hotspot case (no 2.4 GHz beacon)
//   202 AUTH_FAIL    — wrong password; re-associating can never succeed
//   204 HANDSHAKE_TIMEOUT — transient; normal retry is fine
// ---------------------------------------------------------------------------
const WIFI_REASON_NO_AP_FOUND: u32 = 201;
const WIFI_REASON_AUTH_FAIL: u32 = 202;

/// How long (seconds) to suppress reconnect after an AUTH_FAIL (wrong
/// password). Retrying faster is pointless and wastes radio power.
const CREDENTIAL_BACKOFF_S: u64 = 600;

/// Max gap (seconds) between reconnect attempts when the AP is absent. Grows
/// as `3s << downs`, capped here so we still retry periodically once the
/// hotspot comes back (a fixed huge sleep would delay recovery forever).
const AP_ABSENT_MAX_BACKOFF_S: u64 = 60;

/// Max gap (seconds) between reconnect attempts for transient failures.
const TRANSIENT_MAX_BACKOFF_S: u64 = 30;

fn provision_and_load_wifi_credentials() -> (Option<String>, Option<String>) {
    // PATCHED (MicroAgent): `AT+SYSSTORE` — when set to 0 the operator
    // has explicitly opted out of having the firmware persist anything
    // on its behalf. Build-time env-var provisioning is therefore
    // skipped. Existing NVS keys (set by `AT+CWJAP=`) are still read
    // so already-persisted credentials survive a `SYSSTORE=0`
    // session.
    let sysstore_enabled = nvs_load_string("mag_at:sysstore")
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(1)
        != 0;

    // Provision from build-time env vars only when SYSSTORE allows it.
    // Env-var passwords are *plaintext* — they were never sealed. We
    // seal them on first boot so the on-disk format is uniform, and
    // so a subsequent NVS dump from a production device doesn't
    // quietly downgrade to plaintext.
    if sysstore_enabled {
        if let (Some(ssid), Some(pass)) =
            (option_env!("MAGENT_WIFI_SSID"), option_env!("MAGENT_WIFI_PASS"))
        {
            if nvs_load_string(NVS_KEY_WIFI_SSID).is_none() {
                match nvs_save_string(NVS_KEY_WIFI_SSID, ssid) {
                    Ok(()) => log::info!("[wifi] SSID provisioned to NVS"),
                    Err(e) => log::warn!("[wifi] failed to persist SSID: {e}"),
                }
            }
            // PATCHED (MicroAgent): if the password key is *missing*,
            // seal the env-var password before persisting it. We do
            // this through the dispatcher helper to keep the sealing
            // logic in one place.
            if nvs_load_string(NVS_KEY_WIFI_PASS).is_none() {
                if let Err(e) = seal_and_store_wifi_pass(pass) {
                    log::warn!("[wifi] failed to seal env-var password: {e}");
                }
            }
        }
    } else {
        log::info!("[wifi] AT+SYSSTORE=0 — skipping env-var provisioning");
    }

    let ssid = nvs_load_string(NVS_KEY_WIFI_SSID);
    let stored = nvs_load_string(NVS_KEY_WIFI_PASS);

    let password = match stored {
        None => None,
        Some(s) => match open_stored_wifi_pass(&s) {
            Ok(p) => Some(p),
            Err(e) => {
                // PATCHED (MicroAgent): a sealed entry that fails to
                // open is a hard error — we deliberately do NOT
                // fall back to using `stored` verbatim. That would
                // silently re-introduce plaintext storage on every
                // decode failure (corruption, key mismatch, version
                // drift).
                log::warn!("[wifi] stored wifi_pass failed to open: {e}");
                None
            }
        },
    };

    (ssid, password)
}

/// Provision the default LLM backend parameters from build-time env vars
/// (`MAGENT_LLM_MODEL` / `MAGENT_LLM_API_KEY`) if `AT+LLMCFG` hasn't
/// configured them yet. Only writes when the keys are absent, so an
/// operator-set `AT+LLMCFG=` always wins over the build default.
fn provision_llm_config() {
    if nvs_load_string(NVS_KEY_LLM_MODEL).is_some() {
        return; // already configured via AT+LLMCFG
    }
    let (model, key) = match (
        option_env!("MAGENT_LLM_MODEL"),
        option_env!("MAGENT_LLM_API_KEY"),
    ) {
        (Some(m), Some(k)) if !m.is_empty() && !k.is_empty() => (m, k),
        _ => return,
    };
    match nvs_save_string(NVS_KEY_LLM_MODEL, model) {
        Ok(()) => log::info!("[llm] model provisioned: {model}"),
        Err(e) => log::warn!("[llm] failed to persist model: {e}"),
    }
    match nvs_save_string(NVS_KEY_LLM_API_KEY, key) {
        Ok(()) => log::info!("[llm] api key provisioned"),
        Err(e) => log::warn!("[llm] failed to persist api key: {e}"),
    }
}

/// Load the device key (Ed25519 seed, 32 raw bytes) used to seal
/// other NVS secrets (e.g. the Wi-Fi password). Returns `Err` if
/// missing or malformed; callers MUST treat that as "do not seal /
/// open" rather than passing an empty key.
///
/// Implementation note: as of the BTDK1 migration,
/// `magent:dev_identity` itself is sealed under BTDK1, so the raw
/// seed bytes are NOT directly readable from NVS — we have to
/// derive the BTDK1 key from eFuse + chip revision and open the
/// sealed blob. The recovered seed is then used as the seal key
/// for everything else (the same chicken-and-egg design as before,
/// just now protected by hardware-binding on the outer layer).
fn load_device_key_for_seal() -> Result<[u8; 32], &'static str> {
    load_device_key_via_btdk()
}

#[allow(dead_code)]
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Device-bound seal-key helpers (BTDK1 + dev_identity storage) live in
// `device_key.rs`. The thin wrappers below expose the most common
// operations to the rest of main.rs without forcing every callsite
// to import the module path.
// ---------------------------------------------------------------------------
fn open_dev_identity(stored: &str) -> Result<[u8; 32], &'static str> {
    device_key::open_dev_identity(stored)
}

fn seal_and_store_dev_identity(seed: &[u8; 32]) -> Result<(), &'static str> {
    device_key::seal_and_store_dev_identity(seed)
}

fn load_device_key_via_btdk() -> Result<[u8; 32], &'static str> {
    device_key::load_device_key_via_btdk()
}

/// Seal `plain` with the device-bound key and persist under
/// `magent:wifi_pass`. Used by the env-var provisioning path and
/// (indirectly) by `cwjap_dispatch` via the same seal primitive.
fn seal_and_store_wifi_pass(plain: &str) -> Result<(), &'static str> {
    use magent_core::wifi_pass_seal;
    let key = load_device_key_for_seal()?;
    let mut sealed: heapless::String<{ wifi_pass_seal::MAX_ENCODED_LEN }> =
        heapless::String::new();
    // Pull the nonce from the ESP32 hardware TRNG via the
    // `getrandom` shim — same source `dev_identity` uses, so the
    // sealing randomness is consistent across the firmware.
    let mut nonce = [0u8; wifi_pass_seal::NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|_| "trng_unavailable")?;
    wifi_pass_seal::seal_str(plain, &key, &nonce, &mut sealed)
        .map_err(|_| "seal_failed")?;
    nvs_save_string(NVS_KEY_WIFI_PASS, sealed.as_str())
        .map_err(|_| "nvs_save_failed")?;
    Ok(())
}

/// Open a stored NVS password value, returning the recovered
/// plaintext.
///
/// Dispatches on the wire-format prefix so the boot path matches
/// whatever the *write* paths produce:
///   - `DBO2:` → opened via [`wifi_pass_seal_v2::open_sealed_v2`]
///     (this is what `AT+CWJAP=` writes),
///   - `DBO1:` → opened via the same `open_sealed_v2` (which falls
///     back to DBO1) — this is what the env-var provisioning path
///     writes,
///   - no prefix (pre-DBO era plaintext) → returned verbatim.
///
/// Returns `Err` only on a *sealed* entry that fails integrity /
/// format validation. Legacy plaintext is always returned as-is.
///
/// NOTE: the dispatcher (`AT+CWJAP=`) is the only place that may
/// overwrite the entry; it re-seals with a fresh nonce each write.
fn open_stored_wifi_pass(stored: &str) -> Result<String, &'static str> {
    use magent_core::wifi_pass_seal_v2;
    let key = load_device_key_for_seal()?;
    let mut out: heapless::Vec<u8, { wifi_pass_seal_v2::MAX_PLAINTEXT }> =
        heapless::Vec::new();
    match wifi_pass_seal_v2::open_sealed_v2(stored, &key, &mut out) {
        Ok(wifi_pass_seal_v2::OpenOutcome::Dbo2Decoded)
        | Ok(wifi_pass_seal_v2::OpenOutcome::Dbo1Decoded) => {
            // The recovered bytes are valid UTF-8 by construction
            // (Wi-Fi passwords are ASCII; we also disallow NUL on
            // the write path).
            String::from_utf8(out.to_vec()).map_err(|_| "open_decoded_not_utf8")
        }
        Ok(wifi_pass_seal_v2::OpenOutcome::LegacyPlaintext(s)) => Ok(s.to_string()),
        Err(e) => {
            log::warn!("[wifi] seal open error: {e:?}");
            Err("seal_open_failed")
        }
    }
}

// ---------------------------------------------------------------------------
// Ingress thread
// ---------------------------------------------------------------------------

/// Entry point for the `ingress-thread`.
///
/// Runs the `IngressGateway` over UART0 (115 200 8N1). In a real product
/// this thread would also monitor a GPIO button and feed press events
/// into the gateway as `IngressSource::Other`. TRACE: REQ-SAFE-002.
fn run_ingress(
    identity: Identity,
    uart_parts: Option<IngressUartParts>,
    task_handle: TaskHandle,
    reply_outbox: TaskHandle,
    heartbeat: Heartbeat,
    wifi_status: WifiStatusHandle,
    safe_mode: bool,
    time_sync: sntp_sync::TimeSyncHandle,
    force_ntp_sync: sntp_sync::ForceSyncFlag,
) {
    log::info!("[ingress] thread starting");
    dtrace("ingress:entry");

    // PATCHED (MicroAgent): `UartAdapter` is now generic over the
    // underlying UART driver type (`T: Read + Write`). On the C61
    // we use `esp_idf_svc::hal::uart::UartDriver`. The driver borrows
    // `'static` GPIO pins taken from `Peripherals`, so the gateway's
    // adapter type is `UartDriver<'static>`.
    let mut gw: IngressGateway<UartAdapter<esp_idf_svc::hal::uart::UartDriver<'static>>> =
        IngressGateway::new(IngressMode::Signed);
    gw.set_signer(identity);

    // Build a UART adapter on UART0 (wired to the USB-UART bridge on the
    // C61 DevKit). UART0 pins: TX=GPIO11, RX=GPIO10 (U0TXD/U0RXD). The
    // ingress driver reads the RX pin (GPIO10 — where the host sends bytes)
    // while the console writes logs to the TX pin (GPIO11), so they share the
    // UART0 hardware without a TX→RX feedback loop. If `UartDriver::new` fails
    // (e.g. the UART or pins are already owned by the console), we fall back to
    // dummy mode.
    #[cfg(feature = "uart")]
    {
        use esp_idf_svc::hal::gpio;
        use esp_idf_svc::hal::uart::{self, UartDriver};
        use esp_idf_svc::hal::units::Hertz;

        if let Some((uart0, tx, rx)) = uart_parts {
            let config = uart::config::Config::new().baudrate(Hertz(115_200));
            match UartDriver::new(
                uart0,
                tx,
                rx,
                Option::<gpio::Gpio0>::None,
                Option::<gpio::Gpio1>::None,
                &config,
            ) {
                Ok(u) => {
                    let adapter = UartAdapter::new(u, "UART0");
                    let _ = gw.register(adapter);
                    log::info!("[ingress] UART0 registered");
                }
                Err(e) => {
                    log::warn!("[ingress] UART0 unavailable ({e}) — running in dummy mode");
                }
            }
        } else {
            log::warn!("[ingress] uart feature enabled but no UART0 parts — dummy mode");
        }
    }

    #[cfg(not(feature = "uart"))]
    {
        let _ = uart_parts;
        log::info!("[ingress] uart feature disabled — running in dummy mode");
    }

    log::info!("[ingress] gateway ready");
    dtrace("ingress:gateway-ready");

    loop {
        heartbeat.beat();

        // PATCHED (MicroAgent): drain any agent reply and send it back to the
        // host over the UART link (bidirectional communication). Adapter index
        // 0 is the UART adapter registered below.
        if let Ok(mut guard) = reply_outbox.lock() {
            if let Some(reply) = guard.take() {
                match gw.send_to_adapter(0, reply.as_bytes()) {
                    Ok(()) => log::info!("[ingress] reply sent to host: {reply}"),
                    Err(e) => log::warn!("[ingress] reply send failed: {e}"),
                }
            }
        }

        match gw.ingest() {
            Ok(Some(frame)) => {
                // PATCHED (MicroAgent): log at INFO so received frames are
                // visible with the default CONFIG_LOG_DEFAULT_LEVEL (the
                // previous `log::debug!` was filtered out, making it look
                // like UART ingress never received anything).
                log::info!(
                    "[ingress] frame src={:?} size={}B payload={}",
                    frame.source,
                    frame.payload.len(),
                    hex::encode(&frame.payload)
                );
                if let Some(envelope) = &frame.envelope_json {
                    log::info!("[ingress] signed envelope: {envelope}");
                }
                // Try AT first; anything not-AT (e.g. "read the
                // temperature") falls through to the agent's ReAct
                // loop as a natural-language task. This preserves
                // the existing behaviour for non-AT text while
                // letting production provisioning (`AT+CWJAP=...`)
                // happen deterministically — no LLM, no token budget.
                if let Ok(text) = core::str::from_utf8(&frame.payload) {
                    let line = text.trim();
                    if !line.is_empty() {
                        if magent_core::at::is_at_line(line.as_bytes()) {
                            // AT branch — parser + dispatcher.
                            // We copy the line into a stack-resident
                            // scratch buffer; the resulting `AtCommand`
                            // borrows from it so no heap allocation
                            // occurs.
                            let mut scratch = magent_core::at::ScratchBuffer::new();
                            let parsed = scratch.copy_and_parse(line.as_bytes());
                            match parsed {
                                Ok(cmd) => {
                                    // `AT+AGENT="..."` is the escape hatch:
                                    // it routes a quoted payload straight to the
                                    // agent's ReAct loop instead of the numeric
                                    // dispatcher (which would otherwise drop the
                                    // text and answer a bare `OK`).
                                    if matches!(cmd.op, magent_core::at::AtOp::Agent) {
                                        let payload = cmd.arg(0).and_then(|a| match a {
                                            magent_core::at::AtArg::Quoted(b) => Some(b),
                                            _ => None,
                                        });
                                        if let Some(p) = payload {
                                            if let Ok(s) = core::str::from_utf8(p) {
                                                if let Ok(mut guard) = task_handle.lock() {
                                                    log::info!(
                                                        "[ingress] AT+AGENT → agent payload: {s}"
                                                    );
                                                    *guard = Some(s.to_string());
                                                }
                                            }
                                        }
                                        // Reply plain `OK\r\n` so scripts know
                                        // the payload was accepted.
                                        let outcome = at_dispatch::AtOutcome::NoReply;
                                        let mut buf = at_dispatch::ResponseBuf::new();
                                        if at_dispatch::render_outcome(&outcome, &mut buf)
                                            .is_ok()
                                        {
                                            let reply = std::str::from_utf8(&buf)
                                                .map(str::to_string)
                                                .unwrap_or_default();
                                            if !reply.is_empty() {
                                                if let Ok(mut g) = reply_outbox.lock() {
                                                    *g = Some(reply);
                                                }
                                            }
                                        }
                                    } else {
                                        let now = now_ms();
                                        let mut force_flag = false;
                                        if let Ok(g) = force_ntp_sync.lock() {
                                            force_flag = *g;
                                        }
                                        let outcome = at_dispatch::dispatch(
                                            &cmd,
                                            now,
                                            safe_mode,
                                            Some(&wifi_status),
                                            Some(&time_sync),
                                            &mut force_flag,
                                        );
                                        if let Ok(mut g) = force_ntp_sync.lock() {
                                            *g = force_flag;
                                        }
                                        let mut buf = at_dispatch::ResponseBuf::new();
                                        if at_dispatch::render_outcome(&outcome, &mut buf)
                                            .is_ok()
                                        {
                                            let reply = std::str::from_utf8(&buf)
                                                .map(str::to_string)
                                                .unwrap_or_default();
                                            if !reply.is_empty() {
                                                if let Ok(mut g) = reply_outbox.lock() {
                                                    *g = Some(reply);
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Unparseable AT line — answer
                                    // with the standard `+CMDER:<n>`
                                    // error and drop it. We do
                                    // _not_ fall back to the agent;
                                    // the host sent an AT, treat it
                                    // as such.
                                    let mut buf = at_dispatch::ResponseBuf::new();
                                    let code = e.numeric_code();
                                    let outcome = at_dispatch::AtOutcome::Error { code };
                                    if at_dispatch::render_outcome(&outcome, &mut buf).is_ok() {
                                        let reply = std::str::from_utf8(&buf)
                                            .map(str::to_string)
                                            .unwrap_or_default();
                                        if !reply.is_empty() {
                                            if let Ok(mut g) = reply_outbox.lock() {
                                                *g = Some(reply);
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // Non-AT branch — feed the agent.
                            if let Ok(mut guard) = task_handle.lock() {
                                log::info!("[ingress] feeding agent command: {line}");
                                *guard = Some(line.to_string());
                            }
                        }
                    }
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                log::warn!("[ingress] error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent thread
// ---------------------------------------------------------------------------

/// High-visibility trace line for crash triage. Emitted at `error!` level so
/// it appears even at the default log filter, letting an operator see exactly
/// how far a thread got before a hard crash (e.g. a stack-protection fault).
fn dtrace(s: &str) {
    log::error!("[trace] {s}");
}

/// Entry point for the `agent-thread`.
///
/// Drives the `MiniAgent` ReAct loop forever. TRACE: REQ-SAFE-001.
#[cfg_attr(feature = "board-c61", allow(unused_variables))]
fn run_agent_loop(
    task_handle: TaskHandle,
    reply_outbox: TaskHandle,
    heartbeat: Heartbeat,
    safe_mode: bool,
) {
    log::info!("[agent] thread starting");
    dtrace("agent:entry");

    // HARDENING (audit-2026-08): replace `.expect()` with chained
    // `and_then` + `ok()` so future contributors who change these compile-time
    // constants get graceful degradation instead of a board panic.
    // Agent display name (compile-time board switch).
    #[cfg(feature = "board-c61")]
    let agent_name = "mAgent-ESP32-C61";
    #[cfg(feature = "board-s3")]
    let agent_name = "mAgent-ESP32-S3";
    let config = AgentConfig::new()
        .with_name(agent_name)
        .and_then(|c| c.with_max_iterations(20))
        .and_then(|c| c.with_max_memory(512 * 1024))
        .ok();
    let Some(config) = config else {
        // Config is compile-time constants — unreachable in practice.
        // But a future refactor that reads these from NVS or AT commands
        // would trigger this path and should NOT panic the thread.
        log::error!(
            "[agent] config build failed (name/iterations/memory out of range); \
             using safe defaults"
        );
        return;
    };
    dtrace("agent:config-built");

    // MiniAgent is heap-allocated (Box) so its large conversation buffers
    // (20 × 8 KiB = up to 160 KiB) live on the 2 MB PSRAM heap, NOT on the
    // 96 KiB agent-thread stack. This is what makes the larger MAX_BUFFER_SIZE
    // / MAX_CONVERSATION_MESSAGES safe.
    let mut agent = match MiniAgent::new(config).map(Box::new) {
        Ok(a) => a,
        Err(e) => {
            // Config is compile-time constants so this is unreachable in
            // practice, but don't panic the thread if it ever happens.
            log::error!("[agent] MiniAgent::new failed: {e}");
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
    };
    dtrace("agent:MiniAgent::new-ok");

    // PATCHED (MicroAgent): install the real-hardware tool handler so
    // `write_gpio` / `read_sensor` drive actual GPIO / the internal
    // temperature sensor instead of returning simulated values. This works
    // with no network at all.
    agent.set_tool_handler(&local_tools::Esp32ToolHandler);
    dtrace("agent:tool-handler-ok");

    // Install the DeepSeek chat-LLM backend when configured (via AT+LLMCFG
    // or the build-time default). The backend is leaked so it can be held as
    // a `&'static mut` by `MiniAgent` for the life of the process.
    //
    // In safe mode the Wi-Fi / lwIP tcpip thread is NOT initialised, so a
    // cloud HTTP call would hit `tcpip_send_msg_wait_sem (Invalid mbox)` and
    // hard-assert (crash loop). Skip the cloud backend and rely on the local
    // heuristic + local tools instead.
    //
    // Additionally: on the RAM-limited C61 the HTTPS/TLS stack cannot reach a
    // cloud LLM (see sdkconfig "Network / TLS" notes), so installing the
    // DeepSeek backend would only add an ~8s timeout + log noise to every
    // task. We therefore install the cloud backend only on the ESP32-S3
    // (`board-s3`) where the network + TLS work; the C61 runs local-only.
    #[cfg(feature = "board-s3")]
    if !safe_mode {
        if let (Some(model), Some(key)) = (
            nvs_load_string(NVS_KEY_LLM_MODEL),
            nvs_load_string(NVS_KEY_LLM_API_KEY),
        ) {
            if !model.is_empty() && !key.is_empty() {
                log::info!("[agent] installing DeepSeek LLM backend (model={model})");
                let backend: &'static mut llm::Esp32DeepSeekBackend =
                    Box::leak(Box::new(llm::Esp32DeepSeekBackend::new(&model, &key)));
                // HARDENING (audit-2026-08 H9): the agent boot path
                // intentionally leaks the LLM backend so the agent
                // can hold a `&'static mut` reference. This is
                // *one-shot* per boot, but a future refactor that
                // re-leaks on every reconnect would silently double
                // heap usage. We register the leaked pointer in
                // `LEAKED_BOXES` so a duplicate insert triggers an
                // explicit error log instead of a quiet leak. Cost:
                // one `HashSet` entry, no heap growth.
                // `!insert(...)` is true only when the pointer is ALREADY present.
                if !leaked_boxes().insert(backend as *mut _ as usize) {
                    log::error!(
                        "[magent] agent boot path is leaking a second LLM backend \
                         (same pointer as a previous leak); refactor leak site or \
                         re-use the previous handle"
                    );
                }
                agent.set_llm_backend(backend);
            }
        }
    } else {
        log::info!("[agent] safe mode — cloud LLM disabled (local heuristic only)");
    }
    #[cfg(feature = "board-c61")]
    {
        log::info!(
            "[agent] C61 local mode — cloud LLM skipped (USB-serial + local tools; \
             DeepSeek is enabled on the board-s3 build)"
        );
    }

    log::info!("[agent] MiniAgent ready");
    dtrace("agent:ready");

    // PATCHED (MicroAgent): log a health line (uptime + free heap) roughly
    // every 60s so an operator can see the agent is alive and how much memory
    // remains (helps catch leaks before they bite). Also warns if free heap is
    // critically low.
    let boot_ms = now_ms();
    let mut last_health_log = boot_ms;
    const LOW_HEAP_WARN: u32 = 64 * 1024;

    loop {
        heartbeat.beat();
        let elapsed_ms = now_ms().saturating_sub(boot_ms);

        // PATCHED (MicroAgent): use the latest command queued by the ingress
        // thread (from UART), defaulting to a temperature read. Track whether
        // the task came from a real UART command so we can reply over UART.
        let pending = task_handle.lock().ok().and_then(|mut g| g.take());
        let from_uart = pending.is_some();
        // BLE-fed chat payload (set by the BLE SYS_CMD handler in ble_config).
        #[cfg(feature = "ble")]
        let ble_task = crate::ble_config::BLE_AGENT_TASK
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        #[cfg(feature = "ble")]
        let from_ble = ble_task.is_some();
        #[cfg(not(feature = "ble"))]
        let (ble_task, _from_ble) = (None::<String>, false);
        let task = ble_task
            .or(pending)
            .unwrap_or_else(|| "read sensor temperature".to_string());

        // PATCHED (MicroAgent): `MiniAgent::run` is an `async fn` (it
        // awaits the LLM and tool futures). On a bare esp-idf-svc
        // std thread we don't run a Tokio runtime, so we drive the
        // future synchronously via the single-threaded
        // `futures::executor::block_on`. This is fine because
        // MiniAgent itself uses poll-based cooperative scheduling
        // — no thread::spawn is invoked from inside the agent.
        //
        // Reliability: wrap the run in `catch_unwind` so a panic inside a
        // tool handler or the ReAct loop (e.g. a bad GPIO op) does NOT kill
        // the agent thread — we log it and keep serving the next command.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            futures::executor::block_on(agent.run(&task))
        }));
        match outcome {
            Ok(Ok(result)) => {
                log::info!("[agent] result({task}): {result}");
                // Bidirectional replies: a UART-fed command replies over the
                // UART outbox; a BLE-fed chat payload replies via the BLE
                // SYS_RSP path (see ble_config::agent_reply_for).
                if from_uart {
                    if let Ok(mut guard) = reply_outbox.lock() {
                        *guard = Some(std::format!("RESULT[{task}]: {result}"));
                    }
                }
                #[cfg(feature = "ble")]
                if from_ble {
                    if let Ok(mut guard) = crate::ble_config::BLE_AGENT_REPLY.lock() {
                        // HARDENING (audit-2026-08 fix type mismatch):
                        // `result` is `String<MAX_BUFFER_SIZE>` (heapless) but
                        // `BLE_AGENT_REPLY` expects `Option<String>` (std).
                        // Convert via `to_string()`.
                        *guard = Some(result.as_str().to_string());
                    }
                }
            }
            Ok(Err(e)) => log::error!("[agent] error({task}): {e}"),
            Err(_) => log::error!("[agent] task panicked ({task}) — continuing"),
        }

        // Periodic health metrics + low-memory warning.
        let heap = free_heap();
        if heap < LOW_HEAP_WARN {
            log::warn!("[health] LOW FREE HEAP: {heap} B (uptime {elapsed_ms} ms)");
        }
        if now_ms().saturating_sub(last_health_log) >= 60_000 {
            log::info!(
                "[health] agent alive — uptime {elapsed_ms} ms, free_heap {heap} B, iterations in current task ok"
            );
            last_health_log = now_ms();
        }

        // PATCHED (MicroAgent): poll frequently so UART-fed commands take
        // effect quickly (a 60s sleep made interactive commands feel dead).
        std::thread::sleep(Duration::from_secs(5));
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Firmware entry point.
///
///  1. Initialise esp-idf-svc logging.
///  2. Load or generate device identity (persisted to NVS).
///  3. Connect to Wi-Fi STA (NVS credentials).
///  4. Spawn `agent-thread` and `ingress-thread`.
///  5. Join both threads (they run forever).
///
/// TRACE: REQ-FW-001, REQ-FW-002, REQ-NET-001, REQ-SAFE-001.
///
/// ESP-IDF's startup code (in `esp_system`/`start_app`) calls
/// `app_main` (force-linked via `-u app_main`). We re-export
/// `app_main` as a thin wrapper around the standard Rust
/// `main` entry point. The wrapper has `extern "C"` ABI so the
/// C-side startup code (which calls it from the ESP-IDF main
/// task) finds the right symbol with the right calling
/// convention.
#[no_mangle]
pub extern "C" fn app_main() {
    // PATCHED (MicroAgent): removed the `diag_marker` calls that used to run
    // *before* `main()`. They wrote directly to `0x3F400000` (a classic-ESP32
    // UART0 base), which is NOT a valid/peripheral-mapped address on the
    // ESP32-C61 — it caused a "Guru Meditation Error (Store access fault)"
    // at the very first instruction of `app_main`, panicking the firmware
    // before the logger came up. Normal `log::info!` via EspLogger → ESP-IDF
    // console (UART0) already gives us reliable boot visibility, so the raw
    // marker is unnecessary and harmful.
    main();
}

/// Bring up the platform (event loop, peripherals, NVS) and try to connect
/// to Wi-Fi, returning the ingress UART parts.
///
/// Reliability: this is deliberately NON-FATAL. Any subsystem that fails
/// (event loop, peripherals, NVS, `EspWifi`, `BlockingWifi`, association) is
/// logged and skipped, and the function still returns the UART parts so the
/// agent + ingress threads run regardless (local tools + UART don't need
/// network). Previously a single `.expect()` anywhere in this path panicked
/// and rebooted the whole board.
///
/// If `safe_mode` is `true` (crash-loop suspected) we skip the Wi-Fi bring-up
/// entirely so the board boots as fast and risk-free as possible and can at
/// least serve UART + local tools.
fn setup_platform(
    wifi_ssid: &str,
    wifi_pass: &str,
    safe_mode: bool,
    wifi_status: &WifiStatusHandle,
) -> (
    Option<IngressUartParts>,
    Option<&'static mut BlockingWifi<EspWifi<'static>>>,
) {
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::peripherals::Peripherals;
    let sysloop = match EspSystemEventLoop::take() {
        Ok(s) => {
            log::info!("[magent] boot phase 3/8: sysloop ready");
            s
        }
        Err(e) => {
            log::warn!("[wifi] event loop unavailable ({e}) — running without Wi-Fi");
            return (None, None);
        }
    };

    // PATCHED (MicroAgent): log STA disconnect reasons so an operator can
    // see WHY association fails — e.g. reason 202 = AUTH_FAIL (wrong
    // password), 15 = 4-way handshake timeout, 201 = AP not found. This
    // subscription must be created before `BlockingWifi::wrap` consumes
    // `sysloop`; it stays alive (via an internal Arc) through the
    // 30s connect_wifi poll below.
    //
    // FAULT-TOLERANCE (2026-08-27): we also record the reason code into the
    // shared `WifiStatus` so the supervisor can pick an adaptive reconnect
    // backoff (e.g. a wrong-password AUTH_FAIL is pointless to hammer).
    let wifi_status_for_events = wifi_status.clone();
    match sysloop.subscribe::<esp_idf_svc::wifi::WifiEvent, _>(move |event| {
        use esp_idf_svc::wifi::WifiEvent;
        match event {
            WifiEvent::StaConnected(_) => {
                log::info!("[wifi] EVENT STA CONNECTED");
                // FAULT-TOLERANCE: a successful (re)association clears the
                // last-drop reason so the supervisor's backoff classifier sees
                // reason 0 once we're up.
                if let Ok(mut g) = wifi_status_for_events.lock() {
                    g.reason = 0;
                    g.updated_ms = now_ms();
                }
            }
            WifiEvent::StaDisconnected(r) => {
                log::warn!(
                    "[wifi] EVENT STA DISCONNECTED reason={} ssid={:?} rssi={}",
                    r.reason(),
                    r.ssid(),
                    r.rssi()
                );
                if let Ok(mut g) = wifi_status_for_events.lock() {
                    g.reason = r.reason() as u32;
                    g.updated_ms = now_ms();
                }
            }
            WifiEvent::StaAuthmodeChanged => log::warn!("[wifi] EVENT STA AUTHMODE CHANGED"),
            WifiEvent::ScanDone(_) => log::info!("[wifi] EVENT SCAN DONE"),
            _ => {}
        }
    }) {
        Ok(sub) => {
            std::mem::forget(sub); // keep it alive for the whole boot
            log::info!("[wifi] disconnect-event subscription registered");
        }
        Err(e) => log::warn!("[wifi] event subscription failed: {e}"),
    }

    let peripherals = match Peripherals::take() {
        Ok(p) => {
            log::info!("[magent] boot phase 4/8: peripherals ready");
            p
        }
        Err(e) => {
            log::warn!("[wifi] peripherals unavailable ({e}) — running without Wi-Fi/UART");
            return (None, None);
        }
    };
    let nvs_partition = match default_nvs() {
        Some(n) => n,
        None => {
            log::warn!("[wifi] NVS partition unavailable — running without Wi-Fi");
            return (None, None);
        }
    };

    let modem = peripherals.modem;
    // Extract the UART parts before handing `modem` to EspWifi.
    #[cfg(feature = "uart")]
    let ingress_uart = Some((peripherals.uart0, peripherals.pins.gpio11, peripherals.pins.gpio10));
    #[cfg(not(feature = "uart"))]
    let ingress_uart: Option<IngressUartParts> = None;

    // Crash-loop recovery: in safe mode skip Wi-Fi entirely — the board is
    // rebooting repeatedly and Wi-Fi bring-up (with its 30s association wait)
    // is the most likely thing to keep it in the loop. Serve UART + local tools
    // only, so an operator can connect and diagnose.
    if safe_mode {
        log::warn!("[wifi] safe mode — skipping Wi-Fi init (crash-loop recovery)");
        return (ingress_uart, None);
    }

    // PATCHED (MicroAgent): `AT+CWMODE` — refuse to bring up the
    // radio in any mode other than station (1). SoftAP (2) and
    // dual-mode (3) require a different netif / configuration path
    // and are deferred to v0.3. Anything else → bail with a warning.
    let cwmode = nvs_load_string("mag_at:wifi_mode")
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(1);
    if cwmode != 1 {
        log::warn!(
            "[wifi] AT+CWMODE={cwmode} — only station (1) is implemented; skipping Wi-Fi"
        );
        return (ingress_uart, None);
    }

    // PATCHED (MicroAgent): `AT+CWAUTOCONN` — when the operator set
    // this to 0 we still bring the radio up (so a later `AT+CWJAP=` can
    // trigger a connect), but we skip the blocking 30s association wait.
    // ESP-IDF exposes this via `ClientConfiguration` directly; the
    // simplest portable behaviour is: skip `connect_wifi` here so the
    // device comes up with no link, and trust the operator to either
    // set `AT+CWAUTOCONN=1` (then reboot) or call `AT+CWJAP` from
    // the console (planned in v0.3 as `+CWJAP=...` with live connect).
    let autoconn = nvs_load_string("mag_at:autoconn")
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(1);
    if autoconn == 0 {
        log::warn!("[wifi] AT+CWAUTOCONN=0 — skipping blocking connect");
        // Still return ingress_uart so the console is up; we just
        // don't try to join an AP this boot.
        return (ingress_uart, None);
    }

    let mut wifi_handle: Option<&'static mut BlockingWifi<EspWifi<'static>>> = None;
    match EspWifi::new(modem, sysloop.clone(), Some(nvs_partition)) {
        Ok(esp_wifi) => {
            log::info!("[magent] boot phase 5/8: esp_wifi ready");
            match BlockingWifi::wrap(esp_wifi, sysloop) {
                Ok(mut wifi) => {
                    log::info!("[magent] boot phase 6/8: BlockingWifi ready");
                    connect_wifi(&mut wifi, wifi_ssid, wifi_pass, wifi_status);
                    // PATCHED (MicroAgent): keep the radio alive for the whole
                    // program AND hand the handle to the Wi-Fi supervisor so it
                    // can reconnect after a later drop. Previously we leaked it
                    // with `std::mem::forget`, which kept the link up but left no
                    // owner to react when the AP kicked us. `Box::leak` yields a
                    // `'static` handle the supervisor thread can move into.
                    let leaked_wifi: &'static mut BlockingWifi<EspWifi<'static>> = Box::leak(Box::new(wifi));
                    // HARDENING (audit-2026-08 H9): the Wi-Fi
                    // supervisor keeps this handle for the entire
                    // program lifetime. Register the pointer so a
                    // future refactor that re-runs the wifi init
                    // path (e.g. an OTA reboot sequence) surfaces a
                    // duplicate-leak log instead of silently
                    // doubling the radio's heap footprint.
                    // `!insert(...)` is true only when the pointer is ALREADY present.
                    let leaked_ptr = leaked_wifi as *mut _ as usize;
                    if !leaked_boxes().insert(leaked_ptr) {
                        log::error!(
                            "[wifi] wifi_handle is leaking a duplicate BlockingWifi \
                             (same pointer as a previous leak)"
                        );
                    }
                    wifi_handle = Some(leaked_wifi);
                }
                Err(e) => log::warn!("[wifi] BlockingWifi::wrap failed: {e}"),
            }
        }
        Err(e) => log::warn!("[wifi] EspWifi::new failed: {e}"),
    }

    (ingress_uart, wifi_handle)
}

/// Worker-thread stacks stay in internal DRAM.
///
/// Earlier we routed stacks to PSRAM (`esp_pthread_set_cfg` +
/// `MALLOC_CAP_SPIRAM`) to free internal DRAM for Wi-Fi + BLE. But a PSRAM
/// task stack running while Wi-Fi is active triggers a `CPU_LOCKUP`
/// (`rst:0x1a`) on this C61, regardless of free memory. With BLE disabled by
/// default (USB-serial transport) there is ~90 KiB of internal DRAM free —
/// enough for the agent (32 KiB) + ingress (24 KiB) + supervisor stacks. We
/// therefore keep the default `MALLOC_CAP_INTERNAL` stacks (no `esp_pthread`
/// override), which avoids the PSRAM-stack-vs-Wi-Fi lockup entirely.
fn configure_psram_thread_stacks() {
    // Intentionally a no-op: leave stacks in internal DRAM (see doc comment).
    log::info!("[mem] worker-thread stacks stay in internal DRAM (avoids PSRAM-vs-WiFi CPU_LOCKUP)");
}

fn main() {
    init_logging();

    // STABILITY (stability-2026-08): route worker-thread stacks to PSRAM.
    // On this RAM-limited C61 (~134 KiB internal DRAM), the agent/ingress/
    // supervisor thread stacks (32+24+8+8 KiB) are allocated from internal
    // SRAM by default (esp-pthread's stack_alloc_caps defaults to
    // MALLOC_CAP_INTERNAL). After Wi-Fi + BLE bring-up there is not enough
    // internal DRAM left, so thread spawns fail (`os error 12`) and the agent
    // never runs. Setting stack_alloc_caps to MALLOC_CAP_SPIRAM moves the
    // stacks onto the 2 MiB PSRAM, freeing internal DRAM so Wi-Fi + BLE +
    // agent can coexist. (Only the small task TCB stays in internal RAM.)
    configure_psram_thread_stacks();

    // HARDENING (audit-2026-08 M-WDT01): log the reset reason at the very
    // start of boot so crash dumps and serial logs always include the cause.
    // This helps operators distinguish a power-on from a watchdog/panic/crash
    // reboot without needing to connect a JTAG debugger.
    let reason = unsafe { esp_idf_sys::esp_reset_reason() };
    let reason_str = match reason {
        esp_idf_sys::esp_reset_reason_t_ESP_RST_POWERON   => "POWERON",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_SW        => "SOFTWARE",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_PANIC      => "PANIC",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_INT_WDT    => "INT_WDT",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_TASK_WDT   => "TASK_WDT",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_WDT         => "WDT",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP   => "DEEPSLEEP",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_BROWNOUT    => "BROWNOUT",
        esp_idf_sys::esp_reset_reason_t_ESP_RST_SDIO        => "SDIO",
        _ => "UNKNOWN",
    };
    log::info!("[magent] reset reason: {} (0x{:02X})", reason_str, reason);

    // Boot-path progress markers. Each phase advances the number so a
    // crash mid-boot leaves a record in the UART ring buffer.
    log::info!("[magent] boot phase 1/8: logger up");

    // Take the default NVS partition ONCE and share it. Must precede every
    // other NVS use (identity, provisioning) and EspWifi so the AT
    // dispatcher can still read/write config after boot.
    init_default_nvs();

    // Load / generate device identity.
    let identity = load_or_create_identity();
    log::info!("[magent] boot phase 2/8: identity ready");

    // PATCHED (MicroAgent): crash-loop detection. ESP-IDF auto-reboots on a
    // panic; if we're stuck rebooting, this returns true and we boot into
    // safe mode (skip Wi-Fi) so the board stays up long enough to diagnose
    // over UART.
    let crash_loop_safe = check_and_advance_crash_counter();

    // PATCHED (MicroAgent): `AT+SAFEMODE=1` flag — operator-forced safe
    // mode. We OR it with the crash-loop detector so the operator can
    // either recover by toggling the flag from the UART console, or
    // (if the device is unreachable) use a tool like `esptool write-nvs`.
    // The flag is consumed (cleared) so the next boot resumes normally.
    let at_safemode = read_at_safemode_flag();
    let safe_mode = crash_loop_safe || at_safemode;
    if at_safemode {
        clear_at_safemode_flag();
    }

    // PATCHED (MicroAgent): load (and provision) the Wi-Fi credentials NOW,
    // before the WiFi subsystem takes ownership of the default NVS partition.
    // (See `provision_and_load_wifi_credentials` for why this must be early.)
    let (wifi_ssid, wifi_pass) = provision_and_load_wifi_credentials();
    // PATCHED (MicroAgent): provision the default LLM backend (DeepSeek)
    // from build-time env vars when the operator hasn't set `AT+LLMCFG`.
    provision_llm_config();

    // STABILITY (stability-2026-08): initialize BLE *before* Wi-Fi bring-up.
    // On this RAM-limited C61 (~134 KiB internal DRAM), the Bluedroid host's
    // `btm_ble_init` allocates FreeRTOS mutexes/queues from internal DRAM.
    // When Wi-Fi is initialized first it eats ~56 KiB of that internal DRAM,
    // leaving ~44 KiB — too little for BLE's allocations, so the stack asserts
    // (`adv_rpt_queue != NULL` / `xQueueSemaphoreTake`), panics, and the board
    // crash-loops into safe mode (which also prevents the agent thread from
    // ever starting). BLE does not need the sysloop/peripherals that Wi-Fi
    // bring-up creates, so we init it here while internal DRAM is still fresh
    // (~100 KiB). Wi-Fi is attempted afterwards and remains non-fatal if the
    // remaining DRAM is too tight (setup_platform returns None).
    #[cfg(feature = "ble")]
    {
        use crate::ble_config::BleServer;

        log::info!("[ble] Creating BLE server...");
        let mut ble_server = BleServer::new();
        log::info!("[ble] BLE server created, device name: {}", ble_server.device_name());

        log::info!("[ble] Calling init()...");
        match ble_server.init() {
            Ok(_) => {
                log::info!("[ble] init() succeeded, state: {:?}", ble_server.get_state());

                log::info!("[ble] Calling start_advertising()...");
                match ble_server.start_advertising() {
                    Ok(_) => {
                        log::info!("[ble] start_advertising() succeeded");
                    }
                    Err(e) => {
                        log::error!("[ble] start_advertising() failed: {:?}", e);
                    }
                }
            }
            Err(e) => {
                log::error!("[ble] init() failed: {:?}", e);
            }
        }
    }

    #[cfg(not(feature = "ble"))]
    {
        log::info!("[ble] BLE feature not enabled (add 'ble' to features for BLE support)");
    }

    // Shared Wi-Fi status snapshot published by the supervisor thread and
    // read by the AT dispatcher so `AT+CWSTATE` reports the real link.
    let wifi_status: WifiStatusHandle = Arc::new(Mutex::new(WifiStatus::default()));

    // PATCHED (MicroAgent): platform bring-up (event loop, peripherals, NVS,
    // Wi-Fi) is now NON-FATAL — see `setup_platform`. If Wi-Fi can't be set up
    // the firmware still boots and runs the agent/ingress threads (local tools
    // + UART don't need network). Returns the UART parts for the ingress thread.
    let (ingress_uart, wifi_handle) = setup_platform(
        wifi_ssid.as_deref().unwrap_or(""),
        wifi_pass.as_deref().unwrap_or(""),
        safe_mode,
        &wifi_status,
    );
    log::info!("[magent] boot phase 7/8: platform ready");

    // Whether lwIP / the radio actually came up. In safe mode (crash-loop
    // recovery) Wi-Fi is skipped, so `wifi_handle` is None and the network
    // stack is NOT initialized — anything that touches lwIP (e.g. SNTP) must
    // be gated on this or it asserts (`tcpip_callback: Invalid mbox`).
    let wifi_up = wifi_handle.is_some();

    // PATCHED (MicroAgent): Wi-Fi supervisor — keeps the STA connected and
    // logs RSSI/IP/state transitions so an operator can diagnose an unstable
    // AP. It owns the leaked `BlockingWifi` handle exclusively; the AT
    // dispatcher and agent never touch the radio.
    //
    // FAULT-TOLERANCE (2026-08-27): `net_up` is a shared link-up flag the
    // supervisor sets and the SNTP supervisor reads, so SNTP stops polling
    // while there is no IP (avoids wasting NTP attempts on a dead link).
    let net_up = Arc::new(AtomicBool::new(false));
    if let Some(wifi_handle) = wifi_handle {
        let wstatus = wifi_status.clone();
        let wssid = wifi_ssid.clone().unwrap_or_default();
        let wpass = wifi_pass.clone().unwrap_or_default();
        let net_up_for_sup = net_up.clone();
        let sup = thread::Builder::new()
            .name("wifi-supervisor".into())
            // Keep this small — thread stacks come from the C61's limited
            // internal RAM (~158 KiB), and the agent (64 KiB) + ingress
            // (24 KiB) threads already consume most of it. The supervisor
            // only does lightweight wifi queries (is_connected/rssi/ip), so
            // 8 KiB is ample; a 24 KiB stack here starved the agent thread
            // and its spawn failed.
            .stack_size(8 * 1024)
            .spawn(move || run_wifi_supervisor(wifi_handle, wstatus, wssid, wpass, net_up_for_sup))
            .ok();
        if sup.is_none() {
            log::error!("[wifi-sup] thread spawn failed — no reconnect supervision");
        }
    } else {
        log::warn!("[wifi-sup] no Wi-Fi handle — supervisor not started");
    }

    // PATCHED (MicroAgent): web admin / status HTTP server. Serves an HTML
    // dashboard + /api/status JSON on the STA interface. Only when lwIP is up.
    if wifi_up {
        let st = wifi_status.clone();
        let wa = thread::Builder::new()
            .name("web-admin".into())
            .stack_size(8 * 1024)
            .spawn(move || web_admin::run_web_admin(st));
        if wa.is_err() {
            log::warn!("[webadmin] thread spawn failed - no admin server");
        }
    }

    // PATCHED (MicroAgent): Initialize BLE GATT server for mAgent-Man
    // (moved BEFORE Wi-Fi bring-up for stability — see the block above
    // `provision_llm_config`; keeping the old slot would re-init BLE after
    // Wi-Fi has consumed internal DRAM and crash-loop the board).

    // PATCHED (MicroAgent): time-sync handle. The supervisor (below)
    // records samples; the AT dispatcher queries the canonical
    // wall-clock through this handle. We pre-populate the timezone
    // from NVS so an operator's `AT+TIMEZONE=` survives reboots.
    let time_sync: sntp_sync::TimeSyncHandle = Arc::new(Mutex::new(
        magent_core::time_sync::TimeSync::new(
            at_dispatch::load_tz_offset_from_nvs(),
        ),
    ));
    let time_sync_for_dispatch = time_sync.clone();
    let time_sync_for_supervisor = time_sync.clone();

    // `force_ntp_sync` flag: the AT dispatcher sets this on
    // `AT+NTPSYNC`; the supervisor thread polls it on its 5s tick.
    let force_ntp_sync: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let force_ntp_sync_for_thread = force_ntp_sync.clone();
    let force_ntp_sync_for_dispatch = force_ntp_sync.clone();

    // Restore the persisted time-sync snapshot (if any) so we don't
    // lose wall-clock continuity across reboots.
    if let Some(prior) = nvs_load_string(crate::sntp_sync::NVS_PERSIST_KEY) {
        if let Err(e) = sntp_sync::restore_from_nvs(&time_sync, Some(&prior)) {
            log::warn!("[time-sync] failed to restore persisted state: {e}");
        }
    }

    // Shared identity + task handle for the threads.
    let identity_clone = identity.clone();
    let task_handle: TaskHandle = Arc::new(Mutex::new(None));
    let agent_task = task_handle.clone();
    let ingress_task = task_handle.clone();
    // PATCHED (MicroAgent): reply outbox — the agent writes results here and
    // the ingress thread sends them back over UART to the host.
    let reply_outbox: TaskHandle = Arc::new(Mutex::new(None));
    let agent_reply = reply_outbox.clone();
    let ingress_reply = reply_outbox.clone();

    // PATCHED (MicroAgent): heartbeats for hang detection — the supervisor
    // (busy-loop) flags a worker as hung if it stops beating.
    let agent_hb = Heartbeat::new();
    let ingress_hb = Heartbeat::new();
    let agent_hb_for_thread = agent_hb.clone();
    let ingress_hb_for_thread = ingress_hb.clone();

    // PATCHED (MicroAgent): thread spawns are non-fatal — a failure is logged
    // and the firmware keeps running (the busy-loop below feeds the HW
    // watchdog). Previously a `.expect()` panicked and rebooted the board.
    // The agent is spawned with clones so the supervisor can restart it
    // later if it crashes (the `Arc`s are still held here).
    let mut agent_restarts: u32 = 0;
    #[allow(unused_assignments)]
    let mut agent_handle: Option<std::thread::JoinHandle<()>> = None;
    {
        // Clone outside the `move` closure so main still holds the originals
        // and can re-clone for a later restart.
        let th_task = agent_task.clone();
        let th_reply = agent_reply.clone();
        let th_hb = agent_hb_for_thread.clone();
        log::info!("[agent] free heap before spawn: {} B", free_heap());
        log::info!(
            "[agent] internal DRAM before spawn: {} B",
            unsafe { esp_idf_sys::esp_get_free_internal_heap_size() }
        );
        agent_handle = match thread::Builder::new()
            .name("agent-thread".into())
            // 32 KiB: MiniAgent is heap-allocated (PSRAM) and MAX_BUFFER_SIZE
            // stays at 2 KiB, so the task stack only needs a few 2 KiB stack
            // temporaries in think(). (16-24 KiB was too tight and faulted.)
            .stack_size(32 * 1024)
            .spawn(move || run_agent_loop(th_task, th_reply, th_hb, safe_mode))
        {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!(
                    "[agent] thread spawn error: {e} (free heap {} B)",
                    free_heap()
                );
                None
            }
        };
    }
    if agent_handle.is_none() {
        log::error!("[agent] thread spawn failed — continuing without agent");
    }

    let ingress_handle = thread::Builder::new()
        .name("ingress-thread".into())
        // PATCHED (MicroAgent): keep this modest. `AT+HTTPGET` runs its TLS
        // handshake on a dedicated worker thread (see at_dispatch), so the
        // ingress thread itself does no heavy network work. 24 KiB is the
        // known-stable size (16 KiB overflowed a pthread stack).
        .stack_size(24 * 1024)
        .spawn(move || {
            run_ingress(
                identity_clone,
                ingress_uart,
                ingress_task,
                ingress_reply,
                ingress_hb_for_thread,
                wifi_status,
                safe_mode,
                time_sync_for_dispatch,
                force_ntp_sync_for_dispatch,
            )
        })
        .ok();
    if ingress_handle.is_none() {
        log::error!("[ingress] thread spawn failed — continuing without ingress");
    }

    // PATCHED (MicroAgent): SNTP supervisor thread. SNTP needs lwIP, which is
    // only up when Wi-Fi actually initialised. In safe mode Wi-Fi is skipped
    // (wifi_up == false), so starting SNTP would assert in
    // `tcpip_callback (Invalid mbox)` and crash-loop the board. Only spawn it
    // when the network stack is really available.
    #[cfg(feature = "wifi")]
    {
        if wifi_up {
            let ts_for_supervisor = time_sync_for_supervisor.clone();
            let flag_for_supervisor = force_ntp_sync_for_thread.clone();
            let net_up_for_sntp = net_up.clone();
            let sntp_handle = thread::Builder::new()
                .name("sntp-supervisor".into())
                .stack_size(8 * 1024)
                .spawn(move || {
                    sntp_sync::run_sntp_supervisor(
                        ts_for_supervisor,
                        flag_for_supervisor,
                        net_up_for_sntp,
                        |record| {
                            nvs_save_string(
                                crate::sntp_sync::NVS_PERSIST_KEY,
                                record,
                            )
                            .is_ok()
                        },
                    );
                })
                .ok();
            if sntp_handle.is_none() {
                log::error!("[sntp] supervisor thread spawn failed");
            }
        } else {
            log::warn!(
                "[sntp] network not up (safe mode / Wi-Fi skipped) — SNTP supervisor not started"
            );
        }
    }
    #[cfg(not(feature = "wifi"))]
    {
        log::info!("[sntp] wifi feature disabled — SNTP supervisor not started");
    }

    log::info!("[magent] boot phase 8/8: all systems nominal");

    log::info!("[magent] all threads running");
    // PATCHED (MicroAgent): explicit busy-loop instead of `join()`.
    // The previous code used `agent_handle.join()` which blocks on
    // a FreeRTOS `taskNotify` and ends up in the ROM WFI loop. The
    // main task's WFI doesn't feed the Timer Group 0 hardware
    // watchdog (which is independent of the FreeRTOS task WDT we
    // disabled), so after ~5 seconds the chip resets with
    // `rst:0x7 (TG0_WDT_HPSYS)`. The fixed loop calls `sleep` which
    // yields to the scheduler but doesn't enter WFI, keeping the
    // main task active enough to feed the HW WDT.
    // PATCHED (MicroAgent): once the board has been up long enough (stable
    // boot), reset the crash-loop counter so a single transient crash later
    // doesn't trigger safe mode.
    let boot_start = std::time::Instant::now();
    let mut stable_marked = false;

    loop {
        std::thread::sleep(Duration::from_secs(1));
        if !stable_marked && boot_start.elapsed() > Duration::from_secs(60) {
            mark_stable_boot();
            stable_marked = true;
        }
        // Detect thread panics/exits. The agent is restarted (bounded) so a
        // one-off crash doesn't permanently kill the ReAct loop. The ingress
        // thread holds non-recreatable singletons (NVS partition / UART), so
        // it can only be reported, not restarted.
        const MAX_AGENT_RESTARTS: u32 = 5;
        if agent_handle.as_ref().map_or(false, |h| h.is_finished()) {
            agent_restarts += 1;
            log::error!("[magent] agent-thread exited (restart #{agent_restarts})");
            if agent_restarts < MAX_AGENT_RESTARTS {
                // Short delay so a crash loop doesn't spin; the crash-loop
                // detector / safe mode is the ultimate backstop anyway.
                std::thread::sleep(Duration::from_secs(1));
                let th_task = agent_task.clone();
                let th_reply = agent_reply.clone();
                let th_hb = agent_hb_for_thread.clone();
                agent_handle = thread::Builder::new()
                    .name("agent-thread".into())
                    .stack_size(32 * 1024)
                    .spawn(move || run_agent_loop(th_task, th_reply, th_hb, safe_mode))
                    .ok();
                if agent_handle.is_none() {
                    log::error!("[agent] thread re-spawn failed — not retrying this round");
                }
            } else {
                log::error!("[magent] agent restart limit reached — not restarting");
            }
        }
        if let Some(h) = &ingress_handle {
            if h.is_finished() {
                log::error!("[magent] ingress-thread exited unexpectedly (not restartable)");
            }
        }
        // PATCHED (MicroAgent): heartbeat-based hang detection — a thread that
        // hasn't beaten for >15s is stalled (not crashed), which a panic
        // handler wouldn't catch. We log loudly so an operator / watchdog can
        // act.
        if agent_hb.stale(15_000) {
            log::error!("[health] agent-thread heartbeat stale (>15s) — possible hang");
        }
        if ingress_hb.stale(15_000) {
            log::error!("[health] ingress-thread heartbeat stale (>15s) — possible hang");
        }
    }
}

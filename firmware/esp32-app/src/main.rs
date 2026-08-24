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
mod at_dispatch;
mod device_key;
mod llm;

use core::convert::TryFrom;
use core::time::Duration;
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
    log::info!("[magent] v{VERSION} booting (esp-idf-svc 0.52 / ESP32-C61 std)");
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

/// Take the default NVS partition exactly once and keep it for the life
/// of the program. Must be called before EspWifi (or anything else) takes
/// ownership. Callers obtain clones via [`default_nvs`].
pub(crate) fn init_default_nvs() {
    match EspDefaultNvsPartition::take() {
        Ok(p) => {
            let leaked: &'static EspDefaultNvsPartition = Box::leak(Box::new(p));
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
// Device identity
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
            log::error!(
                "[magent] TRNG could not provide a valid identity seed after 8 attempts"
            );
            // Last resort: a panic triggers the watchdog reboot, and the
            // crash counter will eventually move the board into safe mode
            // rather than looping forever.
            panic!("hardware TRNG is required on this platform");
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
fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'_>>, ssid: &str, password: &str) {
    if ssid.is_empty() {
        log::warn!("[wifi] no SSID — skipping Wi-Fi");
        return;
    }

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
    let cfg = ClientConfiguration {
        ssid: HeaplessString::<32>::try_from(ssid)
            .unwrap_or_else(|_| HeaplessString::try_from("invalid").unwrap()),
        password: HeaplessString::<64>::try_from(password)
            .unwrap_or_default(),
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

    loop {
        // (Re)initiate the connection.
        if let Err(e) = wifi.connect() {
            log::warn!("[wifi] connect() failed: {e}");
            return;
        }

        // 1) Wait for the STA to associate within this attempt.
        let attempt_start = std::time::Instant::now();
        while !wifi.is_connected().unwrap_or(false) {
            if attempt_start.elapsed() > Duration::from_secs(PER_ATTEMPT_S)
                || start.elapsed() > Duration::from_secs(ASSOC_TIMEOUT_S)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }

        if wifi.is_connected().unwrap_or(false) {
            log::info!("[wifi] associated (STA connected) — waiting for DHCP");
            // 2) Associated — wait for DHCP to hand out a real IP.
            let dhcp_start = std::time::Instant::now();
            while have_ip(&mut *wifi).is_none() {
                if dhcp_start.elapsed() > Duration::from_secs(DHCP_TIMEOUT_S) {
                    log::warn!(
                        "[wifi] DHCP did not complete in {DHCP_TIMEOUT_S}s — continuing without IP"
                    );
                    return;
                }
                if !wifi.is_connected().unwrap_or(false) {
                    log::warn!("[wifi] dropped after association — reconnecting");
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            if let Some(ip) = have_ip(&mut *wifi) {
                log::info!("[wifi] connected — ip={ip}");
                return;
            }
        } else {
            log::warn!("[wifi] association attempt {} failed — retrying", attempt + 1);
        }

        // 3) Retry if we still have budget and time.
        attempt += 1;
        if attempt >= MAX_ATTEMPTS || start.elapsed() > Duration::from_secs(ASSOC_TIMEOUT_S) {
            log::warn!("[wifi] gave up after {attempt} attempt(s) — continuing without network");
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
    safe_mode: bool,
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
                                        let outcome =
                                            at_dispatch::dispatch(&cmd, now, safe_mode);
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
fn run_agent_loop(task_handle: TaskHandle, reply_outbox: TaskHandle, heartbeat: Heartbeat) {
    log::info!("[agent] thread starting");
    dtrace("agent:entry");

    let config = AgentConfig::new()
        .with_name("mAgent-ESP32-C61")
        .expect("agent name fits")
        .with_max_iterations(20)
        .expect("iterations in range")
        .with_max_memory(256 * 1024) // 256 KiB — max allowed; PSRAM now enabled (2 MB heap)
        .expect("memory budget in range");
    dtrace("agent:config-built");

    let mut agent = match MiniAgent::new(config) {
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
    if let (Some(model), Some(key)) = (
        nvs_load_string(NVS_KEY_LLM_MODEL),
        nvs_load_string(NVS_KEY_LLM_API_KEY),
    ) {
        if !model.is_empty() && !key.is_empty() {
            log::info!("[agent] installing DeepSeek LLM backend (model={model})");
            let backend: &'static mut llm::Esp32DeepSeekBackend =
                Box::leak(Box::new(llm::Esp32DeepSeekBackend::new(&model, &key)));
            agent.set_llm_backend(backend);
        }
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
        let from_command = pending.is_some();
        let task = pending.unwrap_or_else(|| "read sensor temperature".to_string());

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
                // Bidirectional: if this task came from a UART command, put
                // the result into the reply outbox; the ingress thread sends
                // it back over the UART link to the host.
                if from_command {
                    if let Ok(mut guard) = reply_outbox.lock() {
                        *guard = Some(std::format!("RESULT[{task}]: {result}"));
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
fn setup_platform(wifi_ssid: &str, wifi_pass: &str, safe_mode: bool) -> Option<IngressUartParts> {
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::peripherals::Peripherals;
    let sysloop = match EspSystemEventLoop::take() {
        Ok(s) => {
            log::info!("[magent] boot phase 3/8: sysloop ready");
            s
        }
        Err(e) => {
            log::warn!("[wifi] event loop unavailable ({e}) — running without Wi-Fi");
            return None;
        }
    };

    // PATCHED (MicroAgent): log STA disconnect reasons so an operator can
    // see WHY association fails — e.g. reason 202 = AUTH_FAIL (wrong
    // password), 15 = 4-way handshake timeout, 201 = AP not found. This
    // subscription must be created before `BlockingWifi::wrap` consumes
    // `sysloop`; it stays alive (via an internal Arc) through the
    // 30s connect_wifi poll below.
    match sysloop.subscribe::<esp_idf_svc::wifi::WifiEvent, _>(|event| {
        use esp_idf_svc::wifi::WifiEvent;
        match event {
            WifiEvent::StaConnected(_) => log::info!("[wifi] EVENT STA CONNECTED"),
            WifiEvent::StaDisconnected(r) => {
                log::warn!(
                    "[wifi] EVENT STA DISCONNECTED reason={} ssid={:?} rssi={}",
                    r.reason(),
                    r.ssid(),
                    r.rssi()
                );
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
            return None;
        }
    };
    let nvs_partition = match default_nvs() {
        Some(n) => n,
        None => {
            log::warn!("[wifi] NVS partition unavailable — running without Wi-Fi");
            return None;
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
        return ingress_uart;
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
        return ingress_uart;
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
        return ingress_uart;
    }

    match EspWifi::new(modem, sysloop.clone(), Some(nvs_partition)) {
        Ok(esp_wifi) => {
            log::info!("[magent] boot phase 5/8: esp_wifi ready");
            match BlockingWifi::wrap(esp_wifi, sysloop) {
                Ok(mut wifi) => {
                    log::info!("[magent] boot phase 6/8: BlockingWifi ready");
                    connect_wifi(&mut wifi, wifi_ssid, wifi_pass);
                    // PATCHED (MicroAgent): keep the radio alive for the
                    // whole program. `wifi` is a local; if it drops when
                    // setup_platform returns, esp-idf-svc deinitialises the
                    // STA and the device disconnects immediately after
                    // getting an IP (seen as WIFI_REASON_ASSOC_LEAVE).
                    // Leaking the handle keeps the link up so the agent can
                    // use the network (Ollama, MQTT, …) after boot.
                    std::mem::forget(wifi);
                }
                Err(e) => log::warn!("[wifi] BlockingWifi::wrap failed: {e}"),
            }
        }
        Err(e) => log::warn!("[wifi] EspWifi::new failed: {e}"),
    }

    ingress_uart
}

fn main() {
    init_logging();
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

    // PATCHED (MicroAgent): platform bring-up (event loop, peripherals, NVS,
    // Wi-Fi) is now NON-FATAL — see `setup_platform`. If Wi-Fi can't be set up
    // the firmware still boots and runs the agent/ingress threads (local tools
    // + UART don't need network). Returns the UART parts for the ingress thread.
    let ingress_uart = setup_platform(
        wifi_ssid.as_deref().unwrap_or(""),
        wifi_pass.as_deref().unwrap_or(""),
        safe_mode,
    );
    log::info!("[magent] boot phase 7/8: platform ready");

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
    let mut agent_handle: Option<std::thread::JoinHandle<()>> = None;
    {
        // Clone outside the `move` closure so main still holds the originals
        // and can re-clone for a later restart.
        let th_task = agent_task.clone();
        let th_reply = agent_reply.clone();
        let th_hb = agent_hb_for_thread.clone();
        agent_handle = thread::Builder::new()
            .name("agent-thread".into())
            .stack_size(64 * 1024)
            .spawn(move || run_agent_loop(th_task, th_reply, th_hb))
            .ok();
    }
    if agent_handle.is_none() {
        log::error!("[agent] thread spawn failed — continuing without agent");
    }

    let ingress_handle = thread::Builder::new()
        .name("ingress-thread".into())
        // PATCHED (MicroAgent): keep this modest. `AT+HTTPGET` runs its TLS
        // handshake on a dedicated worker thread (see at_dispatch), so the
        // ingress thread itself does no heavy network work. A 64 KiB stack
        // here made `pthread` fail to create the task on the C61's limited
        // internal RAM.
        .stack_size(24 * 1024)
        .spawn(move || {
            run_ingress(identity_clone, ingress_uart, ingress_task, ingress_reply, ingress_hb_for_thread, safe_mode)
        })
        .ok();
    if ingress_handle.is_none() {
        log::error!("[ingress] thread spawn failed — continuing without ingress");
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
                    .stack_size(64 * 1024)
                    .spawn(move || run_agent_loop(th_task, th_reply, th_hb))
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

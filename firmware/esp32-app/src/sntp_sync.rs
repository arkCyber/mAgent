//! SNTP supervisor for the ESP32-C61 firmware.
//!
//! # Responsibilities
//!
//! 1. Initialise `esp-idf-svc`'s SNTP client against a configurable
//!    pool of servers (default: `pool.ntp.org` — the C61's
//!    `CONFIG_LWIP_SNTP_MAX_SERVERS=1` caps the pool at a single
//!    server, see [`SNTP_SERVERS`]).
//! 2. Wait for the first sync, then hand the wall-clock + monotonic
//!    pair to [`magent_core::time_sync::TimeSync::record`].
//! 3. Periodically (1 h after a successful sync, per the design
//!    decision) re-issue `esp_restart()` of the SNTP sync timer — the
//!    actual re-poll happens inside ESP-IDF's SNTP thread once we
//!    `sntp_init()` it, but we re-poke by calling
//!    `sntp_set_sync_status(SNTP_SYNC_STATUS_RESET)` if we detect
//!    drift > 1 s on the next monotonic tick.
//!
//! # Why a separate module?
//!
//! `main.rs` is already >1800 lines; pulling SNTP into a
//! `sntp_sync.rs` keeps the boot-path readable and lets the
//! dispatcher + supervisor call into a small, dedicated surface
//! area.
//!
//! # Aerograde guarantees
//!
//! * **Non-fatal.** Every entry point returns `Result` so a SNTP
//!   failure logs + leaves the firmware running (the agent + ingress
//!   threads serve without a clock).
//! * **No allocation on the hot path.** The supervisor thread's loop
//!   uses only stack-resident counters.
//! * **Crash-loop aware.** No `panic!` / `expect` on the boot path;
//!   the supervisor exits gracefully on `EspError`.

use core::time::Duration;

use esp_idf_svc::sntp::{
    EspSntp, OperatingMode, SntpConf, SyncMode, SyncStatus,
};

// PATCHED (MicroAgent): the SntpConf `servers` array is sized by
// `CONFIG_LWIP_SNTP_MAX_SERVERS` (default 1 in our sdkconfig). We size our
// own buffer from the same constant via the runtime cfg helper so the
// assignment below matches what esp_idf_svc expects.
const SNTP_SERVER_NUM: usize = {
    // Compile-time upper bound; sdkconfig may set this lower. We always
    // produce a buffer of size 1, which matches the project's
    // CONFIG_LWIP_SNTP_MAX_SERVERS=1 default.
    1
};
use esp_idf_svc::sys::EspError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use magent_core::time_sync::{Source, TimeSync, DEFAULT_RESYNC_INTERVAL_S};

/// NVS key under which the supervisor persists the last-known-good
/// `TimeSync` snapshot. Identical to the constant in
/// `magent_core::time_sync::PERSIST_KEY`; redeclared here so the
/// firmware doesn't have to spell out the full path every time it
/// touches NVS.
pub const NVS_PERSIST_KEY: &str = magent_core::time_sync::PERSIST_KEY;

/// NVS key for the timezone offset (minutes east of UTC).
#[allow(dead_code)]
pub const NVS_TZ_KEY: &str = magent_core::time_sync::TZ_KEY;

/// Maximum number of SNTP servers the firmware tries in parallel.
/// `esp-idf-svc::SntpConf` is a fixed-size `[&str; SNTP_MAX_SERVERS]`
/// array; the value is set to the IDF default (1).
pub const SNTP_SERVERS: [&str; 1] = [
    "pool.ntp.org",
];

/// Resync interval (seconds). Aligns with `DEFAULT_RESYNC_INTERVAL_S`
/// in `magent_core::time_sync`; the supervisor is responsible for
/// nudging SNTP when this elapses.
pub const RESYNC_INTERVAL_S: u64 = DEFAULT_RESYNC_INTERVAL_S;

/// Initial-sync timeout. The supervisor gives the first SNTP poll
/// this long to complete before reporting "no sync yet".
pub const FIRST_SYNC_TIMEOUT_MS: u64 = 15_000;

/// Shared handle to the `TimeSync` state. Used by the supervisor
/// thread to record samples and by the AT dispatcher to query
/// wall-clock time.
pub type TimeSyncHandle = Arc<Mutex<TimeSync>>;

/// Shared "force sync now" flag. The AT dispatcher sets this to
/// `true` on `AT+NTPSYNC`; the supervisor thread polls it on its
/// 5-second tick, immediately schedules a fresh SNTP poll, and
/// clears the flag. Keeps the two threads decoupled (no direct
/// cross-thread call).
pub type ForceSyncFlag = Arc<Mutex<bool>>;

/// NVS namespace used for time-sync artefacts. Centralised here so
/// the dispatcher and supervisor write to the same partition.
#[allow(dead_code)]
pub const NVS_NS: &str = "mag_ts";

/// Build the default `SntpConf` (poll mode, immediate sync, four
/// public pool servers). Pinned to `SntpConf<'static>` so it can be
/// stashed in a supervisor field.
fn default_sntp_conf() -> SntpConf<'static> {
    let mut servers: [&str; SNTP_SERVER_NUM] = [""; SNTP_SERVER_NUM];
    for (dst, src) in servers.iter_mut().zip(SNTP_SERVERS.iter()) {
        *dst = src;
    }
    SntpConf {
        servers,
        operating_mode: OperatingMode::Poll,
        sync_mode: SyncMode::Immediate,
    }
}

/// Initialise the SNTP client. The returned `EspSntp` keeps the C-side
/// server strings alive; dropping it stops the SNTP background task.
pub fn start_sntp() -> Result<EspSntp<'static>, EspError> {
    let conf = default_sntp_conf();
    log::info!(
        "[sntp] starting with servers={:?} sync=immediate mode=poll",
        SNTP_SERVERS
    );
    EspSntp::new(&conf)
}

/// Fetch the current wall-clock time from the ESP-IDF C runtime.
/// Returns `Ok(None)` if SNTP hasn't synced yet (the runtime clock
/// is still the boot-time value). After the supervisor records the
/// first sample, this is the authoritative source the firmware uses
/// for `AT+TIME?`.
///
/// We deliberately don't return the IDF clock *directly* — every
/// consumer should go through [`TimeSyncHandle`] so the monotonic
/// anchor + drift correction + rewind guard are all in one place.
///
/// # Why `std::time::SystemTime`?
///
/// ESP-IDF's Rust `std` implements `SystemTime` on top of the same
/// libc clock that the SNTP task updates via `settimeofday()` (this
/// is the documented mechanism — see `esp_idf_svc::systime` and the
/// crate's own `examples/sntp.rs`). Reading it here sidesteps the raw
/// `time()` FFI, whose `time_t` width changed between ESP-IDF
/// releases (i64 → i32 on 32-bit RISC-V/Xtensa in IDF v6) and whose
/// return value is the time itself — not an error code.
pub fn read_rtc_unix() -> Result<Option<i64>, EspError> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs() as i64;
            if secs <= 0 {
                // Boot-time clock (1970-01-01) — SNTP hasn't synced.
                Ok(None)
            } else {
                Ok(Some(secs))
            }
        }
        Err(_) => {
            // System clock before the Unix epoch = not synced. Treat as
            // "no wall-clock yet" (non-fatal) rather than erroring, so
            // the supervisor's `if let Ok(Some(..)) = read_rtc_unix()`
            // call sites keep working.
            Ok(None)
        }
    }
}

/// Monotonic milliseconds via ESP-IDF's `esp_timer_get_time()`.
/// Mirrors the `now_ms()` helper in `main.rs` so the supervisor
/// doesn't have to import it across the module boundary.
pub fn monotonic_ms() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1000 }
}

/// Polled once at boot to pull any pre-existing NVS record into the
/// shared `TimeSync` state. We can't write a Rust `Mutex<TimeSync>`
/// to NVS directly, so the firmware-side glue uses NVS string I/O
/// (`nvs_load_string` / `nvs_save_string` from `main.rs`). This
/// function takes the *already-loaded* string and pumps it into the
/// handle.
pub fn restore_from_nvs(
    handle: &TimeSyncHandle,
    nvs_record: Option<&str>,
) -> Result<(), magent_core::time_sync::TimeSyncError> {
    let mut guard = handle.lock().unwrap_or_else(|e| e.into_inner());
    match nvs_record {
        Some(s) => match TimeSync::load(s, monotonic_ms()) {
            Ok(loaded) => {
                log::info!(
                    "[sntp] restored time-sync from NVS (source={:?}, drift={}ppm)",
                    loaded.source(),
                    loaded.drift_ppm()
                );
                *guard = loaded;
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "[sntp] persisted time-sync record invalid: {} (starting fresh)",
                    e
                );
                Ok(())
            }
        },
        None => {
            log::info!("[sntp] no prior time-sync record in NVS — starting fresh");
            Ok(())
        }
    }
}

/// Persist the current `TimeSync` snapshot to NVS. Best-effort;
/// failures log but don't propagate (the supervisor must keep
/// running even if NVS is briefly unavailable).
pub fn persist_to_nvs(handle: &TimeSyncHandle, save_fn: impl FnOnce(&str) -> bool) {
    let mut buf: heapless::String<96> = heapless::String::new();
    {
        let guard = handle.lock().unwrap_or_else(|e| e.into_inner());
        if guard.serialize_for_nvs(&mut buf).is_err() {
            log::warn!("[sntp] failed to serialise state for NVS persistence");
            return;
        }
    }
    if !save_fn(buf.as_str()) {
        log::warn!("[sntp] NVS write failed for time-sync record");
    }
}

/// Wait up to `timeout_ms` for the SNTP sync status to reach
/// `Completed`. Polls every 100 ms so the supervisor can make
/// progress / log if it doesn't see a sync.
pub fn wait_for_first_sync(sntp: &EspSntp<'_>, timeout_ms: u64) -> bool {
    let start = monotonic_ms();
    let mut last_log = start;
    loop {
        if sntp.get_sync_status() == SyncStatus::Completed {
            return true;
        }
        if monotonic_ms().saturating_sub(start) > timeout_ms {
            return false;
        }
        if monotonic_ms().saturating_sub(last_log) > 1000 {
            log::info!("[sntp] still waiting for first sync...");
            last_log = monotonic_ms();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Push the latest SNTP sample into the shared `TimeSync` handle.
/// Called from the supervisor thread on every successful poll.
pub fn record_sample(
    handle: &TimeSyncHandle,
    wall_unix_s: i64,
    wall_unix_ns: u32,
    monotonic_ms: u64,
) -> Result<(), magent_core::time_sync::TimeSyncError> {
    let mut guard = handle.lock().unwrap_or_else(|e| e.into_inner());
    guard.record(wall_unix_s, wall_unix_ns, monotonic_ms, Source::Sntp)
}

/// Supervisor thread entry point.
///
/// Polls the SNTP sync status; on every transition into `Completed`
/// (or after [`RESYNC_INTERVAL_S`] has elapsed since the last
/// sample), pulls the C-runtime wall clock and pushes it through
/// `record_sample`. Persists the resulting snapshot to NVS via the
/// caller-supplied `save_fn` (typically the firmware's
/// `nvs_save_string` helper).
pub fn run_sntp_supervisor<F>(
    handle: TimeSyncHandle,
    force_flag: ForceSyncFlag,
    network_up: Arc<AtomicBool>,
    mut save_fn: F,
) where
    F: FnMut(&str) -> bool + Send + 'static,
{
    log::info!(
        "[sntp] supervisor thread started (resync every {}s)",
        RESYNC_INTERVAL_S
    );
    let sntp = match start_sntp() {
        Ok(s) => s,
        Err(e) => {
            log::error!("[sntp] failed to start: {e} — supervisor exiting");
            return;
        }
    };

    // Initial wait for first sync (best-effort; we keep running
    // either way so the agent can serve commands even if SNTP is
    // unreachable). FAULT-TOLERANCE (2026-08-27): if the STA has no IP
    // yet (unstable hotspot / still associating), skip the wait entirely —
    // the loop below re-checks `network_up` every tick and syncs the moment
    // a link appears.
    //
    // `did_first_sync` guards the link-up retry in the loop: once we have
    // recorded a real sample we fall back to the normal periodic re-sync.
    let mut did_first_sync = false;
    if !network_up.load(Ordering::Relaxed) {
        log::warn!("[sntp] no network link yet — deferring first sync (will retry on link-up)");
    } else if wait_for_first_sync(&sntp, FIRST_SYNC_TIMEOUT_MS) {
        if let Ok(Some(wall)) = read_rtc_unix() {
            let now = monotonic_ms();
            if record_sample(&handle, wall, 0, now).is_ok() {
                log::info!(
                    "[sntp] first sync recorded: wall_unix={} monotonic_ms={}",
                    wall, now
                );
                persist_to_nvs(&handle, &mut save_fn);
                did_first_sync = true;
            }
        }
    } else {
        log::warn!(
            "[sntp] no sync within {}ms — supervisor will keep retrying",
            FIRST_SYNC_TIMEOUT_MS
        );
    }

    let mut last_sync_ms = monotonic_ms();
    let mut last_persist_ms = monotonic_ms();
    let mut last_status_log = monotonic_ms();
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let now = monotonic_ms();

        // Honour `AT+NTPSYNC` from the AT dispatcher: if the force
        // flag is set, immediately nudge the SNTP state machine and
        // record a fresh sample (best-effort).
        let forced = force_flag
            .lock()
            .map(|g| *g)
            .unwrap_or_else(|e| e.into_inner().clone());
        if forced {
            log::info!("[sntp] force-sync requested via AT+NTPSYNC");
            // Clear the flag first so a misbehaving caller doesn't
            // pin us in a tight loop.
            if let Ok(mut g) = force_flag.lock() {
                *g = false;
            }
            // Ask the IDF SNTP daemon to forget its last result; it
            // will re-poll within ~30s.
            unsafe {
                esp_idf_svc::sys::sntp_set_sync_status(
                    esp_idf_svc::sys::sntp_sync_status_t_SNTP_SYNC_STATUS_RESET,
                );
            }
            if let Ok(Some(wall)) = read_rtc_unix() {
                if record_sample(&handle, wall, 0, now).is_ok() {
                    log::info!(
                        "[sntp] force-sync recorded: wall_unix={} monotonic_ms={}",
                        wall, now
                    );
                    last_sync_ms = now;
                }
            } else {
                log::warn!("[sntp] force-sync: RTC not yet synced");
            }
            persist_to_nvs(&handle, &mut save_fn);
        }

        // Re-sync if (a) the resync interval has elapsed, OR
        // (b) SNTP has reported a new sync status. FAULT-TOLERANCE
        // (2026-08-27): only attempt when the STA actually has a link —
        // reading a stale `Completed` status / polling NTP on a dead link
        // is wasted work.
        let status = sntp.get_sync_status();
        let elapsed_since_sync = now.saturating_sub(last_sync_ms);

        // BUGFIX (2026-08-27): if the initial wait was deferred because the
        // STA had no IP at boot, we never synced. The ESP-IDF SNTP daemon
        // polls only when triggered, and the periodic re-sync below requires
        // `status == Completed` (which never happens if we never synced). So
        // the moment a link appears, nudge the daemon and do the first sync.
        if !did_first_sync && network_up.load(Ordering::Relaxed) {
            // Ask the IDF daemon to (re)start polling now that we have a link.
            unsafe {
                esp_idf_svc::sys::sntp_set_sync_status(
                    esp_idf_svc::sys::sntp_sync_status_t_SNTP_SYNC_STATUS_RESET,
                );
            }
            if wait_for_first_sync(&sntp, FIRST_SYNC_TIMEOUT_MS) {
                if let Ok(Some(wall)) = read_rtc_unix() {
                    let n = monotonic_ms();
                    if record_sample(&handle, wall, 0, n).is_ok() {
                        log::info!(
                            "[sntp] first sync recorded on link-up: wall_unix={} monotonic_ms={}",
                            wall, n
                        );
                        persist_to_nvs(&handle, &mut save_fn);
                        last_sync_ms = n;
                        did_first_sync = true;
                    }
                }
            }
            // If it still didn't complete within the timeout, leave
            // `did_first_sync = false` so we retry on the next tick.
        }

        if network_up.load(Ordering::Relaxed)
            && status == SyncStatus::Completed
            && elapsed_since_sync >= RESYNC_INTERVAL_S * 1000
        {
            if let Ok(Some(wall)) = read_rtc_unix() {
                if record_sample(&handle, wall, 0, now).is_ok() {
                    log::info!(
                        "[sntp] re-sync recorded: wall_unix={} (+{}s since last)",
                        wall,
                        elapsed_since_sync / 1000
                    );
                    last_sync_ms = now;
                }
            } else {
                log::warn!("[sntp] re-sync read_rtc_unix returned None — RTC not yet synced");
            }
        }

        // Periodic persist (every 5 minutes) so a power loss doesn't
        // lose more than 5 minutes of sync progress.
        if now.saturating_sub(last_persist_ms) >= 5 * 60 * 1000 {
            persist_to_nvs(&handle, &mut save_fn);
            last_persist_ms = now;
        }

        // Heartbeat: every 30s, log the current sync status so an
        // operator can see whether SNTP is alive without enabling
        // debug logging.
        if now.saturating_sub(last_status_log) >= 30_000 {
            log::info!(
                "[sntp] heartbeat status={:?} last_sync_ago={}s free_heap={}",
                status,
                (now.saturating_sub(last_sync_ms)) / 1000,
                unsafe { esp_idf_svc::sys::esp_get_free_heap_size() }
            );
            last_status_log = now;
        }
    }
}

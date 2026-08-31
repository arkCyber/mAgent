//! Real-time task watchdog — "watchdog isolation" (REQ-SCHED-001 / P3).
//!
//! Enables the ESP-IDF **Task Watchdog Timer** and subscribes ONLY the
//! real-time threads (the agent ReAct FSM and the ingress AT dispatcher, both
//! on Core 1). A genuine hang there (no progress past the timeout) panics and
//! reboots the board (fail-safe). The network workers on Core 0 — which
//! *legitimately* block on lwIP/TLS for up to 12s — are deliberately NOT
//! subscribed, so network-induced blocking never trips the watchdog: that is
//! the "isolation".
//!
//! The timeout (15s) exceeds the worst-case *designed* blocking of the RT
//! threads (12s HTTPGET worker wait; 10s per LLM channel round-trip), and the
//! LLM wait itself re-feeds the WDT every second, so a long multi-iteration
//! ReAct task does not false-trip it either.
//!
//! To avoid the main task (which IDLEs after spawning the worker threads)
//! tripping the WDT, we set `CONFIG_ESP_TASK_WDT_INIT=n` so ESP-IDF does NOT
//! auto-subscribe the main task — only the RT threads we explicitly subscribe
//! are monitored.
//!
//! On the single-core C61 (and host builds) this module is a no-op: the
//! shipped C61 keeps its watchdogs disabled.

/// RT watchdog timeout — above the worst-case single blocking hop on a real-
/// time thread: the `fetch_web` tool (~11s DNS+TCP+TLS) and the 10s per-LLM-
/// call bound. Long waits re-feed every second (llm.rs / at_dispatch.rs), and
/// `fetch_web` feeds at entry, so a legitimate wait never trips it while a
/// real hang is still caught within ~18s.
///
/// `rt_wdt` is a `cargo:rustc-cfg` emitted by `build.rs` when the effective
/// ESP-IDF build has `CONFIG_ESP_TASK_WDT_EN=y`. When the target disables the
/// task watchdog (e.g. the ESP32-S3 `sdkconfig.s3.defaults` sets it =n, so
/// `esp_task_wdt.c` is not compiled and the symbols are absent), the `rt_wdt`
/// cfg is unset and this module degrades to a no-op — matching the config and
/// keeping the firmware linkable.
#[cfg(all(feature = "board-s3", rt_wdt))]
const RT_WDT_TIMEOUT_MS: u32 = 18_000;

/// Arm the RT watchdog once at boot. Fail-open: if the subsystem can't start,
/// we log and run without it (never risk a boot that immediately watchdog-
/// resets). Returns `true` if armed.
#[cfg(all(feature = "board-s3", rt_wdt))]
pub fn arm() -> bool {
    use esp_idf_sys::*;
    let config = esp_task_wdt_config_t {
        timeout_ms: RT_WDT_TIMEOUT_MS,
        trigger_panic: true,
        // Subscribe the idle tasks on both cores (they are fed by the RTOS
        // scheduler automatically, so the WDT stays alive while the RT
        // threads are legitimately blocked in a wait).
        idle_core_mask: 0b11,
    };
    let rc = unsafe { esp_task_wdt_init(&config as *const esp_task_wdt_config_t) };
    if rc == ESP_OK {
        log::info!(
            "[wdt] RT task watchdog armed ({RT_WDT_TIMEOUT_MS}ms, panic-on-trigger)"
        );
        true
    } else {
        log::warn!("[wdt] esp_task_wdt_init failed (0x{rc:x}) — running without HW watchdog");
        false
    }
}

/// No-op when the board isn't the S3, or when the S3 build doesn't enable the
/// task watchdog (`ESP_TASK_WDT_EN=n` — see module docs).
#[cfg(not(all(feature = "board-s3", rt_wdt)))]
pub fn arm() -> bool {
    false
}

/// Subscribe the *current* task to the RT watchdog. Call at the top of each RT
/// thread so the WDT monitors it. Best-effort.
#[cfg(all(feature = "board-s3", rt_wdt))]
pub fn subscribe_current() {
    use esp_idf_sys::*;
    let rc = unsafe { esp_task_wdt_add(core::ptr::null_mut()) };
    if rc != ESP_OK {
        log::warn!("[wdt] esp_task_wdt_add failed (0x{rc:x})");
    }
}

/// No-op when the task watchdog isn't compiled in.
#[cfg(not(all(feature = "board-s3", rt_wdt)))]
pub fn subscribe_current() {}

/// Reset (feed) the RT watchdog for the current task. Call on every RT loop
/// iteration and during long blocking waits (e.g. the LLM channel wait) so a
/// *designed* wait never looks like a hang.
#[cfg(all(feature = "board-s3", rt_wdt))]
pub fn feed() {
    unsafe { esp_idf_sys::esp_task_wdt_reset() };
}

/// No-op when the task watchdog isn't compiled in.
#[cfg(not(all(feature = "board-s3", rt_wdt)))]
pub fn feed() {}

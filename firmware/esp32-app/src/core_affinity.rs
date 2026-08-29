//! Dual-core affinity + priority helpers for the ESP32-S3 port (REQ-SCHED-001).
//!
//! The ESP32-C61 (the default build target) is a **single-core** RISC-V
//! RV32IMAC chip — every std thread (agent, ingress, Wi-Fi supervisor, …)
//! shares the one core. The ESP32-S3 (`feature = "board-s3"`) is a
//! **dual-core** Xtensa part (PRO = Core0, APP = Core1), so on that chip we
//! hard-partition work across two cores *and* assign each thread a FreeRTOS
//! priority so the real-time path can always preempt the reasoning path:
//!
//! | Profile            | Core  | Priority | Threads                                      |
//! |--------------------|-------|----------|----------------------------------------------|
//! | I/O (`IO_NETWORK`) | Core 0 (PRO) | 10 | Wi-Fi supervisor, web-admin HTTP, SNTP,      |
//! |                    |       |          | LLM worker, HTTPGET / OTA workers (block on  |
//! |                    |       |          | lwIP / TLS).                                 |
//! | Real-time          | Core 1 (APP) | 20 | ingress thread (AT parse + hardware cmd)     |
//! | (`REALTIME_INGRESS`)|      |          |                                              |
//! | Reasoning          | Core 1 (APP) | 15 | MiniAgent ReAct FSM, Lua app host (yield LLM)|
//! | (`REALTIME_AGENT`) |       |          |                                              |
//!
//! Priorities keep the user threads *below* the ESP-IDF radio stack (Wi-Fi
//! task ~23, lwIP tcpip ~18 — the old default of 24 could starve the radio)
//! while the operator-facing ingress/hardware path on Core 1 sits above the
//! ReAct FSM and the network workers.
//!
//! # Implementation
//!
//! `std::thread` on ESP-IDF wraps a FreeRTOS task created via
//! `xTaskCreatePinnedToCore`. The core + priority come from the global
//! `esp_pthread_cfg_t` (`esp_pthread_set_cfg`), exposed by `esp-idf-hal` as
//! [`ThreadSpawnConfiguration::set`]. Parameters passed explicitly to
//! `std::thread::Builder` (name, stack size) take precedence over that global
//! config, but `pin_to_core` and `priority` are only read from it — so we set
//! the whole profile *immediately before* each spawn.
//!
//! On the single-core C61 (and host builds) `apply_profile` is a compile-time
//! no-op: `Core::Core1` does not exist, and the shipped C61 keeps its existing
//! single-core scheduling unchanged.

/// FreeRTOS priorities — ESP-IDF valid range is `[1, configMAX_PRIORITIES-1]`
/// = `[1, 24]`. Higher number = more urgent (FreeRTOS convention).
pub const PRIO_INGRESS: u8 = 20; // AT parse + hardware command dispatch (Core 1)
pub const PRIO_AGENT: u8 = 15; // MiniAgent ReAct FSM / Lua host (Core 1)
pub const PRIO_IO: u8 = 10; // Wi-Fi sup, web-admin, SNTP, LLM/HTTP/OTA workers (Core 0)
pub const PRIO_TRIVIAL: u8 = 8; // short-lived delay+reboot threads

/// Which core a thread runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorePlacement {
    /// Core 0 (PRO) — the I/O / network domain.
    Io,
    /// Core 1 (APP) — the real-time MiniAgent domain.
    Realtime,
    /// Let the FreeRTOS scheduler decide (any core).
    Any,
}

/// Core + FreeRTOS priority for a thread (REQ-SCHED-001). Kept uniform across
/// targets so the spawn call sites don't need `#[cfg]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadProfile {
    /// Which core to run on.
    pub core: CorePlacement,
    /// FreeRTOS priority ([1, 24]).
    pub priority: u8,
}

impl ThreadProfile {
    /// Real-time AT / hardware command dispatch on Core 1 (highest user priority).
    pub const REALTIME_INGRESS: ThreadProfile = ThreadProfile {
        core: CorePlacement::Realtime,
        priority: PRIO_INGRESS,
    };
    /// The MiniAgent ReAct FSM / Lua host on Core 1 (below the ingress path).
    pub const REALTIME_AGENT: ThreadProfile = ThreadProfile {
        core: CorePlacement::Realtime,
        priority: PRIO_AGENT,
    };
    /// Background I/O / network workers on Core 0.
    pub const IO_NETWORK: ThreadProfile = ThreadProfile {
        core: CorePlacement::Io,
        priority: PRIO_IO,
    };
    /// No pinning, trivial priority (delay+reboot threads).
    pub const UNPINNED: ThreadProfile = ThreadProfile {
        core: CorePlacement::Any,
        priority: PRIO_TRIVIAL,
    };
}

/// Applies the profile (core + priority) for the *next* `std::thread` spawn.
/// Must be called immediately before `std::thread::Builder::spawn`. Always
/// resets the global config (either to a pinned core or back to `None` for
/// `Any`), so a prior call can never leak its affinity onto a later thread.
#[cfg(feature = "board-s3")]
pub fn apply_profile(profile: ThreadProfile) {
    use esp_idf_svc::hal::cpu::Core;
    use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;

    let mut cfg = ThreadSpawnConfiguration::default();
    cfg.priority = profile.priority;
    cfg.pin_to_core = match profile.core {
        CorePlacement::Io => Some(Core::Core0),
        CorePlacement::Realtime => Some(Core::Core1),
        CorePlacement::Any => None,
    };
    cfg.set()
        .unwrap_or_else(|e| log::warn!("[core] profile set failed: {e}"));
}

/// Single-core fallback: nothing to pin, and the shipped C61 keeps its
/// existing scheduling unchanged. Kept so the call sites compile unchanged
/// for the ESP32-C61 and host builds.
#[cfg(not(feature = "board-s3"))]
pub fn apply_profile(_profile: ThreadProfile) {
    // No-op: the C61 has a single RISC-V core; Core1 does not exist.
}

/// Spawn a std thread pinned to the requested core + priority.
///
/// Equivalent to
/// `thread::Builder::new().name(name).stack_size(stack_size).spawn(f)` —
/// same return type (`std::io::Result<JoinHandle<()>>`) — except that the
/// thread's core affinity + priority are set first on the ESP32-S3. On the
/// C61 this is exactly the old spawn with no scheduling changes.
///
/// # Panics
///
/// Never panics on its own; a failure to set the profile is downgraded to a
/// `log::warn!` so the thread still spawns (fail-open).
pub fn spawn_thread<F>(
    name: &'static str,
    stack_size: usize,
    profile: ThreadProfile,
    f: F,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    apply_profile(profile);
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(stack_size)
        .spawn(f)
}


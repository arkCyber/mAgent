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
//! the whole profile *immediately before* each spawn, holding [`SPAWN_LOCK`]
//! across the pair so two concurrent spawners cannot read each other's profile
//! (see that lock's doc).
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
/// Private on purpose: the caller must hold [`SPAWN_LOCK`] across this call
/// *and* the spawn that reads it, so go through [`spawn_thread`] /
/// [`spawn_with_profile`]. Always resets the global config (either to a pinned
/// core or back to `None` for `Any`), so a prior call can never leak its
/// affinity onto a later thread.
#[cfg(feature = "board-s3")]
fn apply_profile(profile: ThreadProfile) {
    use esp_idf_svc::hal::cpu::Core;
    use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;

    // `ThreadSpawnConfiguration::set` **panics** when the priority is outside
    // FreeRTOS's `[1, configMAX_PRIORITIES - 1]`, and the `unwrap_or_else`
    // below cannot catch a panic — so a priority constant above the ceiling
    // would abort the spawn instead of the fail-open this function documents.
    // Clamp into range first (the `.max(1)` also keeps `clamp` itself from
    // panicking if the ceiling ever computes to 0).
    let max_priority = (esp_idf_sys::configMAX_PRIORITIES as u8)
        .saturating_sub(1)
        .max(1);
    let priority = profile.priority.clamp(1, max_priority);
    if priority != profile.priority {
        log::warn!(
            "[core] priority {} outside FreeRTOS range [1, {max_priority}] — clamped to {priority}",
            profile.priority
        );
    }

    let mut cfg = ThreadSpawnConfiguration::default();
    cfg.priority = priority;
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
fn apply_profile(_profile: ThreadProfile) {
    // No-op: the C61 has a single RISC-V core; Core1 does not exist.
}

/// Serialises the `apply_profile` → spawn pair.
///
/// The profile lives in a **process-global** `esp_pthread_cfg_t` that
/// `pthread_create` reads at creation time, so `apply_profile` followed by a
/// spawn is only atomic while no other thread is doing the same thing. Two
/// concurrent spawners can interleave — A sets profile X, B sets profile Y,
/// A's `pthread_create` then reads Y — which silently places A's thread on
/// the wrong core at the wrong priority. That is reachable on the S3 today:
/// the Lua host thread spawns its `llm-worker` (Core 0) at an arbitrary moment
/// while the boot path (or an `at_dispatch` `AT+OTA` / `AT+RST`) spawns on
/// Core 1. Every spawn that installs a profile holds this lock across both
/// steps, so call [`spawn_thread`] / [`spawn_with_profile`] rather than
/// calling [`apply_profile`] directly.
static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Spawn a std thread pinned to the requested core + priority.
///
/// Equivalent to
/// `thread::Builder::new().name(name).stack_size(stack_size).spawn(f)` —
/// same return type (`std::io::Result<JoinHandle<()>>`) — except that the
/// thread's core affinity + priority are set first on the ESP32-S3. On the
/// C61 this is exactly the old spawn with no scheduling changes.
///
/// The profile is applied under `SPAWN_LOCK`, so a concurrent
/// [`spawn_with_profile`] cannot clobber it between the set and the create.
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
    let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    apply_profile(profile);
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(stack_size)
        .spawn(f)
}

/// Spawn on the **default** stack with `profile`, not a named/bounded one.
///
/// Same contract as [`spawn_thread`] (profile applied under `SPAWN_LOCK`)
/// for call sites that only need the affinity/priority — the `AT+RST` /
/// `AT+RESTORE` delay+reboot threads and the `AT+OTA` worker. Keeping the
/// set-and-spawn atomic is the point: a direct
/// `apply_profile(...); std::thread::spawn(...)` elsewhere is exactly the
/// race this helper exists to remove.
///
/// Returns the OS error instead of panicking when the thread cannot be
/// created: on the memory-constrained S3 an ENOMEM spawn failure inside the
/// ingress thread would abort the whole board (`std::thread::spawn` panics).
pub fn spawn_with_profile<F>(
    profile: ThreadProfile,
    f: F,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    apply_profile(profile);
    std::thread::Builder::new().spawn(f)
}


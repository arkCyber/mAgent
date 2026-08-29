//! `AppRuntime` — a supervised boot + event-loop for the Lua application.
//!
//! This is the **"user App as brain, AI agent as brain-trust"** runtime the
//! firmware will drive. It boots the Lua chunk once ([`AppRuntime::boot`]),
//! then advances an event loop ([`AppRuntime::tick`]) that calls a Lua
//! `on_tick(now_ms)` handler and, when the handler returns a recognised
//! action, dispatches it to hardware. A single-tick error is contained so the
//! device keeps running; a supervisor observes [`AppRuntime::tick_count`] /
//! [`AppRuntime::uptime_ms`] as a heartbeat / watchdog.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::action::{apply_action, Action};
use crate::engine::LuaEngine;
use crate::error::{LuaHostError, Result};
use crate::hardware::SharedHardware;
#[cfg(feature = "mlua")]
use crate::vm::LuaVm;

/// The name of the Lua per-tick handler invoked by [`AppRuntime::tick`].
pub const ON_TICK: &str = "on_tick";

/// Result of one event-loop tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    /// Monotonic tick index since boot.
    pub index: u64,
    /// Wall-clock uptime in ms at this tick.
    pub uptime_ms: u64,
    /// The string `on_tick` returned (may be empty).
    pub result: String,
    /// Whether the returned string was dispatched as an action.
    pub dispatched: bool,
}

/// A point-in-time health snapshot a supervisor / watchdog reads.
///
/// Obtained from [`AppRuntime::health`] with a single call so the supervisor
/// does not need to know the runtime's internals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    /// Wall-clock uptime in ms since the runtime was created.
    pub uptime_ms: u64,
    /// Monotonic ticks advanced since boot.
    pub tick_count: u64,
    /// Cumulative number of failed ticks (handler / dispatch errors).
    pub error_count: u64,
    /// The most recent error, if any.
    pub last_error: Option<String>,
    /// Whether the loop has stopped being driven for `watchdog_timeout`.
    pub stale: bool,
}

/// A supervised Lua application runtime, generic over the Lua engine.
///
/// With the default `mlua` feature, the default engine is [`LuaVm`]. To run on
/// the pure-Rust `piccolo` engine (the ESP32-S3 path), construct with
/// `AppRuntime::<PiccoloVm>`.
#[cfg(feature = "mlua")]
pub struct AppRuntime<T: LuaEngine = LuaVm> {
    vm: T,
    hardware: SharedHardware,
    booted_at: Instant,
    ticks: u64,
    last_tick_at: Instant,
    error_count: u64,
    last_error: Option<String>,
    stop: Arc<AtomicBool>,
}

/// A supervised Lua application runtime, generic over the Lua engine (built
/// without the `mlua` feature — e.g. `--no-default-features --features
/// piccolo` for the ESP32-S3).
#[cfg(not(feature = "mlua"))]
pub struct AppRuntime<T: LuaEngine> {
    vm: T,
    hardware: SharedHardware,
    booted_at: Instant,
    ticks: u64,
    last_tick_at: Instant,
    error_count: u64,
    last_error: Option<String>,
    stop: Arc<AtomicBool>,
}

impl<T: LuaEngine> AppRuntime<T> {
    /// Create the runtime around an already-bound Lua engine.
    pub fn new(vm: T, hardware: SharedHardware) -> Self {
        let booted_at = Instant::now();
        Self {
            vm,
            hardware,
            booted_at,
            ticks: 0,
            last_tick_at: booted_at,
            error_count: 0,
            last_error: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run a boot chunk (e.g. `main.lua`). A failure here is fatal to boot.
    pub fn boot(&mut self, script: &str) -> Result<()> {
        self.vm.run_script(script)
    }

    /// Hot-reload the application: rebuild the Lua state from a clean slate
    /// (so stale globals from the previous app cannot leak), reset the runtime
    /// counters, then boot `script`.
    ///
    /// This lets a firmware update `main.lua` at runtime without a reboot.
    pub fn reload(&mut self, script: &str) -> Result<()> {
        self.vm.reload_state()?;
        self.ticks = 0;
        self.error_count = 0;
        self.last_error = None;
        self.booted_at = Instant::now();
        self.last_tick_at = Instant::now();
        self.vm.run_script(script)
    }

    /// Advance one event-loop tick.
    ///
    /// Calls the Lua `on_tick(now_ms)` handler if defined. If the handler
    /// returns a **recognised** action, that action is applied to the
    /// hardware backend. A missing handler yields an empty tick; a handler
    /// error is returned, recorded in [`Health`], and contained (the next
    /// `tick` may succeed).
    pub fn tick(&mut self) -> Result<Tick> {
        let now = Instant::now();
        // Any tick attempt — even one that later errors — refreshes the
        // watchdog timestamp, so `is_stale` reflects a loop that stopped
        // *being driven* rather than a single bad handler call.
        self.last_tick_at = now;
        self.ticks = self.ticks.saturating_add(1);
        match self.tick_inner() {
            Ok(tick) => Ok(tick),
            Err(e) => {
                self.error_count = self.error_count.saturating_add(1);
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// The body of [`AppRuntime::tick`]; errors are recorded by the wrapper.
    fn tick_inner(&mut self) -> Result<Tick> {
        let uptime_ms = self.booted_at.elapsed().as_millis() as u64;
        let index = self.ticks;

        if !self.vm.has(ON_TICK) {
            return Ok(Tick {
                index,
                uptime_ms,
                result: String::new(),
                dispatched: false,
            });
        }

        let result = self.vm.call(ON_TICK, &[uptime_ms.to_string()])?;

        // Only dispatch when the returned string is a known command. A plain
        // informational string (e.g. raw agent prose) is surfaced but not
        // applied, and never errors the tick.
        let mut dispatched = false;
        if let Some(action) = Action::parse(&result) {
            if action.is_known() {
                let mut hw = self
                    .hardware
                    .lock()
                    .map_err(|_| LuaHostError::Hardware("hardware lock poisoned".into()))?;
                apply_action(&mut *hw, &action).map_err(LuaHostError::Hardware)?;
                dispatched = true;
            }
        }

        Ok(Tick {
            index,
            uptime_ms,
            result,
            dispatched,
        })
    }

    /// Number of ticks advanced since boot (monotonic heartbeat).
    pub fn tick_count(&self) -> u64 {
        self.ticks
    }

    /// Wall-clock uptime in ms since the runtime was created.
    pub fn uptime_ms(&self) -> u64 {
        self.booted_at.elapsed().as_millis() as u64
    }

    /// Watchdog probe: `true` if no [`AppRuntime::tick`] has completed within
    /// the last `timeout`, i.e. the app loop has stopped being driven.
    ///
    /// A supervisor polls this from a separate thread/timer; if the Lua VM
    /// hangs (infinite loop) the instruction budget still stops it, but this
    /// catches a supervisor that forgot to call `tick`.
    pub fn is_stale(&self, timeout: Duration) -> bool {
        self.last_tick_at.elapsed() > timeout
    }

    /// Return a point-in-time [`Health`] snapshot for the supervisor.
    ///
    /// `watchdog_timeout` is forwarded to [`AppRuntime::is_stale`] so the
    /// snapshot includes the liveness flag in the same call.
    pub fn health(&self, watchdog_timeout: Duration) -> Health {
        Health {
            uptime_ms: self.uptime_ms(),
            tick_count: self.tick_count(),
            error_count: self.error_count,
            last_error: self.last_error.clone(),
            stale: self.is_stale(watchdog_timeout),
        }
    }

    /// Get a handle a *different thread* (e.g. a firmware supervisor) can use
    /// to request a clean stop of the event loop.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// Request a clean stop of the event loop (idempotent).
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Whether a stop has been requested.
    pub fn is_stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Run the event loop until a stop is requested or `max_ticks` is reached.
    ///
    /// This is the firmware's main loop: boot once, then call this to drive
    /// `tick()` forever (or until a supervisor sets the [`stop_flag`]
    /// / [`AppRuntime::stop_flag`]). Per-tick errors are contained and logged
    /// via the [`Health`] counters; returns the number of ticks completed.
    pub fn run_until_stop(&mut self, tick_period: Duration, max_ticks: Option<u64>) -> u64 {
        let mut ticks = 0u64;
        loop {
            if self.is_stop_requested() {
                break;
            }
            if let Some(m) = max_ticks {
                if ticks >= m {
                    break;
                }
            }
            let _ = self.tick(); // contained per-tick error
            ticks = ticks.saturating_add(1);
            if !tick_period.is_zero() {
                std::thread::sleep(tick_period);
            }
        }
        ticks
    }
}

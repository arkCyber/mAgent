//! mAgent Application - Smartwatch AI Agent (nRF52840)
//!
//! Full-featured Embassy firmware with AI agent, BLE, sensors, and power management.

#![no_std]
#![no_main]

mod ble;
mod sensors;
mod power;
mod watchdog;

use core::cell::RefCell;
use defmt::{info, debug, error};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use defmt_rtt as _;
use embassy_executor::Executor;
use embassy_nrf::config::Config as NrfConfig;
use embassy_time::{Duration, Instant, Timer};
use embedded_alloc::Heap;
// TRACE: AEROSPACE-AUDIT-2026-09-15 §6 FW-R4 — replace `panic-probe`
// with a reset-on-panic handler. The previous handler bkpt()'d in a
// tight loop, which wedges the board without ever feeding the WDT.
// The replacement arms `SCB::sys_reset()` so a panic surfaces as a
// clean reset, and the watchdog-fed main task continues to make
// progress between panics.
use panic_reset as _;

use magent_core::{
    MiniAgent,
    AgentConfig,
    VERSION,
};

#[global_allocator]
static HEAP: Heap = Heap::empty();

// =============================================================================
// Static Resources
// =============================================================================

static EXECUTOR: static_cell::StaticCell<Executor> = static_cell::StaticCell::new();

// TRACE: AEROSPACE-AUDIT-2026-09-15 §6 FW-R3 — resolved 2026-09-20.
//
// These were `static mut` accessed through `unsafe` blocks whose only
// justification was a comment ("Embassy serialises the two tasks on one
// core"). That reasoning was sound but unenforced, and it made every
// access site a `static_mut_refs` soundness lint: taking `&`/`&mut` to a
// `static mut` is exactly the aliasing hazard Rust forbids. If a future
// change added a second core or a second `main_task` spawn, the invariant
// would break *silently*.
//
// The exclusivity is now enforced by the type system instead: a
// critical-section `Mutex` around a `RefCell`. `lock()` runs its closure
// with interrupts masked, so the borrow can never overlap across the two
// tasks, and the compiler — not a comment — upholds the invariant.
//
// No borrow is held across an `.await`: every `lock()` closure below is
// synchronous. `RefCell` borrows are released at the end of the closure,
// so a `borrow_mut()` panic is not reachable from the task interleaving.
static WATCHDOG: Mutex<CriticalSectionRawMutex, RefCell<Option<watchdog::Watchdog>>> =
    Mutex::new(RefCell::new(None));
// `BleState` is an *enum* (Idle/Advertising/Connected/…); the struct that
// carries the runtime connection fields is `BleStateManager`. The initial
// literal is written out explicitly rather than via `new()` so it stays a
// `const` initializer.
static BLE_STATE: Mutex<CriticalSectionRawMutex, RefCell<ble::BleStateManager>> =
    Mutex::new(RefCell::new(ble::BleStateManager {
        state: ble::BleState::Idle,
        connection_handle: None,
        is_connected: false,
        mtu: 23,
        battery_level: 100,
    }));
static POWER_STATE: Mutex<CriticalSectionRawMutex, RefCell<power::PowerState>> =
    Mutex::new(RefCell::new(power::PowerState {
        mode: power::PowerMode::Active,
        battery_level: 100,
        estimated_runtime_hours: 0.0,
        sleep_count: 0,
        wake_count: 0,
    }));
/// Holds the agent across an interrupt boundary.
///
/// `MiniAgent` owns a `dyn LlmBackend` and `dyn ToolHandler`, both of which are
/// deliberately `!Send` (they are not required to be thread-safe on the host
/// `std` build). A `static` must be `Sync`, so `Mutex<_, RefCell<Option<MiniAgent>>>`
/// cannot be declared directly. This newtype carries the one assertion that
/// makes it legal, in one auditable place.
///
/// SAFETY: the nRF52840 is single-core, and `AGENT` is reachable only through
/// `AGENT.lock(..)`, which masks interrupts for the whole closure
/// (`CriticalSectionRawMutex`). Nothing else can observe or mutate the value
/// concurrently, so there is no cross-context data race for `Send`/`Sync` to
/// guard against. Every lock closure is synchronous — no borrow is held across
/// an `.await` — so the `RefCell` cannot be re-entered from a task interleave.
///
/// `Send` is needed as well as `Sync` because `embassy_sync`'s
/// `blocking_mutex::Mutex<T>` is `Sync` only when `T: Send`, which is what a
/// `static` declaration requires.
struct AgentSlot(RefCell<Option<MiniAgent>>);
unsafe impl Send for AgentSlot {}
unsafe impl Sync for AgentSlot {}

static AGENT: Mutex<CriticalSectionRawMutex, AgentSlot> = Mutex::new(AgentSlot(RefCell::new(None)));

// =============================================================================
// Configuration
// =============================================================================

const HEAP_SIZE: usize = 8192;
const WATCHDOG_TIMEOUT_MS: u32 = 60_000;
const BLE_ADV_INTERVAL_MS: u64 = 5000;
const AGENT_TASK_INTERVAL_MS: u64 = 10000;

// =============================================================================
// Main Entry Point
// =============================================================================

#[cortex_m_rt::entry]
fn main() -> ! {
    info!("========================================");
    info!("mAgent v{} starting on nRF52840...", VERSION);
    info!("========================================");

    // Initialize heap allocator
    {
        static mut HEAP_MEM: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
        unsafe {
            HEAP.init(core::ptr::addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE);
        }
    }

    // Initialize nRF peripherals
    let config = NrfConfig::default();
    let _p = embassy_nrf::init(config);

    // Initialize static resources
    WATCHDOG.lock(|w| {
        *w.borrow_mut() = Some(watchdog::Watchdog::new(&watchdog::WatchdogConfig::default()));
    });

    // Initialize the AI Agent
    {
        let agent_config = AgentConfig::new()
            .with_max_iterations(50u16)
            .expect("valid max_iterations");

        match MiniAgent::new(agent_config) {
            Ok(agent) => {
                info!("AI Agent initialized successfully");
                AGENT.lock(|a| *a.0.borrow_mut() = Some(agent));
            }
            Err(e) => {
                error!("Failed to initialize AI Agent: {:?}", e);
            }
        }
    }

    // Initialize BLE
    let _ = ble::init_softdevice();

    // Start BLE advertising
    let ble_config = ble::BleConfig::default();
    let _ = ble::start_advertising(&ble_config);

    info!("System initialization complete");
    info!("Watchdog: {}s timeout", WATCHDOG_TIMEOUT_MS / 1000);

    // Start the executor
    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        spawner.spawn(main_task()).unwrap();
        spawner.spawn(agent_task()).unwrap();
    })
}

// =============================================================================
// Main Application Task
// =============================================================================

#[embassy_executor::task]
async fn main_task() {
    info!("[MAIN] Task started");

    let mut cycle_count: u32 = 0;
    let mut battery_level: u8 = 75;

    loop {
        cycle_count += 1;

        // Feed watchdog.
        //
        // `feed` takes the *current time*, not the timeout: it stores
        // `last_feed = current_time_ms` and `remaining_ms`/`check_timeout`
        // subtract from it. Passing `WATCHDOG_TIMEOUT_MS` here (a constant)
        // made `last_feed` equal the timeout, so `remaining_ms` computed
        // `0` elapsed and the status line always printed the full
        // `60000ms remaining` — a countdown that could not count. The
        // monotonic clock the loop already sleeps on drives it now, so the
        // figure decreases across the 5 s cycle.
        let now_ms = Instant::now().as_millis();
        WATCHDOG.lock(|w| {
            if let Some(wd) = w.borrow_mut().as_mut() {
                wd.feed(now_ms);
            }
        });

        // Placeholder battery figure. There is no SAADC reading here — the
        // number starts at 75 and ticks down on a cycle counter, so it is a
        // liveness indicator, not a measurement. It is labelled `(simulated)`
        // everywhere it is logged below rather than being presented as a
        // reading the hardware did not take.
        if cycle_count % 60 == 0 {
            battery_level = battery_level.saturating_sub(1);
        }
        POWER_STATE.lock(|p| p.borrow_mut().battery_level = battery_level);

        // Get BLE status
        let ble_status = BLE_STATE.lock(|s| ble::connection_status(&s.borrow()));

        // Log periodic status
        if cycle_count % 5 == 0 {
            let now_ms = Instant::now().as_millis();
            let watchdog_remaining = WATCHDOG.lock(|w| {
                w.borrow()
                    .as_ref()
                    .map(|wd| wd.remaining_ms(now_ms))
                    .unwrap_or(0)
            });
            let agent_ready = AGENT.lock(|a| a.0.borrow().is_some());

            info!("========================================");
            info!(
                "[MAIN] Cycle #{} | Battery: {}% (simulated)",
                cycle_count, battery_level
            );
            info!("[MAIN] BLE: {}", ble_status);
            info!("[MAIN] Watchdog: {}ms remaining", watchdog_remaining);
            info!("[MAIN] Agent: {}",
                if agent_ready { "Ready" } else { "Not initialized" });
            info!("========================================");
        }

        // Simulate agent work
        debug!("[MAIN] System heartbeat");

        Timer::after(Duration::from_millis(BLE_ADV_INTERVAL_MS)).await;
    }
}

// =============================================================================
// AI Agent Task
// =============================================================================

#[embassy_executor::task]
async fn agent_task() {
    info!("[AGENT] AI Agent task started");

    // SCAFFOLD — and it now says so out loud.
    //
    // `MiniAgent` is constructed in `main`, but driving it needs an
    // `LlmBackend`, which this target has none of: no network stack, no local
    // model, no tool handler. The previous version of this task looped over
    // four canned task strings and logged `Running task: <task>`,
    // `Agent is running` and `Budget check passed` without ever calling the
    // agent — three claims of work that never happened, written to the
    // device's real log where an operator (or a log-ingest tool) reads them
    // as fact. Until a backend exists, report readiness once and stay quiet.
    let mut announced = false;
    loop {
        if !announced && AGENT.lock(|a| a.0.borrow().is_some()) {
            info!(
                "[AGENT] MiniAgent ready; agent loop is a scaffold (no LLM backend wired), no task is run"
            );
            announced = true;
        }

        // Wait before the next readiness check.
        Timer::after(Duration::from_millis(AGENT_TASK_INTERVAL_MS)).await;
    }
}

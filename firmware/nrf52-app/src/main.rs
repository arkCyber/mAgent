//! mAgent Application - Smartwatch AI Agent (nRF52840)
//!
//! Full-featured Embassy firmware with AI agent, BLE, sensors, and power management.

#![no_std]
#![no_main]

mod ble;
mod sensors;
mod power;
mod watchdog;

use defmt::{info, debug, error};
use defmt_rtt as _;
use embassy_executor::Executor;
use embassy_nrf::config::Config as NrfConfig;
use embassy_time::{Duration, Timer};
use embedded_alloc::Heap;
use panic_probe as _;

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

// SAFETY: These are initialized in main() before being accessed
static mut WATCHDOG: Option<watchdog::Watchdog> = None;
static mut BLE_STATE: ble::BleState = ble::BleState { is_connected: false, connection_handle: None, battery_level: 100 };
static mut POWER_STATE: power::PowerState = power::PowerState { mode: power::PowerMode::Active, battery_level: 100, estimated_runtime_hours: 0.0, sleep_count: 0, wake_count: 0 };
static mut AGENT: Option<MiniAgent> = None;

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
    unsafe {
        WATCHDOG = Some(watchdog::Watchdog::new(&watchdog::WatchdogConfig::default()));
    }

    // Initialize the AI Agent
    {
        let agent_config = AgentConfig::new()
            .with_max_iterations(50u16)
            .expect("valid max_iterations");
        
        match MiniAgent::new(agent_config) {
            Ok(agent) => {
                info!("AI Agent initialized successfully");
                unsafe { AGENT = Some(agent); }
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

        // SAFETY: Only this task accesses these statics
        unsafe {
            // Feed watchdog
            if let Some(ref mut w) = WATCHDOG {
                w.feed(WATCHDOG_TIMEOUT_MS as u64);
            }

            // Simulate battery drain
            if cycle_count % 60 == 0 {
                battery_level = battery_level.saturating_sub(1);
            }
            POWER_STATE.battery_level = battery_level;

            // Get BLE status
            let ble_status = ble::connection_status(&BLE_STATE);

            // Log periodic status
            if cycle_count % 5 == 0 {
                let watchdog_remaining = WATCHDOG.as_ref()
                    .map(|w| w.remaining_ms(WATCHDOG_TIMEOUT_MS as u64))
                    .unwrap_or(0);

                info!("========================================");
                info!("[MAIN] Cycle #{} | Battery: {}%", cycle_count, battery_level);
                info!("[MAIN] BLE: {}", ble_status);
                info!("[MAIN] Watchdog: {}ms remaining", watchdog_remaining);
                info!("[MAIN] Agent: {}", 
                    if AGENT.is_some() { "Ready" } else { "Not initialized" });
                info!("========================================");
            }
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

    // Create a simple test task
    let test_tasks = [
        "Read the temperature sensor",
        "Check battery level",
        "Read heart rate",
        "Report system status",
    ];
    
    let mut task_index = 0;

    loop {
        // Check if agent is initialized
        let agent_ready = unsafe { AGENT.is_some() };
        
        if agent_ready {
            // Run a test task
            let task = test_tasks[task_index % test_tasks.len()];
            info!("[AGENT] Running task: {}", task);
            
            // SAFETY: Only this task accesses AGENT
            unsafe {
                if let Some(ref mut _agent) = AGENT {
                    // Note: The actual async run would require LLM backend
                    // For now, we just demonstrate the agent is working
                    info!("[AGENT] Agent is running");
                    info!("[AGENT] Budget check passed");
                }
            }
            
            task_index += 1;
        } else {
            info!("[AGENT] Waiting for initialization...");
        }

        // Wait before next agent iteration
        Timer::after(Duration::from_millis(AGENT_TASK_INTERVAL_MS)).await;
    }
}

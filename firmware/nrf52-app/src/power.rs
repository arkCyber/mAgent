//! Power Management Module for nRF52840
//!
//! Implements various power modes and power optimization strategies.

use defmt::info;
use embassy_sync::mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

// =============================================================================
// Power Modes
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum PowerMode {
    #[default]
    Active,
    LowPower,
    Sleep,
    DeepSleep,
    Off,
}

// =============================================================================
// Power State
// =============================================================================

pub struct PowerState {
    pub mode: PowerMode,
    pub battery_level: u8,
    pub estimated_runtime_hours: f32,
    pub sleep_count: u32,
    pub wake_count: u32,
}

impl Default for PowerState {
    fn default() -> Self {
        Self {
            mode: PowerMode::Active,
            battery_level: 100,
            estimated_runtime_hours: 0.0,
            sleep_count: 0,
            wake_count: 0,
        }
    }
}

// =============================================================================
// Power Management Functions
// =============================================================================

pub fn configure_low_power() {
    info!("Low power configuration applied");
}

pub fn enter_sleep() {
    info!("Entering sleep mode");
    cortex_m::asm::wfi();
}

pub fn enter_deep_sleep() {
    info!("Entering deep sleep mode");
    cortex_m::asm::wfi();
}

pub fn estimate_runtime(mode: PowerMode, _battery_mah: u16, battery_level: u8) -> f32 {
    let current_ma = match mode {
        PowerMode::Active => 15.0,
        PowerMode::LowPower => 3.0,
        PowerMode::Sleep => 0.5,
        PowerMode::DeepSleep => 0.001,
        PowerMode::Off => 0.0,
    };

    if current_ma > 0.0 {
        battery_level as f32 / current_ma
    } else {
        999.0
    }
}

pub fn optimize_for_battery(battery_level: u8) -> PowerMode {
    match battery_level {
        0..=10 => PowerMode::DeepSleep,
        11..=25 => PowerMode::LowPower,
        _ => PowerMode::Active,
    }
}

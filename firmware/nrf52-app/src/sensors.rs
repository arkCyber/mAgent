//! Sensors Module for nRF52840
//!
//! Supports common sensors typically found on nRF52840-based smartwatches.

// =============================================================================
// Sensor Addresses
// =============================================================================

pub mod addresses {
    pub const LIS2DW12: u8 = 0x18;
    pub const BME280: u8 = 0x76;
}

// =============================================================================
// Sensor Data Types
// =============================================================================

#[derive(Clone, Copy, Debug, Default)]
pub struct AccelerometerData {
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub steps: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentData {
    pub temperature_c: i16,
    pub humidity_percent: u16,
    pub pressure_hpa: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BatteryData {
    pub voltage_mv: u16,
    pub level_percent: u8,
    pub is_charging: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SensorData {
    pub accelerometer: AccelerometerData,
    pub environment: EnvironmentData,
    pub battery: BatteryData,
    pub timestamp_ms: u64,
}

// =============================================================================
// Battery Configuration
// =============================================================================

pub struct BatteryConfig {
    pub voltage_full_mv: u16,
    pub voltage_empty_mv: u16,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self {
            voltage_full_mv: 4200,
            voltage_empty_mv: 3200,
        }
    }
}

pub fn calculate_battery_level(config: &BatteryConfig, voltage_mv: u16) -> u8 {
    let voltage = voltage_mv as i32;
    let full = config.voltage_full_mv as i32;
    let empty = config.voltage_empty_mv as i32;

    if voltage >= full {
        100
    } else if voltage <= empty {
        0
    } else {
        ((voltage - empty) * 100 / (full - empty)) as u8
    }
}

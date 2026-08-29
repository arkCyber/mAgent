//! Power management for mAgent
//!
//! Provides low power mode support and battery monitoring
//! for aerospace-grade power efficiency.

use crate::error::{AgentError, Result};
use core::cell::Cell;

/// Power mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PowerMode {
    /// Active mode - full performance
    Active = 0,
    /// Idle mode - CPU clocked, peripherals off
    Idle = 1,
    /// Low power mode - reduced clock
    LowPower = 2,
    /// Deep sleep - minimal power
    DeepSleep = 3,
}

/// Battery status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    /// Voltage in millivolts
    pub voltage_mv: u16,
    /// Percentage (0-100)
    pub percentage: u8,
    /// Is charging
    pub charging: bool,
    /// Is low battery
    pub low_battery: bool,
}

/// Power manager
pub struct PowerManager {
    current_mode: Cell<PowerMode>,
    battery_threshold: Cell<u16>, // mV
}

impl PowerManager {
    /// Create a new power manager
    pub fn new() -> Self {
        Self {
            current_mode: Cell::new(PowerMode::Active),
            battery_threshold: Cell::new(3300), // 3.3V low battery threshold
        }
    }

    /// Get current power mode
    pub fn current_mode(&self) -> PowerMode {
        self.current_mode.get()
    }

    /// Set power mode
    pub fn set_mode(&self, mode: PowerMode) -> Result<()> {
        // Validate mode transition
        let current = self.current_mode.get();
        if !self.is_valid_transition(current, mode) {
            return Err(AgentError::InvalidStateTransition {
                from: match current {
                    PowerMode::Active => "Active",
                    PowerMode::Idle => "Idle",
                    PowerMode::LowPower => "LowPower",
                    PowerMode::DeepSleep => "DeepSleep",
                },
                to: match mode {
                    PowerMode::Active => "Active",
                    PowerMode::Idle => "Idle",
                    PowerMode::LowPower => "LowPower",
                    PowerMode::DeepSleep => "DeepSleep",
                },
            });
        }

        self.current_mode.set(mode);
        Ok(())
    }

    /// Check if mode transition is valid.
    ///
    /// The state machine is intentionally permissive:
    ///
    /// * `Active` (the initial mode) can transition to any other mode
    ///   — the hardware is fully on, so dropping into a lower power
    ///   state is always safe.
    /// * Any non-Active mode can wake back up to `Active` (the only
    ///   way to leave `LowPower` / `DeepSleep`).
    /// * Adjacent non-Active modes can step *down* one level at a
    ///   time (`Idle → LowPower`, `Idle → DeepSleep`, `LowPower →
    ///   DeepSleep`), which lets a sleep-orchestration sequence like
    ///   `Idle → LowPower → DeepSleep → Active` succeed.
    ///
    /// The previous implementation restricted every non-Active mode
    /// to *only* `Active` (plus `Idle → DeepSleep`), which broke the
    /// obvious test sequence `Idle → LowPower → DeepSleep → Active`
    /// (an `Idle → LowPower` step used to fail with
    /// `InvalidStateTransition`).
    fn is_valid_transition(&self, from: PowerMode, to: PowerMode) -> bool {
        // No-op transitions are always allowed.
        if from == to {
            return true;
        }
        // Active can transition to any other mode.
        if from == PowerMode::Active {
            return true;
        }
        // Any non-Active mode can wake back up to Active.
        if to == PowerMode::Active {
            return true;
        }
        match (from, to) {
            // Idle can step down to LowPower or DeepSleep.
            (PowerMode::Idle, PowerMode::LowPower) => true,
            (PowerMode::Idle, PowerMode::DeepSleep) => true,
            // LowPower can step down to DeepSleep.
            (PowerMode::LowPower, PowerMode::DeepSleep) => true,
            // Stepping *up* without going through Active
            // (e.g. DeepSleep → Idle, LowPower → Idle) is not a
            // meaningful transition for our hardware, so reject.
            _ => false,
        }
    }

    /// Enter idle mode
    pub fn enter_idle(&self) -> Result<()> {
        self.set_mode(PowerMode::Idle)
    }

    /// Enter low power mode
    pub fn enter_low_power(&self) -> Result<()> {
        self.set_mode(PowerMode::LowPower)
    }

    /// Enter deep sleep
    pub fn enter_deep_sleep(&self) -> Result<()> {
        self.set_mode(PowerMode::DeepSleep)
    }

    /// Wake up to active mode
    pub fn wake_up(&self) -> Result<()> {
        self.set_mode(PowerMode::Active)
    }

    /// Get battery threshold
    pub fn battery_threshold(&self) -> u16 {
        self.battery_threshold.get()
    }

    /// Set battery threshold
    pub fn set_battery_threshold(&self, threshold_mv: u16) {
        self.battery_threshold.set(threshold_mv);
    }

    /// Simulate battery status (for testing)
    pub fn read_battery_status(&self) -> BatteryStatus {
        // In real implementation, this would read ADC
        BatteryStatus {
            voltage_mv: 3700, // 3.7V
            percentage: 85,
            charging: false,
            low_battery: false,
        }
    }

    /// Check if should enter low power mode
    pub fn should_enter_low_power(&self) -> bool {
        let battery = self.read_battery_status();
        battery.voltage_mv < self.battery_threshold.get()
    }
}

impl Default for PowerManager {
    fn default() -> Self {
        Self::new()
    }
}

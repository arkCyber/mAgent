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

    /// Map a battery charge percentage to the power mode the firmware should
    /// drop into, mirroring the nRF52840 `power.rs` model:
    ///
    /// * `0..=10` → [`PowerMode::DeepSleep`] (critical — preserve the last of
    ///   the charge and wait for a charge source).
    /// * `11..=25` → [`PowerMode::LowPower`] (economy — throttled peripherals,
    ///   reduced sensor cadence).
    /// * anything else → [`PowerMode::Active`] (full performance).
    ///
    /// The input is clamped to `0..=100` so a corrupt ADC reading can never
    /// select an unintended mode.
    pub fn optimize_for_battery(battery_level: u8) -> PowerMode {
        let level = battery_level.min(100);
        match level {
            0..=10 => PowerMode::DeepSleep,
            11..=25 => PowerMode::LowPower,
            _ => PowerMode::Active,
        }
    }
}

impl Default for PowerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_defaults_to_active_with_3300mv_threshold() {
        let pm = PowerManager::new();
        assert_eq!(pm.current_mode(), PowerMode::Active);
        assert_eq!(pm.battery_threshold(), 3300);
        assert_eq!(PowerManager::default().current_mode(), PowerMode::Active);
    }

    #[test]
    fn set_mode_allows_permissive_downward_sequence() {
        // Active → Idle → LowPower → DeepSleep → Active is the canonical
        // sleep-orchestration sequence and must succeed end-to-end.
        let pm = PowerManager::new();
        assert!(pm.set_mode(PowerMode::Idle).is_ok());
        assert_eq!(pm.current_mode(), PowerMode::Idle);
        assert!(pm.set_mode(PowerMode::LowPower).is_ok());
        assert_eq!(pm.current_mode(), PowerMode::LowPower);
        assert!(pm.set_mode(PowerMode::DeepSleep).is_ok());
        assert_eq!(pm.current_mode(), PowerMode::DeepSleep);
        assert!(pm.set_mode(PowerMode::Active).is_ok());
        assert_eq!(pm.current_mode(), PowerMode::Active);
    }

    #[test]
    fn noop_transition_is_allowed() {
        let pm = PowerManager::new();
        assert!(pm.set_mode(PowerMode::Active).is_ok());
        assert_eq!(pm.current_mode(), PowerMode::Active);
    }

    #[test]
    fn set_mode_rejects_upward_step_without_active() {
        // DeepSleep → Idle is an "up" step that must round-trip through
        // Active; the state machine rejects it.
        let pm = PowerManager::new();
        pm.set_mode(PowerMode::DeepSleep).unwrap();
        let err = pm.set_mode(PowerMode::Idle).unwrap_err();
        assert!(matches!(err, AgentError::InvalidStateTransition { .. }));
    }

    #[test]
    fn invalid_transition_carries_from_and_to_names() {
        let pm = PowerManager::new();
        pm.set_mode(PowerMode::LowPower).unwrap();
        let err = pm.set_mode(PowerMode::Idle).unwrap_err();
        match err {
            AgentError::InvalidStateTransition { from, to } => {
                assert_eq!(from, "LowPower");
                assert_eq!(to, "Idle");
            }
            other => panic!("expected InvalidStateTransition, got {other:?}"),
        }
    }

    #[test]
    fn convenience_entry_methods() {
        let pm = PowerManager::new();
        assert!(pm.enter_idle().is_ok());
        assert_eq!(pm.current_mode(), PowerMode::Idle);
        assert!(pm.enter_low_power().is_ok());
        assert_eq!(pm.current_mode(), PowerMode::LowPower);
        assert!(pm.enter_deep_sleep().is_ok());
        assert_eq!(pm.current_mode(), PowerMode::DeepSleep);
        assert!(pm.wake_up().is_ok());
        assert_eq!(pm.current_mode(), PowerMode::Active);
    }

    #[test]
    fn battery_threshold_get_set() {
        let pm = PowerManager::new();
        assert_eq!(pm.battery_threshold(), 3300);
        pm.set_battery_threshold(3000);
        assert_eq!(pm.battery_threshold(), 3000);
    }

    #[test]
    fn should_enter_low_power_respects_threshold() {
        let pm = PowerManager::new();
        // Simulated battery reads 3700 mV; default threshold is 3300 mV.
        assert!(!pm.should_enter_low_power());
        // Raise the threshold above the simulated reading → low power.
        pm.set_battery_threshold(4000);
        assert!(pm.should_enter_low_power());
    }

    #[test]
    fn read_battery_status_defaults() {
        let pm = PowerManager::new();
        let s = pm.read_battery_status();
        assert_eq!(s.voltage_mv, 3700);
        assert_eq!(s.percentage, 85);
        assert!(!s.charging);
        assert!(!s.low_battery);
    }

    #[test]
    fn optimize_for_battery_maps_percent_to_mode() {
        // Mirrors the nRF52840 firmware power model.
        assert_eq!(PowerManager::optimize_for_battery(0), PowerMode::DeepSleep);
        assert_eq!(PowerManager::optimize_for_battery(5), PowerMode::DeepSleep);
        assert_eq!(PowerManager::optimize_for_battery(10), PowerMode::DeepSleep);
        assert_eq!(PowerManager::optimize_for_battery(11), PowerMode::LowPower);
        assert_eq!(PowerManager::optimize_for_battery(25), PowerMode::LowPower);
        assert_eq!(PowerManager::optimize_for_battery(26), PowerMode::Active);
        assert_eq!(PowerManager::optimize_for_battery(50), PowerMode::Active);
        assert_eq!(PowerManager::optimize_for_battery(100), PowerMode::Active);
    }

    #[test]
    fn optimize_for_battery_clamps_overflow() {
        // A corrupt ADC reading above 100 must clamp to Active (never
        // underflow / wrap into a lower-power band).
        assert_eq!(PowerManager::optimize_for_battery(101), PowerMode::Active);
        assert_eq!(PowerManager::optimize_for_battery(255), PowerMode::Active);
    }
}

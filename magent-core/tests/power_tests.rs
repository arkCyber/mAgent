//! Power-management integration tests for the host-side `PowerManager`.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p magent-core --features std,power --test power_tests
//! ```
//!
//! These exercise the public power-management surface end-to-end: the mode
//! state machine (including the permitted downward sleep-orchestration
//! sequence and the rejections), the battery-threshold / low-power decision,
//! and the battery-percentage → mode map that mirrors the nRF52840 firmware
//! power model. They are the "电源管理" complement to the crypto known-answer
//! tests in `security.rs`.

#![cfg(feature = "std")]

use magent_core::power::{PowerManager, PowerMode};

#[test]
fn initial_state_is_active_with_default_threshold() {
    let pm = PowerManager::new();
    assert_eq!(pm.current_mode(), PowerMode::Active);
    assert_eq!(pm.battery_threshold(), 3300);
    assert_eq!(PowerManager::default().current_mode(), PowerMode::Active);
}

#[test]
fn sleep_orchestration_sequence_round_trips() {
    // Active → Idle → LowPower → DeepSleep → Active is the canonical
    // sleep-orchestration path and must succeed end-to-end.
    let pm = PowerManager::new();
    assert!(pm.enter_idle().is_ok());
    assert!(pm.enter_low_power().is_ok());
    assert!(pm.enter_deep_sleep().is_ok());
    assert!(pm.wake_up().is_ok());
    assert_eq!(pm.current_mode(), PowerMode::Active);
}

#[test]
fn upward_step_without_active_is_rejected() {
    // DeepSleep → Idle must round-trip through Active; the state machine
    // rejects the direct up-step.
    let pm = PowerManager::new();
    pm.set_mode(PowerMode::DeepSleep).unwrap();
    let err = pm.set_mode(PowerMode::Idle).unwrap_err();
    assert!(matches!(err, magent_core::error::AgentError::InvalidStateTransition { .. }));
}

#[test]
fn low_power_decision_respects_threshold() {
    // Simulated battery reads 3700 mV; default threshold is 3300 mV.
    let pm = PowerManager::new();
    assert!(!pm.should_enter_low_power());
    pm.set_battery_threshold(4000);
    assert!(pm.should_enter_low_power());
}

#[test]
fn battery_to_mode_map_covers_all_bands() {
    assert_eq!(PowerManager::optimize_for_battery(0), PowerMode::DeepSleep);
    assert_eq!(PowerManager::optimize_for_battery(10), PowerMode::DeepSleep);
    assert_eq!(PowerManager::optimize_for_battery(11), PowerMode::LowPower);
    assert_eq!(PowerManager::optimize_for_battery(25), PowerMode::LowPower);
    assert_eq!(PowerManager::optimize_for_battery(26), PowerMode::Active);
    assert_eq!(PowerManager::optimize_for_battery(100), PowerMode::Active);
}

#[test]
fn battery_to_mode_clamps_out_of_range_readings() {
    // A corrupt ADC reading must never select a lower-power band by wrapping.
    assert_eq!(PowerManager::optimize_for_battery(101), PowerMode::Active);
    assert_eq!(PowerManager::optimize_for_battery(255), PowerMode::Active);
}

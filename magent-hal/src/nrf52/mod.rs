//! nRF52840 HAL support — chip-agnostic trait adapters + rich simulator.
//!
//! This module groups everything nRF52840-specific:
//!
//! * [`adapter`] — `NrfGpio`, `NrfFlash`, `NrfBle`, `NrfTemperature`,
//!   `NrfPower`. These are thin host-side stubs that satisfy the
//!   chip-agnostic traits in [`crate`]. Firmware code is expected to
//!   supply its own driver-backed implementations that also satisfy
//!   those traits.
//!
//! * [`sim`] — `Nrf52Simulator` and friends. The chip-faithful desktop
//!   simulator used by tests. Includes a 1 MiB simulated flash, BLE
//!   radio, RTC, battery, TRNG, and the smartwatch-specific sensors
//!   (heart rate, SpO2, accelerometer, step counter).
//!
//! Both sub-modules are unconditionally compiled (this crate is
//! `std`-only).

/// Chip-agnostic trait adapters for nRF52840.
pub mod adapter;

/// Rich host-side nRF52840 simulator.
pub mod sim;
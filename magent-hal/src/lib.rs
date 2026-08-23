//! Chip-agnostic Hardware Abstraction Layer (HAL) for mAgent.
//!
//! This crate provides portable traits and types that abstract over the
//! concrete HAL used by a target chip (nRF52840, ESP32, etc.), plus
//! the host-side adapters that satisfy them for the two chips we
//! currently support.
//!
//! # Goals
//!
//! - Allow the same `magent-core` agent API to run on multiple chip
//!   families.
//! - Keep the `no_std` `magent-core` free of chip-specific dependencies
//!   and the rich nRF52840 simulator.
//! - Make it cheap to add a new chip (e.g. RP2040, STM32) without
//!   disturbing the public agent API.
//!
//! # Trait families
//!
//! - [`Gpio`]: digital pin control (set high/low, configure direction).
//! - [`Flash`]: byte-addressable persistent storage with erase semantics.
//! - [`Ble`]: minimal Bluetooth Low Energy send/receive operations.
//! - [`Sensor`]: read a typed value from a sensor abstraction.
//! - [`Power`]: enter low-power / wake-up control.
//!
//! All traits are deliberately tiny so each chip implementation only
//! needs to provide a few small functions.

use core::fmt::Debug;

/// Pin state (high or low).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinLevel {
    /// Logic low (0 V).
    Low,
    /// Logic high (Vcc).
    High,
}

/// Pin direction / drive mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    /// Input, no pull.
    Input,
    /// Input with internal pull-up.
    InputPullUp,
    /// Input with internal pull-down.
    InputPullDown,
    /// Output.
    Output,
}

/// A single digital GPIO pin.
pub trait Gpio {
    /// Concrete error type returned by the chip driver.
    type Error: Debug;

    /// Configure the pin direction / pull resistors.
    fn configure(&mut self, mode: PinMode) -> Result<(), Self::Error>;

    /// Drive the pin to a specific level.
    fn set_level(&mut self, level: PinLevel) -> Result<(), Self::Error>;

    /// Read the current level of the pin.
    fn read_level(&self) -> Result<PinLevel, Self::Error>;

    /// Toggle the pin and return the new level.
    fn toggle(&mut self) -> Result<PinLevel, Self::Error> {
        let new = match self.read_level()? {
            PinLevel::Low => PinLevel::High,
            PinLevel::High => PinLevel::Low,
        };
        self.set_level(new)?;
        Ok(new)
    }
}

/// Persistent storage abstraction (a thin wrapper over internal flash).
pub trait Flash {
    /// Concrete error type returned by the chip driver.
    type Error: Debug;

    /// Total usable size in bytes.
    fn capacity(&self) -> usize;

    /// Read `buf.len()` bytes from `address` into `buf`.
    fn read(&self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error>;

    /// Write `data` to `address`. The implementation is responsible for
    /// performing any required erase-before-write cycle.
    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error>;

    /// Erase a single sector that contains `address`.
    fn erase_sector(&mut self, address: u32) -> Result<(), Self::Error>;
}

/// Minimal Bluetooth Low Energy interface.
///
/// The real implementations will delegate to the chip's radio driver
/// (e.g. `nrf-softdevice` for nRF52, `esp-wifi` BLE for ESP32).
pub trait Ble {
    /// Concrete error type returned by the chip driver.
    type Error: Debug;

    /// Whether the link layer is currently connected to a peer.
    fn is_connected(&self) -> bool;

    /// Send raw bytes to the currently connected peer.
    fn send(&mut self, data: &[u8]) -> Result<usize, Self::Error>;
}

/// Sensor reading trait. The associated `Reading` type can be any
/// `Copy` value (e.g. temperature in °C as `f32`).
pub trait Sensor {
    /// Concrete reading type. Must be `Copy` so the value can be moved
    /// around freely.
    type Reading: Copy + Debug;

    /// Concrete error type returned by the chip driver.
    type Error: Debug;

    /// Take a single reading.
    fn read(&mut self) -> Result<Self::Reading, Self::Error>;
}

/// Power mode (the exact set of valid modes is chip-specific).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerProfile {
    /// Full performance, all peripherals enabled.
    Active,
    /// CPU clocked but most peripherals off.
    Idle,
    /// Reduced clock, RAM retained.
    LowPower,
    /// Deep sleep, RAM lost (where supported).
    DeepSleep,
}

/// Power management interface.
pub trait Power {
    /// Concrete error type returned by the chip driver.
    type Error: Debug;

    /// Return the current power profile.
    fn current(&self) -> PowerProfile;

    /// Switch to a different power profile.
    fn set(&mut self, profile: PowerProfile) -> Result<(), Self::Error>;
}

/// Convenience: run `f` with a temporary power profile, restoring the
/// previous profile even if `f` returns an error.
pub fn with_profile<P, F, R, E>(pm: &mut P, target: PowerProfile, f: F) -> Result<R, E>
where
    P: Power,
    F: FnOnce() -> Result<R, E>,
    E: From<P::Error>,
{
    let previous = pm.current();
    pm.set(target)?;
    let result = f();
    // Best effort restore - some targets might refuse to come back from
    // DeepSleep, in which case we propagate the error from `f` only.
    let _ = pm.set(previous);
    result
}

// ============================================================================
// Chip-specific implementations
// ============================================================================
//
// The chip-agnostic traits above are implemented by chip-specific
// sub-modules in this crate. Each sub-module is unconditionally
// compiled (this crate is `std`-only) and provides host-side stubs that
// satisfy the traits; on real hardware the firmware crates are
// expected to bring their own driver bindings that also satisfy these
// traits.

/// Minimal error type used by every adapter / simulator in this crate.
pub mod error;
pub use error::{HalError, HalResult};

/// nRF52840 support: thin trait adapters plus the rich host simulator.
///
/// * [`nrf52::adapter`] — `NrfGpio`, `NrfFlash`, `NrfBle`,
///   `NrfTemperature`, `NrfPower`. Implement the chip-agnostic traits.
/// * [`nrf52::sim`]  — `Nrf52Simulator` and friends. The chip-faithful
///   desktop simulator used by tests.
#[cfg(not(target_os = "espidf"))]
pub mod nrf52;

// Re-export the trait adapters at `magent_hal::nrf52::*` so callers can
// write `use magent_hal::nrf52::*` and pick up `NrfGpio` etc. directly.
#[cfg(not(target_os = "espidf"))]
pub use nrf52::adapter::*;
// Re-export the simulator under the same path so tests can write
// `use magent_hal::nrf52::{Nrf52Simulator, PinState, ...}`. This matches
// the old `magent_core::nrf52_hal::*` import path that existing callers
// already use.
#[cfg(not(target_os = "espidf"))]
pub use nrf52::sim::*;

/// ESP32 support (ESP32-C3 / C6 / S3 family). The stubs here are
/// always available so firmware code can compile against the chip-
/// agnostic traits; on real hardware the `magent-esp32-app` crate
/// brings its own `esp-hal` driver bindings.
pub mod esp32;

pub use esp32::*;

//! Minimal error type used by the host-side simulators in `magent-hal`.
//!
//! This is intentionally a *narrow* type — just enough variants to
//! describe the failure modes of the simulated nRF52840 peripherals
//! (storage read/write errors, GPIO out-of-range, BLE not connected,
//! etc.). It is NOT a replacement for `magent_core::error::AgentError`,
//! which is the much richer error model used by the agent runtime.
//!
//! Code that needs to bridge from a `HalError` into the agent's error
//! model should do so explicitly at the call site (see
//! `magent_core::real_tools::SimulatorExecutor` for an example of how
//! the shim handles it).

use core::fmt;

/// All failure modes produced by the simulated peripherals in
/// `magent-hal::nrf52::sim` (and the trait stubs in `magent-hal::esp32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalError {
    /// A flash address was out of range, or a sector index was invalid.
    StorageOutOfRange,
    /// A GPIO pin number was outside the chip's pin count.
    GpioOutOfRange,
    /// A GPIO operation was attempted in an incompatible direction
    /// (e.g. driving a pin configured as input).
    GpioInvalidMode,
    /// Tried to send over BLE while not connected.
    BleNotConnected,
    /// Catch-all for simulator / driver failures.
    Backend,
}

/// Convenience alias for `Result<T, HalError>`.
pub type HalResult<T> = core::result::Result<T, HalError>;

impl fmt::Display for HalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HalError::StorageOutOfRange => f.write_str("storage address out of range"),
            HalError::GpioOutOfRange => f.write_str("GPIO pin number out of range"),
            HalError::GpioInvalidMode => f.write_str("GPIO operation invalid for current pin mode"),
            HalError::BleNotConnected => f.write_str("BLE link is not connected"),
            HalError::Backend => f.write_str("HAL backend error"),
        }
    }
}

impl std::error::Error for HalError {}

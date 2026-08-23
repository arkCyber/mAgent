//! nRF52840 HAL adapter — chip-agnostic trait implementations.
//!
//! These adapters satisfy the chip-agnostic traits in the crate root
//! ([`Gpio`], [`Flash`], [`Ble`], [`Sensor`], [`Power`]). They use the
//! same in-RAM state the rest of `magent-hal` uses for host tests; on
//! real nRF52840 hardware the firmware crate is expected to provide
//! its own driver-backed implementations of the same traits.
//!
//! The rich chip-faithful simulator ([`crate::nrf52::sim`]) is kept
//! separate because many call sites already use its specific types
//! (`Nrf52Simulator`, `BleController`, `SimulatedFlash`, ...) directly.
//! Re-implementing that whole surface behind the chip-agnostic traits
//! would be a much larger change.
//!
//! Concretely, this module provides:
//!
//! * `NrfGpio` — implements [`Gpio`].
//! * `NrfFlash` — implements [`Flash`].
//! * `NrfBle` — implements [`Ble`].
//! * `NrfTemperature` — implements [`Sensor`].
//! * `NrfPower` — implements [`Power`].

use crate::{Ble, Flash, Gpio, PinLevel, PinMode, Power, PowerProfile, Sensor};

/// Error type for all nRF52840 adapter operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NrfError {
    /// The requested operation isn't supported in the current pin mode.
    InvalidMode,
    /// The flash address was out of range.
    OutOfRange,
    /// The BLE link is not currently connected.
    NotConnected,
    /// A catch-all for simulator / driver failures.
    Backend,
}

impl core::fmt::Display for NrfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NrfError::InvalidMode => f.write_str("invalid pin mode for operation"),
            NrfError::OutOfRange => f.write_str("address out of range"),
            NrfError::NotConnected => f.write_str("BLE link is not connected"),
            NrfError::Backend => f.write_str("nRF52 backend error"),
        }
    }
}

// ---------------------------------------------------------------------------
// GPIO adapter
// ---------------------------------------------------------------------------

/// GPIO adapter. On the host the state is held in RAM; on real
/// hardware this wraps an `embassy_nrf::gpio::Output` / `Input` pin.
pub struct NrfGpio {
    pin: u8,
    mode: PinMode,
    level: PinLevel,
}

impl NrfGpio {
    /// Construct an adapter for the given chip pin number
    /// (e.g. 13 for P0.13 on the nRF52840).
    pub const fn new(pin: u8) -> Self {
        Self {
            pin,
            mode: PinMode::Input,
            level: PinLevel::Low,
        }
    }

    /// Get the underlying pin number.
    pub fn pin(&self) -> u8 {
        self.pin
    }
}

impl core::fmt::Debug for NrfGpio {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NrfGpio")
            .field("pin", &self.pin)
            .field("mode", &self.mode)
            .field("level", &self.level)
            .finish()
    }
}

impl Gpio for NrfGpio {
    type Error = NrfError;

    fn configure(&mut self, mode: PinMode) -> Result<(), Self::Error> {
        self.mode = mode;
        Ok(())
    }

    fn set_level(&mut self, level: PinLevel) -> Result<(), Self::Error> {
        if matches!(
            self.mode,
            PinMode::Input | PinMode::InputPullUp | PinMode::InputPullDown
        ) {
            return Err(NrfError::InvalidMode);
        }
        self.level = level;
        Ok(())
    }

    fn read_level(&self) -> Result<PinLevel, Self::Error> {
        Ok(self.level)
    }
}

// ---------------------------------------------------------------------------
// Flash adapter
// ---------------------------------------------------------------------------

/// Flash adapter. On the host this is a RAM-backed buffer; on real
/// hardware this wraps the NVMC peripheral.
pub struct NrfFlash {
    data: std::vec::Vec<u8>,
    capacity: usize,
}

impl NrfFlash {
    /// Construct a flash adapter with the given total capacity in
    /// bytes. On real hardware the capacity is fixed by the chip
    /// (1 MiB for nRF52840).
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0xFFu8; capacity],
            capacity,
        }
    }
}

impl Default for NrfFlash {
    fn default() -> Self {
        // 1 MiB — matches the nRF52840 internal flash size.
        Self::new(1024 * 1024)
    }
}

impl core::fmt::Debug for NrfFlash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NrfFlash")
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl Flash for NrfFlash {
    type Error = NrfError;

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn read(&self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        let start = address as usize;
        let end = start + buf.len();
        if end > self.data.len() {
            return Err(NrfError::OutOfRange);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error> {
        let start = address as usize;
        let end = start + data.len();
        if end > self.data.len() {
            return Err(NrfError::OutOfRange);
        }
        // Flash can only clear bits, not set them.
        for (i, byte) in data.iter().enumerate() {
            self.data[start + i] &= *byte;
        }
        Ok(())
    }

    fn erase_sector(&mut self, address: u32) -> Result<(), Self::Error> {
        const SECTOR: usize = 4096;
        let sector = (address as usize / SECTOR) * SECTOR;
        let end = (sector + SECTOR).min(self.data.len());
        for byte in &mut self.data[sector..end] {
            *byte = 0xFF;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// BLE adapter
// ---------------------------------------------------------------------------

/// BLE adapter. On the host this is a stub; on real hardware it
/// wraps the SoftDevice BLE stack.
pub struct NrfBle {
    connected: core::sync::atomic::AtomicBool,
    tx_count: core::sync::atomic::AtomicUsize,
}

impl NrfBle {
    /// Construct a disconnected BLE adapter.
    pub const fn new() -> Self {
        Self {
            connected: core::sync::atomic::AtomicBool::new(false),
            tx_count: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Mark the link as connected. Used by the SoftDevice glue code
    /// when a connection event arrives.
    pub fn set_connected(&self, connected: bool) {
        self.connected
            .store(connected, core::sync::atomic::Ordering::Release);
    }
}

impl Default for NrfBle {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for NrfBle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NrfBle")
            .field("connected", &self.connected)
            .finish()
    }
}

impl Ble for NrfBle {
    type Error = NrfError;

    fn is_connected(&self) -> bool {
        self.connected
            .load(core::sync::atomic::Ordering::Acquire)
    }

    fn send(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if !self.is_connected() {
            return Err(NrfError::NotConnected);
        }
        self.tx_count
            .fetch_add(data.len(), core::sync::atomic::Ordering::Relaxed);
        Ok(data.len())
    }
}

// ---------------------------------------------------------------------------
// Temperature sensor adapter
// ---------------------------------------------------------------------------

/// Die-temperature sensor adapter. The nRF52840 has an internal
/// temperature channel on the SAADC.
pub struct NrfTemperature {
    base: core::cell::Cell<f32>,
}

impl NrfTemperature {
    /// Construct a sensor with the given baseline reading (°C).
    pub const fn new(base: f32) -> Self {
        Self {
            base: core::cell::Cell::new(base),
        }
    }
}

impl Default for NrfTemperature {
    fn default() -> Self {
        Self::new(25.0)
    }
}

impl core::fmt::Debug for NrfTemperature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NrfTemperature")
            .field("base", &self.base.get())
            .finish()
    }
}

impl Sensor for NrfTemperature {
    type Reading = f32;
    type Error = NrfError;

    fn read(&mut self) -> Result<Self::Reading, Self::Error> {
        Ok(self.base.get())
    }
}

// ---------------------------------------------------------------------------
// Power manager adapter
// ---------------------------------------------------------------------------

/// Power manager adapter. On the host this is a no-op; on real
/// hardware it controls the nRF52840 System ON / System OFF modes.
pub struct NrfPower {
    current: core::sync::atomic::AtomicU8,
}

impl NrfPower {
    /// Construct a power manager that starts in [`PowerProfile::Active`].
    pub const fn new() -> Self {
        Self {
            current: core::sync::atomic::AtomicU8::new(0),
        }
    }

    fn encode(p: PowerProfile) -> u8 {
        match p {
            PowerProfile::Active => 0,
            PowerProfile::Idle => 1,
            PowerProfile::LowPower => 2,
            PowerProfile::DeepSleep => 3,
        }
    }

    fn decode(v: u8) -> PowerProfile {
        match v {
            1 => PowerProfile::Idle,
            2 => PowerProfile::LowPower,
            3 => PowerProfile::DeepSleep,
            _ => PowerProfile::Active,
        }
    }
}

impl Default for NrfPower {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for NrfPower {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NrfPower")
            .field("current", &Self::decode(self.current.load(core::sync::atomic::Ordering::Acquire)))
            .finish()
    }
}

impl Power for NrfPower {
    type Error = NrfError;

    fn current(&self) -> PowerProfile {
        Self::decode(self.current.load(core::sync::atomic::Ordering::Acquire))
    }

    fn set(&mut self, profile: PowerProfile) -> Result<(), Self::Error> {
        self.current
            .store(Self::encode(profile), core::sync::atomic::Ordering::Release);
        Ok(())
    }
}
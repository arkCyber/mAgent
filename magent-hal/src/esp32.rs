//! ESP32 chip-agnostic HAL stubs.
//!
//! These provide portable types that match the chip-agnostic traits
//! in the crate root. On real ESP32-C3 / C6 / S3 hardware, the
//! `firmware/esp32-app` crate pulls in `esp-hal` / `esp-wifi` and is
//! expected to provide its own driver-backed implementations that also
//! satisfy these traits.

use crate::{Ble, Flash, Gpio, PinLevel, PinMode, Power, PowerProfile, Sensor};
use core::fmt::{self, Debug};
use std::vec::Vec;

/// A simulated ESP32 GPIO pin. Useful for desktop tests; on real hardware
/// `EspGpio` would wrap an `esp-hal::gpio::GpioPin`.
pub struct EspGpio {
    pin: u8,
    mode: PinMode,
    level: PinLevel,
}

impl EspGpio {
    /// Construct a new simulated pin. `pin` is the chip pin number
    /// (e.g. 2 for IO2, 8 for IO8).
    pub const fn new(pin: u8) -> Self {
        Self {
            pin,
            mode: PinMode::Input,
            level: PinLevel::Low,
        }
    }

    /// Get the chip pin number.
    pub fn pin(&self) -> u8 {
        self.pin
    }
}

impl Debug for EspGpio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspGpio")
            .field("pin", &self.pin)
            .field("mode", &self.mode)
            .field("level", &self.level)
            .finish()
    }
}

impl Gpio for EspGpio {
    type Error = EspError;

    fn configure(&mut self, mode: PinMode) -> Result<(), Self::Error> {
        self.mode = mode;
        Ok(())
    }

    fn set_level(&mut self, level: PinLevel) -> Result<(), Self::Error> {
        if matches!(
            self.mode,
            PinMode::Input | PinMode::InputPullUp | PinMode::InputPullDown
        ) {
            return Err(EspError::InvalidMode);
        }
        self.level = level;
        Ok(())
    }

    fn read_level(&self) -> Result<PinLevel, Self::Error> {
        Ok(self.level)
    }
}

/// A simulated ESP32 flash region.
pub struct EspFlash {
    capacity: usize,
    data: Vec<u8>,
}

impl EspFlash {
    /// Construct a simulated flash of `capacity` bytes. The real driver
    /// would map the chip's internal flash (typically 4 MiB) instead of
    /// allocating RAM.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            data: vec![0xFFu8; capacity],
        }
    }
}

impl Debug for EspFlash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspFlash")
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl Flash for EspFlash {
    type Error = EspError;

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn read(&self, address: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        let start = address as usize;
        let end = start + buf.len();
        if end > self.data.len() {
            return Err(EspError::OutOfRange);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), Self::Error> {
        let start = address as usize;
        let end = start + data.len();
        if end > self.data.len() {
            return Err(EspError::OutOfRange);
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

/// A simulated ESP32 BLE link.
pub struct EspBle {
    connected: bool,
    tx_count: usize,
}

impl EspBle {
    /// Create a new, disconnected link.
    pub const fn new() -> Self {
        Self {
            connected: false,
            tx_count: 0,
        }
    }

    /// Mark the link as connected / disconnected. On real hardware the BLE
    /// glue calls this when a connection / disconnection event arrives.
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }
}

impl Default for EspBle {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for EspBle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspBle")
            .field("connected", &self.connected)
            .field("tx_count", &self.tx_count)
            .finish()
    }
}

impl Ble for EspBle {
    type Error = EspError;

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn send(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if !self.connected {
            return Err(EspError::NotConnected);
        }
        self.tx_count += data.len();
        Ok(data.len())
    }
}

/// A simulated temperature sensor reading in degrees Celsius.
pub struct EspTemperatureSensor {
    base: f32,
}

impl EspTemperatureSensor {
    /// Create a new temperature sensor with a baseline of `base` °C.
    pub const fn new(base: f32) -> Self {
        Self { base }
    }
}

impl Debug for EspTemperatureSensor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspTemperatureSensor")
            .field("base", &self.base)
            .finish()
    }
}

impl Sensor for EspTemperatureSensor {
    type Reading = f32;
    type Error = EspError;

    fn read(&mut self) -> Result<Self::Reading, Self::Error> {
        Ok(self.base)
    }
}

/// A simulated ESP32 power manager.
pub struct EspPower {
    current: PowerProfile,
}

impl EspPower {
    /// Create a new power manager that starts in [`PowerProfile::Active`].
    pub const fn new() -> Self {
        Self {
            current: PowerProfile::Active,
        }
    }
}

impl Default for EspPower {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for EspPower {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspPower")
            .field("current", &self.current)
            .finish()
    }
}

impl Power for EspPower {
    type Error = EspError;

    fn current(&self) -> PowerProfile {
        self.current
    }

    fn set(&mut self, profile: PowerProfile) -> Result<(), Self::Error> {
        self.current = profile;
        Ok(())
    }
}

/// Generic ESP32 driver error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EspError {
    /// A pin was used in the wrong mode (e.g. driving an input).
    InvalidMode,
    /// A flash address was out of range.
    OutOfRange,
    /// Tried to send over BLE while not connected.
    NotConnected,
}

impl fmt::Display for EspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EspError::InvalidMode => f.write_str("invalid pin mode for operation"),
            EspError::OutOfRange => f.write_str("address out of range"),
            EspError::NotConnected => f.write_str("BLE link is not connected"),
        }
    }
}

impl std::error::Error for EspError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_set_and_read() {
        let mut p = EspGpio::new(2);
        p.configure(PinMode::Output).unwrap();
        p.set_level(PinLevel::High).unwrap();
        assert_eq!(p.read_level().unwrap(), PinLevel::High);
    }

    #[test]
    fn gpio_blocks_driving_input() {
        let mut p = EspGpio::new(2);
        // Default mode is Input
        assert_eq!(p.set_level(PinLevel::High), Err(EspError::InvalidMode));
    }

    #[test]
    fn flash_round_trip() {
        let mut f = EspFlash::new(1024);
        let payload = b"hello esp32";
        f.write(0, payload).unwrap();
        let mut buf = [0u8; 11];
        f.read(0, &mut buf).unwrap();
        // Flash only clears bits, but we start at 0xFF, so payload is intact.
        assert_eq!(&buf, payload);
    }

    #[test]
    fn ble_send_requires_connection() {
        let mut b = EspBle::new();
        assert!(b.send(&[1, 2, 3]).is_err());
    }

    #[test]
    fn ble_send_succeeds_when_connected() {
        let mut b = EspBle::new();
        b.set_connected(true);
        assert!(b.is_connected());
        assert_eq!(b.send(b"hello").unwrap(), 5);
        b.set_connected(false);
        assert!(!b.is_connected());
        assert!(b.send(b"x").is_err());
    }

    #[test]
    fn temperature_reading_is_stable() {
        let mut s = EspTemperatureSensor::new(25.0);
        assert_eq!(s.read().unwrap(), 25.0);
    }

    #[test]
    fn power_transitions() {
        let mut p = EspPower::new();
        p.set(PowerProfile::LowPower).unwrap();
        assert_eq!(p.current(), PowerProfile::LowPower);
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[test]
    fn esp_error_display_and_std_error() {
        assert_eq!(
            format!("{}", EspError::InvalidMode),
            "invalid pin mode for operation"
        );
        assert_eq!(format!("{}", EspError::OutOfRange), "address out of range");
        assert_eq!(
            format!("{}", EspError::NotConnected),
            "BLE link is not connected"
        );
        // Implements std::error::Error with no source.
        assert!(std::error::Error::source(&EspError::OutOfRange).is_none());
    }

    #[test]
    fn gpio_pin_and_output_write_low() {
        let mut g = EspGpio::new(5);
        assert_eq!(g.pin(), 5);
        g.configure(PinMode::Output).unwrap();
        g.set_level(PinLevel::Low).unwrap();
        assert_eq!(g.read_level().unwrap(), PinLevel::Low);
    }

    #[test]
    fn flash_bounds_erase_and_and_only() {
        let mut f = EspFlash::new(1024);
        assert_eq!(f.capacity(), 1024);
        assert_eq!(f.read(1000, &mut [0u8; 100]), Err(EspError::OutOfRange));
        assert_eq!(f.write(1000, &[0u8; 100]), Err(EspError::OutOfRange));
        // AND-only semantics (flash can only clear bits).
        f.write(0, &[0xF0]).unwrap();
        f.write(0, &[0x0F]).unwrap();
        let mut b = [0u8; 1];
        f.read(0, &mut b).unwrap();
        assert_eq!(b[0], 0x00);
        // Erase restores 0xFF.
        f.erase_sector(0).unwrap();
        f.read(0, &mut b).unwrap();
        assert_eq!(b[0], 0xFF);
    }

    #[test]
    fn power_all_profiles_roundtrip() {
        let mut p = EspPower::default();
        assert_eq!(p.current(), PowerProfile::Active);
        for profile in [
            PowerProfile::Idle,
            PowerProfile::LowPower,
            PowerProfile::DeepSleep,
            PowerProfile::Active,
        ] {
            p.set(profile).unwrap();
            assert_eq!(p.current(), profile);
        }
    }
}

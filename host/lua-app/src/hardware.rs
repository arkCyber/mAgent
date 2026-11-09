//! Hardware surface exposed to Lua as `hardware.*`.
//!
//! Lua scripts never talk to a chip driver directly. They only talk to the
//! narrow [`HardwareBackend`] trait, so the identical script runs on the host
//! simulator ([`SimHardware`]) or a real chip (e.g. the ESP32-S3 firmware's
//! `esp-hal` drivers). Argument types are deliberately plain (integers,
//! strings, byte slices) so the FFI boundary stays trivially portable.

#[cfg(not(target_os = "espidf"))]
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[cfg(not(target_os = "espidf"))]
use magent_hal::nrf52::adapter::{NrfBle, NrfFlash, NrfGpio, NrfPower, NrfTemperature};
#[cfg(not(target_os = "espidf"))]
use magent_hal::{Ble, Flash, Gpio, PinLevel, PinMode, Power, PowerProfile, Sensor};

/// Maximum bytes a single `hardware.flash_read` may request.
///
/// The length comes from the script, so the Lua bindings clamp it to this
/// before calling the backend: a negative Lua integer would otherwise arrive
/// as `usize::MAX` (through the binding's `as usize` cast) and the backend's
/// `vec![0u8; len]` would abort the process with a capacity overflow. One
/// flash sector — the largest granularity the region is addressed in.
pub const MAX_FLASH_READ_LEN: usize = 4096;

/// Maximum bytes a single `hardware.i2c_read` may request. Matches the
/// `MAX_I2C_LEN` bound the write paths already enforce.
pub const MAX_I2C_READ_LEN: usize = 256;

/// Maximum bytes a single `hardware.i2c_write` may send.
///
/// The payload comes from the script and is written to a register file
/// addressed by an 8-bit offset, so `reg.wrapping_add(off as u8)` folds any
/// offset past 255 back onto earlier registers: a longer payload silently
/// overwrites the bytes it just wrote and the caller is told the write
/// succeeded. The `mlua` binding already rejects oversized payloads with a
/// clearer message; this is the shared backstop so the `piccolo` binding
/// (which the ESP32-S3 runs on-device) cannot corrupt state either.
pub const MAX_I2C_WRITE_LEN: usize = 256;

/// Maximum bytes a single `hardware.ble_send` may hand to the link.
pub const MAX_PAYLOAD_LEN: usize = 4096;

/// Maximum combined `agent.reason(context, prompt)` text, in bytes.
pub const MAX_REASON_LEN: usize = 2048;

/// Shared handle to a [`HardwareBackend`] (interior-mutability, engine-agnostic).
///
/// A [`Mutex`] gives interior mutability; a poisoned lock is surfaced as an
/// error, never a panic.
pub type SharedHardware = Arc<Mutex<dyn HardwareBackend>>;

/// A hardware action the Lua `hardware.*` table can perform.
///
/// A chip backend (host simulator, or firmware `esp-hal` drivers) implements
/// this trait. Returning `Err(String)` propagates back to the Lua caller as a
/// runtime error — it never panics.
pub trait HardwareBackend: Send {
    /// Drive `pin` high (`level != 0`) or low (`level == 0`).
    fn gpio_write(&mut self, pin: u8, level: u8) -> std::result::Result<(), String>;
    /// Read a pin's current level as `1` (high) or `0` (low).
    fn gpio_read(&mut self, pin: u8) -> std::result::Result<u8, String>;
    /// Take one reading from a named sensor (e.g. `"temp"`, `"battery"`).
    fn sensor_read(&mut self, name: &str) -> std::result::Result<f64, String>;
    /// Read `len` bytes at `address` from persistent storage.
    fn flash_read(&mut self, address: u32, len: usize) -> std::result::Result<Vec<u8>, String>;
    /// Write `data` at `address`. The backend applies erase-before-write rules.
    fn flash_write(&mut self, address: u32, data: &[u8]) -> std::result::Result<(), String>;
    /// Erase the flash sector containing `address` (sets bytes to 0xFF).
    fn flash_erase_sector(&mut self, address: u32) -> std::result::Result<(), String>;
    /// Read `len` bytes from I2C device `addr`, starting at register `reg`
    /// (sequential read).
    fn i2c_read(&mut self, addr: u8, reg: u8, len: usize) -> std::result::Result<Vec<u8>, String>;
    /// A combined I2C transaction: write `tx` to `addr` at `reg`, then read
    /// `rx_len` bytes back from `addr` at `reg` — the common register-read
    /// pattern. Provided as a default so implementors only need `i2c_write`
    /// and `i2c_read`.
    fn i2c_transfer(
        &mut self,
        addr: u8,
        reg: u8,
        tx: &[u8],
        rx_len: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        self.i2c_write(addr, reg, tx)?;
        self.i2c_read(addr, reg, rx_len)
    }
    /// Read a raw voltage (volts) from an ADC channel / pin.
    fn adc_read(&mut self, pin: u8) -> std::result::Result<f64, String>;
    /// Write `data` to I2C device `addr`, starting at register `reg`
    /// (sequential write).
    fn i2c_write(&mut self, addr: u8, reg: u8, data: &[u8]) -> std::result::Result<(), String>;
    /// Set a PWM duty (0-100 %) on a channel / pin.
    fn pwm_set(&mut self, pin: u8, duty: u8) -> std::result::Result<(), String>;
    /// Send a BLE payload.
    fn ble_send(&mut self, data: &[u8]) -> std::result::Result<(), String>;
    /// Request a power profile (`0` Active, `1` Idle, `2` LowPower, `3`
    /// DeepSleep).
    fn power_set(&mut self, profile: u8) -> std::result::Result<(), String>;
}

/// Host-side [`HardwareBackend`] built on `magent-hal`'s RAM-backed nRF52840
/// adapters. Pins are created lazily on first access. Host-only: the ESP32-S3
/// firmware uses `Esp32Hardware` instead.
#[cfg(not(target_os = "espidf"))]
pub struct SimHardware {
    pins: BTreeMap<u8, NrfGpio>,
    flash: NrfFlash,
    /// I2C register file: `(device address, register)` → byte value.
    i2c: BTreeMap<(u8, u8), u8>,
    /// ADC voltage (volts) per channel/pin.
    adc: BTreeMap<u8, f64>,
    /// PWM duty per channel/pin: pin → duty (0-100 %).
    pwm: BTreeMap<u8, u8>,
    ble: NrfBle,
    temp: NrfTemperature,
    power: NrfPower,
    /// Highest flash address ever written (mirrors the ESP32 NVS backend,
    /// which truncates a read to the stored blob length).
    flash_written_end: u32,
}

#[cfg(not(target_os = "espidf"))]
impl Default for SimHardware {
    fn default() -> Self {
        // The host BLE stub starts connected so `ble_send` succeeds; on real
        // hardware the link comes up through the driver's connection event.
        let ble = NrfBle::default();
        ble.set_connected(true);
        Self {
            pins: BTreeMap::new(),
            flash: NrfFlash::default(),
            i2c: BTreeMap::new(),
            adc: BTreeMap::new(),
            pwm: BTreeMap::new(),
            ble,
            temp: NrfTemperature::default(),
            power: NrfPower::default(),
            flash_written_end: 0,
        }
    }
}

#[cfg(not(target_os = "espidf"))]
impl SimHardware {
    /// Configure the simulated die-temperature baseline in °C. Useful for a
    /// demo or test that needs to exercise the `agent.reason()` path.
    pub fn with_temperature(mut self, base: f32) -> Self {
        self.temp = NrfTemperature::new(base);
        self
    }

    /// Read the stored PWM duty (0-100 %) for `pin`. Host sim only — exposes
    /// state so tests can assert a `pwm_set` took effect.
    pub fn pwm_duty(&self, pin: u8) -> u8 {
        self.pwm.get(&pin).copied().unwrap_or(0)
    }

    /// Set the simulated ADC reading (volts) for `pin`. Host sim only.
    pub fn set_adc(&mut self, pin: u8, volts: f64) {
        self.adc.insert(pin, volts);
    }
}

#[cfg(not(target_os = "espidf"))]
impl HardwareBackend for SimHardware {
    fn gpio_write(&mut self, pin: u8, level: u8) -> std::result::Result<(), String> {
        let gpio = self.pins.entry(pin).or_insert_with(|| NrfGpio::new(pin));
        gpio.configure(PinMode::Output)
            .map_err(|e| format!("gpio configure: {e}"))?;
        let target = if level == 0 {
            PinLevel::Low
        } else {
            PinLevel::High
        };
        gpio.set_level(target).map_err(|e| format!("gpio set: {e}"))
    }

    fn gpio_read(&mut self, pin: u8) -> std::result::Result<u8, String> {
        let gpio = self.pins.entry(pin).or_insert_with(|| NrfGpio::new(pin));
        gpio.configure(PinMode::Input)
            .map_err(|e| format!("gpio configure: {e}"))?;
        let level = gpio.read_level().map_err(|e| format!("gpio read: {e}"))?;
        Ok(match level {
            PinLevel::High => 1,
            PinLevel::Low => 0,
        })
    }

    fn sensor_read(&mut self, name: &str) -> std::result::Result<f64, String> {
        // A realistic simulated sensor surface matching both the
        // `magent-core` tool names and the firmware's substring aliases
        // (`lua_task.rs` matches "temp"/"die", "mem"/"heap"/"sram"). A
        // script built for the chip must therefore also run here.
        let n = name.to_ascii_lowercase();
        if n.contains("temp") || n.contains("die") {
            Ok(self.temp.read().map_err(|e| format!("temp read: {e}"))? as f64)
        } else if n.contains("mem") || n.contains("heap") || n.contains("sram") {
            Ok(128_000.0) // bytes of free heap
        } else if n.contains("heart") || n == "hr" || n.contains("pulse") {
            Ok(72.0)
        } else if n.contains("hrv") {
            Ok(55.0)
        } else if n.contains("stress") {
            Ok(0.3)
        } else if n.contains("glucose") {
            Ok(95.0)
        } else if n.contains("battery") {
            Ok(3.70) // nominal cell voltage, volts
        } else if n.contains("step") {
            Ok(1200.0)
        } else {
            Err(format!("unknown sensor: {name}"))
        }
    }

    fn flash_read(&mut self, address: u32, len: usize) -> std::result::Result<Vec<u8>, String> {
        // Mirror the device backend: a read past the written data returns
        // only the stored prefix, not `0xFF` filler. Scripts that compare a
        // read-back against the bytes they wrote (e.g. `selftest.lua`) rely
        // on this. The host simulator tracks the highest written address;
        // reads beyond it are trimmed exactly like the ESP32 NVS-backed
        // backend trims to the stored blob.
        let start = address as u64;
        // Out-of-region reads are hardware errors, as on the chip.
        if start >= self.flash.capacity() as u64 {
            return Err("flash read: out of range".to_string());
        }
        let requested_end = start + len as u64;
        let written_end = u64::from(self.flash_written_end);
        let end = requested_end
            .min(written_end.max(start))
            .min(self.flash.capacity() as u64);
        let n = (end.saturating_sub(start)) as usize;
        let mut buf = vec![0u8; n];
        if n > 0 {
            self.flash
                .read(address, &mut buf)
                .map_err(|e| format!("flash read: {e}"))?;
        }
        Ok(buf)
    }

    fn flash_write(&mut self, address: u32, data: &[u8]) -> std::result::Result<(), String> {
        self.flash
            .write(address, data)
            .map_err(|e| format!("flash write: {e}"))?;
        let end = address.saturating_add(data.len() as u32);
        self.flash_written_end = self.flash_written_end.max(end);
        Ok(())
    }

    fn flash_erase_sector(&mut self, address: u32) -> std::result::Result<(), String> {
        self.flash
            .erase_sector(address)
            .map_err(|e| format!("flash erase: {e}"))?;
        // Erasing a sector resets the written extent to that sector's start
        // so a later read does not resurrect stale data.
        const SECTOR: u32 = 4096;
        let sector_start = (address / SECTOR) * SECTOR;
        if self.flash_written_end >= sector_start {
            self.flash_written_end = sector_start;
        }
        Ok(())
    }

    fn i2c_read(&mut self, addr: u8, reg: u8, len: usize) -> std::result::Result<Vec<u8>, String> {
        // Simulated sequential read over a RAM register file. Uninitialized
        // registers read as 0x00 (no error), matching many I2C sensor
        // power-on defaults.
        let mut out = Vec::with_capacity(len);
        for off in 0..len {
            let key = (addr, reg.wrapping_add(off as u8));
            out.push(self.i2c.get(&key).copied().unwrap_or(0));
        }
        Ok(out)
    }

    fn adc_read(&mut self, pin: u8) -> std::result::Result<f64, String> {
        // Uninitialized channels read 0.0 V (no error).
        Ok(self.adc.get(&pin).copied().unwrap_or(0.0))
    }

    fn i2c_write(&mut self, addr: u8, reg: u8, data: &[u8]) -> std::result::Result<(), String> {
        if data.len() > MAX_I2C_WRITE_LEN {
            return Err(format!(
                "i2c write: payload {} exceeds maximum {MAX_I2C_WRITE_LEN}",
                data.len()
            ));
        }
        for (off, &byte) in data.iter().enumerate() {
            let key = (addr, reg.wrapping_add(off as u8));
            self.i2c.insert(key, byte);
        }
        Ok(())
    }

    fn pwm_set(&mut self, pin: u8, duty: u8) -> std::result::Result<(), String> {
        // Clamp duty to a valid 0-100 % range before storing.
        let duty = duty.min(100);
        self.pwm.insert(pin, duty);
        Ok(())
    }

    fn ble_send(&mut self, data: &[u8]) -> std::result::Result<(), String> {
        self.ble.send(data).map_err(|e| format!("ble send: {e}"))?;
        Ok(())
    }

    fn power_set(&mut self, profile: u8) -> std::result::Result<(), String> {
        let target = match profile {
            1 => PowerProfile::Idle,
            2 => PowerProfile::LowPower,
            3 => PowerProfile::DeepSleep,
            _ => PowerProfile::Active,
        };
        self.power
            .set(target)
            .map_err(|e| format!("power set: {e}"))
    }
}

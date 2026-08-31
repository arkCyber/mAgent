//! Hardware interface stubs for mAgent
//!
//! This module provides stub implementations for hardware interfaces
//! that would be implemented using embedded-hal traits in real hardware.

use crate::error::{AgentError, Result};
#[allow(unused_imports)]
use heapless::{String, Vec};

/// I2C sensor interface
pub struct I2cSensor {
    #[allow(dead_code)]
    address: u8,
    initialized: bool,
}

impl I2cSensor {
    /// Create a new I2C sensor
    pub fn new(address: u8) -> Self {
        Self {
            address,
            initialized: false,
        }
    }

    /// Initialize the sensor
    pub fn init(&mut self) -> Result<()> {
        // In real implementation, this would:
        // 1. Configure I2C bus
        // 2. Send initialization commands to sensor
        // 3. Verify sensor response

        self.initialized = true;
        Ok(())
    }

    /// Read sensor data
    pub fn read(&self, register: u8) -> Result<Vec<u8, 8>> {
        if !self.initialized {
            return Err(AgentError::SensorReadFailed {
                sensor: "I2C",
                reason: crate::error::SensorError::NotInitialized,
            });
        }

        // In real implementation, this would:
        // 1. Send register address via I2C
        // 2. Read data from sensor
        // 3. Return parsed data

        // Simulate reading temperature sensor
        let mut data = Vec::new();
        if register == 0x00 {
            // Temperature register
            let _ = data.push(25); // 25°C
            let _ = data.push(5); // 0.5°C
        }

        Ok(data)
    }

    /// Write to sensor register
    pub fn write(&self, _register: u8, _value: u8) -> Result<()> {
        if !self.initialized {
            return Err(AgentError::SensorReadFailed {
                sensor: "I2C",
                reason: crate::error::SensorError::NotInitialized,
            });
        }

        // In real implementation, this would:
        // 1. Send register address and value via I2C
        // 2. Verify write completion

        Ok(())
    }
}

/// SPI sensor interface
pub struct SpiSensor {
    #[allow(dead_code)]
    cs_pin: u8,
    initialized: bool,
}

impl SpiSensor {
    /// Create a new SPI sensor
    pub fn new(cs_pin: u8) -> Self {
        Self {
            cs_pin,
            initialized: false,
        }
    }

    /// Initialize the sensor
    pub fn init(&mut self) -> Result<()> {
        // In real implementation, this would:
        // 1. Configure SPI bus
        // 2. Configure CS pin
        // 3. Send initialization commands

        self.initialized = true;
        Ok(())
    }

    /// Read sensor data
    pub fn read(&self, register: u8) -> Result<Vec<u8, 8>> {
        if !self.initialized {
            return Err(AgentError::SensorReadFailed {
                sensor: "I2C",
                reason: crate::error::SensorError::NotInitialized,
            });
        }

        // In real implementation, this would:
        // 1. Assert CS pin
        // 2. Send register address via SPI
        // 3. Read data via SPI
        // 4. Deassert CS pin

        // Simulate reading accelerometer
        let mut data = Vec::new();
        if register == 0x01 {
            // X-axis
            let _ = data.push(0x01);
            let _ = data.push(0x00);
        }

        Ok(data)
    }

    /// Write to sensor register
    pub fn write(&self, _register: u8, _value: u8) -> Result<()> {
        if !self.initialized {
            return Err(AgentError::SensorReadFailed {
                sensor: "I2C",
                reason: crate::error::SensorError::NotInitialized,
            });
        }

        // In real implementation, this would:
        // 1. Assert CS pin
        // 2. Send register and value via SPI
        // 3. Deassert CS pin

        Ok(())
    }
}

/// GPIO interface
pub struct GpioPin {
    #[allow(dead_code)]
    pin: u8,
    direction: GpioDirection,
    state: GpioState,
}

/// GPIO direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    /// Hi-Z input, no pull.
    Input,
    /// Push-pull output.
    Output,
}

/// GPIO state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioState {
    /// Logic-low (0 V).
    Low,
    /// Logic-high (Vcc).
    High,
}

impl GpioPin {
    /// Create a new GPIO pin
    pub fn new(pin: u8, direction: GpioDirection) -> Self {
        Self {
            pin,
            direction,
            state: GpioState::Low,
        }
    }

    /// Set pin state (output only)
    pub fn set(&mut self, state: GpioState) -> Result<()> {
        if self.direction != GpioDirection::Output {
            return Err(AgentError::ConfigurationError {
                field: "gpio_direction",
                reason: crate::error::ConfigError::TypeMismatch,
            });
        }

        // In real implementation, this would:
        // 1. Configure pin as output
        // 2. Set pin level using embedded-hal

        self.state = state;
        Ok(())
    }

    /// Read pin state
    pub fn read(&self) -> Result<GpioState> {
        // In real implementation, this would:
        // 1. Configure pin as input
        // 2. Read pin level using embedded-hal

        Ok(self.state)
    }

    /// Toggle pin state
    pub fn toggle(&mut self) -> Result<()> {
        let new_state = match self.state {
            GpioState::Low => GpioState::High,
            GpioState::High => GpioState::Low,
        };
        self.set(new_state)
    }
}

/// Temperature sensor (I2C)
pub struct TemperatureSensor {
    i2c: I2cSensor,
}

impl TemperatureSensor {
    /// Create a new temperature sensor
    pub fn new(address: u8) -> Self {
        Self {
            i2c: I2cSensor::new(address),
        }
    }

    /// Initialize sensor
    pub fn init(&mut self) -> Result<()> {
        self.i2c.init()
    }

    /// Read temperature in Celsius
    pub fn read_temperature(&self) -> Result<f32> {
        let data = self.i2c.read(0x00)?;

        // Parse temperature data (format depends on sensor)
        // For simulation, parse the data from I2C read
        if data.len() >= 2 {
            let temp = data[0] as f32 + (data[1] as f32) / 10.0;
            Ok(temp)
        } else {
            Ok(25.5)
        }
    }
}

/// Accelerometer sensor (I2C/SPI)
pub struct Accelerometer {
    spi: SpiSensor,
}

impl Accelerometer {
    /// Create a new accelerometer
    pub fn new(cs_pin: u8) -> Self {
        Self {
            spi: SpiSensor::new(cs_pin),
        }
    }

    /// Initialize sensor
    pub fn init(&mut self) -> Result<()> {
        self.spi.init()
    }

    /// Read acceleration data
    pub fn read_acceleration(&self) -> Result<(f32, f32, f32)> {
        // Read X, Y, Z axes
        let x_data = self.spi.read(0x01)?;
        let y_data = self.spi.read(0x02)?;
        let z_data = self.spi.read(0x03)?;

        // Parse acceleration data (format depends on sensor)
        // For simulation, parse the data from SPI read
        let x = if x_data.len() >= 2 {
            let raw = (x_data[0] as i16) << 8 | x_data[1] as i16;
            raw as f32 / 16384.0 // Convert to g
        } else {
            0.1
        };

        let y = if y_data.len() >= 2 {
            let raw = (y_data[0] as i16) << 8 | y_data[1] as i16;
            raw as f32 / 16384.0
        } else {
            0.2
        };

        let z = if z_data.len() >= 2 {
            let raw = (z_data[0] as i16) << 8 | z_data[1] as i16;
            raw as f32 / 16384.0
        } else {
            9.8
        };

        Ok((x, y, z))
    }
}

/// Humidity sensor (I2C)
pub struct HumiditySensor {
    i2c: I2cSensor,
}

impl HumiditySensor {
    /// Create a new humidity sensor
    pub fn new(address: u8) -> Self {
        Self {
            i2c: I2cSensor::new(address),
        }
    }

    /// Initialize sensor
    pub fn init(&mut self) -> Result<()> {
        self.i2c.init()
    }

    /// Read humidity percentage
    pub fn read_humidity(&self) -> Result<f32> {
        let data = self.i2c.read(0x01)?;

        // Parse humidity data
        if !data.is_empty() {
            Ok(data[0] as f32)
        } else {
            Ok(65.0)
        }
    }
}

/// Pressure sensor (I2C)
pub struct PressureSensor {
    i2c: I2cSensor,
}

impl PressureSensor {
    /// Create a new pressure sensor
    pub fn new(address: u8) -> Self {
        Self {
            i2c: I2cSensor::new(address),
        }
    }

    /// Initialize sensor
    pub fn init(&mut self) -> Result<()> {
        self.i2c.init()
    }

    /// Read pressure in hPa
    pub fn read_pressure(&self) -> Result<f32> {
        let data = self.i2c.read(0x02)?;

        // Parse pressure data (24-bit value)
        if data.len() >= 2 {
            let pressure = (data[0] as u32) << 8 | data[1] as u32;
            Ok(pressure as f32)
        } else {
            Ok(1013.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SensorError;

    #[test]
    fn i2c_sensor_read_fails_before_init() {
        let s = I2cSensor::new(0x40);
        let err = s.read(0x00).unwrap_err();
        assert!(matches!(
            err,
            AgentError::SensorReadFailed {
                sensor: "I2C",
                reason: SensorError::NotInitialized
            }
        ));
        assert!(s.write(0x01, 0xFF).is_err());
    }

    #[test]
    fn i2c_sensor_init_then_read_and_write() {
        let mut s = I2cSensor::new(0x40);
        assert!(s.init().is_ok());
        let data = s.read(0x00).unwrap();
        assert_eq!(&data[..], &[25, 5]); // 25.0°C + 0.5°C fraction
                                         // Unknown register returns an empty (but successful) read.
        assert!(s.read(0x7F).unwrap().is_empty());
        assert!(s.write(0x01, 0xFF).is_ok());
    }

    #[test]
    fn spi_sensor_read_fails_before_init() {
        let s = SpiSensor::new(5);
        assert!(s.read(0x01).is_err());
    }

    #[test]
    fn spi_sensor_init_then_read_axis() {
        let mut s = SpiSensor::new(5);
        assert!(s.init().is_ok());
        let data = s.read(0x01).unwrap();
        assert_eq!(&data[..], &[0x01, 0x00]); // X-axis raw
        assert!(s.write(0x00, 0x00).is_ok());
    }

    #[test]
    fn gpio_input_pin_rejects_set() {
        let mut pin = GpioPin::new(2, GpioDirection::Input);
        let err = pin.set(GpioState::High).unwrap_err();
        assert!(matches!(err, AgentError::ConfigurationError { .. }));
        // Reads work regardless of direction.
        assert_eq!(pin.read().unwrap(), GpioState::Low);
    }

    #[test]
    fn gpio_output_set_read_toggle() {
        let mut pin = GpioPin::new(2, GpioDirection::Output);
        assert_eq!(pin.read().unwrap(), GpioState::Low);
        assert!(pin.set(GpioState::High).is_ok());
        assert_eq!(pin.read().unwrap(), GpioState::High);
        assert!(pin.toggle().is_ok());
        assert_eq!(pin.read().unwrap(), GpioState::Low);
        assert!(pin.toggle().is_ok());
        assert_eq!(pin.read().unwrap(), GpioState::High);
    }

    #[test]
    fn temperature_sensor_reports_celsius() {
        let mut t = TemperatureSensor::new(0x48);
        assert!(t.init().is_ok());
        let temp = t.read_temperature().unwrap();
        assert!((temp - 25.5).abs() < 1e-6);
    }

    #[test]
    fn accelerometer_reports_axis_values() {
        let mut a = Accelerometer::new(4);
        assert!(a.init().is_ok());
        let (x, y, z) = a.read_acceleration().unwrap();
        // X-axis raw 0x0100 = 256 → 256/16384 g.
        assert!((x - (256.0 / 16384.0)).abs() < 1e-6);
        // Y/Z registers are not simulated → fallback values.
        assert_eq!(y, 0.2);
        assert_eq!(z, 9.8);
    }

    #[test]
    fn humidity_sensor_reports_default() {
        let mut h = HumiditySensor::new(0x27);
        assert!(h.init().is_ok());
        // Register 0x01 isn't filled by the I2C stub → fallback 65.0.
        assert_eq!(h.read_humidity().unwrap(), 65.0);
    }

    #[test]
    fn pressure_sensor_reports_default() {
        let mut p = PressureSensor::new(0x76);
        assert!(p.init().is_ok());
        // Register 0x02 isn't filled by the I2C stub → fallback 1013.0.
        assert_eq!(p.read_pressure().unwrap(), 1013.0);
    }
}

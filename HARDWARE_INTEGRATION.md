# Hardware Integration Guide

## Overview

This document describes the hardware integration interfaces for mAgent, including I2C, SPI, and GPIO interfaces for embedded systems.

## Hardware Interfaces

### I2C Sensor Interface

The `I2cSensor` provides a generic interface for I2C-based sensors.

```rust
use magent_core::hardware::I2cSensor;

// Create I2C sensor with address
let mut sensor = I2cSensor::new(0x48);

// Initialize sensor
sensor.init()?;

// Read from register
let data = sensor.read(0x00)?;
```

**Supported Sensors**:
- TemperatureSensor (I2C address 0x48)
- HumiditySensor (I2C address 0x40)
- PressureSensor (I2C address 0x50)

### SPI Sensor Interface

The `SpiSensor` provides a generic interface for SPI-based sensors.

```rust
use magent_core::hardware::SpiSensor;

// Create SPI sensor with CS pin
let mut sensor = SpiSensor::new(5);

// Initialize sensor
sensor.init()?;

// Read from register
let data = sensor.read(0x01)?;
```

**Supported Sensors**:
- Accelerometer (CS pin 5)

### GPIO Interface

The `GpioPin` provides GPIO control with direction and state management.

```rust
use magent_core::hardware::{GpioPin, GpioDirection, GpioState};

// Create GPIO pin as output
let mut pin = GpioPin::new(10, GpioDirection::Output);

// Set pin state
pin.set(GpioState::High)?;

// Read pin state
let state = pin.read()?;
```

## Sensor-Specific Interfaces

### Temperature Sensor

```rust
use magent_core::hardware::TemperatureSensor;

let mut sensor = TemperatureSensor::new(0x48);
sensor.init()?;
let temp = sensor.read_temperature()?; // Returns f32 in Celsius
```

### Accelerometer

```rust
use magent_core::hardware::Accelerometer;

let mut sensor = Accelerometer::new(5);
sensor.init()?;
let (x, y, z) = sensor.read_acceleration()?; // Returns (f32, f32, f32) in g
```

### Humidity Sensor

```rust
use magent_core::hardware::HumiditySensor;

let mut sensor = HumiditySensor::new(0x40);
sensor.init()?;
let humidity = sensor.read_humidity()?; // Returns f32 percentage
```

### Pressure Sensor

```rust
use magent_core::hardware::PressureSensor;

let mut sensor = PressureSensor::new(0x50);
sensor.init()?;
let pressure = sensor.read_pressure()?; // Returns f32 in hPa
```

## Real Hardware Integration

To integrate with real hardware, implement the `embedded-hal` traits:

### I2C Implementation

```rust
use embedded_hal::i2c::I2c;

impl I2c for I2cSensor {
    type Error = AgentError;

    fn read(&mut self, addr: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        // Real I2C read implementation
        Ok(())
    }

    fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), Self::Error> {
        // Real I2C write implementation
        Ok(())
    }
}
```

### SPI Implementation

```rust
use embedded_hal::spi::SpiDevice;

impl SpiDevice for SpiSensor {
    type Error = AgentError;

    fn transaction(&mut self, operations: &mut [embedded_hal::spi::Operation<'_, u8>]) -> Result<(), Self::Error> {
        // Real SPI transaction implementation
        Ok(())
    }
}
```

### GPIO Implementation

```rust
use embedded_hal::digital::{OutputPin, InputPin};

impl OutputPin for GpioPin {
    type Error = AgentError;

    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.state = GpioState::Low;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.state = GpioState::High;
        Ok(())
    }
}

impl InputPin for GpioPin {
    type Error = AgentError;

    fn is_high(&self) -> Result<bool, Self::Error> {
        Ok(self.state == GpioState::High)
    }

    fn is_low(&self) -> Result<bool, Self::Error> {
        Ok(self.state == GpioState::Low)
    }
}
```

## Error Handling

All hardware operations return `Result<T, AgentError>` with specific error types:

- `SensorError::NotInitialized` - Sensor not initialized
- `SensorError::Timeout` - Sensor read timeout
- `SensorError::NotAvailable` - Sensor not available
- `SensorError::CalibrationFailed` - Sensor calibration failed
- `SensorError::InvalidValue` - Invalid sensor value

## Testing

### Simulation Mode

The current implementation provides simulation mode for testing without hardware:

```rust
let mut sensor = TemperatureSensor::new(0x48);
sensor.init()?;
let temp = sensor.read_temperature()?; // Returns simulated value
```

### Real Hardware Testing

To test with real hardware:

1. Configure the I2C/SPI bus for your microcontroller
2. Connect sensors to appropriate pins
3. Initialize sensors with correct addresses
4. Read sensor data

## Configuration

### nRF52840 Configuration

For nRF52840 microcontroller:

```rust
use embassy_nrf::gpio::Level;
use embassy_nrf::twim::Twim;
use embassy_nrf::spim::Spim;

// I2C configuration
let i2c = Twim::new(p.TWIM0, Irqs, p.P0_11, p.P0_12, Config::default());

// SPI configuration
let spi = Spim::new(p.SPIM0, Irqs, p.P0_13, p.P0_14, p.P0_15, p.P0_16, Config::default());

// GPIO configuration
let pin = Output::new(p.P0_17, Level::Low, OutputDrive::Standard);
```

## Performance Considerations

- I2C operations: ~100-400 kHz
- SPI operations: ~1-10 MHz
- GPIO operations: ~10-100 ns
- Sensor read latency: 1-10 ms depending on sensor

## Troubleshooting

### Common Issues

1. **Sensor not responding**
   - Check I2C/SPI address
   - Verify wiring connections
   - Ensure sensor is powered

2. **Invalid readings**
   - Check sensor calibration
   - Verify register addresses
   - Check data format

3. **Timeout errors**
   - Increase timeout values
   - Check bus speed
   - Verify interrupt configuration

## Future Enhancements

- DMA support for high-speed transfers
- Interrupt-driven sensor reading
- Sensor fusion algorithms
- Auto-calibration routines
- Low-power sensor modes

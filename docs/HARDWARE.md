# Hardware Integration Guide

## Supported Hardware

### Primary Platform: nRF52840

The nRF52840 is the primary target platform for mAgent.

#### Specifications
- **MCU**: ARM Cortex-M4F @ 64 MHz
- **Flash**: 1 MB
- **RAM**: 256 KB
- **Wireless**: BLE 5.3, Thread, Zigbee, IEEE 802.15.4
- **Security**: ARM TrustZone CryptoCell-310
- **Peripherals**: USB 2.0, SPI, I2C, I2S, QSPI, PDM, ADC, PWM

#### Development Boards
- nRF52840 Development Kit (DK)
- nRF52840 Dongle (PCA10059)
- Custom nRF52840 boards

---

## Pin Configuration

### Default Pin Mapping

| Function | Pin | Description |
|----------|-----|-------------|
| LED1 | P0.13 | Status LED (red) |
| LED2 | P0.14 | Status LED (green) |
| LED3 | P0.15 | Status LED (blue) |
| LED4 | P0.16 | Status LED (active) |
| Button1 | P0.11 | User button 1 |
| Button2 | P0.12 | User button 2 |
| Button3 | P0.24 | User button 3 |
| Button4 | P0.25 | User button 4 |
| UART TX | P0.06 | UART transmit |
| UART RX | P0.08 | UART receive |
| I2C SDA | P0.26 | I2C data |
| I2C SCL | P0.27 | I2C clock |
| SPI SCK | P0.15 | SPI clock |
| SPI MOSI | P0.13 | SPI MOSI |
| SPI MISO | P0.14 | SPI MISO |
| ADC0 | P0.02 | ADC channel 0 |
| ADC1 | P0.03 | ADC channel 1 |
| ADC2 | P0.04 | ADC channel 2 |
| ADC3 | P0.05 | ADC channel 3 |

### LED Indicators

- **Red (P0.13)**: Error state
- **Green (P0.14)**: Normal operation
- **Blue (P0.15)**: Processing/Thinking
- **Active (P0.16)**: Agent active

---

## Sensor Integration

### Temperature Sensor (Internal)

The nRF52840 has an internal temperature sensor.

#### Configuration
```rust
use embassy_nrf::saadc::Saadc;

let mut saadc = Saadc::new(p.SAADC, Irqs);
let mut temp_input = saadc.configure_input(
    &p.P0_02,  // Use any available pin
    saadc::InputConfig::default()
);
```

#### Reading Temperature
```rust
let temp = temp_input.read().await;
let temp_c = saadc::temp_to_celsius(temp);
```

### External Sensors

#### I2C Sensors

**Supported I2C Sensors:**
- BMP280 (Temperature, Pressure)
- BME280 (Temperature, Humidity, Pressure)
- MPU6050 (Accelerometer, Gyroscope)
- LIS3DH (Accelerometer)
- BH1750 (Light)

**Example: BMP280**
```rust
use embassy_nrf::twim::Twim;
use embedded_hal_async::i2c::I2c;

let mut i2c = Twim::new(
    p.TWIM0,
    Irqs,
    p.P0_26,  // SDA
    p.P0_27,  // SCL
    Config::default()
);

// Read BMP280
let mut bmp280 = Bmp280::new(i2c);
let (temp, pressure) = bmp280.read().await?;
```

#### SPI Sensors

**Supported SPI Sensors:**
- SD Card (via SPI)
- External Flash
- Display controllers

**Example: SD Card**
```rust
use embassy_nrf::spim::Spim;
use embedded_hal_async::spi::SpiDevice;

let mut spi = Spim::new(
    p.SPIM0,
    Irqs,
    p.P0_15,  // SCK
    p.P0_13,  // MOSI
    p.P0_14,  // MISO,
    Config::default()
);

let mut sd = SdCard::new(spi, cs_pin);
sd.init().await?;
```

---

## GPIO Configuration

### Output Pins

```rust
use embassy_nrf::gpio::{Level, Output};

let mut led = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);

led.set_high();
led.set_low();
```

### Input Pins

```rust
use embassy_nrf::gpio::{Input, Pull};

let button = Input::new(p.P0_11, Pull::Up);

if button.is_low() {
    // Button pressed
}
```

### Interrupts

```rust
use embassy_nrf::gpio::{Input, Interrupt};

let mut button = Input::new(p.P0_11, Pull::Up);
button.interrupt(Interrupt::Edge(Edge::Rising));

#[embassy_executor::task]
async fn button_task(mut button: Input<'static, AnyPin>) {
    loop {
        button.wait_for_high().await;
        // Handle button press
    }
}
```

---

## BLE Configuration

### SoftDevice Setup

```rust
use nrf_softdevice::ble::peripheral::{Advertise, advertiser};
use nrf_softdevice::Softdevice;

let config = nrf_softdevice::Config::default();
let sd = Softdevice::enable(&config)?;

// Configure BLE
let adv = Advertise::new(&sd)?;
adv.set_data(&[
    0x02, 0x01, 0x06,  // Flags
    0x09, 0x09, b'mAgent',  // Name
])?;
adv.start().await?;
```

### GATT Services

```rust
use nrf_softdevice::ble::gatt_server::*;

#[gatt_service(uuid = "12345678-1234-1234-1234-123456789abc")]
struct MyService {
    #[characteristic(uuid = "12345678-1234-1234-1234-123456789abd", read, write)]
    my_char: u8,
}
```

---

## Power Management

### Low Power Modes

```rust
use embassy_nrf::pac::POWER;

fn enter_low_power() {
    unsafe {
        // Configure low power mode
        (*POWER::ptr()).systemoff.write(|w| w.bits(0));
    }
}
```

### Battery Monitoring

```rust
use embassy_nrf::saadc::Saadc;

let mut battery_input = saadc.configure_input(
    &p.P0_31,  // VDD pin
    saadc::InputConfig::default()
);

let battery_voltage = battery_input.read().await;
let battery_level = (battery_voltage / 4095.0) * 3.3;  // Convert to volts
```

---

## Flash Memory Layout

### Flash Partitioning

```
Address      Size        Description
─────────────────────────────────────
0x00000000   290 KB      Firmware (magent-app)
0x04880000   100 KB      Skills Storage
0x06280000    30 KB      Configuration
0x06A00000   200 KB      Data Storage
0x09A00000   102 KB      Reserved
0x0FFFFFFF              End of Flash
```

### Flash Operations

```rust
use embedded_storage::nor_flash::NorFlash;

// Erase sector
flash.erase(sector_start, sector_end)?;

// Write data
flash.write(address, data)?;

// Read data
flash.read(address, buf)?;
```

---

## UART Configuration

### Debug Output

```rust
use embassy_nrf::uarte::{Uarte, Config};

let mut uart = Uarte::new(
    p.UARTE0,
    Irqs,
    p.P0_06,  // TX
    p.P0_08,  // RX,
    Config::default()
);

// Write to UART
uart.write_all(b"Hello, mAgent!\n").await?;
```

---

## Watchdog Timer

### Hardware Watchdog

```rust
use embassy_nrf::wdt::Wdt;

let mut wdt = Wdt::new(p.WDT);

// Configure watchdog
wdt.start(10_000_000);  // 10 seconds

// Feed watchdog
wdt.pet();
```

---

## Clock Configuration

### High Frequency Clock

```rust
use embassy_nrf::clock::HighFrequencyClock;

let hfclock = HighFrequencyClock::new(p.CLOCK);
hfclock.enable().await;
```

### Low Frequency Clock

```rust
use embassy_nrf::clock::LowFrequencyClock;

let lfclock = LowFrequencyClock::new(p.CLOCK);
lfclock.enable().await;
```

---

## Custom Board Integration

### Board Support Package

For custom nRF52840 boards, create a board support package:

```rust
// boards/my_board/src/lib.rs
use embassy_nrf::peripherals;

pub struct Board {
    pub led: Output<'static, P0_13>,
    pub button: Input<'static, P0_11>,
    pub i2c: Twim<'static, TWIM0>,
    pub spi: Spim<'static, SPIM0>,
}

impl Board {
    pub fn new(p: embassy_nrf::Peripherals) -> Self {
        Self {
            led: Output::new(p.P0_13, Level::Low, OutputDrive::Standard),
            button: Input::new(p.P0_11, Pull::Up),
            i2c: Twim::new(p.TWIM0, Irqs, p.P0_26, p.P0_27, Config::default()),
            spi: Spim::new(p.SPIM0, Irqs, p.P0_15, p.P0_13, p.P0_14, Config::default()),
        }
    }
}
```

---

## Testing Hardware

### Hardware Tests

```rust
#[cfg(test)]
mod hardware_tests {
    use super::*;

    #[test]
    fn test_led_toggle() {
        let mut led = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);
        led.set_high();
        assert!(led.is_set_high());
        led.set_low();
        assert!(led.is_set_low());
    }

    #[test]
    fn test_button_read() {
        let button = Input::new(p.P0_11, Pull::Up);
        let state = button.is_low();
        // Assert expected state
    }

    #[test]
    fn test_i2c_scan() {
        let mut i2c = Twim::new(p.TWIM0, Irqs, p.P0_26, p.P0_27, Config::default());
        
        for addr in 0x08..=0x77 {
            if i2c.write(addr, &[]).await.is_ok() {
                info!("Device found at 0x{:02X}", addr);
            }
        }
    }
}
```

---

## Troubleshooting

### Common Issues

#### Flash Write Fails
- Check if sector is erased before writing
- Verify address alignment
- Check flash protection bits

#### BLE Connection Fails
- Verify SoftDevice is initialized
- Check clock configuration
- Ensure antenna is connected

#### Sensor Read Fails
- Verify I2C/SPI wiring
- Check pull-up resistors
- Verify sensor address

#### Power Consumption High
- Disable unused peripherals
- Enter low power mode when idle
- Check for stuck interrupts

---

## Reference Designs

### Smartwatch Reference Design

```
Components:
- nRF52840 SoC
- 240x240 LCD Display (SPI)
- LSM6DS3 Accelerometer (I2C)
- BMP280 Barometer (I2C)
- 300mAh LiPo Battery
- Charging Circuit
- Vibrator Motor
- Touch Controller (I2C)
```

### Environmental Sensor Reference Design

```
Components:
- nRF52840 SoC
- BME280 Environmental Sensor (I2C)
- PMS5003 Air Quality Sensor (UART)
- Solar Panel
- Supercapacitor
- LoRa Module (SPI)
```

---

## Additional Resources

- [nRF52840 Product Page](https://www.nordicsemi.com/Products/nRF52840)
- [nRF52840 Datasheet](https://infocenter.nordicsemi.com/pdf/nRF52840_PS_v1.1.pdf)
- [nRF Connect SDK](https://www.nordicsemi.com/Software-and-tools/Development-Tools/nRF-Connect-for-Desktop)
- [Embassy Documentation](https://embassy.dev/)
- [Embedded Rust Book](https://rust-embedded.github.io/book/)

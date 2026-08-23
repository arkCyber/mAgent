//! nRF52840 Hardware Abstraction Layer (HAL) Simulation
//!
//! This module provides realistic simulation of the nRF52840 chip
//! for testing mAgent on desktop platforms (macOS/Linux) before
//! deploying to actual embedded hardware.
//!
//! ## Simulated Features
//!
//! - **CPU**: ARM Cortex-M4 @ 64MHz (simulated)
//! - **Flash**: 1MB simulated flash storage
//! - **RAM**: 256KB simulated RAM
//! - **BLE**: Bluetooth 5.3 peripheral/central
//! - **Timers**: Hardware timer simulation
//! - **GPIO**: 48 GPIO pins
//! - **Peripherals**: I2C, SPI, UART, ADC, PWM
//!
//! ## Usage
//!
//! ```rust
//! use magent_hal::nrf52::sim::{Nrf52Simulator, PinState};
//!
//! let mut sim = Nrf52Simulator::new();
//! sim.gpio.set_pin_state(13, PinState::High);
//! let temp = sim.temperature_sensor.read();
//! ```

/// Internal simulation module for the nRF52840 peripherals.
///
/// Re-exported by the parent module so consumers can write
/// `magent_hal::nrf52::sim::simulation::PinState` (or, more
/// commonly, just `magent_hal::nrf52::sim::PinState` via the
/// top-level `pub use` re-exports below).
//
// The simulator's types are tightly-coupled internal
// implementation details. Most `pub` items only exist so that
// integration tests in sibling crates can poke at them; the
// per-item doc comments above already cover the public surface,
// so we silence the `missing_docs` lint at the module boundary to
// avoid forcing documentation on every `pub` helper.
#[allow(missing_docs)]
pub mod simulation {
    use crate::error::{HalError, HalResult};
    use core::cell::Cell;
    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    // The standard collection types still need to be imported explicitly.
    use std::string::{String, ToString};
    // We need both: heapless::Vec (fixed-capacity, used in struct fields) and
    // std::vec::Vec (dynamic, used in helpers and tests). Import heapless
    // under a distinct alias so struct field syntax `Vec<u16, 4>` keeps
    // working.
    use heapless::Vec as HeaplessVec;
    use std::vec::Vec as StdVec;

    // ============================================================================
    // nRF52840 Constants
    // ============================================================================

    /// Flash size: 1MB
    pub const FLASH_SIZE: usize = 1024 * 1024;

    /// RAM size: 256KB
    pub const RAM_SIZE: usize = 256 * 1024;

    /// Number of GPIO pins
    pub const GPIO_PIN_COUNT: usize = 48;

    /// Number of RTC timers
    pub const RTC_COUNT: usize = 3;

    /// Number of timer instances
    pub const TIMER_COUNT: usize = 5;

    /// BLE advertising interval default (ms)
    pub const BLE_ADV_INTERVAL_MS: u16 = 100;

    /// BLE connection interval default (ms)
    pub const BLE_CONN_INTERVAL_MS: u16 = 50;

    // ============================================================================
    // GPIO Types
    // ============================================================================

    /// GPIO pin state
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PinState {
        /// Drive or sense a logic-low level (0 V).
        Low = 0,
        /// Drive or sense a logic-high level (Vcc).
        High = 1,
    }

    /// GPIO pin direction
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PinDirection {
        /// Hi-Z input, no pull resistor.
        Input,
        /// Push-pull output.
        Output,
        /// Input with internal pull-up enabled.
        InputPullUp,
        /// Input with internal pull-down enabled.
        InputPullDown,
    }

    /// GPIO configuration
    #[derive(Debug, Clone, Copy)]
    pub struct GpioConfig {
        /// Drive direction for the pin (input / output / pulled input).
        pub direction: PinDirection,
        /// Driven level when [`direction`](Self::direction) is
        /// [`Output`](PinDirection::Output); sampled level otherwise.
        pub state: PinState,
        /// When `true`, transitions on the pin wake the MCU from
        /// sleep (`SENSE` field in nRF52840 GPIO terminology).
        pub sense: bool,
    }

    impl Default for GpioConfig {
        fn default() -> Self {
            Self {
                direction: PinDirection::Input,
                state: PinState::Low,
                sense: false,
            }
        }
    }

    // ============================================================================
    // BLE Types
    // ============================================================================

    /// BLE connection state
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BleState {
        /// No peer device connected and no radio activity.
        Disconnected,
        /// Radio is broadcasting advertisements and waiting for a
        /// central to initiate a connection.
        Advertising,
        /// Radio is listening for advertisements from nearby
        /// peripherals (central role).
        Scanning,
        /// Link is up — frames can be exchanged with the peer.
        Connected,
    }

    /// BLE device address
    #[derive(Debug, Clone)]
    pub struct BleAddress {
        /// Six-byte little-endian address, matching how it would be
        /// transmitted over the air.
        pub bytes: [u8; 6],
    }

    impl BleAddress {
        /// Create a random BLE address
        pub fn random() -> Self {
            Self {
                bytes: [
                    rand_u8(),
                    rand_u8(),
                    rand_u8(),
                    rand_u8(),
                    rand_u8(),
                    rand_u8(),
                ],
            }
        }

        /// Create a static address
        pub fn static_address() -> Self {
            Self {
                bytes: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            }
        }

        /// Get as hex string
        pub fn to_hex_string(&self) -> String {
            self.bytes
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect::<StdVec<_>>()
                .join(":")
        }
    }

    /// BLE advertising data
    #[derive(Debug, Clone)]
    pub struct BleAdvData {
        /// Complete local name (if the advertiser chose to include it).
        pub name: Option<String>,
        /// Transmit power advertised with the packet, in dBm. Range
        /// is typically -127..=+20.
        pub tx_power: i8,
        /// Advertising-data flag bits (e.g. LE General Discoverable).
        pub flags: u8,
        /// GATT service UUIDs advertised, up to 4 entries.
        pub service_uuids: HeaplessVec<u16, 4>,
    }

    /// BLE connection parameters
    #[derive(Debug, Clone)]
    pub struct BleConnParams {
        /// Minimum connection interval requested by the central,
        /// in units of 1.25 ms.
        pub min_interval: u16,
        /// Maximum connection interval the peripheral will accept,
        /// in units of 1.25 ms.
        pub max_interval: u16,
        /// Number of connection events the peripheral may skip
        /// before being considered lost.
        pub slave_latency: u16,
        /// Link supervision timeout in units of 10 ms — the
        /// interval after which the link is considered broken if no
        /// packet is received.
        pub supervision_timeout: u16,
    }

    impl Default for BleConnParams {
        fn default() -> Self {
            Self {
                min_interval: 50,    // 50ms
                max_interval: 100,   // 100ms
                slave_latency: 0,
                supervision_timeout: 4000, // 4s
            }
        }
    }

    // ============================================================================
    // Sensor Types (Smartwatch-specific)
    // ============================================================================

    /// Heart rate measurement
    #[derive(Debug, Clone, Copy)]
    pub struct HeartRateMeasurement {
        /// Instantaneous heart rate, in beats per minute (BPM).
        pub rate: u16,
        /// `true` when the optical sensor reports reliable
        /// skin-contact (used to gate alerts that would otherwise
        /// be triggered by motion artefact).
        pub sensor_contact: bool,
        /// Energy expended during the measurement, in kilojoules.
        pub energy: u16,
        /// R-R (inter-beat) intervals in milliseconds — two samples
        /// per Bluetooth Heart Rate Measurement characteristic update.
        pub rr_intervals: [u16; 2],
    }

    /// Step counter data
    #[derive(Debug, Clone, Copy)]
    pub struct StepData {
        /// Cumulative step count since the last reset.
        pub steps: u32,
        /// Most recently measured stride length, in centimetres.
        pub stride_length: u8,
        /// Coarse classification of the wearer's current motion.
        pub activity: ActivityType,
    }

    /// Activity type
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ActivityType {
        /// Sitting / standing still.
        Sedentary,
        /// Casual walking pace.
        Walking,
        /// Sustained running pace.
        Running,
        /// Outdoor or stationary cycling.
        Cycling,
        /// Wearable is in sleep-mode metrics.
        Sleeping,
    }

    /// SpO2 (blood oxygen) measurement
    #[derive(Debug, Clone, Copy)]
    pub struct SpO2Measurement {
        /// Peripheral oxygen saturation as a percentage (typically
        /// 90–100 % for a healthy adult at rest).
        pub saturation: f32,
        /// Algorithm confidence in the reading, as a percentage.
        /// Low-confidence readings should not be displayed to the
        /// user or fed into alert logic.
        pub confidence: u8,
    }

    /// Environmental sensor data
    #[derive(Debug, Clone, Copy)]
    pub struct EnvData {
        /// Ambient temperature, in degrees Celsius.
        pub temperature: f32,
        /// Atmospheric pressure, in hectopascals (hPa).
        pub pressure: f32,
        /// Relative humidity, as a percentage (0–100).
        pub humidity: f32,
        /// Ambient light level, in lux.
        pub light: f32,
    }

    // ============================================================================
    // Timer/RTC Types
    // ============================================================================

    /// RTC tick type
    #[derive(Debug)]
    pub struct RtcTime {
        /// Seconds elapsed since the RTC was last reset. Updated by
        /// [`Self::tick`] every 32 768 increments of `ticks`.
        pub seconds: AtomicU64,
        /// Sub-second counter incremented by [`Self::tick`].
        pub ticks: AtomicU32,
    }

    impl RtcTime {
        /// Create a new RTC starting at 0 ticks / 0 seconds.
        pub fn new() -> Self {
            Self {
                seconds: AtomicU64::new(0),
                ticks: AtomicU32::new(0),
            }
        }

        /// Advance the RTC by one tick. At the standard 32 768 Hz
        /// tick rate this should be called once per millisecond;
        /// every 32 768 ticks roll `seconds` over by one.
        pub fn tick(&self) {
            let ticks = self.ticks.fetch_add(1, Ordering::SeqCst);
            if ticks >= 32768 {
                // 1 second at 32.768kHz
                self.ticks.store(0, Ordering::SeqCst);
                self.seconds.fetch_add(1, Ordering::SeqCst);
            }
        }

        /// Whole seconds elapsed since the RTC was reset.
        pub fn get_seconds(&self) -> u64 {
            self.seconds.load(Ordering::SeqCst)
        }
    }

    impl Default for RtcTime {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // Power Types
    // ============================================================================

    /// Power mode
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PowerMode {
        /// CPU is running at full clock — every peripheral is alive.
        Active,
        /// CPU clock gated but RAM retained; peripherals can wake it
        /// via interrupts.
        Idle,
        /// Most regulators off; only RTC + selected wakeup sources
        /// remain powered. Wake-up latency is in milliseconds.
        LowPower,
        /// Everything off; only a reset pin can wake the chip.
        SystemOff,
    }

    /// Battery state
    #[derive(Debug)]
    pub struct BatteryState {
        /// Last measured battery voltage, in millivolts. Updated by
        /// the simulator's [`Self::drain`] model.
        pub voltage_mv: AtomicU32,
        /// Remaining charge as a percentage in `0..=100`.
        pub percentage: AtomicU32,
        /// `true` while the battery is being charged.
        pub charging: AtomicBool,
        /// Latched `true` once the battery dropped below 20 % and
        /// stays true until cleared.
        pub low_battery: AtomicBool,
    }

    impl BatteryState {
        /// Construct a battery at 3.7 V / 85 % / not charging / not low.
        pub fn new() -> Self {
            Self {
                voltage_mv: AtomicU32::new(3700),
                percentage: AtomicU32::new(85),
                charging: AtomicBool::new(false),
                low_battery: AtomicBool::new(false),
            }
        }

        /// Last measured voltage, in millivolts.
        pub fn voltage(&self) -> u32 {
            self.voltage_mv.load(Ordering::SeqCst)
        }

        /// Remaining charge as a percentage.
        pub fn percentage(&self) -> u32 {
            self.percentage.load(Ordering::SeqCst)
        }

        /// Whether the battery is currently being charged.
        pub fn is_charging(&self) -> bool {
            self.charging.load(Ordering::SeqCst)
        }

        /// Whether the battery is below the low-battery threshold.
        pub fn is_low(&self) -> bool {
            self.low_battery.load(Ordering::SeqCst)
        }

        /// Simulate battery drain
        pub fn drain(&self, percent: u32) {
            let current = self.percentage.load(Ordering::SeqCst);
            let new_percent = current.saturating_sub(percent);
            self.percentage.store(new_percent, Ordering::SeqCst);

            // Update voltage based on percentage: 3.0V at 0% → 4.0V at ~143%.
            let new_voltage = 3000 + new_percent * 7;
            self.voltage_mv.store(new_voltage, Ordering::SeqCst);

            // Update low battery flag
            self.low_battery
                .store(new_percent < 20, Ordering::SeqCst);
        }
    }

    impl Default for BatteryState {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // Random Number Generator (TRNG simulation)
    // ============================================================================

    /// Simple pseudo-random number generator (Xorshift64)
    pub struct Trng {
        state: u64,
    }

    impl Trng {
        /// Create a new TRNG seeded with `seed`.
        pub fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        /// Generate the next pseudo-random `u32`.
        ///
        /// Not named `next` to avoid shadowing
        /// [`std::iter::Iterator::Item`](std::iter::Iterator::Item).
        pub fn next_u32(&mut self) -> u32 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.state = x;
            (x >> 32) as u32
        }

        /// Generate the next pseudo-random `u64`.
        pub fn next_u64(&mut self) -> u64 {
            ((self.next_u32() as u64) << 32) | (self.next_u32() as u64)
        }

        /// Generate a uniform pseudo-random integer in `min..=max`.
        pub fn next_range(&mut self, min: u32, max: u32) -> u32 {
            let range = max - min + 1;
            min + (self.next_u32() % range)
        }
    }

    // ============================================================================
    // Main nRF52840 Simulator
    // ============================================================================

    /// Complete nRF52840 simulation for smartwatch
    pub struct Nrf52Simulator {
        /// Flash storage
        pub flash: SimulatedFlash,

        /// RAM storage
        pub ram: SimulatedRam,

        /// GPIO pins
        pub gpio: GpioController,

        /// BLE interface
        pub ble: BleController,

        /// Real-time clock
        pub rtc: RtcTime,

        /// Battery state
        pub battery: BatteryState,

        /// TRNG
        pub trng: Trng,

        /// Temperature sensor (on-chip)
        pub temperature_sensor: SimTemperatureSensor,

        /// Accelerometer (simulated BMI160/BMM150)
        pub accelerometer: SimAccelerometer,

        /// Heart rate sensor (simulated)
        pub heart_rate_sensor: SimHeartRateSensor,

        /// SpO2 sensor (simulated)
        pub spo2_sensor: SimSpO2Sensor,

        /// Step counter
        pub step_counter: StepCounter,

        /// Power mode
        pub power_mode: Cell<PowerMode>,

        /// Tick counter
        pub tick_count: AtomicU64,

        /// Simulation time step
        pub time_step_ms: u32,
    }

    impl Nrf52Simulator {
        /// Create a new simulator
        pub fn new() -> Self {
            Self {
                flash: SimulatedFlash::new(FLASH_SIZE),
                ram: SimulatedRam::new(RAM_SIZE),
                gpio: GpioController::new(),
                ble: BleController::new(),
                rtc: RtcTime::new(),
                battery: BatteryState::new(),
                trng: Trng::new(0x12345678),
                temperature_sensor: SimTemperatureSensor::new(),
                accelerometer: SimAccelerometer::new(),
                heart_rate_sensor: SimHeartRateSensor::new(),
                spo2_sensor: SimSpO2Sensor::new(),
                step_counter: StepCounter::new(),
                power_mode: Cell::new(PowerMode::Active),
                tick_count: AtomicU64::new(0),
                time_step_ms: 10,
            }
        }

        /// Step simulation forward
        pub fn tick(&mut self) {
            self.tick_count.fetch_add(1, Ordering::SeqCst);
            self.rtc.tick();

            // Update sensors
            self.temperature_sensor.update();
            self.accelerometer.update();
            self.heart_rate_sensor.update();
            self.step_counter.update(&self.accelerometer);

            // Update BLE
            self.ble.update();

            // Simulate battery drain in active mode
            if matches!(self.power_mode.get(), PowerMode::Active)
                && self.tick_count.load(Ordering::SeqCst).is_multiple_of(100)
            {
                self.battery.drain(1);
            }
        }

        /// Set power mode
        pub fn set_power_mode(&self, mode: PowerMode) {
            self.power_mode.set(mode);
        }

        /// Get current power mode
        pub fn get_power_mode(&self) -> PowerMode {
            self.power_mode.get()
        }

        /// Read all sensors for smartwatch
        pub fn read_all_sensors(&mut self) -> SmartwatchData {
            SmartwatchData {
                heart_rate: self.heart_rate_sensor.read(),
                steps: self.step_counter.read(),
                spo2: self.spo2_sensor.read(),
                env: EnvData {
                    temperature: self.temperature_sensor.read(),
                    pressure: 1013.25,
                    humidity: 55.0,
                    light: 500.0,
                },
                battery: BatteryInfo {
                    percentage: self.battery.percentage() as u8,
                    charging: self.battery.is_charging(),
                    low: self.battery.is_low(),
                },
                accelerometer: self.accelerometer.read(),
                rtc: RtcTimeInfo {
                    seconds: self.rtc.get_seconds(),
                },
            }
        }

        /// Get BLE status
        pub fn get_ble_status(&self) -> BleStatus {
            BleStatus {
                state: self.ble.state,
                address: self.ble.address.clone(),
                connected_device: self.ble.connected_device.clone(),
                tx_count: self.ble.tx_count.load(Ordering::SeqCst),
                rx_count: self.ble.rx_count.load(Ordering::SeqCst),
            }
        }

        /// Send data via BLE (if connected)
        pub fn ble_send(&mut self, data: &[u8]) -> HalResult<()> {
            self.ble.send(data)
        }

        /// Get all sensor names for LLM context
        pub fn get_sensor_list(&self) -> StdVec<&'static str> {
            vec![
                "temperature",
                "accelerometer",
                "heart_rate",
                "spo2",
                "steps",
                "battery",
                "light",
                "humidity",
                "pressure",
            ]
        }
    }

    impl Default for Nrf52Simulator {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // Simulated Flash
    // ============================================================================

    /// Simulated flash memory with wear characteristics.
    ///
    /// The on-disk layout is `sector_size = 4096` bytes per sector.
    /// `writes[i]` tracks the cumulative write count for sector `i`
    /// and is used by [`Self::is_sector_worn`] to model flash wear.
    pub struct SimulatedFlash {
        data: StdVec<u8>,
        writes: StdVec<u32>, // Write count per 4KB sector
    }

    impl SimulatedFlash {
        /// Allocate `size` bytes of flash, pre-erased to `0xFF`.
        pub fn new(size: usize) -> Self {
            let sector_size = 4096;
            Self {
                data: vec![0xFF; size],
                writes: vec![0u32; size / sector_size],
            }
        }

        /// Read `buf.len()` bytes starting at `address`.
        /// Returns [`HalError::StorageOutOfRange`] if the read would
        /// extend past the end of flash.
        pub fn read(&self, address: usize, buf: &mut [u8]) -> HalResult<()> {
            if address + buf.len() > self.data.len() {
                return Err(HalError::StorageOutOfRange);
            }
            buf.copy_from_slice(&self.data[address..address + buf.len()]);
            Ok(())
        }

        /// Write `data` starting at `address`.
        ///
        /// Models real flash semantics: existing bits can only be
        /// cleared (logical AND with the new byte), so a write to
        /// already-cleared bits is a no-op. Increments the per-sector
        /// write counter used by [`Self::is_sector_worn`].
        pub fn write(&mut self, address: usize, data: &[u8]) -> HalResult<()> {
            if address + data.len() > self.data.len() {
                return Err(HalError::StorageOutOfRange);
            }

            // Track writes per sector
            let sector = address / 4096;
            if sector < self.writes.len() {
                self.writes[sector] += 1;
            }

            // Flash can only clear bits
            for (i, &byte) in data.iter().enumerate() {
                self.data[address + i] &= byte;
            }
            Ok(())
        }

        /// Erase a 4 KiB sector, restoring all bytes to `0xFF`.
        /// Out-of-range sectors return [`HalError::StorageOutOfRange`].
        pub fn erase(&mut self, sector: usize) -> HalResult<()> {
            let sector_size = 4096;
            let start = sector * sector_size;
            if start >= self.data.len() {
                return Err(HalError::StorageOutOfRange);
            }

            let end = (start + sector_size).min(self.data.len());
            for i in start..end {
                self.data[i] = 0xFF;
            }
            Ok(())
        }

        /// Number of times `sector` has been written since power-on.
        /// Returns `0` if `sector` is outside the flash layout.
        pub fn get_sector_writes(&self, sector: usize) -> u32 {
            self.writes.get(sector).copied().unwrap_or(0)
        }

        /// Whether the cumulative writes to `sector` have reached or
        /// exceeded `threshold`. Models wear-out for endurance tests.
        pub fn is_sector_worn(&self, sector: usize, threshold: u32) -> bool {
            self.get_sector_writes(sector) >= threshold
        }
    }

    // ============================================================================
    // Simulated RAM
    // ============================================================================

    /// Simulated RAM
    pub struct SimulatedRam {
        data: StdVec<u8>,
    }

    impl SimulatedRam {
        /// Allocate `size` bytes of zero-initialised RAM.
        pub fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        /// Read a single byte at `address`. Out-of-range reads return `0`.
        pub fn read(&self, address: usize) -> u8 {
            self.data.get(address).copied().unwrap_or(0)
        }

        /// Write a single byte to `address`. Out-of-range writes are dropped.
        pub fn write(&mut self, address: usize, value: u8) {
            if address < self.data.len() {
                self.data[address] = value;
            }
        }

        /// Total RAM capacity, in bytes.
        pub fn size(&self) -> usize {
            self.data.len()
        }
    }

    // ============================================================================
    // GPIO Controller
    // ============================================================================

    /// GPIO controller for nRF52840
    pub struct GpioController {
        pins: [GpioConfig; GPIO_PIN_COUNT],
    }

    impl GpioController {
        /// Create a new controller with all pins in their default
        /// (`Input` / `Low` / no-sense) configuration.
        pub fn new() -> Self {
            Self {
                pins: [GpioConfig::default(); GPIO_PIN_COUNT],
            }
        }

        /// Apply a full [`GpioConfig`] to `pin`. Returns
        /// [`HalError::GpioOutOfRange`] if `pin` is out of range.
        pub fn configure(&mut self, pin: usize, config: GpioConfig) -> HalResult<()> {
            if pin >= GPIO_PIN_COUNT {
                return Err(HalError::GpioOutOfRange);
            }
            self.pins[pin] = config;
            Ok(())
        }

        /// Drive `pin` to `state`. Auto-promotes an unconfigured
        /// (`Input`) pin to `Output` so first-write doesn't fail.
        /// Returns [`HalError::GpioOutOfRange`] for an invalid pin.
        pub fn set_pin_state(&mut self, pin: usize, state: PinState) -> HalResult<()> {
            if pin >= GPIO_PIN_COUNT {
                return Err(HalError::GpioOutOfRange);
            }
            if !matches!(
                self.pins[pin].direction,
                PinDirection::Output | PinDirection::InputPullUp | PinDirection::InputPullDown
            ) {
                // Default to output if not configured
                self.pins[pin].direction = PinDirection::Output;
            }
            self.pins[pin].state = state;
            Ok(())
        }

        /// Sample the current level of `pin`.
        pub fn get_pin_state(&self, pin: usize) -> HalResult<PinState> {
            if pin >= GPIO_PIN_COUNT {
                return Err(HalError::GpioOutOfRange);
            }
            Ok(self.pins[pin].state)
        }

        /// Flip `pin` to the opposite of its current state.
        pub fn toggle_pin(&mut self, pin: usize) -> HalResult<()> {
            let current = self.get_pin_state(pin)?;
            let new_state = match current {
                PinState::Low => PinState::High,
                PinState::High => PinState::Low,
            };
            self.set_pin_state(pin, new_state)
        }
    }

    impl Default for GpioController {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // BLE Controller
    // ============================================================================

    /// BLE controller simulation
    pub struct BleController {
        /// Current radio state (advertising / connected / …).
        pub state: BleState,
        /// Local device BLE address.
        pub address: BleAddress,
        /// Name of the currently connected peer, if any.
        pub connected_device: Option<String>,
        /// Cumulative number of bytes transmitted since power-on.
        pub tx_count: AtomicU64,
        /// Cumulative number of bytes received since power-on.
        pub rx_count: AtomicU64,
        /// Most recently transmitted payload, kept for test inspection.
        pub last_tx: Cell<Option<StdVec<u8>>>,
        adv_data: BleAdvData,
    }

    impl BleController {
        /// Construct a controller in [`BleState::Disconnected`] with
        /// the well-known demo advertising payload.
        pub fn new() -> Self {
            Self {
                state: BleState::Disconnected,
                address: BleAddress::static_address(),
                connected_device: None,
                tx_count: AtomicU64::new(0),
                rx_count: AtomicU64::new(0),
                last_tx: Cell::new(None),
                adv_data: BleAdvData {
                    name: Some("mAgent-Watch".to_string()),
                    tx_power: 0,
                    flags: 0x06, // LE General Discoverable, BR/EDR Not Supported
                    service_uuids: heapless::Vec::new(),
                },
            }
        }

        /// Begin broadcasting advertisements.
        pub fn start_advertising(&mut self) {
            self.state = BleState::Advertising;
        }

        /// Stop advertising. Only effective from the
        /// [`BleState::Advertising`] state; no-op otherwise.
        pub fn stop_advertising(&mut self) {
            if matches!(self.state, BleState::Advertising) {
                self.state = BleState::Disconnected;
            }
        }

        /// Simulate a connection from a peer with the given display name.
        pub fn connect(&mut self, device_name: &str) {
            self.state = BleState::Connected;
            self.connected_device = Some(device_name.to_string());
        }

        /// Tear down any active link.
        pub fn disconnect(&mut self) {
            self.state = BleState::Disconnected;
            self.connected_device = None;
        }

        /// Transmit `data` over the link. Returns
        /// [`HalError::BleNotConnected`] if there is no peer.
        pub fn send(&mut self, data: &[u8]) -> HalResult<()> {
            if !matches!(self.state, BleState::Connected) {
                return Err(HalError::BleNotConnected);
            }
            self.tx_count.fetch_add(data.len() as u64, Ordering::SeqCst);
            self.last_tx.set(Some(data.to_vec()));
            Ok(())
        }

        /// Inject `data` as if it had been received from the peer
        /// (used by tests to drive the radio without a real socket).
        pub fn receive(&mut self, data: &[u8]) {
            self.rx_count.fetch_add(data.len() as u64, Ordering::SeqCst);
        }

        /// Run one BLE-stack tick. Currently a no-op kept for
        /// future expansion (e.g. connection-parameter renegotiation).
        pub fn update(&self) {
            // Simulate BLE stack updates
        }

        /// Borrow the advertising payload this controller is broadcasting.
        pub fn get_adv_data(&self) -> &BleAdvData {
            &self.adv_data
        }
    }

    impl Default for BleController {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // Simulated Sensors
    // ============================================================================

    /// Simulated temperature sensor
    pub struct SimTemperatureSensor {
        /// Mean temperature the sensor trends toward, in °C.
        base_temp: f32,
        /// Peak-to-peak amplitude of the sinusoidal noise added to
        /// each [`Self::read`] call.
        noise_amplitude: f32,
        /// Monotonic counter used as the noise function's input.
        iteration: u64,
    }

    impl SimTemperatureSensor {
        /// Create a sensor at 25 °C with ±0.5 °C noise amplitude.
        pub fn new() -> Self {
            Self {
                base_temp: 25.0,
                noise_amplitude: 0.5,
                iteration: 0,
            }
        }

        /// Advance the internal iteration counter.
        pub fn update(&mut self) {
            self.iteration += 1;
        }

        /// Sample the current temperature, in °C.
        pub fn read(&self) -> f32 {
            let noise = (self.iteration as f32 * 0.1).sin() * self.noise_amplitude;
            self.base_temp + noise
        }

        /// Override the temperature this sensor trends toward.
        pub fn set_base(&mut self, temp: f32) {
            self.base_temp = temp;
        }
    }

    impl Default for SimTemperatureSensor {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Simulated accelerometer
    pub struct SimAccelerometer {
        x: Cell<f32>,
        y: Cell<f32>,
        z: Cell<f32>,
        activity: Cell<ActivityType>,
        iteration: u64,
    }

    impl SimAccelerometer {
        /// Construct a sensor resting flat on a table (Z = 1g).
        pub fn new() -> Self {
            Self {
                x: Cell::new(0.0),
                y: Cell::new(0.0),
                z: Cell::new(9.8), // Gravity
                activity: Cell::new(ActivityType::Sedentary),
                iteration: 0,
            }
        }

        /// Step the sensor one simulation tick. Updates X/Y/Z with
        /// synthesised noise and classifies the wearer's activity
        /// based on the magnitude of the acceleration vector.
        pub fn update(&mut self) {
            self.iteration += 1;
            let t = self.iteration as f32;

            // Add realistic noise and movement
            let noise = || (t * 17.3).sin() * 0.01;
            self.x.set(noise());
            self.y.set(noise());

            // Z stays close to 1g when flat, with a synthesized step
            // pulse every ~10 ticks to simulate a walking pattern.
            // The pulse is a short-lived spike above 10.5 so the
            // step counter's peak detector can latch onto it.
            let pulse = if t.fract() < 0.1 || (t as u64).is_multiple_of(10) {
                0.95
            } else {
                0.0
            };
            self.z.set(9.8 + noise() + pulse);

            // Detect activity based on movement
            let magnitude =
                (self.x.get().powi(2) + self.y.get().powi(2) + self.z.get().powi(2)).sqrt();
            if magnitude > 11.0 {
                self.activity.set(ActivityType::Running);
            } else if magnitude > 10.5 {
                self.activity.set(ActivityType::Walking);
            } else {
                self.activity.set(ActivityType::Sedentary);
            }
        }

        /// Sample the current X/Y/Z acceleration vector (in m/s²).
        pub fn read(&self) -> (f32, f32, f32) {
            (self.x.get(), self.y.get(), self.z.get())
        }

        /// Latest activity classification derived from the magnitude.
        pub fn get_activity(&self) -> ActivityType {
            self.activity.get()
        }
    }

    impl Default for SimAccelerometer {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Simulated heart rate sensor
    pub struct SimHeartRateSensor {
        /// Current heart-rate baseline, in BPM. Updated by
        /// [`Self::update`].
        base_rate: u16,
        /// Monotonic counter used as the variability input.
        iteration: u64,
    }

    impl SimHeartRateSensor {
        /// Construct a sensor with a resting heart rate of 72 BPM.
        pub fn new() -> Self {
            Self {
                base_rate: 72, // Resting heart rate
                iteration: 0,
            }
        }

        /// Step the sensor, applying a sinusoidal variability term
        /// bounded to the physiologically realistic 50–180 BPM range.
        pub fn update(&mut self) {
            self.iteration += 1;
            // Simulate heart rate variability
            let variation = (self.iteration as f32 * 0.05).sin() * 5.0;
            self.base_rate = (70.0 + variation).clamp(50.0, 180.0) as u16;
        }

        /// Sample the current heart-rate measurement.
        pub fn read(&self) -> HeartRateMeasurement {
            HeartRateMeasurement {
                rate: self.base_rate,
                sensor_contact: true,
                energy: 0,
                rr_intervals: [
                    (60000 / self.base_rate as u32) as u16,
                    (60000 / self.base_rate as u32) as u16,
                ],
            }
        }
    }

    impl Default for SimHeartRateSensor {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Simulated SpO2 sensor
    pub struct SimSpO2Sensor {
        /// Current peripheral oxygen saturation, in percent.
        saturation: Cell<f32>,
        /// Monotonic counter used as the variability input.
        iteration: u64,
    }

    impl SimSpO2Sensor {
        /// Construct a sensor at 98 % SpO₂.
        pub fn new() -> Self {
            Self {
                saturation: Cell::new(98.0),
                iteration: 0,
            }
        }

        /// Step the sensor with a small sinusoidal drift, clamped
        /// to the physiologically realistic 90–100 % range.
        pub fn update(&mut self) {
            self.iteration += 1;
            // Small variation in SpO2
            let variation = (self.iteration as f32 * 0.02).sin() * 0.5;
            self.saturation.set((98.0 + variation).clamp(90.0, 100.0));
        }

        /// Sample the current SpO₂ measurement.
        pub fn read(&self) -> SpO2Measurement {
            SpO2Measurement {
                saturation: self.saturation.get(),
                confidence: 95,
            }
        }
    }

    impl Default for SimSpO2Sensor {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Step counter
    pub struct StepCounter {
        steps: u32,
        /// Edge-detector latch: `true` between a step's rising edge
        /// and its trailing edge so we don't double-count.
        stride_detected: bool,
        /// Counter of how many times [`Self::update`] has been
        /// called since the counter was created.
        last_update: u64,
    }

    impl StepCounter {
        /// Construct a fresh counter at zero steps.
        pub fn new() -> Self {
            Self {
                steps: 0,
                stride_detected: false,
                last_update: 0,
            }
        }

        /// Run one step-detection pass against the supplied
        /// accelerometer. Counts a step whenever the Z axis crosses
        /// above 9.85 m/s² before having crossed back below 9.75.
        pub fn update(&mut self, accel: &SimAccelerometer) {
            self.last_update += 1;

            // Simple step detection based on Z-axis peaks.
            // The simulator's Z hovers around 1g (9.8 m/s^2) with a
            // synthesized step pulse injected every ~10 ticks to
            // mimic a real walking pattern. We detect the rising edge
            // of each pulse using a small hysteresis band around 9.8.
            let z = accel.read().2;
            if z > 9.85 && !self.stride_detected {
                self.stride_detected = true;
                self.steps += 1;
            } else if z < 9.75 {
                self.stride_detected = false;
            }
        }

        /// Current step-count snapshot (cumulative steps, 75 cm
        /// stride, classified as `Walking`).
        pub fn read(&self) -> StepData {
            StepData {
                steps: self.steps,
                stride_length: 75, // Average stride length in cm
                activity: ActivityType::Walking,
            }
        }

        /// Reset the cumulative step counter to zero.
        pub fn reset(&mut self) {
            self.steps = 0;
        }
    }

    impl Default for StepCounter {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // Smartwatch Data Types
    // ============================================================================

    /// Complete smartwatch sensor data
    #[derive(Debug, Clone)]
    pub struct SmartwatchData {
        /// Latest heart-rate measurement.
        pub heart_rate: HeartRateMeasurement,
        /// Latest step counter snapshot.
        pub steps: StepData,
        /// Latest SpO₂ measurement.
        pub spo2: SpO2Measurement,
        /// Latest environmental sensor bundle.
        pub env: EnvData,
        /// Battery summary (percentage, charging, low-battery flag).
        pub battery: BatteryInfo,
        /// Latest accelerometer X/Y/Z triple.
        pub accelerometer: (f32, f32, f32),
        /// Wall-clock seconds since the RTC was reset.
        pub rtc: RtcTimeInfo,
    }

    /// Battery information
    #[derive(Debug, Clone, Copy)]
    pub struct BatteryInfo {
        /// Remaining charge, in percent (0–100).
        pub percentage: u8,
        /// `true` while a charger is connected.
        pub charging: bool,
        /// `true` if the battery is below the low-battery threshold.
        pub low: bool,
    }

    /// RTC time information
    #[derive(Debug, Clone, Copy)]
    pub struct RtcTimeInfo {
        /// Whole seconds since the RTC was reset.
        pub seconds: u64,
    }

    /// BLE status information
    #[derive(Debug, Clone)]
    pub struct BleStatus {
        /// Current radio state.
        pub state: BleState,
        /// Local device address.
        pub address: BleAddress,
        /// Name of the connected peer, if any.
        pub connected_device: Option<String>,
        /// Cumulative bytes transmitted.
        pub tx_count: u64,
        /// Cumulative bytes received.
        pub rx_count: u64,
    }

    // ============================================================================
    // Utility Functions
    // ============================================================================

    fn rand_u8() -> u8 {
        use std::time::SystemTime;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        ((nanos as u64 * 1103515245 + 12345) >> 16) as u8
    }
}

// Re-export for use in tests
pub use simulation::*;

#[cfg(test)]
mod tests {
    use super::simulation::*;

    #[test]
    fn test_nrf52_simulator_creation() {
        let sim = Nrf52Simulator::new();
        assert_eq!(sim.get_power_mode(), PowerMode::Active);
    }

    #[test]
    fn test_gpio_operations() {
        let mut sim = Nrf52Simulator::new();

        sim.gpio.set_pin_state(13, PinState::High).unwrap();
        assert_eq!(sim.gpio.get_pin_state(13).unwrap(), PinState::High);

        sim.gpio.toggle_pin(13).unwrap();
        assert_eq!(sim.gpio.get_pin_state(13).unwrap(), PinState::Low);
    }

    #[test]
    fn test_temperature_sensor() {
        let sim = Nrf52Simulator::new();
        let temp = sim.temperature_sensor.read();
        assert!(temp > 20.0 && temp < 30.0);
    }

    #[test]
    fn test_accelerometer() {
        let sim = Nrf52Simulator::new();
        let (_x, _y, z) = sim.accelerometer.read();
        // Z should be close to 9.8 (gravity)
        assert!(z > 9.0 && z < 11.0);
    }

    #[test]
    fn test_heart_rate_sensor() {
        let sim = Nrf52Simulator::new();
        let hr = sim.heart_rate_sensor.read();
        assert!(hr.rate >= 50 && hr.rate <= 180);
        assert!(hr.sensor_contact);
    }

    #[test]
    fn test_spo2_sensor() {
        let sim = Nrf52Simulator::new();
        let spo2 = sim.spo2_sensor.read();
        assert!(spo2.saturation >= 90.0 && spo2.saturation <= 100.0);
    }

    #[test]
    fn test_flash_operations() {
        let mut sim = Nrf52Simulator::new();

        let test_data = [1u8, 2, 3, 4, 5];
        sim.flash.write(0, &test_data).unwrap();

        let mut read_buf = [0u8; 5];
        sim.flash.read(0, &mut read_buf).unwrap();

        assert_eq!(read_buf, test_data);
    }

    #[test]
    fn test_ble_connection() {
        let mut sim = Nrf52Simulator::new();

        assert!(matches!(sim.ble.state, BleState::Disconnected));

        sim.ble.start_advertising();
        assert!(matches!(sim.ble.state, BleState::Advertising));

        sim.ble.connect("Phone");
        assert!(matches!(sim.ble.state, BleState::Connected));
        assert_eq!(sim.ble.connected_device.as_deref(), Some("Phone"));
    }

    #[test]
    fn test_battery_state() {
        let sim = Nrf52Simulator::new();

        assert_eq!(sim.battery.percentage(), 85);
        assert!(!sim.battery.is_low());

        sim.battery.drain(70);
        assert!(sim.battery.is_low());
    }

    #[test]
    fn test_smartwatch_data() {
        let mut sim = Nrf52Simulator::new();
        sim.tick();
        sim.tick();
        sim.tick();

        let data = sim.read_all_sensors();
        assert!(data.heart_rate.rate >= 50);
        assert!(data.spo2.saturation >= 90.0);
        assert!(data.battery.percentage <= 100);
    }

    #[test]
    fn test_power_mode() {
        let sim = Nrf52Simulator::new();

        sim.set_power_mode(PowerMode::LowPower);
        assert_eq!(sim.get_power_mode(), PowerMode::LowPower);

        sim.set_power_mode(PowerMode::SystemOff);
        assert_eq!(sim.get_power_mode(), PowerMode::SystemOff);
    }

    #[test]
    fn test_ble_send_when_disconnected() {
        let mut sim = Nrf52Simulator::new();

        let result = sim.ble_send(&[1, 2, 3]);
        assert!(result.is_err());
    }

    #[test]
    fn test_ble_send_when_connected() {
        let mut sim = Nrf52Simulator::new();
        sim.ble.connect("TestDevice");

        let result = sim.ble_send(&[1, 2, 3, 4]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trng() {
        let mut trng = Trng::new(42);
        let val1 = trng.next_u32();
        let val2 = trng.next_u32();
        assert_ne!(val1, val2);
    }
}

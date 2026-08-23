//! nRF52840 Hardware Simulation Tests
//!
//! Standalone tests for nRF52840 simulation without embedded dependencies.
//! Run with: cargo test -p magent-core --features std --test nrf52_simulation_tests

#![cfg(all(feature = "std", target_arch = "x86_64"))]

use std::sync::{Arc, Mutex};

// ============================================================================
// Mock Error Types for Testing
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    WriteProtected,
    CorruptedData,
    OutOfSpace,
    BadAddress,
    ReadError,
    EraseError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    ConnectionRefused,
    ConnectionReset,
    HostUnreachable,
    NetworkDown,
    Timeout,
    InvalidResponse,
    AuthenticationFailed,
    EncryptionFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioOperation {
    Read,
    Write,
    Configure,
}

// ============================================================================
// nRF52840 Constants
// ============================================================================

const FLASH_SIZE: usize = 1024 * 1024; // 1MB
const RAM_SIZE: usize = 256 * 1024; // 256KB
const GPIO_PIN_COUNT: usize = 48;

// ============================================================================
// GPIO Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinState {
    Low = 0,
    High = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDirection {
    Input,
    Output,
    InputPullUp,
    InputPullDown,
}

#[derive(Debug, Clone)]
pub struct GpioConfig {
    pub direction: PinDirection,
    pub state: PinState,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleState {
    Disconnected,
    Advertising,
    Scanning,
    Connected,
}

#[derive(Debug, Clone)]
pub struct BleAddress {
    pub bytes: [u8; 6],
}

impl BleAddress {
    pub fn random() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        Self {
            bytes: [
                ((nanos >> 0) & 0xFF) as u8,
                ((nanos >> 8) & 0xFF) as u8,
                ((nanos >> 16) & 0xFF) as u8,
                ((nanos >> 24) & 0xFF) as u8,
                ((nanos >> 32) & 0xFF) as u8,
                ((nanos >> 40) & 0xFF) as u8,
            ],
        }
    }

    pub fn static_address() -> Self {
        Self {
            bytes: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
        }
    }

    pub fn to_hex_string(&self) -> String {
        self.bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":")
    }
}

// ============================================================================
// Sensor Types
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub struct HeartRateMeasurement {
    pub rate: u16,
    pub sensor_contact: bool,
    pub energy: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct StepData {
    pub steps: u32,
    pub stride_length: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct SpO2Measurement {
    pub saturation: f32,
    pub confidence: u8,
}

// ============================================================================
// Power Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerMode {
    Active,
    Idle,
    LowPower,
    SystemOff,
}

#[derive(Debug, Clone)]
pub struct BatteryState {
    pub voltage_mv: u32,
    pub percentage: u32,
    pub charging: bool,
    pub low_battery: bool,
}

impl BatteryState {
    pub fn new() -> Self {
        Self {
            voltage_mv: 3700,
            percentage: 85,
            charging: false,
            low_battery: false,
        }
    }

    pub fn drain(&mut self, percent: u32) {
        self.percentage = self.percentage.saturating_sub(percent);
        self.voltage_mv = 3000 + self.percentage * 7;
        self.low_battery = self.percentage < 20;
    }
}

impl Default for BatteryState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Simulated Flash
// ============================================================================

#[derive(Clone)]
pub struct SimulatedFlash {
    data: Vec<u8>,
    writes: Vec<u32>,
    total_sectors: usize,
}

impl SimulatedFlash {
    pub fn new(size: usize) -> Self {
        let sector_size = 4096;
        let total_sectors = size / sector_size;
        Self {
            data: vec![0xFF; size],
            writes: vec![0u32; total_sectors],
            total_sectors,
        }
    }

    pub fn read(&self, address: usize, buf: &mut [u8]) -> Result<(), String> {
        if address + buf.len() > self.data.len() {
            return Err("Bad address".to_string());
        }
        buf.copy_from_slice(&self.data[address..address + buf.len()]);
        Ok(())
    }

    pub fn write(&mut self, address: usize, data: &[u8]) -> Result<(), String> {
        if address + data.len() > self.data.len() {
            return Err("Bad address".to_string());
        }

        let sector = address / 4096;
        if sector < self.writes.len() {
            self.writes[sector] += 1;
        }

        for (i, &byte) in data.iter().enumerate() {
            self.data[address + i] &= byte;
        }
        Ok(())
    }

    pub fn erase(&mut self, sector: usize) -> Result<(), String> {
        let sector_size = 4096;
        let start = sector * sector_size;
        if start >= self.data.len() {
            return Err("Bad sector".to_string());
        }

        let end = (start + sector_size).min(self.data.len());
        for i in start..end {
            self.data[i] = 0xFF;
        }
        Ok(())
    }

    pub fn get_sector_writes(&self, sector: usize) -> u32 {
        self.writes.get(sector).copied().unwrap_or(0)
    }
}

// ============================================================================
// GPIO Controller
// ============================================================================

pub struct GpioController {
    pins: Vec<GpioConfig>,
}

impl GpioController {
    pub fn new() -> Self {
        Self {
            pins: vec![GpioConfig::default(); GPIO_PIN_COUNT],
        }
    }

    pub fn configure(&mut self, pin: usize, config: GpioConfig) -> Result<(), String> {
        if pin >= GPIO_PIN_COUNT {
            return Err(format!("Invalid pin: {}", pin));
        }
        self.pins[pin] = config;
        Ok(())
    }

    pub fn set_pin_state(&mut self, pin: usize, state: PinState) -> Result<(), String> {
        if pin >= GPIO_PIN_COUNT {
            return Err(format!("Invalid pin: {}", pin));
        }
        self.pins[pin].state = state;
        Ok(())
    }

    pub fn get_pin_state(&self, pin: usize) -> Result<PinState, String> {
        if pin >= GPIO_PIN_COUNT {
            return Err(format!("Invalid pin: {}", pin));
        }
        Ok(self.pins[pin].state)
    }

    pub fn toggle_pin(&mut self, pin: usize) -> Result<(), String> {
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

#[derive(Clone)]
pub struct BleController {
    pub state: BleState,
    pub address: BleAddress,
    pub connected_device: Option<String>,
    pub tx_count: usize,
    pub rx_count: usize,
}

impl BleController {
    pub fn new() -> Self {
        Self {
            state: BleState::Disconnected,
            address: BleAddress::static_address(),
            connected_device: None,
            tx_count: 0,
            rx_count: 0,
        }
    }

    pub fn start_advertising(&mut self) {
        self.state = BleState::Advertising;
    }

    pub fn stop_advertising(&mut self) {
        if matches!(self.state, BleState::Advertising) {
            self.state = BleState::Disconnected;
        }
    }

    pub fn connect(&mut self, device_name: &str) {
        self.state = BleState::Connected;
        self.connected_device = Some(device_name.to_string());
    }

    pub fn disconnect(&mut self) {
        self.state = BleState::Disconnected;
        self.connected_device = None;
    }

    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        if !matches!(self.state, BleState::Connected) {
            return Err("Not connected".to_string());
        }
        self.tx_count += data.len();
        Ok(())
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

pub struct SimTemperatureSensor {
    base_temp: f32,
    iteration: u64,
}

impl SimTemperatureSensor {
    pub fn new() -> Self {
        Self {
            base_temp: 25.0,
            iteration: 0,
        }
    }

    pub fn update(&mut self) {
        self.iteration += 1;
    }

    pub fn read(&self) -> f32 {
        let noise = (self.iteration as f32 * 0.1).sin() * 0.5;
        self.base_temp + noise
    }
}

impl Default for SimTemperatureSensor {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SimAccelerometer {
    x: f32,
    y: f32,
    z: f32,
    iteration: u64,
}

impl SimAccelerometer {
    pub fn new() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 9.8,
            iteration: 0,
        }
    }

    pub fn update(&mut self) {
        self.iteration += 1;
        let t = self.iteration as f32;
        let noise = || (t * 17.3).sin() * 0.01;
        self.x = noise();
        self.y = noise();
        self.z = 9.8 + noise();
    }

    pub fn read(&self) -> (f32, f32, f32) {
        (self.x, self.y, self.z)
    }
}

impl Default for SimAccelerometer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SimHeartRateSensor {
    base_rate: u16,
    iteration: u64,
}

impl SimHeartRateSensor {
    pub fn new() -> Self {
        Self {
            base_rate: 72,
            iteration: 0,
        }
    }

    pub fn update(&mut self) {
        self.iteration += 1;
        let variation = ((self.iteration as f32 * 0.05).sin() * 5.0) as i16;
        self.base_rate = (70 + variation).max(50).min(180) as u16;
    }

    pub fn read(&self) -> HeartRateMeasurement {
        HeartRateMeasurement {
            rate: self.base_rate,
            sensor_contact: true,
            energy: 0,
        }
    }
}

impl Default for SimHeartRateSensor {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SimSpO2Sensor {
    saturation: f32,
    iteration: u64,
}

impl SimSpO2Sensor {
    pub fn new() -> Self {
        Self {
            saturation: 98.0,
            iteration: 0,
        }
    }

    pub fn update(&mut self) {
        self.iteration += 1;
        let variation = ((self.iteration as f32 * 0.02).sin() * 0.5);
        self.saturation = (98.0 + variation).max(90.0).min(100.0);
    }

    pub fn read(&self) -> SpO2Measurement {
        SpO2Measurement {
            saturation: self.saturation,
            confidence: 95,
        }
    }
}

impl Default for SimSpO2Sensor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Main nRF52840 Simulator
// ============================================================================

pub struct Nrf52Simulator {
    pub flash: SimulatedFlash,
    pub gpio: GpioController,
    pub ble: BleController,
    pub temperature_sensor: SimTemperatureSensor,
    pub accelerometer: SimAccelerometer,
    pub heart_rate_sensor: SimHeartRateSensor,
    pub spo2_sensor: SimSpO2Sensor,
    pub battery: BatteryState,
    pub power_mode: PowerMode,
}

impl Nrf52Simulator {
    pub fn new() -> Self {
        Self {
            flash: SimulatedFlash::new(FLASH_SIZE),
            gpio: GpioController::new(),
            ble: BleController::new(),
            temperature_sensor: SimTemperatureSensor::new(),
            accelerometer: SimAccelerometer::new(),
            heart_rate_sensor: SimHeartRateSensor::new(),
            spo2_sensor: SimSpO2Sensor::new(),
            battery: BatteryState::new(),
            power_mode: PowerMode::Active,
        }
    }

    pub fn tick(&mut self) {
        self.temperature_sensor.update();
        self.accelerometer.update();
        self.heart_rate_sensor.update();
        self.spo2_sensor.update();
    }

    pub fn set_power_mode(&mut self, mode: PowerMode) {
        self.power_mode = mode;
    }

    pub fn get_power_mode(&self) -> PowerMode {
        self.power_mode
    }

    /// Helper used by the original test suite: forward BLE writes to
    /// the controller. Lives here instead of `BleController` so the
    /// controller stays focused on state-machine semantics.
    pub fn ble_send(&mut self, data: &[u8]) -> Result<(), String> {
        self.ble.send(data)
    }
}

impl Default for Nrf52Simulator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_nrf52_simulator_creation() {
    let sim = Nrf52Simulator::new();
    assert_eq!(sim.get_power_mode(), PowerMode::Active);
}

#[test]
fn test_gpio_pin_operations() {
    let mut sim = Nrf52Simulator::new();

    sim.gpio.set_pin_state(13, PinState::High).unwrap();
    assert_eq!(sim.gpio.get_pin_state(13).unwrap(), PinState::High);

    sim.gpio.toggle_pin(13).unwrap();
    assert_eq!(sim.gpio.get_pin_state(13).unwrap(), PinState::Low);
}

#[test]
fn test_gpio_invalid_pin() {
    let mut sim = Nrf52Simulator::new();

    let result = sim.gpio.set_pin_state(100, PinState::High);
    assert!(result.is_err());
}

#[test]
fn test_temperature_sensor() {
    let sim = Nrf52Simulator::new();
    let temp = sim.temperature_sensor.read();
    assert!(temp > 20.0 && temp < 30.0);
}

#[test]
fn test_accelerometer() {
    let mut sim = Nrf52Simulator::new();

    for _ in 0..100 {
        sim.tick();
    }

    let (x, y, z) = sim.accelerometer.read();
    assert!(z > 9.0 && z < 11.0);
}

#[test]
fn test_heart_rate_sensor() {
    let mut sim = Nrf52Simulator::new();

    for _ in 0..50 {
        sim.tick();
    }

    let hr = sim.heart_rate_sensor.read();
    assert!(hr.rate >= 50 && hr.rate <= 180);
    assert!(hr.sensor_contact);
}

#[test]
fn test_spo2_sensor() {
    let mut sim = Nrf52Simulator::new();

    for _ in 0..30 {
        sim.tick();
    }

    let spo2 = sim.spo2_sensor.read();
    assert!(spo2.saturation >= 90.0 && spo2.saturation <= 100.0);
}

#[test]
fn test_flash_write_and_read() {
    let mut sim = Nrf52Simulator::new();

    let test_data = b"Hello, mAgent!";
    sim.flash.write(0, test_data).unwrap();

    let mut read_buf = vec![0u8; test_data.len()];
    sim.flash.read(0, &mut read_buf).unwrap();

    assert_eq!(&read_buf, test_data);
}

#[test]
fn test_flash_wear_tracking() {
    let mut sim = Nrf52Simulator::new();

    for i in 0..10 {
        let data = [i as u8; 64];
        sim.flash.write(i * 64, &data).unwrap();
    }

    let writes = sim.flash.get_sector_writes(0);
    assert_eq!(writes, 10);
}

#[test]
fn test_ble_disconnected() {
    let sim = Nrf52Simulator::new();
    assert!(matches!(sim.ble.state, BleState::Disconnected));
}

#[test]
fn test_ble_advertising() {
    let mut sim = Nrf52Simulator::new();

    sim.ble.start_advertising();
    assert!(matches!(sim.ble.state, BleState::Advertising));

    sim.ble.stop_advertising();
    assert!(matches!(sim.ble.state, BleState::Disconnected));
}

#[test]
fn test_ble_connection() {
    let mut sim = Nrf52Simulator::new();

    sim.ble.connect("iPhone");
    assert!(matches!(sim.ble.state, BleState::Connected));
    assert_eq!(sim.ble.connected_device.as_deref(), Some("iPhone"));
}

#[test]
fn test_ble_send_when_connected() {
    let mut sim = Nrf52Simulator::new();

    sim.ble.connect("TestDevice");
    let result = sim.ble_send(b"Hello");
    assert!(result.is_ok());
}

#[test]
fn test_ble_send_when_disconnected() {
    let mut sim = Nrf52Simulator::new();

    let result = sim.ble_send(b"Hello");
    assert!(result.is_err());
}

#[test]
fn test_ble_address() {
    let sim = Nrf52Simulator::new();

    let addr = &sim.ble.address;
    assert_eq!(addr.bytes, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
}

#[test]
fn test_power_mode() {
    let mut sim = Nrf52Simulator::new();

    sim.set_power_mode(PowerMode::Idle);
    assert_eq!(sim.get_power_mode(), PowerMode::Idle);

    sim.set_power_mode(PowerMode::LowPower);
    assert_eq!(sim.get_power_mode(), PowerMode::LowPower);

    sim.set_power_mode(PowerMode::Active);
    assert_eq!(sim.get_power_mode(), PowerMode::Active);
}

#[test]
fn test_battery_state() {
    let mut battery = BatteryState::new();

    assert_eq!(battery.percentage, 85);
    assert!(!battery.low_battery);

    battery.drain(70);
    assert!(battery.low_battery);
    assert!(battery.percentage < 20);
}

#[test]
fn test_multiple_sensor_updates() {
    let mut sim = Nrf52Simulator::new();

    // Update all sensors multiple times
    for _ in 0..100 {
        sim.tick();
    }

    // Verify sensors are still functional
    let temp = sim.temperature_sensor.read();
    assert!(temp > 20.0 && temp < 30.0);

    let hr = sim.heart_rate_sensor.read();
    assert!(hr.rate >= 50 && hr.rate <= 180);

    let spo2 = sim.spo2_sensor.read();
    assert!(spo2.saturation >= 90.0);
}
